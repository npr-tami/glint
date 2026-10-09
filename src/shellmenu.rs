//! Windows' own right-click menu - the "shell" menu, with Open with ›, Send to ›, 7-Zip, Git Bash,
//! Scan with Microsoft Defender and whatever else other apps add - for Glint's right-click menus.
//!
//! How it works:
//! - Windows' menu is made by objects (IContextMenu) that Windows and other apps install. Asking
//!   them for their items loads their code and can take a while: a cold start, a slow app, a folder
//!   on a sleeping network drive. So it never happens on the UI thread. One "shell thread" does it.
//!   It starts the first time a menu is wanted, lives as long as Glint, and sleeps in GetMessage
//!   between jobs (no CPU while idle).
//! - The objects can only be used on the thread that made them, so everything about one menu
//!   happens there: building it, filling its submenus, running the command picked from it, and
//!   letting it go. The thread is initialised with OleInitialize, because Windows' own Copy and Cut
//!   put files on the clipboard through OLE, which needs that (and a thread that stays).
//! - Each menu is a "session" with a number. The window asks with [`request`]; when the menu is
//!   built the thread posts WM_APP_SHELL_MENU (wparam = that number) to the window, which collects
//!   it with [`take`]: the items, read as plain data, and Windows' own menu handle.
//! - The popup itself is shown by the UI thread, owned by Glint's window like every other menu (so
//!   the window stays active and keeps painting while it's open). Some items need their maker while
//!   the menu is open: "Send to" fills itself when it opens (WM_INITMENUPOPUP), and many apps draw
//!   their own icons (WM_MEASUREITEM, WM_DRAWITEM). Those messages are handed over with [`forward`]:
//!   a SendMessage to the shell thread, which passes them to the session's IContextMenu3/2. The UI
//!   thread waits for that, but only while the menu itself is open, and never longer than
//!   FORWARD_WAIT_MS (SUBMENU_WAIT_MS for a submenu about to open).
//! - The picked command runs on the shell thread too ([`invoke`]); its session is let go a minute
//!   later (LINGER), as some commands finish their work after the click.
//!   Properties windows ([`properties`]) are opened from there as well, since the thread stays.
//!
//! If the thread can't be started, or a menu can't be built (Wine, a path the shell doesn't know,
//! a folder that is gone), Glint's menus simply show without Windows' items.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{w, Interface, PCSTR, PCWSTR, PSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IDataObject};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleFlushClipboard, OleInitialize, OleUninitialize};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IContextMenu3, IShellFolder, SHBindToObject, SHBindToParent, SHCreateDataObject,
    SHMultiFileProperties, SHParseDisplayName, CMF_CANRENAME, CMF_EXPLORE, CMF_EXTENDEDVERBS, CMF_NORMAL,
    CMIC_MASK_CONTROL_DOWN, CMIC_MASK_PTINVOKE, CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, GCS_VERBW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, GetMenuItemCount, GetMenuItemID,
    GetMenuItemInfoW, GetSubMenu, GetMessageW, PostMessageW, PostQuitMessage, RegisterClassExW, SendMessageTimeoutW, TranslateMessage, HMENU,
    HWND_MESSAGE, MENUITEMINFOW, MFT_OWNERDRAW, MFT_SEPARATOR, MIIM_BITMAP, MIIM_CHECKMARKS, MIIM_DATA, MIIM_FTYPE, MIIM_ID,
    MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, MSG, SMTO_ABORTIFHUNG, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_DRAWITEM, WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR, WNDCLASSEXW,
};
// Leaving out items Windows lists twice (`build`).
use windows::Win32::Graphics::Gdi::{GetBitmapBits, GetObjectW, BITMAP, HGDIOBJ};
use windows::Win32::UI::WindowsAndMessaging::{DeleteMenu, RemoveMenu, MF_BYCOMMAND, MF_BYPOSITION};

use crate::folder::{self, wide};
use crate::log;
use crate::shell;
use crate::ui::{ShellRequest, ShellTarget};

/// The command ids Windows' items get. Glint's own items in the same menu stay below SHELL_FIRST.
pub const SHELL_FIRST: u32 = 0x1000;
pub const SHELL_LAST: u32 = 0x7FFF;

/// Posted to the main window when a menu asked for with [`request`] is ready (wparam = its number).
pub const WM_APP_SHELL_MENU: u32 = WM_APP + 20;

/// On the shell thread's own window: "there is work in the queue".
const WM_SHELL_JOB: u32 = WM_APP + 1;
/// On the shell thread's own window, sent: lparam points to a `Forward`.
const WM_SHELL_FORWARD: u32 = WM_APP + 2;

/// Longest the UI thread waits for the shell thread to handle one menu message (a hung app's code
/// would otherwise hang the menu). The item it was about just stays empty.
const FORWARD_WAIT_MS: u32 = 1000;
/// The same for a submenu about to open (WM_INITMENUPOPUP): filling one can take a few seconds on
/// a slower PC ("Send to", "Give access to", an archiver's list), and a submenu that isn't filled
/// in time doesn't open at all.
const SUBMENU_WAIT_MS: u32 = 8000;

/// The shell thread is handling a forwarded menu message right now (one the UI thread may have
/// stopped waiting for).
static IN_FORWARD: AtomicBool = AtomicBool::new(false);

/// Whether the shell thread is still busy with a forwarded menu message.
pub fn forward_in_progress() -> bool {
    IN_FORWARD.load(Ordering::SeqCst)
}

/// At most this many items are handed to Windows for one menu (Explorer has a similar limit: a
/// menu for 50,000 files would take very long to build and nobody reads it).
const MAX_ITEMS: usize = 2000;

/// This PC, for the empty-space menu there.
const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";

/// CMIC_MASK_UNICODE (the windows crate doesn't name it): the wide fields of
/// CMINVOKECOMMANDINFOEX are filled.
const CMIC_MASK_UNICODE: u32 = 0x4000;
/// CMIC_MASK_ASYNCOK: the command may finish its work after InvokeCommand returns (as Explorer
/// lets it); archivers and "Send to" often do.
const CMIC_MASK_ASYNCOK: u32 = 0x0010_0000;
/// How long a menu whose command ran is kept alive afterwards, for commands that finish their work
/// later (they may still use the files and objects the menu gave them).
const LINGER: std::time::Duration = std::time::Duration::from_secs(60);

/// One item of Windows' menu, read on the shell thread so the UI thread can copy it into Glint's
/// menu (popup.rs). The handles belong to the session and stay valid until it's released.
#[derive(Clone, Debug, Default)]
pub struct ShellItem {
    /// The command id (SHELL_FIRST and up for commands).
    pub id: u32,
    /// The text as Windows has it: "&Open with Code", maybe with "\tCtrl+X". Empty for separators
    /// and for items the app draws itself.
    pub text: String,
    /// The language-neutral name of the command ("open", "properties", "rename"...), if it has one.
    pub verb: String,
    /// MENUITEMINFOW's fType / fState.
    pub ftype: u32,
    pub fstate: u32,
    /// Its submenu (Send to, Open with...), owned by the session.
    pub submenu: isize,
    /// The picture: a bitmap, or HBMMENU_CALLBACK (-1) when the app draws it on request.
    pub bitmap: isize,
    pub checked_bitmap: isize,
    pub unchecked_bitmap: isize,
    /// The app's own data for items it draws itself.
    pub data: usize,
    /// A fingerprint of the picture (`picture_of`), to tell the same item listed twice apart from
    /// two items that only share a name (menu::repeats): 0 = none, PICTURE_UNKNOWN = the app draws
    /// it itself, so it can't be compared.
    pub picture: u64,
    /// How many entries its submenu had when the menu was read (0 for no submenu). Some submenus
    /// are filled only when they open ("Send to"); an app's Windows 11 entry can come with an empty
    /// one that only File Explorer's own menu fills (menu::repeats keeps the other copy then).
    pub sub_items: u32,
}

/// See [`ShellItem::picture`].
pub const PICTURE_UNKNOWN: u64 = u64::MAX;

impl ShellItem {
    pub fn is_separator(&self) -> bool {
        self.ftype & MFT_SEPARATOR.0 != 0
    }
    pub fn is_owner_drawn(&self) -> bool {
        self.ftype & MFT_OWNERDRAW.0 != 0
    }
}

/// A Windows menu ready to show: what [`take`] gives the window.
#[derive(Debug)]
pub struct Built {
    pub session: u64,
    /// Windows' whole menu. It belongs to the shell thread: show it, never destroy it.
    pub hmenu: isize,
    /// Its top-level items, in order (for merging into Glint's menu).
    pub items: Vec<ShellItem>,
}

impl Built {
    /// The verb of a top-level command ("" when unknown or in a submenu).
    pub fn verb_of(&self, cmd: u32) -> &str {
        self.items.iter().find(|i| i.id == cmd && i.submenu == 0).map_or("", |i| i.verb.as_str())
    }

    /// Whether command `cmd` sits in the submenu of a top-level item called `label` (compared as
    /// menu::label_key gives it: "new" finds "Ne&w"). Asked on the UI thread right after the pick,
    /// while the session (and so its menus) is still alive; a menu can be read from any thread.
    pub fn in_submenu_of(&self, label: &str, cmd: u32) -> bool {
        self.items
            .iter()
            .filter(|i| i.submenu != 0 && crate::menu::label_key(&i.text) == label)
            .any(|i| menu_has_command(HMENU(i.submenu as *mut _), cmd, 0))
    }
}

/// Whether `cmd` is a command of `menu` or of its submenus (a few levels deep).
fn menu_has_command(menu: HMENU, cmd: u32, depth: u32) -> bool {
    if depth > 4 {
        return false;
    }
    // SAFETY: plain queries on a menu the live session owns.
    let n = unsafe { GetMenuItemCount(Some(menu)) }.max(0);
    (0..n).any(|pos| {
        // SAFETY: as above; GetMenuItemID gives u32::MAX for an item that opens a submenu.
        let (id, sub) = unsafe { (GetMenuItemID(menu, pos), GetSubMenu(menu, pos)) };
        id == cmd || (!sub.is_invalid() && menu_has_command(sub, cmd, depth + 1))
    })
}

/// Work for the shell thread.
enum Job {
    Build { session: u64, owner: isize, req: ShellRequest },
    Invoke { session: u64, owner: isize, cmd: u32, point: (i32, i32), shift: bool, ctrl: bool },
    Release(u64),
    Properties { owner: isize, paths: Vec<String> },
    Quit,
}

/// A menu message handed from the UI thread to the shell thread. It travels as an
/// `Arc<Mutex<Forward>>` (WM_SHELL_FORWARD's lparam), not as a pointer into the UI thread's stack:
/// if an app's code takes too long, the UI thread stops waiting and moves on, and the shell thread
/// must still be able to finish with it safely. For the same reason the item structs are copies.
#[derive(Clone, Default)]
struct Forward {
    session: u64,
    msg: u32,
    wparam: usize,
    lparam: isize,
    /// WM_MEASUREITEM / WM_DRAWITEM: a copy of the struct lparam points to.
    measure: Option<MEASUREITEMSTRUCT>,
    draw: Option<DRAWITEMSTRUCT>,
    result: isize,
    handled: bool,
}

// SAFETY: what makes Forward not Send are the window and device-context handles in DRAWITEMSTRUCT.
// Those are process-wide handles (numbers), handed to the shell thread to draw with while the menu
// is open, exactly as Windows hands them to a window procedure.
unsafe impl Send for Forward {}

static JOBS: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
/// Built menus (None: it couldn't be built) waiting for the window to take them.
static READY: Mutex<Vec<(u64, Option<Built>)>> = Mutex::new(Vec::new());
/// The shell thread's window (0 until it exists).
static THREAD_WINDOW: AtomicIsize = AtomicIsize::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
/// The thread couldn't be started or couldn't make its window: no Windows menus this run.
static BROKEN: AtomicBool = AtomicBool::new(false);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
/// When the job the shell thread is doing now began (milliseconds on `clock`), 0 while it waits.
static BUSY_SINCE: AtomicU64 = AtomicU64::new(0);

/// A job busy for longer than this (Windows' own Delete with its progress window runs on the
/// shell thread until it's done, say): menus don't wait for Windows' items meanwhile.
const BUSY_LIMIT_MS: u64 = 500;

/// Milliseconds since the first call (never 0).
fn clock() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Nothing panics while holding these locks, but if something ever did, the data is still fine.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Queues a job and wakes the thread. False if there is no shell thread.
fn send(job: Job) -> bool {
    if BROKEN.load(Ordering::SeqCst) {
        return false;
    }
    if !STARTED.swap(true, Ordering::SeqCst) && !shell::background("Windows menus", thread_main) {
        BROKEN.store(true, Ordering::SeqCst);
        log::line("shell menus: no thread could be started; menus show without Windows' items");
        return false;
    }
    lock(&JOBS).push_back(job);
    // If the window isn't there yet, the thread looks at the queue as soon as it is.
    let h = THREAD_WINDOW.load(Ordering::SeqCst);
    if h != 0 {
        // SAFETY: posting to a window handle is safe even if the window is gone (the call fails).
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_SHELL_JOB, WPARAM(0), LPARAM(0));
        }
    }
    true
}

// ------------------------------------------------------------ the UI thread's side

/// Asks for Windows' menu. `owner` (the main window) gets WM_APP_SHELL_MENU with the returned
/// number when it's ready - or when it couldn't be built. None: there is no shell thread.
pub fn request(owner: HWND, req: ShellRequest) -> Option<u64> {
    let busy = BUSY_SINCE.load(Ordering::SeqCst);
    if busy != 0 && clock().saturating_sub(busy) > BUSY_LIMIT_MS {
        log::line("shell menu: the shell thread is still busy with a command; this menu shows without Windows' items");
        return None;
    }
    let session = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
    send(Job::Build { session, owner: owner.0 as isize, req }).then_some(session)
}

/// The menu for `session` once it has arrived: Some(Some(menu)), or Some(None) if Windows gave
/// none. None while it's still being built.
pub fn take(session: u64) -> Option<Option<Built>> {
    let mut ready = lock(&READY);
    let i = ready.iter().position(|(s, _)| *s == session)?;
    Some(ready.swap_remove(i).1)
}

/// Runs command `cmd` (picked from the session's menu) on the shell thread, then lets the session
/// go. `point` is where the menu was (screen pixels): some commands open their window there.
pub fn invoke(owner: HWND, session: u64, cmd: u32, point: POINT, shift: bool, ctrl: bool) {
    send(Job::Invoke { session, owner: owner.0 as isize, cmd, point: (point.x, point.y), shift, ctrl });
}

/// Lets a session go (its menu closed without a Windows command picked).
pub fn release(session: u64) {
    send(Job::Release(session));
}

/// Windows' Properties window for these items (all in one folder) or this folder.
pub fn properties(owner: HWND, paths: Vec<String>) -> bool {
    !paths.is_empty() && send(Job::Properties { owner: owner.0 as isize, paths })
}

/// What became of a menu message handed to the shell thread (see [`forward`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Forwarded {
    /// Windows' item handled it; this is the answer for the menu.
    Handled(LRESULT),
    /// Nobody there wanted it.
    NotHandled,
    /// The shell thread didn't answer within FORWARD_WAIT_MS (an app's code hangs, or the thread
    /// is stuck in a long command): asking again for every item would freeze the menu a second each.
    TimedOut,
}

/// While a session's menu is open: hands a menu message to its maker on the shell thread.
pub fn forward(session: u64, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Forwarded {
    let h = THREAD_WINDOW.load(Ordering::SeqCst);
    if h == 0 || !is_menu_message(msg, lparam) {
        return Forwarded::NotHandled;
    }
    let mut f = Forward { session, msg, wparam: wparam.0, lparam: lparam.0, ..Default::default() };
    // SAFETY: for these two messages lparam points to the struct Windows passed (checked non-null
    // by is_menu_message).
    unsafe {
        match msg {
            WM_MEASUREITEM => f.measure = Some(*(lparam.0 as *const MEASUREITEMSTRUCT)),
            WM_DRAWITEM => f.draw = Some(*(lparam.0 as *const DRAWITEMSTRUCT)),
            _ => {}
        }
    }
    let shared = Arc::new(Mutex::new(f));
    let theirs = Arc::into_raw(shared.clone());
    let mut answer = 0usize;
    // SAFETY: the shell thread takes over `theirs` (Arc::from_raw) when the message arrives.
    let sent = unsafe {
        let wait = if msg == WM_INITMENUPOPUP { SUBMENU_WAIT_MS } else { FORWARD_WAIT_MS };
        SendMessageTimeoutW(HWND(h as *mut _), WM_SHELL_FORWARD, WPARAM(0), LPARAM(theirs as isize), SMTO_ABORTIFHUNG, wait, Some(&mut answer))
    };
    if sent.0 == 0 {
        // Timed out (the message may still be handled later; it then has its own copy) or never
        // delivered (then that one copy is never freed: a few bytes).
        return Forwarded::TimedOut;
    }
    let f = lock(&shared).clone();
    if !f.handled {
        return Forwarded::NotHandled;
    }
    if let Some(m) = f.measure {
        // SAFETY: as above: the menu's own MEASUREITEMSTRUCT, still waiting for its answer.
        unsafe {
            let out = &mut *(lparam.0 as *mut MEASUREITEMSTRUCT);
            out.itemWidth = m.itemWidth;
            out.itemHeight = m.itemHeight;
        }
    }
    Forwarded::Handled(LRESULT(f.result))
}

/// Glint is closing: whatever Windows' Copy or Cut put on the clipboard stays there after Glint ends
/// (OleFlushClipboard), and the thread lets everything go. Waits at most a moment.
pub fn shutdown() {
    let h = THREAD_WINDOW.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    lock(&JOBS).push_back(Job::Quit);
    let mut answer = 0usize;
    // SAFETY: a plain synchronous message to our own thread's window; nothing points into our stack.
    unsafe {
        let _ = SendMessageTimeoutW(HWND(h as *mut _), WM_SHELL_JOB, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 1500, Some(&mut answer));
    }
}

/// Whether a menu message is one the shell's items may need while the menu is open.
pub fn is_menu_message(msg: u32, lparam: LPARAM) -> bool {
    const ODT_MENU: u32 = 1;
    match msg {
        WM_INITMENUPOPUP | WM_MENUCHAR => true,
        // Only the ones about menu items (both structs start with CtlType).
        // SAFETY: for these two messages lparam points to a MEASUREITEMSTRUCT / DRAWITEMSTRUCT.
        WM_MEASUREITEM | WM_DRAWITEM => lparam.0 != 0 && unsafe { *(lparam.0 as *const u32) } == ODT_MENU,
        _ => false,
    }
}

// ------------------------------------------------------------ the shell thread

/// A Windows menu kept alive on the shell thread until its command ran or it was released.
struct Session {
    hmenu: HMENU,
    menu: IContextMenu,
    menu2: Option<IContextMenu2>,
    menu3: Option<IContextMenu3>,
    /// The folder the items are in (commands start there), NUL-terminated; empty for none.
    dir: Vec<u16>,
    /// Kept so it outlives the menu made from it; let go after it.
    _folder: IShellFolder,
    /// Item ids, freed last.
    _pidls: Vec<Pidl>,
    /// Submenus of items taken out of the menu because they repeat another one (`build`): kept
    /// until the session ends, since the app that made them may still hold them.
    orphans: Vec<HMENU>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the menu was made by this session (CreatePopupMenu) on this thread; destroying it
        // also destroys its submenus, which the UI thread no longer uses (it took them out of its
        // own menu before it let the session go). The orphans were part of it and are destroyed
        // with it, once.
        unsafe {
            let _ = DestroyMenu(self.hmenu);
            for sub in self.orphans.drain(..) {
                let _ = DestroyMenu(sub);
            }
        }
    }
}

/// An item id list from SHParseDisplayName, freed when dropped.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: allocated by the shell with the COM allocator, freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

fn parse(path: &str) -> Option<Pidl> {
    let w = wide(path);
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `w` is NUL-terminated and outlives the call; `pidl` is a valid out-pointer.
    unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }.ok()?;
    (!pidl.is_null()).then_some(Pidl(pidl))
}

thread_local! {
    static SESSIONS: RefCell<HashMap<u64, Session>> = RefCell::new(HashMap::new());
    /// Menus whose command ran, kept until LINGER has passed (let go at the next job after that,
    /// or when Glint closes).
    static LINGERING: RefCell<Vec<(std::time::Instant, Session)>> = const { RefCell::new(Vec::new()) };
    /// A job is running (a job can pump messages and so meet WM_SHELL_JOB again: that one leaves
    /// the queue to the loop already running).
    static RUNNING: Cell<bool> = const { Cell::new(false) };
}

fn thread_main() {
    // SAFETY: OleInitialize pairs with OleUninitialize at the end of this thread. It makes this a
    // single-threaded apartment, as shell extensions expect, and lets Windows' Copy use the clipboard.
    let ole = unsafe { OleInitialize(None) }.is_ok();
    let Some(hwnd) = make_window() else {
        BROKEN.store(true, Ordering::SeqCst);
        log::line("shell menus: the thread's window couldn't be made; menus show without Windows' items");
        if ole {
            // SAFETY: matches the OleInitialize above.
            unsafe { OleUninitialize() };
        }
        return;
    };
    THREAD_WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
    run_jobs();
    let mut msg = MSG::default();
    // SAFETY: the standard message loop of this thread. It also serves windows that shell
    // extensions open on this thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    THREAD_WINDOW.store(0, Ordering::SeqCst);
    SESSIONS.with(|s| s.borrow_mut().clear());
    // SAFETY: our own window; then OLE is closed down as it was started.
    unsafe {
        let _ = DestroyWindow(hwnd);
        if ole {
            OleUninitialize();
        }
    }
}

/// The thread's message-only window: jobs are posted to it and menu messages sent to it.
fn make_window() -> Option<HWND> {
    // SAFETY: standard class registration and window creation on this thread.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(thread_wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("GlintShellMenus"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GlintShellMenus"),
            w!(""),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance.into()),
            None,
        )
        .ok()
    }
}

unsafe extern "system" fn thread_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SHELL_JOB => {
            run_jobs();
            LRESULT(0)
        }
        WM_SHELL_FORWARD if lparam.0 != 0 => {
            // SAFETY: `forward` made this with Arc::into_raw and handed its count to us.
            let shared = unsafe { Arc::from_raw(lparam.0 as *const Mutex<Forward>) };
            // Worked on as a copy, so the lock isn't held while an app's code runs.
            let mut f = lock(&shared).clone();
            IN_FORWARD.store(true, Ordering::SeqCst);
            handle_forward(&mut f);
            IN_FORWARD.store(false, Ordering::SeqCst);
            *lock(&shared) = f;
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn run_jobs() {
    if RUNNING.with(|r| r.replace(true)) {
        return; // the loop below, further up the stack, takes the job
    }
    loop {
        let Some(job) = lock(&JOBS).pop_front() else { break };
        BUSY_SINCE.store(clock(), Ordering::SeqCst);
        // Menus whose command ran long enough ago are let go now (taken out first, then dropped,
        // so no borrow is held while their makers' code runs).
        let done: Vec<Session> = LINGERING.with(|l| {
            let mut l = l.borrow_mut();
            let (old, keep): (Vec<_>, Vec<_>) = l.drain(..).partition(|(at, _)| at.elapsed() >= LINGER);
            *l = keep;
            old.into_iter().map(|(_, s)| s).collect()
        });
        drop(done);
        match job {
            Job::Build { session, owner, req } => {
                if let Some(wait) = test_delay() {
                    std::thread::sleep(wait);
                }
                let started = std::time::Instant::now();
                let built = build(session, HWND(owner as *mut _), &req);
                log::line(&format!(
                    "shell menu {session}: {} in {:.1} ms",
                    match &built {
                        Some(b) => format!("{} items", b.items.len()),
                        None => "none".to_string(),
                    },
                    started.elapsed().as_secs_f64() * 1000.0
                ));
                lock(&READY).push((session, built));
                // SAFETY: posting to a window handle is safe even if the window is gone.
                let posted = unsafe { PostMessageW(Some(HWND(owner as *mut _)), WM_APP_SHELL_MENU, WPARAM(session as usize), LPARAM(0)) };
                if posted.is_err() {
                    // Nobody will take it.
                    if let Some(Some(b)) = take(session) {
                        SESSIONS.with(|s| s.borrow_mut().remove(&b.session));
                    }
                }
            }
            Job::Invoke { session, owner, cmd, point, shift, ctrl } => {
                // Taken out of the table first: the command may pump messages (and so run other
                // jobs) while it works.
                let s = SESSIONS.with(|s| s.borrow_mut().remove(&session));
                if let Some(s) = s {
                    if (SHELL_FIRST..=SHELL_LAST).contains(&cmd) {
                        let offset = (cmd - SHELL_FIRST) as usize;
                        let keys = Keys { point: Some(point), shift, ctrl };
                        let verb = verb(&s.menu, offset as u32);
                        match invoke_command(&s, HWND(owner as *mut _), Verb::Offset(offset), keys) {
                            Ok(()) => log::line(&format!("shell menu {session}: ran command {offset} (verb \"{verb}\")")),
                            // Cancelled by the person (a UAC prompt, say) is not worth a word.
                            Err(e) if e.code().0 as u32 == 0x800704C7 => {}
                            Err(e) => log::line(&format!("shell menu {session}: command {offset} (verb \"{verb}\") failed: {e}")),
                        }
                    }
                    LINGERING.with(|l| l.borrow_mut().push((std::time::Instant::now(), s)));
                }
            }
            Job::Release(session) => {
                let s = SESSIONS.with(|s| s.borrow_mut().remove(&session));
                drop(s);
            }
            Job::Properties { owner, paths } => show_properties(HWND(owner as *mut _), &paths),
            Job::Quit => {
                LINGERING.with(|l| l.borrow_mut().clear());
                SESSIONS.with(|s| s.borrow_mut().clear());
                // SAFETY: keeps what Windows' Copy put on the clipboard there after Glint ends; then
                // ends this thread's message loop.
                unsafe {
                    let _ = OleFlushClipboard();
                    PostQuitMessage(0);
                }
            }
        }
    }
    BUSY_SINCE.store(0, Ordering::SeqCst);
    RUNNING.with(|r| r.set(false));
}

/// Builds Windows' menu for a request and keeps the session. None when Windows has none for it.
fn build(session: u64, owner: HWND, req: &ShellRequest) -> Option<Built> {
    let mut s = make_session(owner, req)?;
    // SAFETY: the session's menu was just filled by its IContextMenu.
    let mut items = unsafe { read_items(&s.menu, s.hmenu) };
    // The same item twice (an app whose old and new menu handlers both add it, like PowerToys'
    // "Rename with PowerRename"): the second goes, from Windows' whole menu too.
    let repeats = crate::menu::repeats(&items);
    if !repeats.is_empty() {
        let names: Vec<String> = repeats.iter().map(|&i| format!("\"{}\" ({} in its submenu)", crate::menu::label_key(&items[i].text), items[i].sub_items)).collect();
        log::line(&format!("shell menu {session}: leaving out {}", names.join(", ")));
        // SAFETY: the session's own menu, on this thread; the items list was just read from it.
        unsafe {
            for &i in repeats.iter().rev() {
                remove_item(&mut s, &items[i]);
            }
            tidy_separators(s.hmenu);
            items = read_items(&s.menu, s.hmenu);
        }
        log::line(&format!("shell menu {session}: {} repeated item(s) left out", repeats.len()));
    }
    let hmenu = s.hmenu.0 as isize;
    SESSIONS.with(|all| all.borrow_mut().insert(session, s));
    Some(Built { session, hmenu, items })
}

/// Asks Windows for its menu: the IContextMenu for the items (or the folder's empty space), and
/// a popup menu it filled.
fn make_session(owner: HWND, req: &ShellRequest) -> Option<Session> {
    let (folder, menu, pidls, dir) = match &req.target {
        ShellTarget::Items(paths) => {
            let mut pidls = Vec::new();
            let mut children: Vec<*const ITEMIDLIST> = Vec::new();
            let mut parent: Option<IShellFolder> = None;
            for p in paths.iter().take(MAX_ITEMS) {
                let Some(pidl) = parse(p) else { continue };
                let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
                // SAFETY: `pidl` is a valid absolute id list; `child` points into it and is only used
                // while `pidl` is alive (it's kept in the session).
                let Ok(f) = (unsafe { SHBindToParent::<IShellFolder>(pidl.0, Some(&mut child)) }) else { continue };
                if child.is_null() {
                    continue;
                }
                // The items all come from one folder (the list's); the first one's folder is used.
                parent.get_or_insert(f);
                children.push(child as *const _);
                pidls.push(pidl);
            }
            let folder = parent?;
            // SAFETY: the child ids are valid (see above) and relative to `folder`.
            let menu: IContextMenu = unsafe { folder.GetUIObjectOf(owner, &children, None) }.ok()?;
            let dir = paths.first().and_then(|p| folder::parent(p)).unwrap_or_default();
            (folder, menu, pidls, dir)
        }
        ShellTarget::Background(path) => {
            let pidl = parse(if path.is_empty() { THIS_PC } else { path })?;
            // SAFETY: a valid absolute id list; binding from the desktop.
            let folder: IShellFolder = unsafe { SHBindToObject(None::<&IShellFolder>, pidl.0, None::<&IBindCtx>) }.ok()?;
            // SAFETY: plain COM call on this thread.
            let menu: IContextMenu = unsafe { folder.CreateViewObject(owner) }.ok()?;
            (folder, menu, vec![pidl], path.clone())
        }
    };
    let mut flags = CMF_NORMAL | CMF_EXPLORE;
    if req.extended {
        flags |= CMF_EXTENDEDVERBS;
    }
    if matches!(req.target, ShellTarget::Items(_)) {
        // Windows then includes "Rename"; picking it opens Glint's own name box (see menu.rs).
        flags |= CMF_CANRENAME;
    }
    // SAFETY: a fresh menu, filled by the shell; destroyed with the session.
    unsafe {
        let hmenu = CreatePopupMenu().ok()?;
        let hr = menu.QueryContextMenu(hmenu, 0, SHELL_FIRST, SHELL_LAST, flags);
        if hr.is_err() {
            let _ = DestroyMenu(hmenu);
            return None;
        }
        Some(Session {
            hmenu,
            menu2: menu.cast().ok(),
            menu3: menu.cast().ok(),
            menu,
            dir: if dir.is_empty() { Vec::new() } else { wide(&dir) },
            _folder: folder,
            _pidls: pidls,
            orphans: Vec::new(),
        })
    }
}

/// The top-level items of a menu Windows filled.
///
/// # Safety
/// `hmenu` must be a valid menu filled by `menu`.
unsafe fn read_items(menu: &IContextMenu, hmenu: HMENU) -> Vec<ShellItem> {
    let mut items = Vec::new();
    // SAFETY: (the whole body) menu queries on a valid menu with properly sized structs and buffers.
    unsafe {
        let n = GetMenuItemCount(Some(hmenu)).max(0) as u32;
        for pos in 0..n {
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_DATA | MIIM_BITMAP | MIIM_CHECKMARKS | MIIM_STRING,
                ..Default::default()
            };
            if GetMenuItemInfoW(hmenu, pos, true, &mut mii).is_err() {
                continue;
            }
            let mut item = ShellItem {
                id: mii.wID,
                ftype: mii.fType.0,
                fstate: mii.fState.0,
                submenu: mii.hSubMenu.0 as isize,
                bitmap: mii.hbmpItem.0 as isize,
                checked_bitmap: mii.hbmpChecked.0 as isize,
                unchecked_bitmap: mii.hbmpUnchecked.0 as isize,
                data: mii.dwItemData,
                picture: picture_of(mii.hbmpItem.0 as isize),
                sub_items: if mii.hSubMenu.is_invalid() { 0 } else { GetMenuItemCount(Some(mii.hSubMenu)).max(0) as u32 },
                ..Default::default()
            };
            if !item.is_separator() && !item.is_owner_drawn() && mii.cch > 0 {
                let mut buf = vec![0u16; mii.cch as usize + 1];
                let mut text = MENUITEMINFOW {
                    cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_STRING,
                    dwTypeData: PWSTR(buf.as_mut_ptr()),
                    cch: buf.len() as u32,
                    ..Default::default()
                };
                if GetMenuItemInfoW(hmenu, pos, true, &mut text).is_ok() {
                    let len = (text.cch as usize).min(buf.len());
                    item.text = String::from_utf16_lossy(&buf[..len]);
                }
            }
            if item.submenu == 0 && (SHELL_FIRST..=SHELL_LAST).contains(&item.id) {
                item.verb = verb(menu, item.id - SHELL_FIRST);
            }
            items.push(item);
        }
    }
    items
}

/// A fingerprint of a menu item's picture (see [`ShellItem::picture`]): 0 for none, the number
/// itself for Windows' standard menu pictures, PICTURE_UNKNOWN when the app draws it when asked
/// (or it can't be read), otherwise a hash of its size and pixels.
fn picture_of(h: isize) -> u64 {
    const HBMMENU_CALLBACK: isize = -1;
    match h {
        0 => return 0,
        HBMMENU_CALLBACK => return PICTURE_UNKNOWN,
        // HBMMENU_SYSTEM ... HBMMENU_POPUP_MINIMIZE: Windows' own, the same number is the same picture.
        1..=11 => return h as u64,
        _ => {}
    }
    let mut bm = BITMAP::default();
    // SAFETY: `bm` is a valid BITMAP to fill; a handle that isn't a bitmap makes the call fail (0).
    let got = unsafe { GetObjectW(HGDIOBJ(h as *mut _), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut _)) };
    let bytes = bm.bmWidthBytes.max(0) as usize * bm.bmHeight.unsigned_abs() as usize;
    if got == 0 || bytes == 0 || bytes > 1 << 20 {
        return PICTURE_UNKNOWN;
    }
    let mut pixels = vec![0u8; bytes];
    // SAFETY: the buffer holds `bytes` bytes, which is what the call may write.
    let read = unsafe { GetBitmapBits(HBITMAP(h as *mut _), bytes as i32, pixels.as_mut_ptr() as *mut _) };
    if read <= 0 {
        return PICTURE_UNKNOWN;
    }
    // FNV-1a over the size and the pixels (a picture of a few hundred bytes: no need for more).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let size = [bm.bmWidth.to_le_bytes(), bm.bmHeight.to_le_bytes(), (bm.bmBitsPixel as i32).to_le_bytes()];
    for b in size.iter().flatten().chain(&pixels[..read as usize]) {
        hash = (hash ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    // (Never one of the special values above.)
    hash.clamp(12, PICTURE_UNKNOWN - 1)
}

/// Takes one item out of the session's menu (a repeat of another, see `build`). A submenu it opens
/// is kept with the session until it ends.
///
/// # Safety
/// `s.hmenu` must be the session's live menu, on this thread.
unsafe fn remove_item(s: &mut Session, item: &ShellItem) {
    // SAFETY: (the whole body) plain menu calls on the session's own menu.
    unsafe {
        if item.submenu == 0 {
            if item.is_separator() {
                return;
            }
            let _ = DeleteMenu(s.hmenu, item.id, MF_BYCOMMAND);
            return;
        }
        let n = GetMenuItemCount(Some(s.hmenu)).max(0);
        if let Some(pos) = (0..n).find(|&pos| GetSubMenu(s.hmenu, pos).0 as isize == item.submenu) {
            if RemoveMenu(s.hmenu, pos as u32, MF_BYPOSITION).is_ok() {
                s.orphans.push(HMENU(item.submenu as *mut _));
            }
        }
    }
}

/// Removes separators that would show at the start or the end of a menu, or two in a row (after
/// items were taken out).
///
/// # Safety
/// `hmenu` must be a live menu of this thread.
unsafe fn tidy_separators(hmenu: HMENU) {
    // SAFETY: (the whole body) plain menu calls on a live menu with a properly sized struct.
    unsafe {
        let is_line = |pos: i32| {
            let mut mii = MENUITEMINFOW { cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_FTYPE, ..Default::default() };
            GetMenuItemInfoW(hmenu, pos as u32, true, &mut mii).is_ok() && mii.fType.0 & MFT_SEPARATOR.0 != 0
        };
        // From the end: a line goes when the item after it is a line too, or when it's last.
        let mut pos = GetMenuItemCount(Some(hmenu)) - 1;
        let mut after_line = true;
        while pos >= 0 {
            let line = is_line(pos);
            if line && after_line {
                let _ = DeleteMenu(hmenu, pos as u32, MF_BYPOSITION);
            } else {
                after_line = line;
            }
            pos -= 1;
        }
        // And a line left at the start.
        if GetMenuItemCount(Some(hmenu)) > 0 && is_line(0) {
            let _ = DeleteMenu(hmenu, 0, MF_BYPOSITION);
        }
    }
}

/// The language-neutral name of a command ("open", "properties"...), or "".
fn verb(menu: &IContextMenu, offset: u32) -> String {
    let mut buf = [0u16; 128];
    // SAFETY: GCS_VERBW writes at most `cchMax` wide characters into `buf` (cast as the API wants).
    let ok = unsafe { menu.GetCommandString(offset as usize, GCS_VERBW, None, PSTR(buf.as_mut_ptr() as *mut u8), buf.len() as u32 - 1) };
    if ok.is_err() {
        return String::new();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Which command of a menu to run.
enum Verb<'a> {
    /// By its place in the menu (the picked id minus SHELL_FIRST), as Windows' menus expect for
    /// picked items.
    Offset(usize),
    /// By its language-neutral name ("properties").
    Name(&'a str),
}

/// The state of things when a command was picked: where the menu was (screen pixels; some
/// commands open their window there) and the keys held.
#[derive(Default)]
struct Keys {
    point: Option<(i32, i32)>,
    shift: bool,
    ctrl: bool,
}

fn invoke_command(s: &Session, owner: HWND, verb: Verb, keys: Keys) -> windows::core::Result<()> {
    let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_ASYNCOK;
    if keys.point.is_some() {
        mask |= CMIC_MASK_PTINVOKE;
    }
    if keys.shift {
        mask |= CMIC_MASK_SHIFT_DOWN;
    }
    if keys.ctrl {
        mask |= CMIC_MASK_CONTROL_DOWN;
    }
    let (name_a, name_w) = match verb {
        Verb::Name(n) => (format!("{n}\0").into_bytes(), wide(n)),
        Verb::Offset(_) => (Vec::new(), Vec::new()),
    };
    let (verb_a, verb_w) = match verb {
        // MAKEINTRESOURCE(offset).
        Verb::Offset(o) => (PCSTR(o as *const u8), PCWSTR(o as *const u16)),
        Verb::Name(_) => (PCSTR(name_a.as_ptr()), PCWSTR(name_w.as_ptr())),
    };
    let dir = if s.dir.is_empty() { PCWSTR::null() } else { PCWSTR(s.dir.as_ptr()) };
    let (x, y) = keys.point.unwrap_or_default();
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: mask,
        hwnd: owner,
        lpVerb: verb_a,
        lpVerbW: verb_w,
        lpDirectoryW: dir,
        nShow: SW_SHOWNORMAL.0,
        ptInvoke: POINT { x, y },
        ..Default::default()
    };
    // SAFETY: CMINVOKECOMMANDINFOEX starts like CMINVOKECOMMANDINFO and cbSize says which it is;
    // the strings it points to live until the call returns.
    unsafe { s.menu.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO) }
}

fn handle_forward(f: &mut Forward) {
    // The interfaces are cloned out (one AddRef) so no borrow is held while the app's code runs.
    let (m3, m2) = SESSIONS.with(|s| s.borrow().get(&f.session).map(|s| (s.menu3.clone(), s.menu2.clone()))).unwrap_or_default();
    // The item structs point to our copies.
    let lparam = match (&mut f.measure, &mut f.draw) {
        (Some(m), _) => LPARAM(m as *mut MEASUREITEMSTRUCT as isize),
        (_, Some(d)) => LPARAM(d as *mut DRAWITEMSTRUCT as isize),
        _ => LPARAM(f.lparam),
    };
    let wparam = WPARAM(f.wparam);
    // SAFETY: the message and its parameters come straight from the menu, as the API expects.
    unsafe {
        if let Some(m3) = m3 {
            let mut result = LRESULT(0);
            if m3.HandleMenuMsg2(f.msg, wparam, lparam, Some(&mut result)).is_ok() {
                f.result = result.0;
                f.handled = true;
            }
        } else if let Some(m2) = m2 {
            // IContextMenu2 doesn't know WM_MENUCHAR.
            if f.msg != WM_MENUCHAR && m2.HandleMenuMsg(f.msg, wparam, lparam).is_ok() {
                f.result = if matches!(f.msg, WM_MEASUREITEM | WM_DRAWITEM) { 1 } else { 0 };
                f.handled = true;
            }
        }
    }
}

/// Properties: the "properties" command of the items' own Windows menu, as Explorer does it (one
/// window for one item, a combined one for several, a drive's for a drive). If that fails for
/// several items, SHMultiFileProperties is tried directly. Windows shows these windows without
/// waiting for them to close.
fn show_properties(owner: HWND, paths: &[String]) {
    let req = ShellRequest { target: ShellTarget::Items(paths.to_vec()), extended: false, full: false };
    let done = match make_session(owner, &req) {
        Some(s) => match invoke_command(&s, owner, Verb::Name("properties"), Keys::default()) {
            Ok(()) => true,
            Err(e) => {
                log::line(&format!("properties of {} item(s) failed: {e}", paths.len()));
                false
            }
        },
        None => false,
    };
    if !done && !(paths.len() > 1 && multi_properties(paths)) {
        shell::tell(owner, "Windows couldn't show the properties.".to_string());
    }
}

fn multi_properties(paths: &[String]) -> bool {
    let Some(parent) = folder::parent(&paths[0]) else { return false };
    let Some(parent_pidl) = parse(&parent) else { return false };
    let mut pidls = Vec::new();
    let mut children: Vec<*const ITEMIDLIST> = Vec::new();
    for p in paths.iter().take(MAX_ITEMS) {
        let Some(pidl) = parse(p) else { continue };
        let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: as in `build`; the folder itself is not needed, only the child's place in `pidl`.
        if unsafe { SHBindToParent::<IShellFolder>(pidl.0, Some(&mut child)) }.is_ok() && !child.is_null() {
            children.push(child as *const _);
            pidls.push(pidl);
        }
    }
    if children.is_empty() {
        return false;
    }
    // SAFETY: valid id lists that outlive the calls; the data object holds its own copies.
    unsafe {
        let Ok(data) = SHCreateDataObject::<_, IDataObject>(Some(parent_pidl.0 as *const _), Some(&children), None::<&IDataObject>) else {
            return false;
        };
        SHMultiFileProperties(&data, 0).is_ok()
    }
}

/// For testing what happens when Windows' items are slow (a hung app, a sleeping network drive):
/// GLINT_SHELL_MENU_DELAY=ms makes the shell thread wait that long before building each menu.
fn test_delay() -> Option<std::time::Duration> {
    static DELAY: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    DELAY.get_or_init(|| std::env::var("GLINT_SHELL_MENU_DELAY").ok().and_then(|v| v.trim().parse().ok())).map(std::time::Duration::from_millis)
}

/// For popup.rs: a bitmap handle from its number.
pub fn bitmap(h: isize) -> HBITMAP {
    HBITMAP(h as *mut _)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_are_found_only_for_top_level_commands() {
        let b = Built {
            session: 1,
            hmenu: 0,
            items: vec![
                ShellItem { id: SHELL_FIRST + 3, verb: "rename".into(), ..Default::default() },
                ShellItem { id: 0, submenu: 77, text: "Send to".into(), ..Default::default() },
            ],
        };
        assert_eq!(b.verb_of(SHELL_FIRST + 3), "rename");
        assert_eq!(b.verb_of(SHELL_FIRST + 4), "");
        assert_eq!(b.verb_of(0), "");
    }

    #[test]
    fn commands_in_windows_new_menu_are_found() {
        use windows::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_POPUP, MF_STRING};
        // SAFETY: test menus made, read and destroyed here (the inner one goes with the outer one).
        unsafe {
            let new_menu = CreatePopupMenu().unwrap();
            let deeper = CreatePopupMenu().unwrap();
            let _ = AppendMenuW(new_menu, MF_STRING, (SHELL_FIRST + 5) as usize, w!("Folder"));
            let _ = AppendMenuW(deeper, MF_STRING, (SHELL_FIRST + 9) as usize, w!("Something deeper"));
            let _ = AppendMenuW(new_menu, MF_POPUP, deeper.0 as usize, w!("More"));
            let b = Built {
                session: 1,
                hmenu: 0,
                items: vec![
                    ShellItem { id: SHELL_FIRST + 1, text: "&Open".into(), ..Default::default() },
                    ShellItem { submenu: new_menu.0 as isize, text: "Ne&w".into(), ..Default::default() },
                ],
            };
            assert!(b.in_submenu_of("new", SHELL_FIRST + 5));
            assert!(b.in_submenu_of("new", SHELL_FIRST + 9), "a submenu of New counts too");
            assert!(!b.in_submenu_of("new", SHELL_FIRST + 1), "Open isn't in New");
            assert!(!b.in_submenu_of("send to", SHELL_FIRST + 5));
            let _ = DestroyMenu(new_menu);
        }
    }
}
