//! Drag and drop, as in File Explorer: dragging files out of Glint (into Explorer, the desktop, an
//! email, Discord, an editor, another Glint window) and dropping files onto Glint (from any of those).
//! This file is Windows' side of it; the window's side - where a drop would go, the highlight, the
//! list scrolling near its edges - is window/drag.rs, and the work is a file job (app/drop.rs,
//! fileops.rs), so copies and moves show Windows' own progress and "Replace or Skip" windows.
//!
//! **Dropping onto Glint.** The main window is registered with Windows as a drop target
//! ([`register`]). Windows calls [`Target`]'s DragEnter / DragOver / DragLeave / Drop on the UI
//! thread: from its own drag loop when the drag started in Glint, through COM when it started in
//! another program. Target never touches the window's state itself. It reads the dragged files
//! (CF_HDROP: real files and folders) and hands each step to the window as a [`WM_APP_DRAG`]
//! message (SendMessage, so the window's re-entrancy guard applies: a step that comes while the
//! window is busy gets "no drop"). The window answers with the effect (copy, move, shortcut or
//! nothing) and, on Drop, a [`Plan`]. A right-button drop shows Windows' usual menu first
//! ("Copy here", "Move here", "Create shortcuts here", "Cancel"). Windows' drag picture is shown
//! over Glint too (IDropTargetHelper), with "Move to Documents" under it where the program the drag
//! came from allows that (the "DropDescription" Explorer reads).
//!
//! **What a drop does** ([`folder_effect`]): Explorer's rules. Into a folder on the same drive the
//! items move, onto another drive they're copied; Ctrl copies, Shift moves, Alt (or Ctrl+Shift)
//! makes shortcuts. A folder never goes into itself, and moving items into the folder they're in
//! does nothing. The Recycle Bin deletes (Shift: for good, after Glint's question).
//!
//! **When the source may let go of its files.** A program that dragged files it made for the drag
//! (an archive manager unpacking to a temporary folder, say) deletes them as soon as the drop is
//! over. Explorer copies such files before Drop returns; so does Glint, waiting for the job while
//! keeping its own window alive ([`wait_for`]). A source that says it can wait
//! (IDataObjectAsyncCapability, as File Explorer's own drags do) isn't kept waiting: it's told when
//! the job has finished ([`finish_async`]). Glint's own drags never wait. A move done by Glint is
//! reported to the source as "nothing for you to do" (DROPEFFECT_NONE, Explorer's "optimized
//! move"), so no source ever deletes originals because of Glint.
//!
//! **Dragging out of Glint** ([`drag_out`]): the selected items become a shell data object (the
//! one File Explorer makes for the same items, so every program that takes files from Explorer
//! takes them from Glint), with Windows' drag picture, and Windows' drag loop runs (SHDoDragDrop).
//! Esc calls it off; with the right button, the program dropped into shows its own menu. Whatever
//! the drop does is done by that program; Glint never deletes anything afterwards.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use windows::core::{implement, w, Interface, Ref, BOOL, HRESULT, PCWSTR, Result as WinResult};
use windows::Win32::Foundation::{
    CloseHandle, GlobalFree, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, E_FAIL, HANDLE, HGLOBAL, HWND, LPARAM, POINT, POINTL,
    S_OK, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, IDataObject, CLSCTX_INPROC_SERVER, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::System::Ole::{
    DoDragDrop, OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK,
    DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl,
};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_CONTROL, MK_LBUTTON, MK_RBUTTON, MK_SHIFT};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, INFINITE};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IDataObjectAsyncCapability, IDragSourceHelper, IDropTargetHelper, IShellFolder, SHBindToObject, SHCreateDataObject, SHDoDragDrop,
    SHParseDisplayName, CLSID_DragDropHelper, DROPDESCRIPTION, DROPIMAGETYPE, DROPIMAGE_COPY, DROPIMAGE_INVALID, DROPIMAGE_LINK,
    DROPIMAGE_MOVE, DROPIMAGE_NONE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, PostMessageW, PostQuitMessage,
    SendMessageW, SetForegroundWindow, SetMenuDefaultItem, TrackPopupMenu, TranslateMessage, MF_SEPARATOR, MF_STRING, MSG,
    MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, WM_APP, WM_NULL, WM_QUIT,
};

use crate::fileops::{self, is_full_item_path, is_same_or_inside, same_path, DropHow};
use crate::folder::{parent, wide};
use crate::log;
use crate::recycle;

/// From [`Target`] to the window: one step of a drag over it (lparam: a `*mut DragMsg`, valid for
/// the SendMessage). See window/drag.rs.
pub const WM_APP_DRAG: u32 = WM_APP + 60;
/// From a drop's file job to the window when it has finished, for a source that waits to hear it
/// (wparam: the number from [`Signal::Async`], lparam: 1 if it all went through). The window
/// passes it to [`finish_async`].
pub const WM_APP_DROP_DONE: u32 = WM_APP + 61;

/// The Alt key in a drag's key state (MK_ALT; the windows crate doesn't name it).
pub const MK_ALT: u32 = 0x20;

// ---------------------------------------------------------------- what a drop does

/// Where a drop goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dest {
    /// Into this folder.
    Folder(String),
    /// Into the Recycle Bin: the items are deleted.
    RecycleBin,
    /// Onto the sidebar's "Pinned": the folders are pinned.
    Pin,
}

/// What a drop does in the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Copy, move or make shortcuts into a folder.
    Transfer(DropHow),
    /// Delete: to the Recycle Bin, or for good (Shift held).
    Recycle { permanent: bool },
    /// Pin the dropped folders to the sidebar.
    Pin,
}

/// A drop the window has decided on (window/drag.rs): carried out by `AppState::drop_files`.
#[derive(Clone, Debug)]
pub struct Plan {
    pub dest: Dest,
    /// The dropped items' full paths.
    pub files: Vec<String>,
    /// What it does (for a right-button drop: the menu's bold item).
    pub action: Action,
    /// For a right-button drop into a folder: what the menu offers.
    pub choices: Vec<DropHow>,
    /// The tab that gets the new items selected (when it shows the folder dropped into).
    pub tab_id: u64,
    /// The drag started in this Glint window (its own drag loop is waiting): no need to wait.
    pub from_glint: bool,
}

/// The effect number Windows uses for a way of dropping.
pub fn effect_of(how: DropHow) -> u32 {
    match how {
        DropHow::Copy => DROPEFFECT_COPY.0,
        DropHow::Move => DROPEFFECT_MOVE.0,
        DropHow::Link => DROPEFFECT_LINK.0,
    }
}

/// The drive part of a path, to tell drives apart: "c:" or "\\server\share" (lower case).
fn volume(path: &str) -> String {
    let lower = path.to_lowercase();
    if let Some(rest) = lower.strip_prefix("\\\\") {
        let mut parts = rest.splitn(3, '\\');
        let (server, share) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        return format!("\\\\{server}\\{share}");
    }
    lower.chars().take(2).collect()
}

/// Explorer's rules for dropping `sources` into the folder `dest`, with the keys held (`keys`: the
/// MK_ flags of the drag) and what the program they came from allows (`allowed`: DROPEFFECT
/// flags). None: they can't go there (a folder into itself, nothing but drives, moving items to
/// where they are, or nothing the source allows).
pub fn folder_effect(sources: &[String], dest: &str, keys: u32, allowed: u32) -> Option<DropHow> {
    if dest.is_empty() || recycle::is_bin(dest) {
        return None;
    }
    let items: Vec<&String> = sources.iter().filter(|p| is_full_item_path(p)).collect();
    let first = items.first()?;
    if items.iter().any(|s| is_same_or_inside(dest, s)) {
        return None; // a folder can't go into itself (or a folder inside it)
    }
    let same_folder = items.iter().all(|s| parent(s).is_some_and(|p| same_path(&p, dest)));
    let (ctrl, shift, alt) = (keys & MK_CONTROL.0 != 0, keys & MK_SHIFT.0 != 0, keys & MK_ALT != 0);
    let keyed = ctrl || shift || alt;
    let wanted = if alt || (ctrl && shift) {
        DropHow::Link
    } else if ctrl {
        DropHow::Copy
    } else if shift || volume(first) == volume(dest) {
        DropHow::Move
    } else {
        DropHow::Copy
    };
    let possible = |how: DropHow| allowed & effect_of(how) != 0 && !(how == DropHow::Move && same_folder);
    if possible(wanted) {
        return Some(wanted);
    }
    // The program the drag came from doesn't allow that (a read-only place offers only copying).
    // Explorer then does what is allowed - unless a key asked for something in particular, or the
    // items are already where they were dropped (that does nothing; Ctrl copies them there). With
    // the right button the drop shows a menu of what can be done ("Create shortcuts here" right
    // where the items are), so it goes on with the first of those.
    if (keyed || same_folder) && keys & MK_RBUTTON.0 == 0 {
        return None;
    }
    [DropHow::Copy, DropHow::Move, DropHow::Link].into_iter().find(|&h| possible(h))
}

/// What a right-button drop into `dest` can offer: copy, move (not into the folder they're in) and
/// shortcuts, as far as the source allows.
pub fn folder_choices(sources: &[String], dest: &str, allowed: u32) -> Vec<DropHow> {
    [DropHow::Copy, DropHow::Move, DropHow::Link]
        .into_iter()
        .filter(|&h| folder_effect(sources, dest, forced_keys(h), allowed) == Some(h))
        .collect()
}

/// The keys that ask for `how` (for [`folder_choices`]).
fn forced_keys(how: DropHow) -> u32 {
    match how {
        DropHow::Copy => MK_CONTROL.0,
        DropHow::Move => MK_SHIFT.0,
        DropHow::Link => MK_ALT,
    }
}

/// Dropping on the Recycle Bin: the items are moved there (deleted), when the source allows moving
/// and they are real items (not drives).
pub fn bin_effect(sources: &[String], allowed: u32) -> u32 {
    let any = sources.iter().any(|p| is_full_item_path(p));
    if any && allowed & DROPEFFECT_MOVE.0 != 0 {
        DROPEFFECT_MOVE.0
    } else {
        0
    }
}

/// What the program the drag came from is told when Drop returns. Glint copies and makes shortcuts
/// itself, and a move is an "optimized move" done by Glint: the source is told there's nothing left
/// for it to do (Explorer does the same), so it never deletes originals because of Glint.
fn told_source(action: Action) -> u32 {
    match action {
        Action::Transfer(DropHow::Copy) => DROPEFFECT_COPY.0,
        Action::Transfer(DropHow::Link) => DROPEFFECT_LINK.0,
        Action::Transfer(DropHow::Move) | Action::Recycle { .. } | Action::Pin => DROPEFFECT_NONE.0,
    }
}

// ---------------------------------------------------------------- talking to the window

/// Which step of a drag a [`DragMsg`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The drag came over the window (`files` holds what is dragged).
    Enter,
    /// It moved (or a key changed).
    Over,
    /// It left the window, or was called off.
    Leave,
    /// It was dropped: the window decides (`plan`), nothing is done yet.
    Drop,
    /// Do `plan` with `action` (after the menu of a right-button drop).
    Perform,
}

/// One step of a drag, sent to the window (WM_APP_DRAG) and answered there.
pub struct DragMsg {
    pub phase: Phase,
    /// Where the mouse is, in screen pixels.
    pub at: POINT,
    /// The drag's key state (MK_ flags: buttons, Ctrl, Shift, Alt).
    pub keys: u32,
    /// What the source allows (DROPEFFECT flags).
    pub allowed: u32,
    /// The dragged files (Enter and Drop).
    pub files: Vec<String>,
    /// The window handled the message (false: it was busy, and nothing may happen).
    pub handled: bool,
    /// Answer to Enter / Over / Drop: what dropping here does (DROPEFFECT).
    pub effect: u32,
    /// Answer to Enter / Over: the name of the place it would go to ("Documents").
    pub label: Option<String>,
    /// Answer to Drop, and what Perform carries out.
    pub plan: Option<Plan>,
    /// Perform: what to do (the plan's, or the right-button menu's pick).
    pub action: Option<Action>,
    /// Perform: told when the job is done (taken by the window).
    pub signal: Option<Signal>,
    /// Answer to Perform: a job was started (its end fires the signal).
    pub started: bool,
}

impl DragMsg {
    fn new(phase: Phase, at: POINT, keys: u32, allowed: u32) -> DragMsg {
        DragMsg {
            phase,
            at,
            keys,
            allowed,
            files: Vec::new(),
            handled: false,
            effect: 0,
            label: None,
            plan: None,
            action: None,
            signal: None,
            started: false,
        }
    }

    /// Hands this step to the window and returns once it has answered.
    fn send(&mut self, hwnd: HWND) {
        // SAFETY: SendMessage to our own window on its own thread runs the window procedure right
        // here; `self` lives across the call and the window only uses it during it.
        unsafe {
            SendMessageW(hwnd, WM_APP_DRAG, Some(WPARAM(0)), Some(LPARAM(self as *mut DragMsg as isize)));
        }
    }
}

/// How a drop's file job says it has finished (fileops.rs runs it on its own thread).
pub enum Signal {
    /// Sets this event: the Drop that started the job is waiting for it ([`wait_for`]).
    Event(Arc<Event>),
    /// Posts WM_APP_DROP_DONE with this number to the window (the source waits asynchronously).
    Async { id: u64, hwnd: isize },
}

impl Signal {
    /// The job is done (`ok`: it all went through). Safe from any thread.
    pub fn fire(&self, ok: bool) {
        match self {
            Signal::Event(e) => e.set(),
            Signal::Async { id, hwnd } => {
                // SAFETY: posting to a window from another thread is allowed; a window that is gone
                // just makes it fail.
                unsafe {
                    let _ = PostMessageW(Some(HWND(*hwnd as *mut _)), WM_APP_DROP_DONE, WPARAM(*id as usize), LPARAM(ok as isize));
                }
            }
        }
    }
}

/// A Windows event (for a Drop waiting on its job), closed when the last holder lets go.
pub struct Event(HANDLE);

// SAFETY: an event handle may be used from any thread; it's only set and waited on.
unsafe impl Send for Event {}
// SAFETY: as above.
unsafe impl Sync for Event {}

impl Event {
    fn new() -> Option<Arc<Event>> {
        // SAFETY: a fresh unnamed manual-reset event, closed in Drop.
        unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok().map(|h| Arc::new(Event(h)))
    }

    fn set(&self) {
        // SAFETY: our own live event handle.
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

thread_local! {
    /// A Drop is waiting for its job ([`wait_for`]): no new drag starts and nothing is dropped
    /// until it's done (the program the drag came from is waiting too).
    static WAITING: Cell<bool> = const { Cell::new(false) };
    /// Sources told "it's under way" (IDataObjectAsyncCapability), by number, with the effect to
    /// tell them at the end.
    static ASYNC_OPS: RefCell<Vec<(u64, IDataObjectAsyncCapability, u32)>> = const { RefCell::new(Vec::new()) };
    static NEXT_ASYNC: Cell<u64> = const { Cell::new(1) };
}

/// A drop's job has finished (WM_APP_DROP_DONE): the source that was told "under way" hears how
/// it went, and may let go of its files now.
pub fn finish_async(id: u64, ok: bool) {
    let op = ASYNC_OPS.with(|ops| {
        let mut ops = ops.borrow_mut();
        let i = ops.iter().position(|(n, _, _)| *n == id)?;
        Some(ops.remove(i))
    });
    if let Some((_, op, effect)) = op {
        // SAFETY: a COM call on the thread that got the object (the UI thread).
        unsafe {
            let _ = op.EndOperation(if ok { S_OK } else { E_FAIL }, None::<&IBindCtx>, effect);
        }
    }
}

// ---------------------------------------------------------------- the drop target

/// Makes the main window a drop target. Call once, on the UI thread, after the window exists.
/// Returns false if Windows refused (Glint then works without dropping).
pub fn register(hwnd: HWND) -> bool {
    // SAFETY: OLE on the UI thread (COM is already initialised there as single-threaded, which
    // OleInitialize needs); the target object is kept by Windows until `unregister`.
    unsafe {
        if let Err(e) = OleInitialize(None) {
            log::line(&format!("drag and drop: OLE couldn't start ({e}); dropping onto Glint is off"));
            return false;
        }
        // Windows' drag picture over Glint too; without it, only the mouse pointer shows.
        let helper: Option<IDropTargetHelper> = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok();
        let target: IDropTarget = Target { hwnd, helper, now: RefCell::new(None) }.into();
        match RegisterDragDrop(hwnd, &target) {
            Ok(()) => true,
            Err(e) => {
                log::line(&format!("drag and drop: Windows didn't take Glint as a drop target ({e})"));
                false
            }
        }
    }
}

/// The window is closing: Windows lets go of the drop target.
pub fn unregister(hwnd: HWND) {
    // SAFETY: undoes `register` for our own window (harmless if it never happened).
    unsafe {
        let _ = RevokeDragDrop(hwnd);
    }
}

/// What is being dragged over the window right now.
struct Dragging {
    data: IDataObject,
    /// With the right button: the drop shows a menu.
    right: bool,
    /// The label last put under Windows' drag picture (effect, place), to change it only when needed.
    described: Option<(u32, Option<String>)>,
}

/// The window as Windows' drop target. Lives on the UI thread.
#[implement(IDropTarget)]
struct Target {
    hwnd: HWND,
    helper: Option<IDropTargetHelper>,
    now: RefCell<Option<Dragging>>,
}

impl Target {
    /// Shows `effect` (and where) under Windows' drag picture, when it changed.
    fn describe(&self, effect: u32, label: Option<String>) {
        let mut now = self.now.borrow_mut();
        let Some(d) = now.as_mut() else { return };
        let wanted = Some((effect, label));
        if d.described == wanted {
            return;
        }
        let (effect, label) = wanted.clone().unwrap_or_default();
        set_description(&d.data, effect, label.as_deref());
        d.described = wanted;
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> WinResult<()> {
        // SAFETY: Windows passes a valid in/out pointer.
        let allowed = unsafe { (*effect).0 };
        let at = POINT { x: pt.x, y: pt.y };
        let Some(data) = data.as_ref().cloned() else {
            // SAFETY: as above.
            unsafe { *effect = DROPEFFECT_NONE };
            return Ok(());
        };
        // Nothing is taken while a drop is still being copied (its source is waiting for it).
        let files = if WAITING.with(|w| w.get()) { Vec::new() } else { dropped_files(&data) };
        let mut msg = DragMsg::new(Phase::Enter, at, keys.0, allowed);
        msg.files = files;
        *self.now.borrow_mut() = Some(Dragging { data: data.clone(), right: keys.0 & MK_RBUTTON.0 != 0, described: None });
        msg.send(self.hwnd);
        let shown = if msg.handled { msg.effect & allowed } else { 0 };
        // SAFETY: as above.
        unsafe { *effect = DROPEFFECT(shown) };
        self.describe(shown, msg.label);
        if let Some(h) = &self.helper {
            // SAFETY: COM call with live objects; `at` outlives it.
            unsafe {
                let _ = h.DragEnter(self.hwnd, &data, &at, DROPEFFECT(shown));
            }
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> WinResult<()> {
        // SAFETY: Windows passes a valid in/out pointer.
        let allowed = unsafe { (*effect).0 };
        let at = POINT { x: pt.x, y: pt.y };
        let mut msg = DragMsg::new(Phase::Over, at, keys.0, allowed);
        if self.now.borrow().is_some() {
            msg.send(self.hwnd);
        }
        let shown = if msg.handled { msg.effect & allowed } else { 0 };
        // SAFETY: as above.
        unsafe { *effect = DROPEFFECT(shown) };
        self.describe(shown, msg.label);
        if let Some(h) = &self.helper {
            // SAFETY: COM call; `at` outlives it.
            unsafe {
                let _ = h.DragOver(&at, DROPEFFECT(shown));
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> WinResult<()> {
        if let Some(d) = self.now.borrow_mut().take() {
            if d.described.is_some() {
                set_description(&d.data, 0, None); // Windows' own words again over the next place
            }
        }
        let mut msg = DragMsg::new(Phase::Leave, POINT::default(), 0, 0);
        msg.send(self.hwnd);
        if let Some(h) = &self.helper {
            // SAFETY: plain COM call.
            unsafe {
                let _ = h.DragLeave();
            }
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> WinResult<()> {
        // SAFETY: Windows passes a valid in/out pointer. Nothing is done unless decided below.
        let allowed = unsafe { (*effect).0 };
        unsafe { *effect = DROPEFFECT_NONE };
        let at = POINT { x: pt.x, y: pt.y };
        let dragging = self.now.borrow_mut().take();
        let right = dragging.as_ref().is_some_and(|d| d.right);
        let data = data.as_ref().cloned().or_else(|| dragging.map(|d| d.data));
        let files = match &data {
            Some(d) if !WAITING.with(|w| w.get()) => dropped_files(d),
            _ => Vec::new(),
        };
        // The button is up by now, so the key state no longer says which one it was.
        let held = if right { keys.0 | MK_RBUTTON.0 } else { keys.0 };
        let mut msg = DragMsg::new(Phase::Drop, at, held, allowed);
        msg.files = files;
        msg.send(self.hwnd);
        let shown = if msg.handled { msg.effect & allowed } else { 0 };
        if let (Some(h), Some(d)) = (&self.helper, &data) {
            // The drag picture goes away now (before a right-button drop's menu).
            // SAFETY: COM call with live objects; `at` outlives it.
            unsafe {
                let _ = h.Drop(d, &at, DROPEFFECT(shown));
            }
        }
        let Some(plan) = msg.plan.take().filter(|_| msg.handled && shown != 0) else { return Ok(()) };
        let action = if right {
            match drop_menu(self.hwnd, at, &plan) {
                Some(a) => a,
                None => return Ok(()), // Cancel
            }
        } else {
            plan.action
        };
        // How the source hears that the job is done (see the module comment): told later, if it
        // can wait; otherwise this Drop waits for the job. Glint's own drags need neither.
        let transfer = matches!(action, Action::Transfer(_));
        let told = told_source(action);
        let id = NEXT_ASYNC.with(|n| n.replace(n.get() + 1));
        let async_op = data.as_ref().filter(|_| transfer && !plan.from_glint).and_then(async_capable).filter(|op| {
            // SAFETY: COM call on the source's object, on this (its) thread.
            unsafe { op.StartOperation(None::<&IBindCtx>) }.is_ok()
        });
        if let Some(op) = &async_op {
            ASYNC_OPS.with(|ops| ops.borrow_mut().push((id, op.clone(), told)));
        }
        let event = if transfer && !plan.from_glint && async_op.is_none() { Event::new() } else { None };
        let mut perform = DragMsg::new(Phase::Perform, at, keys.0, allowed);
        perform.signal = match (&async_op, &event) {
            (Some(_), _) => Some(Signal::Async { id, hwnd: self.hwnd.0 as isize }),
            (None, Some(e)) => Some(Signal::Event(e.clone())),
            _ => None,
        };
        perform.plan = Some(plan);
        perform.action = Some(action);
        perform.send(self.hwnd);
        let started = perform.handled && perform.started;
        if !started {
            if async_op.is_some() {
                finish_async(id, false); // nothing happened after all
            }
            return Ok(());
        }
        if let Some(e) = &event {
            wait_for(e);
        }
        // SAFETY: as above.
        unsafe { *effect = DROPEFFECT(told) };
        Ok(())
    }
}

/// The source's "I can wait" side, if it has one and wants it used.
fn async_capable(data: &IDataObject) -> Option<IDataObjectAsyncCapability> {
    let op: IDataObjectAsyncCapability = data.cast().ok()?;
    // SAFETY: plain COM call.
    let on = unsafe { op.GetAsyncMode() }.ok()?;
    on.as_bool().then_some(op)
}

/// Waits until `event` is set (the drop's job is done), keeping Glint's window alive meanwhile:
/// messages are handled as the main loop would. A WM_QUIT (the window was closed) ends the wait
/// and is passed on to the main loop.
fn wait_for(event: &Event) {
    WAITING.with(|w| w.set(true));
    let mut quit = None;
    let mut msg = MSG::default();
    // SAFETY: the standard way of waiting with a working window: MsgWaitForMultipleObjectsEx
    // returns for the event or for messages, which are handled here.
    unsafe {
        loop {
            let r = MsgWaitForMultipleObjectsEx(Some(&[event.0]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            if r == WAIT_OBJECT_0 || r == WAIT_FAILED {
                break;
            }
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    quit = Some(msg.wParam.0 as i32);
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if quit.is_some() {
                break;
            }
        }
        WAITING.with(|w| w.set(false));
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
    }
}

/// A right-button drop: Windows' usual menu at the drop point. None: Cancel (or clicked away).
fn drop_menu(hwnd: HWND, at: POINT, plan: &Plan) -> Option<Action> {
    let mut items: Vec<(Action, &str)> = Vec::new();
    match plan.dest {
        Dest::Folder(_) => {
            for &how in &plan.choices {
                let label = match how {
                    DropHow::Copy => "Copy here",
                    DropHow::Move => "Move here",
                    DropHow::Link => "Create shortcuts here",
                };
                items.push((Action::Transfer(how), label));
            }
        }
        Dest::RecycleBin => items.push((plan.action, "Move to the Recycle Bin")),
        Dest::Pin => items.push((Action::Pin, "Pin to the sidebar")),
    }
    if items.is_empty() {
        return None;
    }
    const CANCEL: usize = 100;
    // SAFETY: the menu is made, shown (modal, owned by our window) and destroyed here; label
    // buffers outlive AppendMenuW, which copies them.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for (i, (action, label)) in items.iter().enumerate() {
            let text = wide(label);
            let _ = AppendMenuW(menu, MF_STRING, i + 1, PCWSTR(text.as_ptr()));
            if *action == plan.action {
                let _ = SetMenuDefaultItem(menu, (i + 1) as u32, 0);
            }
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let cancel = wide("Cancel");
        let _ = AppendMenuW(menu, MF_STRING, CANCEL, PCWSTR(cancel.as_ptr()));
        // A menu of a window that isn't in front doesn't close when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN, at.x, at.y, None, hwnd, None).0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        (1..=items.len()).contains(&picked).then(|| items[picked - 1].0)
    }
}

/// The files and folders in a dragged data object (CF_HDROP), as full paths. Empty when it holds
/// none (text, a picture from a web page, an Outlook attachment...).
fn dropped_files(data: &IDataObject) -> Vec<String> {
    let format = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    // SAFETY: the medium Windows gives is read while locked and then released exactly once.
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return Vec::new() };
        let files = if medium.tymed == TYMED_HGLOBAL.0 as u32 { global_bytes(medium.u.hGlobal) } else { None };
        ReleaseStgMedium(&mut medium);
        files.map(|b| fileops::parse_drop_files(&b)).unwrap_or_default()
    }
}

/// A copy of the bytes in a memory block.
///
/// # Safety
/// `mem` must be a live memory block (from a STGMEDIUM that isn't released yet).
unsafe fn global_bytes(mem: HGLOBAL) -> Option<Vec<u8>> {
    // SAFETY: as the caller promises; read only while locked, within its size.
    unsafe {
        let size = GlobalSize(mem);
        let p = GlobalLock(mem) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(mem);
        Some(bytes)
    }
}

/// The words under Windows' drag picture: "Move to %1" with the place ("Documents"), or Windows'
/// own when `label` is None. Only programs whose drag picture allows it show them (File Explorer's
/// do); for the others this does nothing.
fn set_description(data: &IDataObject, effect: u32, label: Option<&str>) {
    let (kind, text): (DROPIMAGETYPE, &str) = match (label, effect) {
        (None, _) => (DROPIMAGE_INVALID, ""),
        (Some(_), e) if e & DROPEFFECT_MOVE.0 != 0 => (DROPIMAGE_MOVE, "Move to %1"),
        (Some(_), e) if e & DROPEFFECT_COPY.0 != 0 => (DROPIMAGE_COPY, "Copy to %1"),
        (Some(_), e) if e & DROPEFFECT_LINK.0 != 0 => (DROPIMAGE_LINK, "Create shortcut in %1"),
        (Some(_), _) => (DROPIMAGE_NONE, ""),
    };
    // (Windows lays this structure out packed, so its text fields are filled as a whole.)
    let (mut message, mut insert) = ([0u16; 260], [0u16; 260]);
    copy_text(&mut message, text);
    copy_text(&mut insert, label.unwrap_or(""));
    let d = DROPDESCRIPTION { r#type: kind, szMessage: message, szInsert: insert };
    // SAFETY: the block is filled while locked, then given to the data object (which owns it when
    // SetData succeeds) or freed here.
    unsafe {
        let format_id = RegisterClipboardFormatW(w!("DropDescription"));
        if format_id == 0 {
            return;
        }
        let size = std::mem::size_of::<DROPDESCRIPTION>();
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size) else { return };
        let p = GlobalLock(mem) as *mut DROPDESCRIPTION;
        if p.is_null() {
            let _ = GlobalFree(Some(mem));
            return;
        }
        p.write(d);
        let _ = GlobalUnlock(mem);
        let format = FORMATETC { cfFormat: format_id as u16, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: mem }, pUnkForRelease: std::mem::ManuallyDrop::new(None) };
        if data.SetData(&format, &medium, true).is_err() {
            let _ = GlobalFree(Some(mem));
        }
    }
}

/// `text` into a fixed, NUL-terminated UTF-16 buffer (cut short if it doesn't fit).
fn copy_text(buf: &mut [u16], text: &str) {
    let n = buf.len().saturating_sub(1);
    for (slot, unit) in buf.iter_mut().zip(text.encode_utf16().take(n).chain(std::iter::once(0))) {
        *slot = unit;
    }
}

// ---------------------------------------------------------------- dragging out of Glint

/// Drags `names` (items of `folder`) out of Glint: into another program, another folder in Glint,
/// or another Glint window. Runs Windows' drag loop, so it returns only when the drag is over.
/// `right`: dragged with the right button (the program dropped into shows its menu). Returns what
/// the drop did (DROPEFFECT) - the program dropped into did it; Glint has nothing left to do - or why
/// the drag couldn't start.
pub fn drag_out(hwnd: HWND, folder: &str, names: &[String], right: bool) -> Result<u32, String> {
    if WAITING.with(|w| w.get()) {
        return Err("Wait until the files dropped a moment ago are copied.".to_string());
    }
    let Some(data) = data_object(hwnd, folder, names) else {
        return Err("Windows couldn't pick these items up to drag them.".to_string());
    };
    // SAFETY: COM calls on this (the UI) thread with live objects. The drop source lives until the
    // drag loop is over.
    unsafe {
        // Windows' drag picture (the items' pictures with their number), as Explorer shows it.
        if let Ok(helper) = CoCreateInstance::<_, IDragSourceHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) {
            let _ = helper.InitializeFromWindow(None, None, &data);
        }
        let source: IDropSource = Source { right }.into();
        let allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
        let effect = match SHDoDragDrop(Some(hwnd), &data, &source, allowed) {
            Ok(e) => e.0,
            Err(e) => {
                // Not there (or refused): OLE's own drag loop does the same, without the extras.
                log::line(&format!("drag and drop: SHDoDragDrop failed ({e}); using DoDragDrop"));
                let mut effect = DROPEFFECT_NONE;
                let hr = DoDragDrop(&data, &source, allowed, &mut effect);
                if hr.is_err() {
                    return Err(format!("Windows couldn't start dragging ({}).", fileops::explain(hr)));
                }
                effect.0
            }
        };
        log::line(&format!("drag and drop: dragged {} item(s) out of \"{folder}\": effect {effect}", names.len()));
        Ok(effect)
    }
}

/// Windows' data object for items of one folder: the one File Explorer makes for them (with the
/// file list, shell ids and whatever else other programs ask for). None if Windows can't make one.
fn data_object(hwnd: HWND, folder: &str, names: &[String]) -> Option<IDataObject> {
    let parent_id = parse(folder)?;
    // SAFETY: COM calls on this thread; every id list outlives the calls that use it and is freed
    // once (Pidl). The data object keeps its own copies.
    unsafe {
        let shell_folder: IShellFolder = SHBindToObject(None::<&IShellFolder>, parent_id.0, None::<&IBindCtx>).ok()?;
        let mut children = Vec::with_capacity(names.len());
        for name in names {
            let w = wide(name);
            let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
            if shell_folder.ParseDisplayName(hwnd, None::<&IBindCtx>, PCWSTR(w.as_ptr()), None, &mut child, std::ptr::null_mut()).is_ok() && !child.is_null() {
                children.push(Pidl(child));
            }
        }
        if children.len() != names.len() {
            // Some went missing since the list was read: drag nothing rather than part of it.
            log::line(&format!("drag and drop: {} of {} items couldn't be found", names.len() - children.len(), names.len()));
            return None;
        }
        let ids: Vec<*const ITEMIDLIST> = children.iter().map(|c| c.0 as *const _).collect();
        match shell_folder.GetUIObjectOf::<IDataObject>(hwnd, &ids, None) {
            Ok(d) => Some(d),
            Err(_) => SHCreateDataObject::<_, IDataObject>(Some(parent_id.0 as *const _), Some(&ids), None::<&IDataObject>).ok(),
        }
    }
}

/// An item id list from the shell, freed when dropped.
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

/// Glint's side of its own drags: when the drag ends, and with what.
#[implement(IDropSource)]
struct Source {
    /// Dragged with the right button.
    right: bool,
}

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        let (mine, other) = if self.right { (MK_RBUTTON.0, MK_LBUTTON.0) } else { (MK_LBUTTON.0, MK_RBUTTON.0) };
        if escape.as_bool() || keys.0 & other != 0 {
            DRAGDROP_S_CANCEL // Esc, or the other button pressed too: called off, as in Explorer
        } else if keys.0 & mine == 0 {
            DRAGDROP_S_DROP // the button came up
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: u32 = 7; // copy | move | link

    fn paths(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn same_drive_moves_another_drive_copies() {
        let files = paths(&["C:\\Users\\me\\a.txt", "C:\\Users\\me\\b"]);
        assert_eq!(folder_effect(&files, "C:\\Backup", 0, ALL), Some(DropHow::Move));
        assert_eq!(folder_effect(&files, "c:\\backup", 0, ALL), Some(DropHow::Move), "drive letters in any case");
        assert_eq!(folder_effect(&files, "D:\\Backup", 0, ALL), Some(DropHow::Copy));
        assert_eq!(folder_effect(&files, "\\\\nas\\share\\x", 0, ALL), Some(DropHow::Copy));
        let unc = paths(&["\\\\nas\\share\\a.txt"]);
        assert_eq!(folder_effect(&unc, "\\\\NAS\\Share\\b", 0, ALL), Some(DropHow::Move), "the same share");
        assert_eq!(folder_effect(&unc, "\\\\nas\\other", 0, ALL), Some(DropHow::Copy), "another share");
    }

    #[test]
    fn keys_choose_like_explorer() {
        let files = paths(&["C:\\a\\x.txt"]);
        let ctrl = MK_CONTROL.0;
        let shift = MK_SHIFT.0;
        assert_eq!(folder_effect(&files, "C:\\b", ctrl, ALL), Some(DropHow::Copy));
        assert_eq!(folder_effect(&files, "D:\\b", shift, ALL), Some(DropHow::Move));
        assert_eq!(folder_effect(&files, "D:\\b", MK_ALT, ALL), Some(DropHow::Link));
        assert_eq!(folder_effect(&files, "D:\\b", ctrl | shift, ALL), Some(DropHow::Link));
        // The buttons themselves don't count.
        assert_eq!(folder_effect(&files, "C:\\b", MK_LBUTTON.0, ALL), Some(DropHow::Move));
        assert_eq!(folder_effect(&files, "C:\\b", MK_RBUTTON.0 | ctrl, ALL), Some(DropHow::Copy));
    }

    #[test]
    fn what_the_source_allows() {
        let files = paths(&["C:\\a\\x.txt"]);
        let copy_only = DROPEFFECT_COPY.0;
        // The default would move, but only copying is offered: copy.
        assert_eq!(folder_effect(&files, "C:\\b", 0, copy_only), Some(DropHow::Copy));
        // Shift asks for moving, which isn't offered: nothing.
        assert_eq!(folder_effect(&files, "C:\\b", MK_SHIFT.0, copy_only), None);
        assert_eq!(folder_effect(&files, "C:\\b", 0, 0), None);
    }

    #[test]
    fn never_into_itself_nor_onto_its_own_folder() {
        let folder = paths(&["C:\\Projects\\glint"]);
        assert_eq!(folder_effect(&folder, "C:\\Projects\\glint", 0, ALL), None, "onto itself");
        assert_eq!(folder_effect(&folder, "C:\\Projects\\glint\\src", 0, ALL), None, "into a folder inside it");
        assert_eq!(folder_effect(&folder, "C:\\Projects\\glint\\src", MK_CONTROL.0, ALL), None, "not even a copy");
        assert_eq!(folder_effect(&folder, "C:\\Projects\\glint2", 0, ALL), Some(DropHow::Move), "a folder that only starts the same");
        // Moving into the folder they're in does nothing; copying there makes "- Copy" items.
        let here = paths(&["C:\\x\\a.txt", "C:\\x\\b.txt"]);
        assert_eq!(folder_effect(&here, "C:\\x", 0, ALL), None, "dropped where they are: nothing, as in Explorer");
        assert_eq!(folder_effect(&here, "C:\\x", 0, DROPEFFECT_COPY.0), None, "not even when only copying is offered");
        assert_eq!(folder_effect(&here, "C:\\x", MK_SHIFT.0, ALL), None);
        assert_eq!(folder_effect(&here, "C:\\x\\", MK_CONTROL.0, ALL), Some(DropHow::Copy));
        assert_eq!(folder_choices(&here, "C:\\x", ALL), vec![DropHow::Copy, DropHow::Link]);
    }

    #[test]
    fn a_right_button_drop_where_they_are_shows_its_menu() {
        // Explorer's menu, with "Copy here" and "Create shortcuts here", right where the items are.
        let here = paths(&["C:\\x\\a.txt", "C:\\x\\b.txt"]);
        let right = MK_RBUTTON.0;
        assert_eq!(folder_effect(&here, "C:\\x", right, ALL), Some(DropHow::Copy), "the menu's first choice");
        assert_eq!(folder_effect(&here, "C:\\x", right | MK_SHIFT.0, ALL), Some(DropHow::Copy));
        assert_eq!(folder_effect(&here, "C:\\x", right, DROPEFFECT_MOVE.0), None, "nothing the menu could offer");
        // Never into itself, whatever the button.
        let folder = paths(&["C:\\Projects\\glint"]);
        assert_eq!(folder_effect(&folder, "C:\\Projects\\glint", right, ALL), None);
        assert_eq!(folder_effect(&folder, "C:\\Projects\\glint\\src", right, ALL), None);
    }

    #[test]
    fn only_real_items_are_dropped() {
        assert_eq!(folder_effect(&paths(&["E:\\"]), "C:\\x", 0, ALL), None, "a whole drive");
        assert_eq!(folder_effect(&paths(&["notes.txt"]), "C:\\x", 0, ALL), None, "a bare name");
        assert_eq!(folder_effect(&paths(&["C:\\a.txt"]), "", 0, ALL), None, "This PC");
        assert_eq!(folder_effect(&paths(&["C:\\a.txt"]), recycle::LOCATION, 0, ALL), None, "the bin isn't a folder");
        assert_eq!(bin_effect(&paths(&["C:\\a.txt"]), ALL), DROPEFFECT_MOVE.0);
        assert_eq!(bin_effect(&paths(&["E:\\"]), ALL), 0);
        assert_eq!(bin_effect(&paths(&["C:\\a.txt"]), DROPEFFECT_COPY.0), 0, "a source that can't let go");
    }

    #[test]
    fn sources_are_never_told_to_delete() {
        assert_eq!(told_source(Action::Transfer(DropHow::Move)), 0, "Glint moved them itself");
        assert_eq!(told_source(Action::Recycle { permanent: false }), 0);
        assert_eq!(told_source(Action::Transfer(DropHow::Copy)), DROPEFFECT_COPY.0);
    }

    #[test]
    fn description_text_fits() {
        let mut buf = [7u16; 5];
        copy_text(&mut buf, "Documents");
        assert_eq!(&buf, &[68, 111, 99, 117, 0]);
        let mut buf = [7u16; 5];
        copy_text(&mut buf, "ab");
        assert_eq!(&buf[..3], &[97, 98, 0]);
    }
}
