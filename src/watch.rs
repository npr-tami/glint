//! Watching folders, so the list updates by itself when files change.
//!
//! - **What is watched.** The folder in the active tab, with ReadDirectoryChangesW, on a background
//!   thread of its own ("watch folder"). Only that folder, not its subfolders, like Explorer. Other
//!   tabs aren't watched: app.rs reads a tab again when it's shown.
//! - **The folder itself.** Its parent folder is watched too, for folder names only. So Glint notices
//!   when the folder it shows is renamed (`Event::Moved`: the tab follows it), deleted or moved away
//!   (it's read again and the list says it doesn't exist any more), or comes back (it's read again).
//! - **Bursts.** Changes come in bursts: a copy of 1,000 files, an app saving a file in three
//!   steps. The thread waits until things have been quiet for QUIET, or MAX_WAIT has passed since
//!   the first change, and then sends one `Event::Changed`. The app reads the folder again in the
//!   background and merges it in. So a huge copy costs a few re-reads, not thousands of refreshes,
//!   even when Windows' change list overflows (ERROR_NOTIFY_ENUM_DIR).
//! - **No gap.** A folder read waits (`Armed::wait`, at most ARM_WAIT) until the watch is under
//!   way, so nothing that changes between the read and the start of the watch is missed. If the
//!   read didn't wait that long (a slow network), the watch reports a change once it's running.
//! - **Ejecting a USB drive.** Windows asks every program to let go of a drive before it's ejected
//!   (WM_DEVICECHANGE with DBT_DEVICEQUERYREMOVE, for a handle registered with
//!   RegisterDeviceNotification). The window passes that to `Watcher::device_event`, and the thread
//!   closes its handles at once. So the watch never makes "Safely remove" fail with "in use".
//! - **What isn't watched.** This PC, network folders that don't support watching, and folders
//!   that can't be opened: they simply aren't watched (one line in the log, no message). F5 still
//!   works there. A watch that stops while its folder may still be there (the network dropped, the
//!   computer slept) is tried again now and then (RETRY_FIRST).
//! - **Idle means idle.** With nothing changing, the thread sleeps in WaitForMultipleObjects.
//!
//! The thread owns its handles. The UI thread only sends it orders (stop, let go, watch again)
//! through `Shared` and never waits for it, except for up to RELEASE_WAIT while a drive is being
//! ejected.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, ERROR_IO_INCOMPLETE, ERROR_NOTIFY_ENUM_DIR, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FileStandardInfo, GetDriveTypeW, GetFileInformationByHandleEx, ReadDirectoryChangesW, FILE_STANDARD_INFO, FILE_ACTION_ADDED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME,
    FILE_ACTION_RENAMED_OLD_NAME, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE,
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::WindowsAndMessaging::{
    RegisterDeviceNotificationW, UnregisterDeviceNotification, DBT_DEVICEQUERYREMOVE, DBT_DEVICEQUERYREMOVEFAILED,
    DBT_DEVICEREMOVECOMPLETE, DBT_DEVICEREMOVEPENDING, DBT_DEVTYP_HANDLE, DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_HANDLE,
    DEV_BROADCAST_HDR, HDEVNOTIFY,
};

use crate::folder;
use crate::log;
use crate::shell;

/// Changes are reported once nothing more has happened for this long...
const QUIET: Duration = Duration::from_millis(80);
/// ...or at the latest this long after the first one, so a long copy still shows its progress.
const MAX_WAIT: Duration = Duration::from_millis(400);
/// The longest time between two reports the app may ask for (see `Watcher::set_gap`).
const MAX_GAP: Duration = Duration::from_secs(4);
/// How long a folder read waits for the watch to be under way before it reads anyway. Starting a
/// watch takes well under a millisecond on a local drive; on a slow network share the read goes
/// ahead after this, and the watch reports a change once it's running (so it's read once more).
const ARM_WAIT: Duration = Duration::from_millis(50);
/// How long the window waits for the thread to let go of a drive that is being ejected.
const RELEASE_WAIT: Duration = Duration::from_millis(1000);
/// How long closing a watch waits for Windows to cancel its request (see `DirWatch`'s Drop).
const CANCEL_WAIT_MS: u32 = 3000;
/// A folder that stopped being watched is tried again this long after (`Worker::lost_folder`),
/// then twice as long after each try that fails, up to RETRY_MAX.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// Bytes of changes Windows may collect between two looks. 64 KB is the most a network folder
/// allows. When more piles up, Windows says "overflow" and the folder is simply read again.
const FOLDER_BUFFER: usize = 64 * 1024;
const PARENT_BUFFER: usize = 8 * 1024;
/// Renames remembered for one report (so the selection can follow renamed items); beyond this the
/// rest are only re-read, not followed.
const MAX_RENAMES: usize = 256;

/// What to watch in the folder: names (files and folders), sizes, dates and attributes.
const FOLDER_CHANGES: FILE_NOTIFY_CHANGE = FILE_NOTIFY_CHANGE(
    FILE_NOTIFY_CHANGE_FILE_NAME.0
        | FILE_NOTIFY_CHANGE_DIR_NAME.0
        | FILE_NOTIFY_CHANGE_SIZE.0
        | FILE_NOTIFY_CHANGE_LAST_WRITE.0
        | FILE_NOTIFY_CHANGE_ATTRIBUTES.0,
);

/// What the watch tells the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Things in the folder changed: read it again. `renamed` holds the items renamed meanwhile
    /// (old name, new name, both in lower case), so the selection can follow them.
    Changed { renamed: Vec<(String, String)> },
    /// The folder itself was renamed: this is its new path.
    Moved(String),
}

/// GLINT_WATCH_LOG=1: write every report and every re-read to the log (for checking how the watch
/// behaves, e.g. during a big copy).
pub fn verbose() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GLINT_WATCH_LOG").is_some())
}

/// Whether a path is something that can be watched: a folder path, not This PC ("").
pub fn can_watch(path: &str) -> bool {
    let b = path.as_bytes();
    // (Not inside a .zip or .7z: archive.rs reads it, and nothing changes it while it's shown.)
    (path.starts_with("\\\\") || (b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\'))) && crate::archive::split(path).is_none()
}

// ---------------------------------------------------------------- the app's side

/// The folder watch of the active tab. Lives in AppState, on the UI thread.
pub struct Watcher {
    current: Option<Current>,
    tx: Sender<(u64, Event)>,
    rx: Receiver<(u64, Event)>,
    /// The window to wake when there's news (0 until it exists).
    hwnd: Arc<AtomicIsize>,
    next_id: u64,
    /// The shortest time between two reports, in ms (see `set_gap`).
    gap_ms: u32,
}

/// The watch that is running.
struct Current {
    /// Tells this watch's news apart from that of a watch that was stopped.
    id: u64,
    tab_id: u64,
    path: String,
    shared: Arc<Shared>,
}

impl Watcher {
    pub fn new(hwnd: Arc<AtomicIsize>) -> Watcher {
        let (tx, rx) = channel();
        Watcher { current: None, tx, rx, hwnd, next_id: 1, gap_ms: 0 }
    }

    /// Whether this tab's folder is the one being watched (or tried to be: a folder that can't
    /// be watched isn't tried again and again).
    pub fn is_on(&self, tab_id: u64, path: &str) -> bool {
        self.current.as_ref().is_some_and(|c| c.tab_id == tab_id && c.path == path)
    }

    /// Whether this tab's folder is being watched and the watch works.
    pub fn is_working(&self, tab_id: u64, path: &str) -> bool {
        self.is_on(tab_id, path) && self.current.as_ref().is_some_and(|c| !c.shared.failed.load(Ordering::SeqCst))
    }

    /// Starts watching `path` for tab `tab_id`, instead of whatever was watched before. A folder
    /// read should wait for the returned signal before it starts (see the module comment).
    pub fn watch(&mut self, tab_id: u64, path: &str) -> Armed {
        self.stop();
        let id = self.next_id;
        self.next_id += 1;
        // A new folder: how long it takes to read is learnt again from its first re-read.
        self.gap_ms = 0;
        let armed = Armed(Some(Arc::default()));
        let shared = Arc::new(Shared {
            wake: Signal::new(false),
            orders: Mutex::new(Orders::default()),
            armed: armed.clone(),
            released: Mutex::new(false),
            released_cv: Condvar::new(),
            notify: AtomicIsize::new(0),
            failed: AtomicBool::new(false),
            gap_ms: AtomicU32::new(self.gap_ms),
        });
        let (sh, tx, hwnd, p) = (shared.clone(), self.tx.clone(), self.hwnd.clone(), path.to_string());
        let started = shared.wake.is_some() && shell::background("watch folder", move || Worker::new(id, p, sh, tx, hwnd).run());
        if !started {
            // No event or thread to be had (Windows is out of resources): not watched (F5 tries
            // again), and nobody waits for it.
            shared.failed.store(true, Ordering::SeqCst);
            shared.armed.set();
        }
        self.current = Some(Current { id, tab_id, path: path.to_string(), shared });
        armed
    }

    /// The running watch's signal (already set once it's under way), for a read that is about to start.
    pub fn armed(&self) -> Armed {
        self.current.as_ref().map(|c| c.shared.armed.clone()).unwrap_or_default()
    }

    /// Stops watching. The thread closes its handles and ends by itself; nothing waits for it.
    pub fn stop(&mut self) {
        if let Some(c) = self.current.take() {
            c.shared.order(|o| o.stop = true);
        }
    }

    /// The news of the running watch, as (tab id, event). News of watches already stopped is dropped.
    pub fn take(&mut self) -> Vec<(u64, Event)> {
        let mut out = Vec::new();
        while let Ok((id, event)) = self.rx.try_recv() {
            if let Some(c) = self.current.as_ref().filter(|c| c.id == id) {
                out.push((c.tab_id, event));
            }
        }
        out
    }

    /// The shortest time between two reports. The app sets it from how long reading the folder
    /// again takes, so a folder of 100,000 files that keeps changing isn't read over and over.
    pub fn set_gap(&mut self, gap: Duration) {
        self.gap_ms = gap.min(MAX_GAP).as_millis() as u32;
        if let Some(c) = &self.current {
            c.shared.gap_ms.store(self.gap_ms, Ordering::SeqCst);
        }
    }

    /// The folder couldn't be read: let go of it, but keep watching its parent. A folder that is
    /// being deleted while someone has it open is only "delete pending" (older Windows, USB sticks
    /// and network drives; Wine too) and really goes once nobody holds it - the parent's watch then
    /// sees it go, and the list says it doesn't exist.
    pub fn let_go(&self, tab_id: u64) {
        if let Some(c) = self.current.as_ref().filter(|c| c.tab_id == tab_id) {
            c.shared.order(|o| o.let_go = true);
        }
    }

    /// The window exists now: the watch can ask to be told about drives being ejected.
    pub fn window_ready(&self) {
        if let Some(c) = &self.current {
            c.shared.order(|o| o.window = true);
        }
    }

    /// WM_DEVICECHANGE: when Windows wants to eject the drive the watched folder is on, let go of
    /// it (and watch again if ejecting is called off). Other device events are none of its business.
    /// Gives the watched folder when its drive is about to be ejected, so whatever else is reading
    /// that drive can stop too.
    pub fn device_event(&self, event: u32, data: isize) -> Option<String> {
        let c = self.current.as_ref()?;
        let mine = c.shared.notify.load(Ordering::SeqCst);
        if mine == 0 || data == 0 || !matches!(event, DBT_DEVICEQUERYREMOVE | DBT_DEVICEQUERYREMOVEFAILED | DBT_DEVICEREMOVEPENDING | DBT_DEVICEREMOVECOMPLETE) {
            return None;
        }
        // SAFETY: for these events Windows passes a DEV_BROADCAST_HDR in lparam. The handle fields
        // are read only when the header says it is a DEV_BROADCAST_HANDLE that is long enough, and
        // unaligned reads make no assumption about where it sits.
        let notify = unsafe {
            let hdr = std::ptr::read_unaligned(data as *const DEV_BROADCAST_HDR);
            let needed = std::mem::offset_of!(DEV_BROADCAST_HANDLE, dbch_eventguid);
            if hdr.dbch_devicetype != DBT_DEVTYP_HANDLE || (hdr.dbch_size as usize) < needed {
                return None;
            }
            let at = (data as *const u8).add(std::mem::offset_of!(DEV_BROADCAST_HANDLE, dbch_hdevnotify));
            std::ptr::read_unaligned(at as *const HDEVNOTIFY)
        };
        if notify.0 as isize != mine {
            return None;
        }
        match event {
            DBT_DEVICEQUERYREMOVE => {
                let done = c.shared.release(RELEASE_WAIT);
                log::line(&format!("watch: a drive is being ejected; let go of \"{}\": {}", c.path, if done { "yes" } else { "not in time" }));
                return Some(c.path.clone());
            }
            DBT_DEVICEQUERYREMOVEFAILED => c.shared.order(|o| o.reopen = true),
            _ => c.shared.order(|o| o.gone = true),
        }
        None
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Tells a folder read when the watch on that folder is under way, so the read can start after it
/// and miss nothing. `Armed::none()` (the default) never waits.
#[derive(Clone, Default)]
pub struct Armed(Option<Arc<ArmFlag>>);

#[derive(Default)]
struct ArmFlag {
    /// (the watch is under way or couldn't start, a read stopped waiting before that)
    state: Mutex<(bool, bool)>,
    cv: Condvar,
}

impl Armed {
    pub fn none() -> Armed {
        Armed(None)
    }

    /// Waits until the watch is under way (or couldn't start), but at most ARM_WAIT.
    pub fn wait(&self) {
        let Some(f) = &self.0 else { return };
        let guard = lock(&f.state);
        let mut guard = match f.cv.wait_timeout_while(guard, ARM_WAIT, |s| !s.0) {
            Ok((g, _)) => g,
            Err(poisoned) => poisoned.into_inner().0,
        };
        if !guard.0 {
            guard.1 = true;
        }
    }

    /// The watch is under way. Returns true if a read stopped waiting before (and may have missed
    /// something).
    fn set(&self) -> bool {
        let Some(f) = &self.0 else { return false };
        let mut s = lock(&f.state);
        s.0 = true;
        f.cv.notify_all();
        s.1
    }
}

// ---------------------------------------------------------------- between the two threads

/// What the UI thread asks the watch thread to do.
#[derive(Default)]
struct Orders {
    stop: bool,
    /// A drive is being ejected: close the handles now.
    release: bool,
    /// Ejecting was called off: watch again.
    reopen: bool,
    /// The drive is gone.
    gone: bool,
    /// The folder couldn't be read: close it, keep the parent (see `Watcher::let_go`).
    let_go: bool,
    /// The window exists now (register for eject messages).
    window: bool,
}

struct Shared {
    /// Set to make the thread look at `orders` (auto-reset). None if Windows couldn't make one (then
    /// no thread is started).
    wake: Option<Signal>,
    orders: Mutex<Orders>,
    armed: Armed,
    /// The thread has let go of its handles (answer to `release`).
    released: Mutex<bool>,
    released_cv: Condvar,
    /// The device notification the thread registered (0 = none), to recognise the eject messages
    /// meant for this watch.
    notify: AtomicIsize,
    /// The folder couldn't be watched, or stopped being watchable.
    failed: AtomicBool,
    /// The shortest time between two reports, in ms.
    gap_ms: AtomicU32,
}

impl Shared {
    fn order(&self, f: impl FnOnce(&mut Orders)) {
        f(&mut lock(&self.orders));
        if let Some(w) = &self.wake {
            w.set();
        }
    }

    /// Asks the thread to close its handles and waits (at most `wait`) until it has. True if it did.
    fn release(&self, wait: Duration) -> bool {
        *lock(&self.released) = false;
        self.order(|o| o.release = true);
        let guard = lock(&self.released);
        match self.released_cv.wait_timeout_while(guard, wait, |done| !*done) {
            Ok((g, _)) => *g,
            Err(poisoned) => *poisoned.into_inner().0,
        }
    }

    fn say_released(&self) {
        *lock(&self.released) = true;
        self.released_cv.notify_all();
    }
}

/// A Windows event object, closed when dropped.
struct Signal(HANDLE);

// SAFETY: an event handle may be used and closed from any thread.
unsafe impl Send for Signal {}
// SAFETY: as above; setting and waiting are thread-safe operations on the kernel object.
unsafe impl Sync for Signal {}

impl Signal {
    fn new(manual_reset: bool) -> Option<Signal> {
        // SAFETY: a plain unnamed event; the handle is owned by the Signal.
        unsafe { CreateEventW(None, manual_reset, false, PCWSTR::null()) }.ok().map(Signal)
    }

    fn set(&self) {
        // SAFETY: our own event.
        unsafe {
            let _ = SetEvent(self.0);
        }
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        // SAFETY: closes the event we created; nobody uses it after this.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A thread can't panic halfway (panics end the app), so a poisoned lock still holds sound data.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Logs that a folder can't be watched - once per folder, so the log doesn't fill up.
fn log_once(path: &str, what: &str, code: u32) {
    static SAID: Mutex<Vec<String>> = Mutex::new(Vec::new());
    {
        let mut said = lock(&SAID);
        if said.iter().any(|p| p == path) {
            return;
        }
        if said.len() >= 64 {
            said.remove(0);
        }
        said.push(path.to_string());
    }
    log::line(&format!("watch: \"{path}\" {what} (Windows error {code}); it updates on F5 only"));
}

/// The Windows error number inside an error.
fn win32_code(e: &windows::core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

// ---------------------------------------------------------------- one folder handle

/// What a finished ReadDirectoryChangesW request brought.
enum Outcome {
    /// (action, name) of each change.
    Records(Vec<(u32, String)>),
    /// More changed than fits in the buffer: the details are lost, the folder must be read again.
    Overflow,
    /// The folder can't be watched any more (deleted, drive pulled out, network gone).
    Failed(u32),
    /// Not finished after all (shouldn't happen: the event is only set when it is).
    NotYet,
}

/// One folder handle with a ReadDirectoryChangesW request waiting on it.
struct DirWatch {
    handle: HANDLE,
    /// Set (manual reset) when the request finishes.
    done: Signal,
    /// The request's OVERLAPPED and buffer. Boxed, because Windows writes into them until the
    /// request has ended, wherever this struct moves meanwhile.
    ov: ManuallyDrop<Box<OVERLAPPED>>,
    buf: ManuallyDrop<Box<[u32]>>,
    filter: FILE_NOTIFY_CHANGE,
    /// A request is under way.
    pending: bool,
}

impl DirWatch {
    /// Opens a folder and asks for its first changes. Err(Windows error) if it can't be watched.
    fn open(path: &str, filter: FILE_NOTIFY_CHANGE, bytes: usize) -> Result<DirWatch, u32> {
        let w = folder::wide_path(path);
        // SAFETY: `w` is NUL-terminated and outlives the call. Sharing read, write and delete means
        // the watch never stands in anyone's way: files can still be changed, and the folder itself
        // renamed or deleted (it's let go of as soon as that's seen).
        let handle = unsafe {
            CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map_err(|e| win32_code(&e))?;
        let Some(done) = Signal::new(true) else {
            // SAFETY: the handle we just opened; nothing else has it.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(0);
        };
        let mut d = DirWatch {
            handle,
            done,
            ov: ManuallyDrop::new(Box::default()),
            buf: ManuallyDrop::new(vec![0u32; bytes / 4].into_boxed_slice()),
            filter,
            pending: false,
        };
        d.ask()?;
        Ok(d)
    }

    /// Asks Windows for the next changes.
    fn ask(&mut self) -> Result<(), u32> {
        **self.ov = OVERLAPPED { hEvent: self.done.0, ..Default::default() };
        let len = (self.buf.len() * 4) as u32;
        let buf: *mut c_void = self.buf.as_mut_ptr().cast();
        let ov: *mut OVERLAPPED = &mut **self.ov;
        // SAFETY: our own event and handle. `buf` and `ov` are boxed and stay valid until the
        // request has ended (see Drop).
        let r = unsafe {
            let _ = ResetEvent(self.done.0);
            ReadDirectoryChangesW(self.handle, buf, len, false, self.filter, None, Some(ov), None)
        };
        self.pending = r.is_ok();
        r.map_err(|e| win32_code(&e))
    }

    /// Whether the folder was deleted while held open here ("delete pending": it really goes once
    /// nobody holds it any more).
    fn delete_pending(&self) -> bool {
        let mut info = FILE_STANDARD_INFO::default();
        // SAFETY: `info` is a writable FILE_STANDARD_INFO and its size is passed.
        let r = unsafe {
            GetFileInformationByHandleEx(self.handle, FileStandardInfo, &mut info as *mut _ as *mut c_void, std::mem::size_of::<FILE_STANDARD_INFO>() as u32)
        };
        r.is_ok() && info.DeletePending
    }

    /// What the finished request brought (call when `done` is set).
    fn take(&mut self) -> Outcome {
        let mut n = 0u32;
        // SAFETY: `ov` belongs to the request on `handle`; `n` is a valid out-pointer.
        let r = unsafe { GetOverlappedResult(self.handle, &**self.ov, &mut n, false) };
        match r {
            Ok(()) => {
                self.pending = false;
                if n == 0 {
                    return Outcome::Overflow; // Windows' way of saying the buffer overflowed
                }
                let n = (n as usize).min(self.buf.len() * 4);
                // SAFETY: viewing initialised u32s as bytes is always valid; `n` is within the buffer.
                let bytes = unsafe { std::slice::from_raw_parts(self.buf.as_ptr().cast::<u8>(), n) };
                Outcome::Records(parse_records(bytes))
            }
            Err(e) => {
                let code = win32_code(&e);
                if code == ERROR_IO_INCOMPLETE.0 {
                    return Outcome::NotYet;
                }
                self.pending = false;
                if code == ERROR_NOTIFY_ENUM_DIR.0 {
                    Outcome::Overflow
                } else {
                    Outcome::Failed(code)
                }
            }
        }
    }
}

impl Drop for DirWatch {
    fn drop(&mut self) {
        // SAFETY: our own handles. `ov` and `buf` are only freed once Windows is done with them.
        unsafe {
            let mut free = true;
            if self.pending {
                let _ = CancelIoEx(self.handle, Some(&**self.ov));
                free = WaitForSingleObject(self.done.0, CANCEL_WAIT_MS) == WAIT_OBJECT_0;
            }
            let _ = CloseHandle(self.handle);
            if free {
                ManuallyDrop::drop(&mut self.ov);
                ManuallyDrop::drop(&mut self.buf);
            }
            // Otherwise a driver ignored the cancel (some network redirectors do): its few KB are
            // left to it rather than freed while it may still write into them.
        }
    }
}

/// The changes in a list of FILE_NOTIFY_INFORMATION records: (action, name). Anything malformed
/// ends the list (never a crash).
fn parse_records(bytes: &[u8]) -> Vec<(u32, String)> {
    let word = |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut out = Vec::new();
    let mut at = 0usize;
    while let (Some(next), Some(action), Some(len)) = (word(at), word(at + 4), word(at + 8)) {
        let Some(name) = at.checked_add(12).and_then(|s| bytes.get(s..s.checked_add(len as usize)?)) else { break };
        let units: Vec<u16> = name.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        at = match at.checked_add(next as usize) {
            Some(a) => a,
            None => break,
        };
    }
    out
}

/// When to report changes first seen at `first` and last seen at `last`: after QUIET with nothing
/// new, or MAX_WAIT after the first, whichever comes first - but never sooner than `gap` after the
/// last report.
fn due(first: Instant, last: Instant, last_report: Option<Instant>, gap: Duration) -> Instant {
    let at = (last + QUIET).min(first + MAX_WAIT);
    match last_report {
        Some(r) => at.max(r + gap),
        None => at,
    }
}

// ---------------------------------------------------------------- the watch thread

/// What the parent folder's changes mean for the watched folder.
enum News {
    Nothing,
    /// It was renamed to this name.
    Renamed(String),
    /// It was deleted or moved somewhere else.
    Gone,
    /// It is there (again).
    Back,
}

/// The watch thread's state. Lives on that thread only.
struct Worker {
    id: u64,
    path: String,
    /// The folder's own name in lower case, and the folder it is in (None for a drive or a share).
    name: String,
    parent: Option<String>,
    /// On a drive that can be ejected (eject messages make no sense for network folders).
    local: bool,
    me: Option<DirWatch>,
    up: Option<DirWatch>,
    notify: Option<HDEVNOTIFY>,
    /// Changes not reported yet: (first seen, last seen).
    pending: Option<(Instant, Instant)>,
    last_report: Option<Instant>,
    renamed: Vec<(String, String)>,
    /// A rename's old name, until its new name comes (they are two records, perhaps in two batches).
    old_name: Option<String>,
    /// The same for the parent's records: the old name was this folder's.
    old_was_mine: bool,
    /// When to try watching the folder again after it stopped being watched, and how long the
    /// wait was.
    retry: Option<(Instant, Duration)>,
    sh: Arc<Shared>,
    tx: Sender<(u64, Event)>,
    hwnd: Arc<AtomicIsize>,
}

impl Worker {
    fn new(id: u64, path: String, sh: Arc<Shared>, tx: Sender<(u64, Event)>, hwnd: Arc<AtomicIsize>) -> Worker {
        let parent = folder::parent(&path).filter(|p| !p.is_empty());
        let name = folder::file_name(&path).to_lowercase();
        let local = is_local(&path);
        Worker {
            id,
            path,
            name,
            parent,
            local,
            me: None,
            up: None,
            notify: None,
            pending: None,
            last_report: None,
            renamed: Vec::new(),
            old_name: None,
            old_was_mine: false,
            retry: None,
            sh,
            tx,
            hwnd,
        }
    }

    fn run(mut self) {
        let Some(wake) = self.sh.wake.as_ref().map(|s| s.0) else { return };
        self.open();
        if self.sh.armed.set() {
            self.note_change(); // a read went ahead without waiting for us: it may have missed something
        }
        self.register();
        loop {
            let mut waits = vec![wake];
            let me_at = self.me.as_ref().map(|d| {
                waits.push(d.done.0);
                waits.len() - 1
            });
            let up_at = self.up.as_ref().map(|d| {
                waits.push(d.done.0);
                waits.len() - 1
            });
            // SAFETY: all handles are live events owned by this thread or by `Shared`.
            let r = unsafe { WaitForMultipleObjects(&waits, false, self.timeout()) };
            if r != WAIT_TIMEOUT {
                let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
                if i == 0 {
                    if !self.obey() {
                        return;
                    }
                } else if Some(i) == me_at {
                    self.folder_changed();
                } else if Some(i) == up_at {
                    if self.parent_changed() {
                        return;
                    }
                } else {
                    log::line(&format!("watch: waiting failed for \"{}\"; it updates on F5 only", self.path));
                    self.sh.failed.store(true, Ordering::SeqCst);
                    return;
                }
            }
            self.retry_if_due();
            self.report_if_due();
        }
    }

    /// Opens what isn't open yet: the folder and its parent. Gives the Windows error when the
    /// folder can't be watched.
    fn open(&mut self) -> Option<u32> {
        let mut error = None;
        if self.me.is_none() {
            match DirWatch::open(&self.path, FOLDER_CHANGES, FOLDER_BUFFER) {
                Ok(d) => {
                    self.me = Some(d);
                    self.retry = None;
                    self.sh.failed.store(false, Ordering::SeqCst);
                }
                Err(code) => {
                    self.sh.failed.store(true, Ordering::SeqCst);
                    log_once(&self.path, "can't be watched", code);
                    error = Some(code);
                }
            }
        }
        if self.up.is_none() {
            if let Some(p) = &self.parent {
                self.up = DirWatch::open(p, FILE_NOTIFY_CHANGE_DIR_NAME, PARENT_BUFFER).ok();
            }
        }
        error
    }

    /// Closes both handles (DirWatch's Drop cancels and waits), and tries nothing again.
    fn close(&mut self) {
        self.me = None;
        self.up = None;
        self.retry = None;
    }

    /// Asks Windows to tell the window before the drive is ejected, so the handles can be closed
    /// in time. Needs the window; done again once it exists (`Orders::window`).
    fn register(&mut self) {
        let h = self.hwnd.load(Ordering::SeqCst);
        if self.notify.is_some() || !self.local || h == 0 {
            return;
        }
        let Some(handle) = self.me.as_ref().or(self.up.as_ref()).map(|d| d.handle) else { return };
        let filter = DEV_BROADCAST_HANDLE {
            dbch_size: std::mem::size_of::<DEV_BROADCAST_HANDLE>() as u32,
            dbch_devicetype: DBT_DEVTYP_HANDLE.0,
            dbch_handle: handle,
            ..Default::default()
        };
        // SAFETY: `filter` is a valid DEV_BROADCAST_HANDLE that outlives the call; a stale window
        // handle just makes the call fail.
        let r = unsafe { RegisterDeviceNotificationW(HANDLE(h as *mut _), &filter as *const _ as *const c_void, DEVICE_NOTIFY_WINDOW_HANDLE) };
        if verbose() {
            let said = r.as_ref().map(|n| format!("yes ({:#x})", n.0 as usize)).unwrap_or_else(|e| format!("no ({e})"));
            log::line(&format!("watch: \"{}\" asked to hear before its drive is ejected: {said}", self.path));
        }
        if let Ok(n) = r {
            self.sh.notify.store(n.0 as isize, Ordering::SeqCst);
            self.notify = Some(n);
        }
        // If it fails (a drive that doesn't support it), ejecting may ask to close Glint first.
    }

    fn unregister(&mut self) {
        if let Some(n) = self.notify.take() {
            self.sh.notify.store(0, Ordering::SeqCst);
            // SAFETY: a notification we registered and haven't unregistered yet.
            unsafe {
                let _ = UnregisterDeviceNotification(n);
            }
        }
    }

    /// Does what the UI thread asked. False: stop.
    fn obey(&mut self) -> bool {
        let o = std::mem::take(&mut *lock(&self.sh.orders));
        if o.stop {
            return false;
        }
        if o.release {
            // A drive is being ejected: let go of it now. The notification stays registered, for
            // "ejecting failed" and "removed".
            self.close();
            self.sh.failed.store(true, Ordering::SeqCst);
            self.sh.say_released();
        }
        if o.reopen {
            // Ejecting was called off: watch again, and read again (something may have changed).
            self.unregister();
            self.open();
            self.register();
            self.note_change();
        }
        if o.gone {
            self.unregister();
            self.close();
            self.sh.failed.store(true, Ordering::SeqCst);
            self.note_change();
        }
        if o.let_go && self.me.is_some() {
            self.let_go_of_folder();
        }
        if o.window {
            self.register();
        }
        true
    }

    /// The folder's request finished.
    fn folder_changed(&mut self) {
        let Some(d) = self.me.as_mut() else { return };
        match d.take() {
            Outcome::NotYet => return,
            Outcome::Records(list) => self.collect_renames(&list),
            Outcome::Overflow => {}
            Outcome::Failed(code) => return self.lost_folder(code),
        }
        self.note_change();
        if let Some(Err(code)) = self.me.as_mut().map(DirWatch::ask) {
            self.lost_folder(code);
        }
    }

    /// The folder can't be watched any more: it was deleted, its drive pulled out, the network
    /// went away. Let go of it (a folder being deleted is only really gone once nobody has it
    /// open) and have it read again: the list then says what happened. It may still be there (the
    /// network dropped for a moment, the computer slept), so it's tried again in a while
    /// (`retry_if_due`); once it's really gone, the parent's watch tells when it's back.
    fn lost_folder(&mut self, code: u32) {
        self.let_go_of_folder();
        log_once(&self.path, "stopped being watched", code);
        self.note_change();
        self.retry = Some((Instant::now() + RETRY_FIRST, RETRY_FIRST));
    }

    /// Tries again to watch the folder after `lost_folder`, once it's time. A folder that isn't
    /// there any more isn't tried again; anything else waits twice as long for the next try.
    fn retry_if_due(&mut self) {
        let Some((at, wait)) = self.retry else { return };
        if Instant::now() < at {
            return;
        }
        self.retry = None;
        match self.open() {
            None => {
                self.register();
                self.note_change(); // read again: what changed meanwhile wasn't seen
            }
            Some(2 | 3) => {} // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
            Some(_) => {
                let wait = (wait * 2).min(RETRY_MAX);
                self.retry = Some((Instant::now() + wait, wait));
            }
        }
    }

    /// Closes the folder's handle but keeps watching its parent. The eject notification may have
    /// been registered on the folder's handle: it's registered again on the parent's (or let go
    /// when nothing of the drive is open any more), so ejecting the drive still asks Glint to close
    /// the parent's handle instead of finding it open and failing with "in use".
    fn let_go_of_folder(&mut self) {
        self.me = None;
        self.retry = None;
        self.sh.failed.store(true, Ordering::SeqCst);
        if self.notify.is_some() {
            self.unregister();
            self.register(); // does nothing when the parent isn't open either
        }
    }

    /// Items renamed in the folder, so the app can keep them selected.
    fn collect_renames(&mut self, list: &[(u32, String)]) {
        for (action, name) in list {
            if *action == FILE_ACTION_RENAMED_OLD_NAME.0 {
                self.old_name = Some(name.to_lowercase());
            } else if *action == FILE_ACTION_RENAMED_NEW_NAME.0 {
                if let Some(old) = self.old_name.take() {
                    if self.renamed.len() < MAX_RENAMES {
                        self.renamed.push((old, name.to_lowercase()));
                    }
                }
            }
        }
    }

    /// The parent's request finished: was the folder itself renamed, deleted or created? True
    /// when the folder was renamed (the app watches the new path; this thread ends).
    fn parent_changed(&mut self) -> bool {
        let Some(d) = self.up.as_mut() else { return false };
        let list = match d.take() {
            Outcome::NotYet => return false,
            Outcome::Records(list) => list,
            Outcome::Overflow => {
                self.note_change(); // can't tell what happened to the folder: read it again
                Vec::new()
            }
            Outcome::Failed(_) => {
                self.up = None; // the parent went too: stop watching it
                return false;
            }
        };
        let mut news = News::Nothing;
        for (action, name) in &list {
            let mine = name.to_lowercase() == self.name;
            match *action {
                a if a == FILE_ACTION_RENAMED_OLD_NAME.0 => self.old_was_mine = mine,
                a if a == FILE_ACTION_RENAMED_NEW_NAME.0 => {
                    if std::mem::take(&mut self.old_was_mine) {
                        news = News::Renamed(name.clone());
                    } else if mine {
                        news = News::Back; // something else was renamed to its name
                    }
                }
                a if a == FILE_ACTION_REMOVED.0 && mine => news = News::Gone,
                a if a == FILE_ACTION_ADDED.0 && mine => news = News::Back,
                _ => {}
            }
        }
        if let Some(Err(_)) = self.up.as_mut().map(DirWatch::ask) {
            self.up = None;
        }
        match news {
            News::Nothing => {}
            News::Renamed(new_name) => {
                let to = folder::join(self.parent.as_deref().unwrap_or_default(), &new_name);
                self.send(Event::Moved(to));
                return true;
            }
            News::Gone => {
                self.let_go_of_folder(); // so the folder can really go
                self.note_change();
            }
            News::Back => {
                if self.me.is_none() {
                    self.open();
                    self.register();
                }
                self.note_change();
            }
        }
        false
    }

    fn note_change(&mut self) {
        let now = Instant::now();
        self.pending = Some(match self.pending {
            Some((first, _)) => (first, now),
            None => (now, now),
        });
    }

    fn gap(&self) -> Duration {
        Duration::from_millis(self.sh.gap_ms.load(Ordering::SeqCst) as u64)
    }

    /// How long to wait for the next thing to happen: until changes are due to be reported or the
    /// folder is to be tried again, or for ever when neither is coming.
    fn timeout(&self) -> u32 {
        let report = self.pending.map(|(first, last)| due(first, last, self.last_report, self.gap()));
        let Some(next) = report.into_iter().chain(self.retry.map(|(at, _)| at)).min() else { return INFINITE };
        let left = next.saturating_duration_since(Instant::now());
        // Rounded up: waking a moment early would only wait again.
        left.as_micros().div_ceil(1000).min(60_000) as u32
    }

    fn report_if_due(&mut self) {
        let Some((first, last)) = self.pending else { return };
        let now = Instant::now();
        if now < due(first, last, self.last_report, self.gap()) {
            return;
        }
        self.pending = None;
        self.last_report = Some(now);
        if self.me.as_ref().is_some_and(DirWatch::delete_pending) {
            // The folder was deleted (its contents first, which is what was seen). Windows usually
            // ends the watch itself then; where it doesn't (Wine, some drives), let go here, so the
            // folder can really go before it's read again.
            self.let_go_of_folder();
        }
        let renamed = std::mem::take(&mut self.renamed);
        if verbose() {
            let waited = now.saturating_duration_since(first).as_secs_f64() * 1000.0;
            log::line(&format!("watch: \"{}\" changed ({waited:.0} ms after the first change)", self.path));
        }
        self.send(Event::Changed { renamed });
    }

    fn send(&self, event: Event) {
        // Send first, then wake the window (if it doesn't exist yet, it picks the news up when it does).
        if self.tx.send((self.id, event)).is_ok() {
            crate::app::wake(&self.hwnd);
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.unregister();
        self.close();
    }
}

/// Whether a folder is on this computer (a drive that could be ejected), not on the network.
fn is_local(path: &str) -> bool {
    if path.starts_with("\\\\") {
        return false;
    }
    let Some(letter) = path.get(..2) else { return false };
    let root = folder::wide(&format!("{letter}\\"));
    // SAFETY: `root` is NUL-terminated and outlives the call.
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) != 4 /* DRIVE_REMOTE */ }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One FILE_NOTIFY_INFORMATION record as Windows lays it out (4-byte aligned).
    fn record(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let len = units.len() * 2;
        let size = (12 + len).div_ceil(4) * 4;
        let mut out = Vec::new();
        out.extend_from_slice(&(if last { 0 } else { size as u32 }).to_le_bytes());
        out.extend_from_slice(&action.to_le_bytes());
        out.extend_from_slice(&(len as u32).to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.resize(size, 0);
        out
    }

    #[test]
    fn records_are_read() {
        let mut buf = record(4, "old name.txt", false);
        buf.extend(record(5, "Ünïcode name.txt", false));
        buf.extend(record(1, "x", true));
        let list = parse_records(&buf);
        assert_eq!(list, vec![(4, "old name.txt".into()), (5, "Ünïcode name.txt".into()), (1, "x".into())]);
    }

    #[test]
    fn malformed_records_end_the_list() {
        assert!(parse_records(&[]).is_empty());
        assert!(parse_records(&[1, 2, 3]).is_empty());
        // A name longer than the buffer.
        let mut bad = record(1, "abc", true);
        bad[8] = 200;
        assert!(parse_records(&bad).is_empty());
        // A "next" that points past the end: the first record still counts.
        let mut buf = record(1, "a", false);
        buf[0] = 250;
        assert_eq!(parse_records(&buf), vec![(1, "a".into())]);
        // A "next" that would wrap around.
        let mut buf = record(2, "b", false);
        buf[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_records(&buf), vec![(2, "b".into())]);
    }

    #[test]
    fn bursts_wait_for_quiet_but_not_for_ever() {
        let t0 = Instant::now();
        // One change: reported after QUIET.
        assert_eq!(due(t0, t0, None, Duration::ZERO), t0 + QUIET);
        // Changes that keep coming: reported MAX_WAIT after the first at the latest.
        let later = t0 + Duration::from_millis(390);
        assert_eq!(due(t0, later, None, Duration::ZERO), t0 + MAX_WAIT);
        // A slow folder: never sooner than the gap after the last report.
        let gap = Duration::from_secs(2);
        assert_eq!(due(t0, t0, Some(t0), gap), t0 + gap);
    }

    #[test]
    fn what_can_be_watched() {
        assert!(can_watch("C:\\"));
        assert!(can_watch("C:\\Users\\me"));
        assert!(can_watch("\\\\server\\share"));
        assert!(!can_watch(""));
        assert!(!can_watch("src"));
    }

    /// Waits (a few seconds at most) for news that `want` accepts.
    fn wait_for(w: &mut Watcher, want: impl Fn(&Event) -> bool) -> Option<Event> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some((_, e)) = w.take().into_iter().find(|(_, e)| want(e)) {
                return Some(e);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// The real thing, on a folder in %TEMP%: new files, renames, the folder itself renamed,
    /// deleted and coming back.
    #[test]
    fn sees_changes_renames_and_the_folder_itself() {
        let base = std::env::temp_dir().join(format!("glint-watch-test-{}", std::process::id()));
        let inner = base.join("inner");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&inner).unwrap();
        let path = inner.to_string_lossy().to_string();

        let mut w = Watcher::new(Arc::new(AtomicIsize::new(0)));
        w.watch(7, &path).wait();
        assert!(w.is_working(7, &path));

        std::fs::write(inner.join("a.txt"), "hello").unwrap();
        assert!(wait_for(&mut w, |e| matches!(e, Event::Changed { .. })).is_some(), "a new file is seen");

        std::fs::rename(inner.join("a.txt"), inner.join("B.txt")).unwrap();
        let e = wait_for(&mut w, |e| matches!(e, Event::Changed { renamed } if !renamed.is_empty()));
        assert_eq!(e, Some(Event::Changed { renamed: vec![("a.txt".into(), "b.txt".into())] }), "a rename is followed");

        // Many changes at once come as a few reports (at most one per MAX_WAIT), not one each.
        let writing = Instant::now();
        for i in 0..300 {
            std::fs::write(inner.join(format!("f{i}.txt")), "x").unwrap();
        }
        let most = writing.elapsed().as_millis() as usize / MAX_WAIT.as_millis() as usize + 2;
        std::thread::sleep(Duration::from_millis(600));
        let reports = w.take().len();
        assert!((1..=most).contains(&reports), "300 new files gave {reports} reports (expected 1 to {most})");

        let renamed = base.join("renamed");
        std::fs::rename(&inner, &renamed).unwrap();
        let moved = wait_for(&mut w, |e| matches!(e, Event::Moved(_)));
        assert_eq!(moved, Some(Event::Moved(renamed.to_string_lossy().to_string())), "the folder itself renamed");

        // Watching the folder under its new name; then it's deleted. Where deleting only marks it
        // "delete pending" while the watch has it open (Wine; older Windows), reading it fails and
        // the app lets go of it - as here. Either way it must really go: making a folder of the
        // same name works, and the watch sees it come back.
        let path = renamed.to_string_lossy().to_string();
        w.watch(7, &path).wait();
        std::fs::remove_dir_all(&renamed).unwrap();
        assert!(wait_for(&mut w, |e| matches!(e, Event::Changed { .. })).is_some(), "the deletion is seen");
        w.let_go(7);
        std::thread::sleep(Duration::from_millis(300));
        w.take();
        assert!(!w.is_working(7, &path), "a deleted folder isn't watched");
        std::fs::create_dir(&renamed).expect("the deleted folder is really gone (nothing holds it)");
        assert!(wait_for(&mut w, |e| matches!(e, Event::Changed { .. })).is_some(), "the folder back again");
        std::thread::sleep(Duration::from_millis(200));
        assert!(w.is_working(7, &path), "watched again once it's back");

        // An empty folder deleted while watched (no files inside to report): only the watch itself
        // can notice. Windows ends the watch ("delete pending"); Wine doesn't, so this is only logged.
        std::fs::remove_dir(&renamed).unwrap();
        let seen = wait_for(&mut w, |e| matches!(e, Event::Changed { .. })).is_some();
        log::line(&format!("watch test: an empty watched folder deleted was noticed by itself: {seen}"));

        w.stop();
        std::thread::sleep(Duration::from_millis(100));
        let _ = std::fs::remove_dir_all(&base);
    }
}
