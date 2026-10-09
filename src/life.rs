//! How Glint runs: one main Glint, extra windows, background mode, the tray icon, Win+E, Glint as the
//! default file explorer, Start with Windows, and the command-line switches.
//!
//! **One main Glint.** The first glint.exe to start becomes the main one: it holds a named mutex
//! (`Local\Glint.Main`, one per Windows session) and opens the "door", a hidden window of its
//! own that other glint.exe's find by its class name. A glint.exe started later (a double-clicked
//! folder, Win+E, a shortcut, a terminal) hands its folder through the door with WM_COPYDATA and
//! ends; the main Glint opens the folder in a new tab and comes to the front. If the main one doesn't
//! answer (it is just quitting, or hung), the newcomer waits a moment to become the main one itself,
//! and otherwise opens a window of its own.
//!
//! The door never touches the window's state: it puts the request in a queue and posts
//! WM_APP_LIFE to the main window, which takes it when it isn't busy (window.rs). So a request can
//! never arrive "inside" another message, and a busy window only means a short wait.
//!
//! **Extra windows (Ctrl+N)** are separate processes (`glint.exe --new-window "folder"`): the
//! window, its parts and their threads (the shell thread, menus, the icon cache) are
//! built around one window per process, and a process per window keeps them simple and independent.
//! An extra window doesn't save its tabs or place (the main window's stay in settings.json), quits
//! when closed, and never receives folders from other programs.
//!
//! **Background mode** ("StayRunning", on by default): closing the main window hides it instead,
//! so the next window opens instantly. While hidden the folder watch is stopped, timers are stopped,
//! and memory goes back to Windows (`trim_memory`). The tray icon (tray.rs, "TrayIcon") shows while
//! hidden. Ctrl+Q, the tray icon's Quit and `glint.exe --quit` end Glint completely.
//!
//! **Switches** (main.rs):
//! - `glint.exe "C:\folder"` opens that folder (a file: its folder, with the file selected);
//! - `--background` starts hidden (Start with Windows uses it); if Glint already runs, nothing happens;
//! - `--quit` asks the running Glint to quit completely and waits (up to 5 s) until it has;
//! - `--new-window "folder"` opens an extra window;
//! - `--set-default [--with-win-e]` and `--restore-explorer` change the registry and end (exit code
//!   0 = done); they read no settings, so they work even when settings.json is broken. Used by
//!   "Make Glint default.bat" and "Restore File Explorer.bat".

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowW, GetWindowThreadProcessId, IsWindow,
    PostMessageW, RegisterClassExW, RegisterWindowMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SW_SHOWNORMAL, WM_APP,
    WM_CLOSE, WM_COPYDATA, WM_HOTKEY, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use crate::folder::{self, wide};
use crate::registry::{self, Places};
use crate::settings::Settings;
use crate::shell::{self, Owner};
use crate::{log, tray, winekey};

/// Posted to the main window: requests from the door are waiting (`take_requests`).
pub const WM_APP_LIFE: u32 = WM_APP + 40;
/// Posted to the door by the Win+E keyboard hook (winekey.rs).
pub const WM_APP_WIN_E: u32 = WM_APP + 41;
/// The tray icon's messages to the door (tray.rs).
pub const WM_APP_TRAY: u32 = WM_APP + 42;
/// To the door: "Win+E opens Glint" may have changed (after the registry was repaired, or switched
/// in an extra window): catch Win+E, or stop, as the registry now says.
const WM_APP_WIN_E_CHANGED: u32 = WM_APP + 43;

/// The switch for an extra window (Ctrl+N).
pub const NEW_WINDOW_SWITCH: &str = "--new-window";
/// One main Glint per Windows session.
const MUTEX_NAME: PCWSTR = w!("Local\\Glint.Main");
/// The door's window class.
const DOOR_CLASS: PCWSTR = w!("GlintDoor");
/// Marks Glint's WM_COPYDATA messages ("GLNT").
const MAGIC: usize = 0x474C_4E54;
/// The longest message the door takes, in bytes (two long paths and some).
const MAX_MESSAGE: usize = 160 * 1024;
/// How long a new glint.exe looks for the door (the main Glint may still be starting) and waits for
/// its answer.
const SEND_WAIT: Duration = Duration::from_secs(3);
/// How long a new glint.exe waits for a main Glint that is still starting (it holds the mutex but
/// hasn't opened its door yet: its window is being made). Two folders double-clicked at once on a
/// slow, cold start would otherwise open two windows.
const STARTING_WAIT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- switches

/// The command-line switches about running (main.rs keeps --light, --dark, --bench).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Switches {
    pub background: bool,
    pub quit: bool,
    pub new_window: bool,
    pub set_default: bool,
    pub with_win_e: bool,
    pub restore_explorer: bool,
}

impl Switches {
    /// Takes `arg` if it's one of these switches.
    pub fn take(&mut self, arg: &str) -> bool {
        let flag = match arg.to_ascii_lowercase().as_str() {
            registry::BACKGROUND_SWITCH => &mut self.background,
            "--quit" => &mut self.quit,
            NEW_WINDOW_SWITCH => &mut self.new_window,
            "--set-default" => &mut self.set_default,
            "--with-win-e" => &mut self.with_win_e,
            "--restore-explorer" => &mut self.restore_explorer,
            _ => return false,
        };
        *flag = true;
        true
    }
}

/// A folder to open, and the file in it to select (when a file was given).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub folder: String,
    pub select: Option<String>,
}

/// What a folder argument means: relative paths ("glint ." in a terminal) are taken from `cwd`, and
/// a file opens its folder with the file selected. Looks at the disk (only at start, or in a new
/// glint.exe that is about to hand over - never on the main window's thread).
pub fn resolve_target(arg: &str, cwd: &str) -> Target {
    let path = folder::resolve(arg, cwd);
    if !path.is_empty() && std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
        if let Some(parent) = folder::parent(&path) {
            return Target { select: Some(folder::file_name(&path).to_string()), folder: parent };
        }
    }
    Target { folder: path, select: None }
}

/// Places Glint can't show (Control Panel, "shell:" places, ::{CLSID}) go to File Explorer - all but
/// the Recycle Bin (its "::{645FF040-…}" path, shell:RecycleBinFolder), which Glint shows itself.
pub fn is_shell_location(arg: &str) -> bool {
    !crate::recycle::means_bin(arg) && (arg.starts_with("::") || arg.contains("::{") || arg.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("shell:")))
}

/// Opens `arg` in File Explorer now (before Glint ends; the caller quits right after).
pub fn open_in_explorer_now(arg: &str) {
    let (verb, exe, args) = (wide("open"), wide("explorer.exe"), wide(&shell::quote_arg(arg)));
    // SAFETY: all strings are NUL-terminated and live across the call; COM is initialised (main.rs).
    unsafe {
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(exe.as_ptr()), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// `--set-default [--with-win-e]` and `--restore-explorer`: change the registry and say how it went
/// (the exit code: 0 done, 1 failed, with the reason in the error log). None for other starts.
pub fn registry_switch(s: &Switches) -> Option<i32> {
    if !s.set_default && !s.restore_explorer {
        return None;
    }
    let Some(exe) = registry::this_exe() else { return Some(1) };
    // The process ends right after, so Explorer is told before that, not on a thread of its own.
    let places = Places::current().telling_the_shell_at_once();
    let result = if s.restore_explorer {
        registry::restore_explorer(&places, &exe)
    } else if let Some(why) = registry::risky_place(&exe) {
        Err(why.to_string())
    } else {
        registry::set_folders(&places, true, &exe).and_then(|()| registry::set_win_e(&places, s.with_win_e, &exe))
    };
    // A Glint running now catches Win+E itself: it looks at the registry again, so after "Restore
    // File Explorer" Win+E opens File Explorer at once (not only once that Glint has quit).
    tell_main_win_e_changed();
    match result {
        Ok(()) => Some(0),
        Err(e) => {
            log::error(&e);
            Some(1)
        }
    }
}

// ---------------------------------------------------------------- requests through the door

/// What another glint.exe, the tray icon or Win+E asks of the main Glint.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Show Glint (from the background too) and bring it to the front; with a folder, open it in a
    /// tab (`Target::select`: a file in it to select).
    Open(Option<Target>),
    /// Quit completely (`--quit`, the tray icon's Quit).
    Quit,
    /// Quit if Glint is only running hidden in the background (an extra window's Ctrl+Q).
    QuitIfHidden,
}

impl Request {
    /// As text for WM_COPYDATA: "open", "open\n<folder>\n<file>", "quit", "quit-if-hidden".
    fn encode(&self) -> String {
        match self {
            Request::Open(None) => "open".into(),
            Request::Open(Some(t)) => format!("open\n{}\n{}", t.folder, t.select.as_deref().unwrap_or("")),
            Request::Quit => "quit".into(),
            Request::QuitIfHidden => "quit-if-hidden".into(),
        }
    }

    /// The other way round; None for anything else (the door ignores it).
    fn parse(text: &str) -> Option<Request> {
        let mut parts = text.split('\n');
        let req = match parts.next()? {
            "open" => match (parts.next(), parts.next()) {
                (None, _) => Request::Open(None),
                (Some(folder), select) => {
                    let select = select.filter(|s| !s.is_empty() && !s.contains(['\\', '/'])).map(str::to_string);
                    Request::Open(Some(Target { folder: folder.to_string(), select }))
                }
            },
            "quit" => Request::Quit,
            "quit-if-hidden" => Request::QuitIfHidden,
            _ => return None,
        };
        (parts.next().is_none() && !text.contains('\0')).then_some(req)
    }
}

thread_local! {
    /// Requests the door took in, waiting for the main window (UI thread only).
    static REQUESTS: RefCell<Vec<Request>> = const { RefCell::new(Vec::new()) };
    /// The door window, while it's open.
    static DOOR: Cell<Option<HWND>> = const { Cell::new(None) };
    /// "TaskbarCreated": Explorer (re)started, so the tray icon must be added again.
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

/// The main window, which the door posts WM_APP_LIFE to.
static MAIN_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The mutex that makes this process the main Glint (0: it isn't).
static MUTEX: AtomicIsize = AtomicIsize::new(0);
/// An extra window asked, on Ctrl+Q, that a main Glint hidden in the background quits too.
static QUIT_HIDDEN_MAIN: AtomicBool = AtomicBool::new(false);

/// The requests waiting (the main window calls this on WM_APP_LIFE).
pub fn take_requests() -> Vec<Request> {
    REQUESTS.with(|r| std::mem::take(&mut *r.borrow_mut()))
}

/// Queues a request for the main window and wakes it. False if there is no main window (any more).
pub fn deliver(r: Request) -> bool {
    let main = HWND(MAIN_WINDOW.load(Ordering::SeqCst) as *mut _);
    // SAFETY: plain queries and a post to a window handle (fails harmlessly if it's gone).
    unsafe {
        if main.is_invalid() || !IsWindow(Some(main)).as_bool() {
            return false;
        }
        REQUESTS.with(|q| q.borrow_mut().push(r));
        PostMessageW(Some(main), WM_APP_LIFE, WPARAM(0), LPARAM(0)).is_ok()
    }
}

/// The door's window procedure. It only queues requests and wakes the main window (tray.rs shows
/// its own little menu from here, which is a modal loop of its own but touches no state).
unsafe extern "system" fn door_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            // SAFETY: for WM_COPYDATA, lparam points to a COPYDATASTRUCT that Windows copied into
            // this process and keeps valid during the call; lpData holds cbData bytes.
            let req = unsafe { (lparam.0 as *const COPYDATASTRUCT).as_ref() }.and_then(|cds| {
                if cds.dwData != MAGIC || cds.lpData.is_null() || cds.cbData as usize > MAX_MESSAGE || cds.cbData % 2 != 0 {
                    return None;
                }
                // Read as bytes: nothing promises the copy is aligned for u16.
                let bytes = unsafe { std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize) };
                let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                Request::parse(&String::from_utf16_lossy(&units))
            });
            LRESULT(req.is_some_and(deliver) as isize)
        }
        WM_HOTKEY if wparam.0 == winekey::HOTKEY_ID as usize => {
            deliver(Request::Open(None));
            LRESULT(0)
        }
        WM_APP_WIN_E => {
            deliver(Request::Open(None));
            LRESULT(0)
        }
        WM_APP_TRAY => {
            if let Some(r) = tray::message(hwnd, lparam) {
                deliver(r);
            }
            LRESULT(0)
        }
        // Another program asks every window of Glint to close (taskkill, an installer through the
        // Restart Manager): Glint quits, instead of only the door going.
        WM_CLOSE => {
            deliver(Request::Quit);
            LRESULT(0)
        }
        WM_APP_WIN_E_CHANGED => {
            if win_e_on() {
                winekey::start(hwnd);
            } else {
                winekey::stop();
            }
            LRESULT(0)
        }
        _ if msg != 0 && msg == TASKBAR_CREATED.with(Cell::get) => {
            tray::taskbar_created();
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Opens the door (main Glint only): from now on other glint.exe's, the tray icon and Win+E reach
/// `main_window`. Returns the door, or None if Windows wouldn't make it (then Glint still works,
/// just without these).
pub fn open_door(main_window: HWND) -> Option<HWND> {
    MAIN_WINDOW.store(main_window.0 as isize, Ordering::SeqCst);
    // SAFETY: standard class registration and creation of a hidden, never-shown window.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(door_proc),
            hInstance: instance.into(),
            lpszClassName: DOOR_CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
        // A hidden top-level window rather than a message-only one: those don't get the
        // "TaskbarCreated" broadcast, and the tray menu needs a window that can be in front.
        let door = CreateWindowExW(WS_EX_TOOLWINDOW, DOOR_CLASS, w!("Glint"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance.into()), None).ok()?;
        DOOR.with(|d| d.set(Some(door)));
        Some(door)
    }
}

/// The door, while it's open.
pub fn door() -> Option<HWND> {
    DOOR.with(Cell::get)
}

/// Whether a main Glint's door is open (in this Windows session).
fn door_open() -> bool {
    // SAFETY: a plain lookup by class name.
    unsafe { FindWindowW(DOOR_CLASS, PCWSTR::null()) }.is_ok_and(|d| !d.is_invalid())
}

/// Sends a request to the main Glint of this session. False if there is none, or it didn't answer.
/// `find_wait`: how long to look for its door (a main Glint that is just starting opens it a moment
/// after it took the mutex).
pub fn send(r: &Request, find_wait: Duration) -> bool {
    let started = Instant::now();
    let door = loop {
        // SAFETY: plain lookup by class name.
        match unsafe { FindWindowW(DOOR_CLASS, PCWSTR::null()) } {
            Ok(d) if !d.is_invalid() => break d,
            _ if started.elapsed() < find_wait => std::thread::sleep(Duration::from_millis(50)),
            _ => return false,
        }
    };
    let bytes: Vec<u8> = r.encode().encode_utf16().flat_map(u16::to_le_bytes).collect();
    let cds = COPYDATASTRUCT { dwData: MAGIC, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
    let mut answer = 0usize;
    // SAFETY: `cds` and `bytes` live across the call (WM_COPYDATA copies them into the other
    // process); the answer is a plain out-value.
    unsafe {
        // Let the main Glint come to the front: Windows only allows that to the process the person
        // was just using (this one), which may pass it on.
        let mut pid = 0u32;
        GetWindowThreadProcessId(door, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let ok = SendMessageTimeoutW(door, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize), SMTO_ABORTIFHUNG, SEND_WAIT.as_millis() as u32, Some(&mut answer));
        ok.0 != 0 && answer == 1
    }
}

// ---------------------------------------------------------------- the main Glint

/// Tries to become the main Glint of this session (takes the mutex). True if this process is it now.
fn take_main() -> bool {
    if MUTEX.load(Ordering::SeqCst) != 0 {
        return true;
    }
    // SAFETY: a named mutex is only used to see whether it exists already; the handle is kept until
    // `release_main` (or the process ends, which closes it).
    unsafe {
        let Ok(h) = CreateMutexW(None, false, MUTEX_NAME) else { return false };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return false;
        }
        MUTEX.store(h.0 as isize, Ordering::SeqCst);
        true
    }
}

/// Gives up being the main Glint (it is quitting), so a glint.exe started now becomes the main one
/// at once - also while this process still waits for copies to finish.
fn release_main() {
    let h = MUTEX.swap(0, Ordering::SeqCst);
    if h != 0 {
        // SAFETY: our own mutex handle, closed once.
        unsafe {
            let _ = CloseHandle(HANDLE(h as *mut _));
        }
    }
}

/// Whether this process is the main Glint.
pub fn is_main() -> bool {
    MUTEX.load(Ordering::SeqCst) != 0
}

/// How this glint.exe goes on (see `start`).
#[derive(Debug, PartialEq)]
pub enum Start {
    /// It's the main Glint now.
    Main,
    /// The running Glint took the folder (or there was nothing to hand over): end now.
    HandedOver,
    /// The running Glint didn't answer: open a window of its own.
    Extra,
}

/// One main Glint: becomes it, or hands `target` to the one running. `background`: started with
/// Windows - if Glint runs already, there is nothing to do.
pub fn start(target: Option<Target>, background: bool) -> Start {
    if take_main() {
        return Start::Main;
    }
    if background {
        return Start::HandedOver;
    }
    // The main Glint opens its door once its window is up. Until then: wait for the door - or for the
    // mutex, if that Glint ended before getting that far.
    let started = Instant::now();
    loop {
        if door_open() {
            if send(&Request::Open(target), SEND_WAIT) {
                return Start::HandedOver;
            }
            break; // it didn't answer: quitting right now, or hung
        }
        if take_main() {
            return Start::Main;
        }
        if started.elapsed() >= STARTING_WAIT {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Nobody answered: the main Glint may be quitting right now. Once it's gone this one takes over.
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(100));
        if take_main() {
            return Start::Main;
        }
    }
    log::line("the running Glint didn't answer: opening a window of its own");
    Start::Extra
}

/// `glint.exe --quit`: asks the running Glint to quit completely and waits (up to 5 s) until it has.
pub fn quit_running() {
    if !take_main() && send(&Request::Quit, SEND_WAIT) {
        for _ in 0..50 {
            if take_main() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    release_main();
}

/// Called after the message loop: the tray icon and Win+E go, the door closes and another
/// glint.exe can become the main one. For an extra window closed with Ctrl+Q, a main Glint hidden in
/// the background is asked to quit too.
pub fn shutdown() {
    winekey::stop();
    tray::hide();
    if let Some(d) = DOOR.with(|d| d.take()) {
        // SAFETY: our own door window, on the thread that made it.
        unsafe {
            let _ = DestroyWindow(d);
        }
    }
    MAIN_WINDOW.store(0, Ordering::SeqCst);
    release_main();
    if QUIT_HIDDEN_MAIN.load(Ordering::SeqCst) {
        // No waiting for a door that isn't there: then no main Glint runs.
        send(&Request::QuitIfHidden, Duration::ZERO);
    }
}

/// An extra window was closed with Ctrl+Q: when it ends, a main Glint that is only hidden in the
/// background quits too (a main window that's on screen stays).
pub fn quit_hidden_main_too() {
    if !is_main() {
        QUIT_HIDDEN_MAIN.store(true, Ordering::SeqCst);
    }
}

/// At the main Glint's start: Win+E is caught while Glint runs (if "Win+E opens Glint" is on), and
/// what points at a glint.exe that was moved is repaired (on a thread of its own).
pub fn main_started() {
    let Some(exe) = registry::this_exe() else { return };
    let Some(door) = door() else { return };
    if registry::win_e_on(&Places::current(), &exe) {
        winekey::start(door);
    }
    // Looking at the old place may wait on a network drive, so not on this thread. Win+E is looked
    // at again afterwards: a repaired registry may point at this glint.exe now.
    let door = door.0 as isize;
    shell::background("repair registry", move || {
        registry::repair_if_moved(&Places::current(), &exe);
        // SAFETY: posting to a window handle is safe even if the window is gone.
        unsafe {
            let _ = PostMessageW(Some(HWND(door as *mut _)), WM_APP_WIN_E_CHANGED, WPARAM(0), LPARAM(0));
        }
    });
}

// ---------------------------------------------------------------- background mode

/// The main window just hid itself: the tray icon shows (if wanted), and memory goes back to
/// Windows.
pub fn entered_background(tray_icon: bool) {
    if tray_icon {
        if let Some(door) = door() {
            tray::show(door);
        }
    }
    let before = crate::status::memory_report();
    trim_memory();
    log::line(&format!("hidden: memory given back to Windows. Before: {before}; after: {}", crate::status::memory_report()));
}

/// The main window is on screen again: the tray icon goes.
pub fn left_background() {
    tray::hide();
}

/// Gives memory back to Windows while hidden: free heap blocks are handed back and the working set
/// is emptied, so Windows can use the memory until the next window (pages come back as needed).
pub fn trim_memory() {
    // SAFETY: plain calls on our own process and its default heap.
    unsafe {
        if let Ok(heap) = GetProcessHeap() {
            HeapCompact(heap, HEAP_FLAGS(0));
        }
        // (SIZE_T)-1 for both: "remove as many pages as possible".
        let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

/// Ctrl+N: an extra window, in its own process, at `folder` ("" = This PC).
pub fn open_new_window(owner: Owner, folder: &str) {
    let Some(exe) = registry::this_exe() else { return };
    let arg = if folder.is_empty() { "This PC".to_string() } else { folder.to_string() };
    let started = shell::background("new window", move || {
        if let Err(e) = std::process::Command::new(&exe).arg(NEW_WINDOW_SWITCH).arg(&arg).spawn() {
            shell::tell(owner.hwnd(), format!("Couldn't open a new window: {e}"));
        }
    });
    if !started {
        shell::tell(owner.hwnd(), "Couldn't open a new window right now".into());
    }
}

// ---------------------------------------------------------------- for the Settings window

/// Whether settings that reach outside Glint (the Run key) are left alone: a test run with its own
/// settings folder (GLINT_SETTINGS_DIR) and no test registry key.
fn testing_without_registry() -> bool {
    let set = |v: &str| std::env::var_os(v).is_some_and(|v| !v.is_empty());
    set("GLINT_SETTINGS_DIR") && !set("GLINT_TEST_REGISTRY")
}

/// "Keep Glint running in the background" ("StayRunning"). Closing the main window reads it; nothing
/// else to do.
pub fn apply_stay_running(settings: &mut Settings, on: bool) {
    settings.stay_running = on;
}

/// "Show the tray icon" ("TrayIcon"): the icon shows while Glint is hidden in the background. (The
/// Settings window is on screen, so nothing shows or hides right now.)
pub fn apply_tray_icon(settings: &mut Settings, on: bool) {
    settings.tray_icon = on;
}

/// "Start with Windows" ("StartWithWindows"): the Run value that starts Glint hidden at sign-in.
/// The setting changes only if the registry did.
pub fn apply_start_with_windows(settings: &mut Settings, on: bool) -> Result<(), String> {
    if !testing_without_registry() {
        let exe = registry::this_exe().ok_or("Glint couldn't find its own glint.exe")?;
        registry::set_start_with_windows(&Places::current(), on, &exe)?;
    }
    settings.start_with_windows = on;
    Ok(())
}

/// Whether "Win+E opens Glint" is on (for this glint.exe). It lives in Windows' registry, not in
/// settings.json: it's on exactly when Windows would open Glint.
pub fn win_e_on() -> bool {
    registry::this_exe().is_some_and(|exe| registry::win_e_on(&Places::current(), &exe))
}

/// "Win+E opens Glint": Windows starts glint.exe for Win+E, and while Glint runs it catches Win+E
/// itself (faster). Only ever changed here, when the person switches it.
pub fn apply_win_e(on: bool) -> Result<(), String> {
    let exe = registry::this_exe().ok_or("Glint couldn't find its own glint.exe")?;
    if let Some(why) = registry::risky_place(&exe).filter(|_| on) {
        return Err(why.to_string());
    }
    registry::set_win_e(&Places::current(), on, &exe)?;
    match door() {
        Some(door) if on => winekey::start(door),
        Some(_) => winekey::stop(),
        // An extra window: the main Glint, which catches Win+E, is told (if one runs).
        None => tell_main_win_e_changed(),
    }
    Ok(())
}

/// Tells the main Glint (if one runs) that "Win+E opens Glint" may have changed: it catches Win+E,
/// or stops, as the registry now says.
fn tell_main_win_e_changed() {
    // SAFETY: plain lookup and a post to a window handle (fails harmlessly if it's gone).
    unsafe {
        if let Ok(main_door) = FindWindowW(DOOR_CLASS, PCWSTR::null()) {
            let _ = PostMessageW(Some(main_door), WM_APP_WIN_E_CHANGED, WPARAM(0), LPARAM(0));
        }
    }
}

/// Whether "Open folders and drives in Glint" is on (for this glint.exe); in the registry, like Win+E.
pub fn default_explorer_on() -> bool {
    registry::this_exe().is_some_and(|exe| registry::folders_on(&Places::current(), &exe))
}

/// "Open folders and drives in Glint": per-user registration (registry.rs), and undoing it. Only
/// ever changed here, when the person switches it.
pub fn apply_default_explorer(on: bool) -> Result<(), String> {
    let exe = registry::this_exe().ok_or("Glint couldn't find its own glint.exe")?;
    if let Some(why) = registry::risky_place(&exe).filter(|_| on) {
        return Err(why.to_string());
    }
    registry::set_folders(&Places::current(), on, &exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches() {
        let mut s = Switches::default();
        for a in ["--background", "--QUIT", "--new-window", "--set-default", "--with-win-e", "--restore-explorer"] {
            assert!(s.take(a), "{a}");
        }
        assert!(s.background && s.quit && s.new_window && s.set_default && s.with_win_e && s.restore_explorer);
        assert!(!s.take("C:\\folder") && !s.take("--light"));
    }

    #[test]
    fn requests_travel_as_text() {
        let all = [
            Request::Open(None),
            Request::Open(Some(Target { folder: "C:\\Users\\me".into(), select: None })),
            Request::Open(Some(Target { folder: "D:\\".into(), select: Some("notes.txt".into()) })),
            Request::Open(Some(Target { folder: String::new(), select: None })), // This PC
            Request::Quit,
            Request::QuitIfHidden,
        ];
        for r in all {
            assert_eq!(Request::parse(&r.encode()), Some(r.clone()), "{r:?}");
        }
        // Anything else is ignored.
        for bad in ["", "OPEN", "delete C:\\", "open\nC:\\\nx\nmore", "quit\n", "open\nC:\\\0"] {
            assert_eq!(Request::parse(bad), None, "{bad:?}");
        }
        // A name to select can't climb out of the folder.
        assert_eq!(Request::parse("open\nC:\\a\n..\\b"), Some(Request::Open(Some(Target { folder: "C:\\a".into(), select: None }))));
    }

    #[test]
    fn shell_locations_go_to_explorer() {
        assert!(is_shell_location("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"));
        assert!(is_shell_location("shell:startup") && is_shell_location("Shell:Downloads"));
        assert!(is_shell_location("C:\\x\\::{645FF040-5081-101B-9F08-00AA002F954E}"));
        assert!(!is_shell_location("C:\\Users") && !is_shell_location("shell") && !is_shell_location(""));
        // The Recycle Bin opens in Glint.
        assert!(!is_shell_location("::{645FF040-5081-101B-9F08-00AA002F954E}") && !is_shell_location("shell:RecycleBinFolder"));
    }

    #[test]
    fn a_file_opens_its_folder_with_it_selected() {
        let dir = std::env::temp_dir().join(format!("glint-life-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hello.txt");
        std::fs::write(&file, "hi").unwrap();
        let d = dir.to_string_lossy().into_owned();
        let t = resolve_target(&file.to_string_lossy(), "C:\\");
        assert_eq!(t.select.as_deref(), Some("hello.txt"));
        assert!(registry::same_exe(&t.folder, &d), "{} vs {d}", t.folder);
        let t = resolve_target("hello.txt", &d);
        assert_eq!(t.select.as_deref(), Some("hello.txt"), "relative to the folder it was started in");
        let t = resolve_target(&d, "C:\\");
        assert_eq!(t.select, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
