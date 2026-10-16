//! Windows' own file icons and type names, looked up once and kept.
//!
//! Icons are cached per extension: every ".txt" shares one picture, so a folder of 10,000 text files
//! costs one lookup (from the extension alone - the disk isn't touched). Files that carry their own
//! icon (.exe, .lnk, .ico, ...) must be read for it, which can take seconds: a shortcut to a sleeping
//! network drive, a program on a slow share. So those are looked up on one background thread, most
//! recently asked first (the rows on screen); until its own icon is in, such a file shows the
//! generic one, and the thread repaints the window when icons arrive. The UI thread never waits.
//! The thread ends after a few idle seconds and is started again when needed.
//!
//! Each icon is turned once into a WIC bitmap (device independent, premultiplied BGRA). The renderer
//! makes its Direct2D copy the first time it draws it (render.rs, `Renderer::icon`).
//!
//! Type names ("Text Document") are cached per extension too, behind a lock, because the folder
//! loader thread asks for them.
//!
//! Zip and 7z files that open in Glint show the icon Settings > Appearance > Zip icon gave them
//! (registry.rs). Glint draws that one straight from glint.exe ([`set_archive_icon`]), so a change
//! shows at once instead of whenever Windows' own answer catches up.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHGetStockIconInfo, SHParseDisplayName, SHFILEINFOW, SHGFI_FLAGS, SHGFI_ICON, SHGFI_PIDL, SHGFI_SMALLICON,
    SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGSI_ICON, SHGSI_SMALLICON, SHSTOCKICONINFO, SIID_DRIVECD, SIID_DRIVEFIXED,
    SIID_DRIVENET, SIID_DRIVERAM, SIID_DRIVEREMOVE,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetSystemMetrics, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXICON, SM_CXSMICON};

use windows::core::w;
use windows::Win32::System::Registry::{RegCloseKey, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ, RRF_RT_ANY, RRF_RT_REG_SZ};

use crate::folder::{wide, Entry, EntryKind};
use crate::shell::{self, ComGuard};

/// Extensions whose files carry their own icon.
const OWN_ICON: &[&str] = &["exe", "lnk", "ico", "cur", "ani", "url", "scr", "msc", "appref-ms", "cpl"];

/// At most this many own-icon files wait for the background thread. Older requests (rows scrolled
/// past long ago) are dropped; they're asked again if they come back into view.
const MAX_WAITING: usize = 256;

/// While icons stream in, the window is repainted at most this often.
const REPAINT_EVERY: Duration = Duration::from_millis(30);

/// The icon thread ends after waiting this long with nothing to do (no thread sits around idle).
const IDLE_END: Duration = Duration::from_secs(3);

/// Starts the cache keys of big icons (the file list's "Large" size), so they never mix with the
/// small ones. (Small generic icons' keys start with '\u{1}'; paths start with neither.)
const LARGE_MARK: char = '\u{2}';

/// A cached icon. Cheap to copy; draw it with `Renderer::icon`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IconId(u32);

impl IconId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

struct Inner {
    wic: Option<IWICImagingFactory>,
    /// By IconId; None when Windows had no icon for it.
    bitmaps: Vec<Option<IWICBitmap>>,
    by_key: HashMap<String, IconId>,
    /// Own-icon files handed to the background thread (and not dropped from its queue since).
    asked: HashSet<String>,
    /// The way to the background thread, made the first time a file needs its own icon.
    loader: Option<Loader>,
    /// The zip and 7z icon changes taken in (see `ARCHIVE_CHANGES`).
    archive_changes: u64,
}

/// The icon cache. Lives on the UI thread (inside AppState).
pub struct Icons {
    inner: RefCell<Inner>,
    /// The window to repaint when icons arrive (0 until it exists).
    hwnd: Arc<AtomicIsize>,
}

impl Default for Icons {
    fn default() -> Self {
        Self::new()
    }
}

impl Icons {
    /// Needs COM on this thread (main.rs starts it before anything else).
    pub fn new() -> Icons {
        // SAFETY: plain COM object creation; the factory is free-threaded and owned by us.
        let wic = unsafe { CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok();
        Icons {
            inner: RefCell::new(Inner {
                wic,
                bitmaps: Vec::new(),
                by_key: HashMap::new(),
                asked: HashSet::new(),
                loader: None,
                archive_changes: 0,
            }),
            hwnd: Arc::new(AtomicIsize::new(0)),
        }
    }

    /// The window to repaint when icons arrive from the background thread.
    pub fn set_window(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
    }

    /// Call at the start of each frame (the window does): takes in the icons that arrived.
    pub fn begin_frame(&self) {
        {
            let mut inner = self.inner.borrow_mut();
            let changes = ARCHIVE_CHANGES.load(Ordering::SeqCst);
            if inner.archive_changes != changes {
                // Zip and 7z files show another icon: Windows' ones from before are asked again.
                inner.archive_changes = changes;
                inner.by_key.retain(|key, _| !matches!(key.get(1..), Some("zip" | "7z")));
            }
        }
        let arrived: Vec<(String, LoadedIcon)> = {
            let inner = self.inner.borrow();
            let Some(loader) = &inner.loader else { return };
            // A file Windows has no icon for keeps its generic one (and isn't asked again).
            loader.results.try_iter().filter_map(|(path, icon)| icon.map(|i| (path, i))).collect()
        };
        for (path, icon) in arrived {
            self.store(path, Some(icon.into_raw()));
        }
    }

    /// The icon for an entry of the folder `dir`.
    pub fn for_entry(&self, dir: &str, e: &Entry) -> IconId {
        self.for_entry_sized(dir, e, false)
    }

    /// The same, as Windows' big icon (32 pixels at 100 % scaling) when `large` - for the file
    /// list's "Large" size. Big icons are cached apart from the small ones.
    pub fn for_entry_sized(&self, dir: &str, e: &Entry, large: bool) -> IconId {
        match e.kind {
            // A drive's own icon comes from the sidebar's background lookup (asking a sleeping DVD
            // drive here could freeze the window); until it's in, Windows' standard drive icon.
            // (The sidebar's are small: a big list shows Windows' big standard ones.)
            EntryKind::Drive if large => self.stock_drive_sized(&e.type_name, true).unwrap_or_else(|| self.generic_sized("\\folder", "folder", FILE_ATTRIBUTE_DIRECTORY, true)),
            EntryKind::Drive => self.drive(e.target.as_deref().unwrap_or(""), &e.type_name),
            EntryKind::Folder => self.generic_sized("\\folder", "folder", FILE_ATTRIBUTE_DIRECTORY, large),
            EntryKind::File if OWN_ICON.contains(&e.ext.as_str()) => {
                // (Kept per version of the file: a changed shortcut or program shows its new icon.)
                let key = own_key(&e.path_in(dir), e.modified, e.size, large);
                if let Some(&id) = self.inner.borrow().by_key.get(&key) {
                    return id;
                }
                // Its own icon comes from the background thread; the generic one until then.
                self.ask(key);
                self.generic_sized(&e.ext, &format!("x.{}", e.ext), FILE_ATTRIBUTE_NORMAL, large)
            }
            EntryKind::File => {
                if let Some(id) = archive_icon(&e.ext).and_then(|id| self.own_sized(id, large)) {
                    return id;
                }
                let probe = if e.ext.is_empty() { "x".to_string() } else { format!("x.{}", e.ext) };
                self.generic_sized(&e.ext, &probe, FILE_ATTRIBUTE_NORMAL, large)
            }
        }
    }

    /// One of glint.exe's own icons (its number in res/glint.rc) at the list's small size, or the
    /// big one when `large`. None if it can't be had.
    fn own_sized(&self, id: u32, large: bool) -> Option<IconId> {
        let key = format!("{}glint:{id}", if large { LARGE_MARK } else { '\u{1}' });
        let known = self.inner.borrow().by_key.get(&key).copied();
        let id = known.unwrap_or_else(|| {
            // (The sizes Windows' own small and big icons have.)
            // SAFETY: plain metric lookups.
            let px = unsafe { GetSystemMetrics(if large { SM_CXICON } else { SM_CXSMICON }) };
            self.store(key, own_icon(id, px))
        });
        self.inner.borrow().bitmaps.get(id.index()).is_some_and(Option::is_some).then_some(id)
    }

    /// A drive's icon without ever asking the drive: its own icon if a background lookup already
    /// brought it in (see sidebar.rs), otherwise Windows' standard icon for this kind of drive
    /// ("Local Disk", "Network Drive", "CD Drive", "Removable Disk").
    pub fn drive(&self, root: &str, kind: &str) -> IconId {
        self.cached(root).or_else(|| self.stock_drive(kind)).unwrap_or_else(|| self.folder())
    }

    /// The icon cached under `path`, if any (never looks anything up).
    pub fn cached(&self, path: &str) -> Option<IconId> {
        let inner = self.inner.borrow();
        inner.by_key.get(path).copied().filter(|id| inner.bitmaps.get(id.index()).is_some_and(Option::is_some))
    }

    /// Windows' standard icon for a kind of drive (no disk access). None if Windows has none.
    pub fn stock_drive(&self, kind: &str) -> Option<IconId> {
        self.stock_drive_sized(kind, false)
    }

    /// The same, big when `large`.
    fn stock_drive_sized(&self, kind: &str, large: bool) -> Option<IconId> {
        let key = format!("{}drive:{kind}", if large { LARGE_MARK } else { '\u{1}' });
        let known = self.inner.borrow().by_key.get(&key).copied();
        let id = known.unwrap_or_else(|| {
            let siid = match kind {
                "Network Drive" => SIID_DRIVENET,
                "CD Drive" => SIID_DRIVECD,
                "Removable Disk" => SIID_DRIVEREMOVE,
                "RAM Disk" => SIID_DRIVERAM,
                _ => SIID_DRIVEFIXED,
            };
            let mut info = SHSTOCKICONINFO { cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
            // (Without SHGSI_SMALLICON Windows gives its big icon.)
            let flags = if large { SHGSI_ICON } else { SHGSI_ICON | SHGSI_SMALLICON };
            // SAFETY: `info` is a valid SHSTOCKICONINFO with its size set; we own the returned icon.
            let icon = unsafe { SHGetStockIconInfo(siid, flags, &mut info) }.ok().filter(|_| !info.hIcon.is_invalid()).map(|_| info.hIcon);
            self.store(key, icon)
        });
        self.inner.borrow().bitmaps.get(id.index()).is_some_and(Option::is_some).then_some(id)
    }

    /// Hands a file to the background thread (once), newest first.
    fn ask(&self, path: String) {
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;
        if inner.asked.contains(&path) {
            return;
        }
        let loader = inner.loader.get_or_insert_with(|| Loader::new(self.hwnd.clone()));
        inner.asked.insert(path.clone());
        // Files that fell off the end of the queue (or all of them, if no thread could be started)
        // are asked again when they're drawn again.
        for dropped in loader.push(path) {
            inner.asked.remove(&dropped);
        }
    }

    /// The real icon of something on disk, cached per path. It asks Windows right here, so only use
    /// it for things that never wait for a disk (the This PC icon); files go through `for_entry`.
    pub fn for_path(&self, path: &str) -> IconId {
        if let Some(&id) = self.inner.borrow().by_key.get(path) {
            return id;
        }
        // ("::{…}" places go by their id list: Windows 11 gives no icon for the name itself.)
        let icon = load_path_icon(path).map(LoadedIcon::into_raw);
        self.store(path.to_string(), icon)
    }

    /// Puts an icon fetched on another thread with [`load_path_icon`] into the cache under `path`,
    /// replacing what was there, so [`Icons::for_path`] finds it from now on too.
    pub fn adopt(&self, path: &str, icon: Option<LoadedIcon>) -> IconId {
        self.store(path.to_string(), icon.map(LoadedIcon::into_raw))
    }

    /// The generic folder icon.
    pub fn folder(&self) -> IconId {
        self.generic_sized("\\folder", "folder", FILE_ATTRIBUTE_DIRECTORY, false)
    }

    /// An icon shared by everything with this key, looked up from a made-up name (no disk access);
    /// Windows' big one when `large`.
    fn generic_sized(&self, key: &str, probe: &str, attrs: FILE_FLAGS_AND_ATTRIBUTES, large: bool) -> IconId {
        let key = format!("{}{key}", if large { LARGE_MARK } else { '\u{1}' });
        if let Some(&id) = self.inner.borrow().by_key.get(&key) {
            return id;
        }
        let icon = shell_icon(probe, attrs, true, large);
        self.store(key, icon)
    }

    fn store(&self, key: String, icon: Option<HICON>) -> IconId {
        let mut inner = self.inner.borrow_mut();
        let bitmap = icon.and_then(|h| {
            let bmp = inner.wic.as_ref().and_then(|wic| to_wic(wic, h));
            // SAFETY: we own the icon handle from SHGetFileInfoW and are done with it.
            unsafe {
                let _ = DestroyIcon(h);
            }
            bmp
        });
        let id = IconId(inner.bitmaps.len() as u32);
        inner.bitmaps.push(bitmap);
        inner.by_key.insert(key, id);
        id
    }

    /// The device-independent picture of an icon (for the renderer).
    pub fn bitmap(&self, id: IconId) -> Option<IWICBitmap> {
        self.inner.borrow().bitmaps.get(id.index()).cloned().flatten()
    }
}

// ---------------------------------------------------------------- zip and 7z files that open in Glint

/// The icon in glint.exe that zip and 7z files show (its number in res/glint.rc), while Glint
/// decides it (registry.rs `sync_archive_icon`); 0 when Windows' own lookup does.
static ZIP_ICON: AtomicU32 = AtomicU32::new(0);
static SEVEN_Z_ICON: AtomicU32 = AtomicU32::new(0);
/// Counts the changes to those: icons and pictures of zip and 7z files from before are forgotten.
static ARCHIVE_CHANGES: AtomicU64 = AtomicU64::new(0);
/// The main window, repainted when they change.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// What zip and 7z files show from now on: `types` (".zip", ".7z") open in Glint and show its icon
/// number `icon`; None, or a type that isn't there: whatever Windows says. From any thread.
pub fn set_archive_icon(types: &[&str], icon: Option<u32>) {
    let id = |ext| if types.contains(&ext) { icon.unwrap_or(0) } else { 0 };
    let zip = ZIP_ICON.swap(id(".zip"), Ordering::SeqCst) != id(".zip");
    let seven_z = SEVEN_Z_ICON.swap(id(".7z"), Ordering::SeqCst) != id(".7z");
    if zip || seven_z {
        ARCHIVE_CHANGES.fetch_add(1, Ordering::SeqCst);
        crate::thumbs::repaint(&WINDOW);
    }
}

/// The icon in glint.exe that files with this extension (lower case, no dot) show, when Glint
/// decides it (see [`set_archive_icon`]).
pub fn archive_icon(ext: &str) -> Option<u32> {
    let id = match ext {
        "zip" => ZIP_ICON.load(Ordering::SeqCst),
        "7z" => SEVEN_Z_ICON.load(Ordering::SeqCst),
        _ => 0,
    };
    (id != 0).then_some(id)
}

/// How many times that changed (thumbs.rs forgets its zip and 7z pictures when it does).
pub fn archive_changes() -> u64 {
    ARCHIVE_CHANGES.load(Ordering::SeqCst)
}

/// One of glint.exe's own icons (its number in res/glint.rc), `px` pixels square. The caller owns
/// it. Any thread.
pub fn own_icon(id: u32, px: i32) -> Option<HICON> {
    // SAFETY: loads an icon resource of this exe by number (MAKEINTRESOURCE); not shared, so ours.
    let loaded = unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        LoadImageW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(id as usize)), IMAGE_ICON, px, px, LR_DEFAULTCOLOR)
    };
    loaded.ok().map(|h| HICON(h.0)).filter(|i| !i.is_invalid())
}

/// What the UI thread and the icon thread share: the files waiting, newest at the front, and
/// whether a thread is there to take them.
struct Queue {
    waiting: Mutex<Waiting>,
    ready: Condvar,
}

struct Waiting {
    paths: VecDeque<String>,
    thread_running: bool,
}

/// The UI thread's end of the icon thread (see the top of this file).
struct Loader {
    queue: Arc<Queue>,
    tx: Sender<(String, Option<LoadedIcon>)>,
    results: Receiver<(String, Option<LoadedIcon>)>,
    /// The window to repaint when icons arrive.
    hwnd: Arc<AtomicIsize>,
}

impl Loader {
    fn new(hwnd: Arc<AtomicIsize>) -> Loader {
        let (tx, results) = channel();
        let queue = Arc::new(Queue { waiting: Mutex::new(Waiting { paths: VecDeque::new(), thread_running: false }), ready: Condvar::new() });
        Loader { queue, tx, results, hwnd }
    }

    /// Queues a file in front, starting the thread if none is running. Returns the files that
    /// were dropped from the queue.
    fn push(&self, path: String) -> Vec<String> {
        let mut dropped = Vec::new();
        let start = {
            let mut w = self.queue.waiting.lock().unwrap_or_else(|e| e.into_inner());
            w.paths.push_front(path);
            while w.paths.len() > MAX_WAITING {
                dropped.extend(w.paths.pop_back());
            }
            !std::mem::replace(&mut w.thread_running, true)
        };
        if start && !self.start_thread() {
            // No thread to be had right now: forget the queue; it is asked for again later.
            let mut w = self.queue.waiting.lock().unwrap_or_else(|e| e.into_inner());
            w.thread_running = false;
            dropped.extend(w.paths.drain(..));
        } else {
            self.queue.ready.notify_one();
        }
        dropped
    }

    /// The icon thread: takes the newest file, asks Windows for its icon, sends it back and
    /// repaints the window; ends after IDLE_END with nothing to do.
    fn start_thread(&self) -> bool {
        let (queue, tx, hwnd) = (self.queue.clone(), self.tx.clone(), self.hwnd.clone());
        shell::background("file icons", move || {
            let _com = ComGuard::init();
            let mut last_repaint: Option<Instant> = None;
            loop {
                let (path, more_waiting) = {
                    let mut w = queue.waiting.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if let Some(p) = w.paths.pop_front() {
                            break (p, !w.paths.is_empty());
                        }
                        let (guard, wait) = queue.ready.wait_timeout(w, IDLE_END).unwrap_or_else(|e| e.into_inner());
                        w = guard;
                        if wait.timed_out() && w.paths.is_empty() {
                            // Decided under the lock, so a file queued right now starts a new thread.
                            w.thread_running = false;
                            return;
                        }
                    }
                };
                let icon = load_queued_icon(&path);
                if tx.send((path, icon)).is_err() {
                    return; // the window is gone
                }
                // Repaint when the last one is in, and now and then while many are coming.
                if !more_waiting || last_repaint.is_none_or(|t| t.elapsed() >= REPAINT_EVERY) {
                    last_repaint = Some(Instant::now());
                    crate::thumbs::repaint(&hwnd);
                }
            }
        })
    }
}

/// A Windows icon looked up on a background thread, on its way to the cache ([`Icons::adopt`]).
/// If it never gets there, dropping it frees the icon.
pub struct LoadedIcon(HICON);

// SAFETY: an icon handle belongs to the process, not to a thread; any thread may use or destroy it.
unsafe impl Send for LoadedIcon {}

impl LoadedIcon {
    /// Hands the handle over (the receiver now owns it and must destroy it).
    fn into_raw(self) -> HICON {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

impl Drop for LoadedIcon {
    fn drop(&mut self) {
        // SAFETY: we own this icon handle (from SHGetFileInfoW) and nobody else has it.
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

/// The real icon of a file, folder or drive on disk, for any thread that has COM started. Asking a
/// sleeping DVD drive or a lost network drive can take seconds, so call this off the UI thread and
/// give the result to [`Icons::adopt`]. A Windows place named "::{…}" (the Recycle Bin) gets its
/// own icon too - for the bin, full or empty as it is right now.
pub fn load_path_icon(path: &str) -> Option<LoadedIcon> {
    if path.starts_with("::") {
        return place_icon(path).map(LoadedIcon);
    }
    shell_icon(path, FILE_ATTRIBUTE_NORMAL, false, false).map(LoadedIcon)
}

/// The icon of a Windows place named "::{…}", looked up by its id list.
fn place_icon(name: &str) -> Option<HICON> {
    let w = wide(name);
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    // SAFETY: `w` is NUL-terminated; the id list Windows makes is freed below, after its last use.
    unsafe {
        SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).ok()?;
        let mut info = SHFILEINFOW::default();
        let flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_PIDL;
        let ok = SHGetFileInfoW(PCWSTR(pidl as *const u16), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, flags);
        CoTaskMemFree(Some(pidl as *const _));
        (ok != 0 && !info.hIcon.is_invalid()).then_some(info.hIcon)
    }
}

/// Ends an own-icon file's cache key: the file's modified time and size follow it.
const STAMP_MARK: char = '\u{0}';

/// The cache key of a file's own icon: its path and the version of it the listing saw (modified
/// time and size), so a file that changed is looked up again; LARGE_MARK first for the big one.
fn own_key(path: &str, modified: u64, size: u64, large: bool) -> String {
    let mark = if large { LARGE_MARK.to_string() } else { String::new() };
    format!("{mark}{path}{STAMP_MARK}{modified:x}.{size:x}")
}

/// The path in a key the icon thread is given, and whether it wants the big icon.
fn queued_path(key: &str) -> (&str, bool) {
    let (key, large) = match key.strip_prefix(LARGE_MARK) {
        Some(rest) => (rest, true),
        None => (key, false),
    };
    (key.split(STAMP_MARK).next().unwrap_or(key), large)
}

/// What the icon thread is asked for: an `own_key`.
fn load_queued_icon(key: &str) -> Option<LoadedIcon> {
    match queued_path(key) {
        (path, true) => shell_icon(path, FILE_ATTRIBUTE_NORMAL, false, true).map(LoadedIcon),
        (path, false) => load_path_icon(path),
    }
}

/// Windows' icon for a path (or a made-up name, by its extension). Big (32 pixels at 100 %
/// scaling) when `large`, small (16) otherwise.
fn shell_icon(path: &str, attrs: FILE_FLAGS_AND_ATTRIBUTES, made_up: bool, large: bool) -> Option<HICON> {
    let w = wide(path);
    let mut info = SHFILEINFOW::default();
    // (Without SHGFI_SMALLICON Windows gives its big icon.)
    let mut flags = if large { SHGFI_ICON } else { SHGFI_ICON | SHGFI_SMALLICON };
    if made_up {
        flags |= SHGFI_USEFILEATTRIBUTES;
    }
    // SAFETY: `w` is NUL-terminated; `info` is a valid SHFILEINFOW and its size is passed.
    let ok = unsafe { SHGetFileInfoW(PCWSTR(w.as_ptr()), attrs, Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, flags) };
    (ok != 0 && !info.hIcon.is_invalid()).then_some(info.hIcon)
}

/// HICON -> premultiplied-BGRA WIC bitmap that owns its pixels.
fn to_wic(wic: &IWICImagingFactory, icon: HICON) -> Option<IWICBitmap> {
    // SAFETY: `icon` is a valid icon handle; every WIC object is owned and released by its wrapper.
    unsafe {
        let src = wic.CreateBitmapFromHICON(icon).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        wic.CreateBitmapFromSource(&conv, WICBitmapCacheOnLoad).ok()
    }
}

/// Whether Windows "knows" files with this extension (lower case, no dot), so the list may leave it
/// out of their names when "HideKnownExtensions" is on - Explorer's rule: the
/// extension points to a registered file type that doesn't ask for its extension to be shown
/// always ("AlwaysShowExt"). Looked up once per extension in the user's own registry (the classes
/// Windows keeps locally; nothing on a disk or network is touched).
pub fn known_type(ext: &str) -> bool {
    thread_local! {
        static KNOWN: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
    }
    if ext.is_empty() {
        return false;
    }
    if let Some(known) = KNOWN.with(|k| k.borrow().get(ext).copied()) {
        return known;
    }
    let known = registered_type(ext);
    KNOWN.with(|k| k.borrow_mut().insert(ext.to_string(), known));
    known
}

fn registered_type(ext: &str) -> bool {
    let dotted = wide(&format!(".{ext}"));
    let mut buf = [0u16; 260];
    let mut bytes = std::mem::size_of_val(&buf) as u32;
    // SAFETY: the key name is NUL-terminated; `buf` and `bytes` are valid and `bytes` is the buffer's
    // true size. Reads the extension's default value: the name of its file type (its "ProgID").
    let read = unsafe { RegGetValueW(HKEY_CLASSES_ROOT, PCWSTR(dotted.as_ptr()), PCWSTR::null(), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut bytes)) };
    if read.is_err() {
        return false;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    if len == 0 {
        return false;
    }
    let prog_id = &buf[..=len.min(buf.len() - 1)];
    let mut key = HKEY::default();
    // SAFETY: `prog_id` is NUL-terminated (see above); the key is closed below.
    if unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, PCWSTR(prog_id.as_ptr()), None, KEY_READ, &mut key) }.is_err() {
        return false;
    }
    // SAFETY: a value query on the key we opened, without reading its data.
    let always_shown = unsafe { RegGetValueW(key, PCWSTR::null(), w!("AlwaysShowExt"), RRF_RT_ANY, None, None, None) }.is_ok();
    // SAFETY: closes the key opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    !always_shown
}

/// What Windows calls files with this extension ("Text Document"), or "File folder". Thread-safe;
/// the calling thread needs COM started (the folder loader does that).
pub fn type_name(ext: &str, is_dir: bool) -> Arc<str> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<str>>>> = OnceLock::new();
    let key = if is_dir { "\\dir".to_string() } else { ext.to_string() };
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(name) = cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return name;
    }
    let probe = if is_dir { "folder".to_string() } else if ext.is_empty() { "x".to_string() } else { format!("x.{ext}") };
    let w = wide(&probe);
    let mut info = SHFILEINFOW::default();
    let attrs = if is_dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL };
    // SAFETY: `w` is NUL-terminated; `info` is a valid SHFILEINFOW and its size is passed.
    let ok = unsafe {
        SHGetFileInfoW(PCWSTR(w.as_ptr()), attrs, Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_FLAGS(SHGFI_TYPENAME.0 | SHGFI_USEFILEATTRIBUTES.0))
    };
    let len = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(0);
    let mut name = if ok != 0 { String::from_utf16_lossy(&info.szTypeName[..len]) } else { String::new() };
    if name.trim().is_empty() {
        name = if is_dir {
            "File folder".into()
        } else if ext.is_empty() {
            "File".into()
        } else {
            format!("{} File", ext.to_uppercase())
        };
    }
    let name: Arc<str> = Arc::from(name);
    if let Ok(mut c) = cache.lock() {
        c.insert(key, name.clone());
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_icons_are_looked_up_again_when_the_file_changes() {
        for path in ["C:\\x\\app.lnk", "C:\\\u{1F600} é\\setup 2.exe", "\\\\server\\share\\a.ico"] {
            for large in [false, true] {
                let key = own_key(path, 0x1d9_0000_0000, 4096, large);
                assert_eq!(queued_path(&key), (path, large), "{key:?}");
                assert_eq!(key, own_key(path, 0x1d9_0000_0000, 4096, large), "the same file, the same key");
                assert_ne!(key, own_key(path, 0x1d9_0000_0001, 4096, large), "changed: looked up again");
                assert_ne!(key, own_key(path, 0x1d9_0000_0000, 4097, large), "other size: looked up again");
                assert_ne!(key, own_key(path, 0x1d9_0000_0000, 4096, !large));
            }
        }
        // Never the same as another path's key, nor a generic one's.
        assert_ne!(own_key("C:\\a1", 0, 0, false), own_key("C:\\a", 0x10, 0, false));
        assert!(!own_key("C:\\a.exe", 0, 0, false).starts_with('\u{1}'));

        // The cache: the icon stored for one version isn't drawn for the next, but is for it again
        // (undo puts the old file back).
        let icons = Icons::new();
        let mut e = crate::folder::Entry {
            name: "app.lnk".into(),
            lower: "app.lnk".into(),
            kind: EntryKind::File,
            size: 10,
            modified: 1,
            created: 0,
            attributes: 0,
            ext: "lnk".into(),
            type_name: Arc::from("Shortcut"),
            target: None,
            free: 0,
        };
        let dir = std::env::temp_dir().join("glint-icon-test-missing").to_string_lossy().into_owned();
        let old = icons.store(own_key(&e.path_in(&dir), 1, 10, false), None);
        assert_eq!(icons.for_entry(&dir, &e), old);
        e.modified = 2;
        assert_ne!(icons.for_entry(&dir, &e), old, "changed since");
        assert!(icons.inner.borrow().asked.contains(&own_key(&e.path_in(&dir), 2, 10, false)), "its new icon is asked for");
        e.modified = 1;
        assert_eq!(icons.for_entry(&dir, &e), old);
    }

    #[test]
    fn zip_and_7z_files_follow_the_icon_glint_gave_them() {
        let before = archive_changes();
        set_archive_icon(&[".7z"], Some(3));
        assert_eq!((archive_icon("7z"), archive_icon("zip"), archive_icon("txt")), (Some(3), None, None));
        assert_eq!(archive_changes(), before + 1);
        // The same again is no change (nothing to forget).
        set_archive_icon(&[".7z"], Some(3));
        assert_eq!(archive_changes(), before + 1);
        set_archive_icon(&[".zip", ".7z"], Some(4));
        assert_eq!((archive_icon("7z"), archive_icon("zip")), (Some(4), Some(4)));
        // Windows decides again: a DefaultIcon that isn't Glint's, or nothing opens in Glint.
        set_archive_icon(&[".zip"], None);
        assert_eq!((archive_icon("7z"), archive_icon("zip")), (None, None));
        set_archive_icon(&[], Some(2));
        assert_eq!(archive_changes(), before + 3);
    }
}
