//! Thumbnails: the picture File Explorer shows for a file (a photo, a frame of a video, the first
//! page of a PDF when Windows has a handler for it) - or, when there is none, its icon at a big
//! size. The details panel, the Space preview and the big icon views all get them from here.
//!
//! **How to use it** (on the UI thread; asking while painting is fine and cheap):
//!
//! ```ignore
//! let px = (box.w.max(box.h) * ctx.scale) as u32; // the longest side wanted, in screen pixels
//! match thumbs::request(&path, px) {
//!     Some(pic) => { r.picture(&pic, dest, None, 1.0, true); } // render.rs
//!     None => { /* not yet (or Windows has nothing): draw a placeholder */ }
//! }
//! ```
//!
//! - The answer fits inside a `px` square and keeps its shape. It can be smaller: a small picture,
//!   or an icon (`Picture::is_icon`; icons look best a little smaller than the box, and stop at 256).
//! - None means "not yet". When the picture is ready the window is repainted, and asking again
//!   (the next frame does) returns it. Asking every frame for what is on screen is the idea: it is
//!   also how the service knows what is still wanted (see "generations" below).
//! - [`request_versioned`] takes a number that changes when the file does (its date and size, see
//!   [`version_of`]): a file that changed gets a new thumbnail, and the old one shows until then.
//! - Draw pictures only in the main window: the renderer lets go of a picture's pixels once it has
//!   its own copy (see [`Picture::take_pixels`]).
//!
//! How it works:
//! - One background thread, with COM in a single-threaded apartment (thumbnail handlers want one),
//!   asks Windows' shell: `IShellItemImageFactory::GetImage` with SIIGBF_THUMBNAILONLY for a real
//!   thumbnail; for a picture Windows has no thumbnail for, WIC decodes the file itself; and if
//!   neither works, GetImage again for the icon (then Windows' big system icon). Handlers can be
//!   slow (a long video, a sleeping network drive), so none of this happens on the UI thread. The
//!   thread ends after a few idle seconds and is started again when needed.
//! - Newest request first, at most MAX_WAITING waiting.
//! - Generations: every frame the window paints is one ([`begin_frame`]). A request nobody asked for
//!   again during the last two frames (scrolled away, the selection moved on) is dropped before the
//!   thread spends any time on it, and a result for a file that changed meanwhile is thrown away.
//! - The shell's bitmap is copied out top-down with GetDIBits and a negative height, whichever way
//!   Windows stores it (otherwise some thumbnails come out upside down), as premultiplied BGRA.
//! - Finished pictures are kept in memory, the least recently used dropped first (MEMORY_BUDGET).
//!   Each becomes a Direct2D bitmap on the UI thread the first time it is drawn (render.rs), which
//!   then lets go of the pixels; after a lost graphics device it is simply fetched again.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, InvalidateRect, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HBITMAP, HGDIOBJ,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICBitmapTransformFlipHorizontal, WICBitmapTransformFlipVertical,
    WICBitmapTransformOptions, WICBitmapTransformRotate0, WICBitmapTransformRotate180, WICBitmapTransformRotate270,
    WICBitmapTransformRotate90, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::Storage::FileSystem::{GetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES, INVALID_FILE_ATTRIBUTES};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SHGetFileInfoW, SHGetImageList, SHFILEINFOW, SHGFI_SYSICONINDEX, SHIL_EXTRALARGE,
    SHIL_JUMBO, SIIGBF, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

use crate::folder::{extension, file_name, wide_path};
use crate::frame;
use crate::log;
use crate::shell::{self, ComGuard};

/// How much memory finished pictures may take (each is width x height x 4 bytes).
const MEMORY_BUDGET: usize = 32 * 1024 * 1024;
/// How many files the service remembers at most (pictures, and files still on their way).
const MAX_ENTRIES: usize = 2048;
/// At most this many requests wait for the thread; older ones are dropped (and asked again if
/// they're still wanted).
const MAX_WAITING: usize = 64;
/// The thread ends after waiting this long with nothing to do.
const IDLE_END: Duration = Duration::from_secs(3);
/// While pictures stream in, the window is repainted at most this often.
const REPAINT_EVERY: Duration = Duration::from_millis(30);
/// The biggest thumbnail made. Bigger requests get this size (the Space preview decodes pictures
/// at full size itself, see peek.rs).
pub const MAX_THUMBNAIL: u32 = 2048;
/// Icons stop at Windows' biggest icon size.
const MAX_ICON: u32 = 256;

/// Extensions WIC can decode on any Windows 10/11 (and under Wine), lower case without the dot.
pub const PICTURES: &[&str] = &["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "ico", "wdp", "jxr", "dds"];
/// Pictures WIC decodes only when Windows has the extra codec (HEIF, WebP, raw camera files): tried
/// all the same, they just fail without it.
pub const MAYBE_PICTURES: &[&str] = &["webp", "heic", "heif", "avif", "cr2", "nef", "arw", "dng"];

/// Whether WIC may be able to decode a file with this extension (lower case, no dot).
pub fn is_picture(ext: &str) -> bool {
    PICTURES.contains(&ext) || MAYBE_PICTURES.contains(&ext)
}

// ---------------------------------------------------------------- the picture

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A finished picture: premultiplied BGRA pixels, top-down rows of `width * 4` bytes. Shared
/// (`Arc`) between the service's memory and whoever draws it; draw it with `Renderer::picture`.
#[derive(Debug)]
pub struct Picture {
    id: u64,
    width: u32,
    height: u32,
    /// The pixels until the renderer has made its own copy (`take_pixels`).
    pixels: Mutex<Option<Vec<u8>>>,
    /// The renderer had no copy any more and the pixels were gone (the graphics device was lost):
    /// the picture has to be made again.
    lost: AtomicBool,
    icon: bool,
}

impl Picture {
    /// A picture from premultiplied BGRA rows, top-down. None when the size and the pixels don't
    /// match.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>, icon: bool) -> Option<Arc<Picture>> {
        if width == 0 || height == 0 || pixels.len() != width as usize * height as usize * 4 {
            return None;
        }
        Some(Arc::new(Picture { id: NEXT_ID.fetch_add(1, Ordering::Relaxed), width, height, pixels: Mutex::new(Some(pixels)), lost: AtomicBool::new(false), icon }))
    }

    /// A number no other picture has (the renderer keeps its copies by it).
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The file's icon rather than a real thumbnail.
    pub fn is_icon(&self) -> bool {
        self.icon
    }

    /// The memory its pixels take.
    pub fn bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Hands the pixels over (once): the renderer makes its Direct2D copy from them and lets them
    /// go, so a picture doesn't take its memory twice.
    pub fn take_pixels(&self) -> Option<Vec<u8>> {
        self.pixels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }

    /// The renderer couldn't make its copy after all: the pixels go back (`take_pixels`).
    pub fn give_back(&self, pixels: Vec<u8>) {
        *self.pixels.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pixels);
    }

    /// The renderer had no copy and the pixels were already gone (after a lost graphics device).
    pub fn mark_lost(&self) {
        self.lost.store(true, Ordering::SeqCst);
    }

    /// Whether it can't be drawn any more and must be made again.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------- the public face

thread_local! {
    /// The service lives on the UI thread; only its worker thread is elsewhere.
    static SERVICE: RefCell<Service> = RefCell::new(Service::new());
}

/// The thumbnail of the file or folder at `path` (or a shell place such as
/// "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", This PC), fitting a `size_px` square. None until it's
/// ready (the window is repainted then). See the top of this file.
pub fn request(path: &str, size_px: u32) -> Option<Arc<Picture>> {
    request_versioned(path, 0, size_px)
}

/// The same, for a file whose `version` changes when the file does (see [`version_of`]): a new
/// version makes a new thumbnail; the old one is returned until it's there.
pub fn request_versioned(path: &str, version: u64, size_px: u32) -> Option<Arc<Picture>> {
    SERVICE.with(|s| s.borrow_mut().request(path, version, size_px))
}

/// A version number for a file from its last change and size (folder.rs `Entry::modified`,
/// `Entry::size`), for [`request_versioned`].
pub fn version_of(modified: u64, size: u64) -> u64 {
    modified ^ size.rotate_left(32)
}

/// The window to repaint when pictures arrive. The main window calls this once it exists.
pub fn set_window(hwnd: HWND) {
    SERVICE.with(|s| s.borrow().shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst));
}

/// Call at the start of every frame of the main window (window.rs does): a new generation, and
/// the pictures that arrived are taken in.
pub fn begin_frame() {
    SERVICE.with(|s| {
        let mut s = s.borrow_mut();
        s.shared.frame.fetch_add(1, Ordering::SeqCst);
        s.take_done();
    });
}

/// Forgets every picture and every request (Glint going to the background gives memory back).
pub fn clear() {
    SERVICE.with(|s| s.borrow_mut().clear());
}

/// The size actually made for a request of `px`: one of a few steps, so a box that grows by a pixel
/// doesn't ask for a new thumbnail every time.
fn bucket(px: u32) -> u32 {
    const STEPS: [u32; 13] = [32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048];
    STEPS.iter().copied().find(|&s| s >= px).unwrap_or(MAX_THUMBNAIL)
}

// ---------------------------------------------------------------- the UI thread's side

/// What the service knows about one file.
struct Known {
    version: u64,
    picture: Option<Arc<Picture>>,
    /// The size (a bucket) the picture was made for; 0 when nothing was made yet. A picture made
    /// for it that is smaller (a small file, an icon) is still the best there is.
    made: u32,
    /// The size asked of the thread and not answered yet (0: nothing on its way), and for which
    /// version.
    asked: u32,
    asked_version: u64,
    /// When it was last asked for: the LRU clock, and the frame.
    used: u64,
    frame: u64,
}

struct Service {
    shared: Arc<Shared>,
    tx: Sender<Done>,
    rx: Receiver<Done>,
    known: HashMap<String, Known>,
    /// Memory the pictures in `known` take.
    bytes: usize,
    clock: u64,
}

/// What the UI thread and the worker share.
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    /// The frame (generation) the window is painting now.
    frame: AtomicU64,
    /// The window to repaint when a picture is ready (0 until it exists).
    hwnd: AtomicIsize,
}

struct Queue {
    /// Newest at the front.
    jobs: VecDeque<Job>,
    thread_running: bool,
}

struct Job {
    key: String,
    path: String,
    version: u64,
    px: u32,
    /// The last frame that asked for it.
    frame: u64,
}

/// A job finished (or dropped as nobody wanted it any more).
struct Done {
    key: String,
    version: u64,
    px: u32,
    picture: Option<Arc<Picture>>,
    dropped: bool,
}

impl Service {
    fn new() -> Service {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { jobs: VecDeque::new(), thread_running: false }),
            ready: Condvar::new(),
            frame: AtomicU64::new(1),
            hwnd: AtomicIsize::new(0),
        });
        Service { shared, tx, rx, known: HashMap::new(), bytes: 0, clock: 0 }
    }

    fn request(&mut self, path: &str, version: u64, size_px: u32) -> Option<Arc<Picture>> {
        if path.is_empty() {
            return None;
        }
        self.take_done();
        let px = bucket(size_px.max(1));
        let key = path.to_lowercase();
        let frame = self.shared.frame.load(Ordering::SeqCst);
        self.clock += 1;
        let clock = self.clock;
        let k = self.known.entry(key.clone()).or_insert(Known { version, picture: None, made: 0, asked: 0, asked_version: version, used: clock, frame });
        k.used = clock;
        k.frame = frame;
        // A picture whose Direct2D copy went with a lost graphics device is made again.
        if k.picture.as_ref().is_some_and(|p| p.is_lost()) {
            if let Some(p) = k.picture.take() {
                self.bytes = self.bytes.saturating_sub(p.bytes());
            }
            k.made = 0;
        }
        let shown = k.picture.clone();
        let enough = k.version == version && k.made >= px;
        let on_its_way = k.asked >= px && k.asked_version == version;
        if enough {
            return shown;
        }
        if on_its_way {
            // Still wanted: the thread mustn't drop it as stale.
            let mut q = self.shared.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(job) = q.jobs.iter_mut().find(|j| j.key == key) {
                job.frame = frame;
            }
            return shown;
        }
        k.asked = px;
        k.asked_version = version;
        self.queue(Job { key, path: path.to_string(), version, px, frame });
        shown
    }

    /// Puts a job at the front of the queue (replacing an older one for the same file) and starts
    /// the thread if none is running.
    fn queue(&mut self, job: Job) {
        let mut dropped = Vec::new();
        let start = {
            let mut q = self.shared.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            q.jobs.retain(|j| j.key != job.key);
            q.jobs.push_front(job);
            while q.jobs.len() > MAX_WAITING {
                dropped.extend(q.jobs.pop_back().map(|j| j.key));
            }
            !std::mem::replace(&mut q.thread_running, true)
        };
        if start && !self.start_thread() {
            // No thread to be had right now: forget the queue; what's still wanted is asked again.
            let mut q = self.shared.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            q.thread_running = false;
            dropped.extend(q.jobs.drain(..).map(|j| j.key));
        } else {
            self.shared.ready.notify_one();
        }
        for key in dropped {
            if let Some(k) = self.known.get_mut(&key) {
                k.asked = 0;
            }
        }
    }

    fn start_thread(&self) -> bool {
        let (shared, tx) = (self.shared.clone(), self.tx.clone());
        shell::background("thumbnails", move || work(&shared, &tx))
    }

    /// Takes in what the thread finished.
    fn take_done(&mut self) {
        let mut changed = false;
        while let Ok(done) = self.rx.try_recv() {
            let Some(k) = self.known.get_mut(&done.key) else { continue };
            if k.asked == done.px && k.asked_version == done.version {
                k.asked = 0;
            }
            if done.dropped || done.version != k.version && done.version != k.asked_version {
                continue; // nobody looked any more, or the file changed since: thrown away
            }
            if done.version != k.version {
                // The newer version a request asked for: it replaces the old one.
                k.version = done.version;
                k.made = 0;
            }
            if done.picture.is_none() && k.picture.is_some() && k.made > 0 {
                // A failed retry (at a bigger size, say) doesn't throw away a good picture.
                k.made = k.made.max(done.px);
                continue;
            }
            if let Some(old) = k.picture.take() {
                self.bytes = self.bytes.saturating_sub(old.bytes());
            }
            self.bytes += done.picture.as_ref().map_or(0, |p| p.bytes());
            k.picture = done.picture;
            k.made = done.px;
            changed = true;
        }
        if changed || self.known.len() > MAX_ENTRIES {
            self.trim();
        }
    }

    /// Drops the least recently used pictures while over the memory budget or the number of
    /// entries - never one asked for in this frame (it's on screen).
    fn trim(&mut self) {
        let frame = self.shared.frame.load(Ordering::SeqCst);
        while self.bytes > MEMORY_BUDGET || self.known.len() > MAX_ENTRIES {
            let over_memory = self.bytes > MEMORY_BUDGET;
            let oldest = self
                .known
                .iter()
                .filter(|(_, k)| k.frame < frame && k.asked == 0 && (!over_memory || k.picture.is_some()))
                .min_by_key(|(_, k)| k.used)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            if let Some(k) = self.known.remove(&key) {
                self.bytes = self.bytes.saturating_sub(k.picture.map_or(0, |p| p.bytes()));
            }
        }
    }

    fn clear(&mut self) {
        self.shared.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).jobs.clear();
        while self.rx.try_recv().is_ok() {}
        self.known.clear();
        self.bytes = 0;
    }
}

// ---------------------------------------------------------------- the thread

/// The worker: takes the newest job, makes its picture, sends it back and repaints the window.
fn work(shared: &Shared, tx: &Sender<Done>) {
    let _com = ComGuard::init();
    let wic = wic_factory();
    let verbose = std::env::var_os("GLINT_THUMB_LOG").is_some();
    let mut last_repaint: Option<Instant> = None;
    loop {
        let (job, more) = {
            let mut q = shared.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(j) = q.jobs.pop_front() {
                    break (j, !q.jobs.is_empty());
                }
                let (guard, wait) = shared.ready.wait_timeout(q, IDLE_END).unwrap_or_else(std::sync::PoisonError::into_inner);
                q = guard;
                if wait.timed_out() && q.jobs.is_empty() {
                    // Decided under the lock, so a job queued right now starts a new thread.
                    q.thread_running = false;
                    return;
                }
            }
        };
        // Nobody asked for it during the last two frames: it's off screen.
        if job.frame + 1 < shared.frame.load(Ordering::SeqCst) {
            if tx.send(Done { key: job.key, version: job.version, px: job.px, picture: None, dropped: true }).is_err() {
                return;
            }
            continue;
        }
        let started = Instant::now();
        let (picture, how) = make(&job.path, job.px, wic.as_ref());
        if verbose {
            log::line(&format!(
                "thumbnail {} at {} px: {how}, {} in {:.1} ms",
                job.path,
                job.px,
                picture.as_ref().map_or("nothing".to_string(), |p| format!("{} x {}", p.width(), p.height())),
                started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        if tx.send(Done { key: job.key, version: job.version, px: job.px, picture, dropped: false }).is_err() {
            return; // the window is gone
        }
        if !more || last_repaint.is_none_or(|t| t.elapsed() >= REPAINT_EVERY) {
            last_repaint = Some(Instant::now());
            repaint(&shared.hwnd);
        }
    }
}

/// Asks a window to paint again (safe from any thread; nothing happens if it's gone).
pub fn repaint(hwnd: &AtomicIsize) {
    let h = hwnd.load(Ordering::SeqCst);
    if h != 0 {
        // SAFETY: invalidating a window from another thread is allowed; a stale handle just fails.
        unsafe {
            let _ = InvalidateRect(Some(HWND(h as *mut _)), None, false);
        }
    }
}

/// A WIC factory for this thread (needs COM started).
pub fn wic_factory() -> Option<IWICImagingFactory> {
    // SAFETY: plain COM object creation; the factory is owned by the returned wrapper.
    unsafe { CoCreateInstance::<_, IWICImagingFactory>(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.ok()
}

/// Makes the picture for one job, and says how (for the log).
fn make(path: &str, px: u32, wic: Option<&IWICImagingFactory>) -> (Option<Arc<Picture>>, &'static str) {
    let ext = extension(file_name(path));
    let picture_file = is_picture(&ext);
    let wide = wide_path(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call; the item is an owned COM pointer.
    let item: Option<IShellItemImageFactory> = unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None) }.ok();
    // Wine's shell has no thumbnail handlers (at most it hands back the icon), so there pictures go
    // straight to WIC and everything else gets its icon.
    if !frame::is_wine() {
        if let Some(pic) = item.as_ref().and_then(|f| shell_image(f, px, SIIGBF_THUMBNAILONLY, wic)) {
            return (Picture::new(pic.width, pic.height, pic.pixels, false), "Windows' thumbnail");
        }
    }
    // (Not a cloud file that isn't on this PC: reading it would download it - only Windows' own
    // thumbnail above, which asks the cloud app, may show it.)
    if picture_file && !online_only(&wide) {
        if let Some(d) = wic.and_then(|w| decode(w, path, px, u64::from(px) * u64::from(px), 0)) {
            return (Picture::new(d.width, d.height, d.pixels, false), "decoded with WIC");
        }
    }
    let icon_px = px.min(MAX_ICON);
    if let Some(pic) = item.as_ref().and_then(|f| shell_image(f, icon_px, SIIGBF_ICONONLY, wic)) {
        return (Picture::new(pic.width, pic.height, pic.pixels, true), "Windows' icon");
    }
    if let Some(pic) = wic.and_then(|w| system_icon(w, path, icon_px)) {
        return (Picture::new(pic.width, pic.height, pic.pixels, true), "system icon");
    }
    (None, "nothing")
}

/// Whether a file is a cloud file that isn't on this PC (OneDrive's "online-only"): its attributes
/// say so without the file being opened (`wide`: its NUL-terminated path).
fn online_only(wide: &[u16]) -> bool {
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let attributes = unsafe { GetFileAttributesW(PCWSTR(wide.as_ptr())) };
    attributes != INVALID_FILE_ATTRIBUTES && attributes & crate::peek::NOT_HERE != 0
}

/// Pixels on their way to becoming a [`Picture`].
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    /// Premultiplied BGRA, top-down.
    pub pixels: Vec<u8>,
}

/// Asks the shell for the item's picture (`flags`: thumbnail only, or icon only), fitting `px`.
fn shell_image(item: &IShellItemImageFactory, px: u32, flags: SIIGBF, wic: Option<&IWICImagingFactory>) -> Option<Pixels> {
    let size = SIZE { cx: px as i32, cy: px as i32 };
    // SAFETY: a plain COM call; the bitmap handle it returns is ours and deleted below.
    let hbm = unsafe { item.GetImage(size, SIIGBF(SIIGBF_RESIZETOFIT.0 | SIIGBF_BIGGERSIZEOK.0 | flags.0)) }.ok()?;
    if hbm.is_invalid() {
        return None;
    }
    let pixels = dib_pixels(hbm);
    // SAFETY: we own the bitmap and are done with it.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(hbm.0));
    }
    let pixels = pixels?;
    // Wine answers with a 1 x 1 bitmap where it has nothing: that's no picture.
    if pixels.width.min(pixels.height) < 4 && px >= 16 {
        return None;
    }
    // BIGGERSIZEOK lets Windows hand back a bigger picture from its cache: made smaller here, so
    // the memory goes to what is shown.
    if pixels.width.max(pixels.height) > px + px / 4 {
        if let Some(smaller) = wic.and_then(|w| shrink(w, &pixels, px)) {
            return Some(smaller);
        }
    }
    Some(pixels)
}

/// BITMAPINFO with room for the three colour masks GetDIBits may write for 32-bit bitmaps.
#[repr(C)]
struct InfoWithMasks {
    info: BITMAPINFO,
    _masks: [u32; 3],
}

/// Copies a GDI bitmap out as premultiplied BGRA, top-down: GetDIBits with a negative height turns
/// the rows the right way round however the bitmap is stored. A bitmap whose every alpha is 0 has
/// no transparency (it's solid); one with alpha has colours checked against it.
fn dib_pixels(hbm: HBITMAP) -> Option<Pixels> {
    let mut bm = BITMAP::default();
    // SAFETY: `bm` is a valid out-struct of the size passed.
    let got = unsafe { GetObjectW(HGDIOBJ(hbm.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut _)) };
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if got == 0 || w <= 0 || h <= 0 || w > 16384 || h > 16384 {
        return None;
    }
    let mut info = InfoWithMasks {
        info: BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        },
        _masks: [0; 3],
    };
    let len = w as usize * h as usize * 4;
    let mut px: Vec<u8> = Vec::new();
    px.try_reserve_exact(len).ok()?;
    px.resize(len, 0);
    // SAFETY: the buffer holds exactly `h` rows of `w` 32-bit pixels; `info` has room for the masks;
    // the screen DC is released right after.
    let lines = unsafe {
        let dc = GetDC(None);
        let n = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut info.info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        n
    };
    if lines != h {
        return None;
    }
    fix_alpha(&mut px, bm.bmBitsPixel == 32);
    Some(Pixels { width: w as u32, height: h as u32, pixels: px })
}

/// Makes GDI's alpha usable: without real alpha (fewer than 32 bits, or every alpha 0) the picture
/// is solid; with it, no colour may be brighter than its alpha (that's what premultiplied means, and
/// a handler that gets it wrong would draw bright fringes).
fn fix_alpha(px: &mut [u8], has_alpha: bool) {
    let any_alpha = has_alpha && px.chunks_exact(4).any(|p| p[3] != 0);
    if !any_alpha {
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
        return;
    }
    for p in px.chunks_exact_mut(4) {
        let a = p[3];
        p[0] = p[0].min(a);
        p[1] = p[1].min(a);
        p[2] = p[2].min(a);
    }
}

/// A smaller copy that fits a `px` square (WIC's Fant scaler, which averages: no shimmer).
pub fn shrink(wic: &IWICImagingFactory, p: &Pixels, px: u32) -> Option<Pixels> {
    let (w, h) = fit_size(p.width, p.height, px, u64::MAX);
    // SAFETY: the WIC objects are owned wrappers; the source buffer outlives the calls.
    unsafe {
        let bmp = wic.CreateBitmapFromMemory(p.width, p.height, &GUID_WICPixelFormat32bppPBGRA, p.width * 4, &p.pixels).ok()?;
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&bmp, w, h, WICBitmapInterpolationModeFant).ok()?;
        copy_out(&scaler.cast::<IWICBitmapSource>().ok()?, w, h)
    }
}

/// Copies a WIC source of `w` x `h` (already premultiplied BGRA) into memory.
///
/// # Safety
/// `src` must be a live WIC source of that size and format.
unsafe fn copy_out(src: &IWICBitmapSource, w: u32, h: u32) -> Option<Pixels> {
    let stride = w.checked_mul(4)?;
    let len = stride as usize * h as usize;
    let mut buf: Vec<u8> = Vec::new();
    buf.try_reserve_exact(len).ok()?;
    buf.resize(len, 0);
    // SAFETY: the buffer is exactly `h` rows of `stride` bytes (the caller vouches for the source).
    unsafe { src.CopyPixels(std::ptr::null(), stride, &mut buf) }.ok()?;
    Some(Pixels { width: w, height: h, pixels: buf })
}

/// The size to make a `w` x `h` picture: as it is when it fits, otherwise shrunk (keeping its
/// shape) to fit a `max_side` square and at most `max_pixels` pixels. Never 0.
pub fn fit_size(w: u32, h: u32, max_side: u32, max_pixels: u64) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (w.max(1), h.max(1));
    }
    let mut scale = 1.0f64;
    let longest = w.max(h) as f64;
    if longest > max_side as f64 {
        scale = max_side as f64 / longest;
    }
    let area = w as f64 * h as f64 * scale * scale;
    if area > max_pixels as f64 {
        scale *= (max_pixels as f64 / area).sqrt();
    }
    if scale >= 1.0 {
        return (w, h);
    }
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

/// A picture file decoded by WIC.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    /// Its size on disk (after turning it the right way up), which may be bigger than what was
    /// decoded.
    pub original: (u32, u32),
}

/// Decodes a picture file with WIC: its first frame, turned the way `orientation` says (EXIF's
/// numbers 1-8, from the shell's System.Photo.Orientation; 0 or 1: as it is), shrunk to fit a
/// `max_side` square and `max_pixels`, as premultiplied BGRA. None when WIC can't read it (or the
/// memory isn't there).
pub fn decode(wic: &IWICImagingFactory, path: &str, max_side: u32, max_pixels: u64, orientation: u32) -> Option<Decoded> {
    let wide = wide_path(path);
    // SAFETY: the name buffer outlives the call; every WIC object is an owned wrapper, released when
    // dropped; sizes are checked before use.
    unsafe {
        let decoder = wic.CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let mut source: IWICBitmapSource = frame.cast().ok()?;
        let turn = transform_for(orientation);
        if turn != WICBitmapTransformRotate0 {
            let rotator = wic.CreateBitmapFlipRotator().ok()?;
            if rotator.Initialize(&source, turn).is_ok() {
                source = rotator.cast().ok()?;
            }
        }
        let (mut w, mut h) = (0u32, 0u32);
        source.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        let (tw, th) = fit_size(w, h, max_side, max_pixels);
        if (tw, th) != (w, h) {
            let scaler = wic.CreateBitmapScaler().ok()?;
            scaler.Initialize(&source, tw, th, WICBitmapInterpolationModeFant).ok()?;
            source = scaler.cast().ok()?;
        }
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&source, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let out = copy_out(&conv.cast().ok()?, tw, th)?;
        Some(Decoded { width: out.width, height: out.height, pixels: out.pixels, original: (w, h) })
    }
}

/// A picture file's size in pixels, from its header only (nothing is decoded).
pub fn picture_size(wic: &IWICImagingFactory, path: &str) -> Option<(u32, u32)> {
    let wide = wide_path(path);
    // SAFETY: as in `decode`.
    unsafe {
        let decoder = wic.CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        (w > 0 && h > 0).then_some((w, h))
    }
}

/// WIC's turn for an EXIF orientation (1 upright, 3 upside down, 6 and 8 on their side, 2/4/5/7
/// the same mirrored).
fn transform_for(orientation: u32) -> WICBitmapTransformOptions {
    let t = |a: WICBitmapTransformOptions, b: WICBitmapTransformOptions| WICBitmapTransformOptions(a.0 | b.0);
    match orientation {
        2 => WICBitmapTransformFlipHorizontal,
        3 => WICBitmapTransformRotate180,
        4 => WICBitmapTransformFlipVertical,
        5 => t(WICBitmapTransformRotate90, WICBitmapTransformFlipHorizontal),
        6 => WICBitmapTransformRotate90,
        7 => t(WICBitmapTransformRotate270, WICBitmapTransformFlipHorizontal),
        8 => WICBitmapTransformRotate270,
        _ => WICBitmapTransformRotate0,
    }
}

/// Windows' big system icon for a file or folder (the "jumbo" 256-pixel list, else the 48-pixel
/// one), for when the shell item gives no picture at all (Wine, odd places).
fn system_icon(wic: &IWICImagingFactory, path: &str, px: u32) -> Option<Pixels> {
    if path.starts_with("::") {
        return None;
    }
    let wide = wide_path(path);
    let mut info = SHFILEINFOW::default();
    // SAFETY: `wide` is NUL-terminated; `info` is a valid SHFILEINFOW and its size is passed.
    let ok = unsafe { SHGetFileInfoW(PCWSTR(wide.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX) };
    if ok == 0 {
        return None;
    }
    for list in [SHIL_JUMBO, SHIL_EXTRALARGE] {
        // SAFETY: plain shell calls; the icon handle is ours and destroyed after use.
        let icon: Option<HICON> = unsafe { SHGetImageList::<IImageList>(list as i32).ok().and_then(|l| l.GetIcon(info.iIcon, ILD_TRANSPARENT.0).ok()) };
        let Some(icon) = icon.filter(|i| !i.is_invalid()) else { continue };
        let pixels = icon_pixels(wic, icon);
        // SAFETY: we own the icon.
        unsafe {
            let _ = DestroyIcon(icon);
        }
        if let Some(p) = pixels.map(trim_small) {
            return Some(if p.width.max(p.height) > px { shrink(wic, &p, px).unwrap_or(p) } else { p });
        }
    }
    None
}

/// A big icon that is really a small picture in its top-left corner (types that have no big icon
/// come back from Windows' 256-pixel list like that): just that picture, so it's drawn at its own
/// size rather than blown up. Others are left as they are.
fn trim_small(p: Pixels) -> Pixels {
    if p.width < 96 || p.height < 96 || p.pixels.len() != (p.width * p.height * 4) as usize {
        return p;
    }
    let (mut right, mut bottom) = (0u32, 0u32);
    for y in 0..p.height {
        for x in 0..p.width {
            if p.pixels[((y * p.width + x) * 4 + 3) as usize] != 0 {
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if right == 0 || right > p.width / 2 || bottom > p.height / 2 {
        return p;
    }
    let side = right.max(bottom);
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        let start = ((y * p.width) * 4) as usize;
        pixels.extend_from_slice(&p.pixels[start..start + (side * 4) as usize]);
    }
    Pixels { width: side, height: side, pixels }
}

/// An icon as premultiplied BGRA pixels.
fn icon_pixels(wic: &IWICImagingFactory, icon: HICON) -> Option<Pixels> {
    // SAFETY: `icon` is a valid icon handle; every WIC object is an owned wrapper.
    unsafe {
        let src = wic.CreateBitmapFromHICON(icon).ok()?;
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&src, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h).ok()?;
        copy_out(&conv.cast().ok()?, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_icon_in_a_big_square_is_cut_out() {
        // A 256 x 256 picture with only its top-left 40 x 48 drawn.
        let mut pixels = vec![0u8; 256 * 256 * 4];
        for y in 0..48 {
            for x in 0..40 {
                pixels[(y * 256 + x) * 4 + 3] = 255;
            }
        }
        let p = trim_small(Pixels { width: 256, height: 256, pixels });
        assert_eq!((p.width, p.height, p.pixels.len()), (48, 48, 48 * 48 * 4));
        // A real big icon stays as it is.
        let full = trim_small(Pixels { width: 256, height: 256, pixels: vec![255u8; 256 * 256 * 4] });
        assert_eq!((full.width, full.height), (256, 256));
    }

    #[test]
    fn sizes_come_in_steps() {
        assert_eq!(bucket(1), 32);
        assert_eq!(bucket(200), 256);
        assert_eq!(bucket(256), 256);
        assert_eq!(bucket(257), 384);
        assert_eq!(bucket(5000), MAX_THUMBNAIL);
    }

    #[test]
    fn pictures_shrink_to_fit_and_keep_their_shape() {
        assert_eq!(fit_size(800, 600, 1000, u64::MAX), (800, 600), "fits: as it is");
        assert_eq!(fit_size(4000, 3000, 1000, u64::MAX), (1000, 750));
        assert_eq!(fit_size(3000, 4000, 1000, u64::MAX), (750, 1000));
        // A pixel limit: 8000 x 6000 is 48 million; at most 12 million is half the width and height.
        assert_eq!(fit_size(8000, 6000, 16384, 12_000_000), (4000, 3000));
        assert_eq!(fit_size(100_000, 1, 1000, u64::MAX), (1000, 1), "never 0");
        assert_eq!(fit_size(0, 5, 10, 10), (1, 5));
    }

    #[test]
    fn alpha_is_made_usable() {
        // No alpha at all: solid.
        let mut px = vec![10, 20, 30, 0, 40, 50, 60, 0];
        fix_alpha(&mut px, true);
        assert_eq!(px, vec![10, 20, 30, 255, 40, 50, 60, 255]);
        // Real alpha: colours never brighter than it (premultiplied).
        let mut px = vec![200, 20, 30, 100, 0, 0, 0, 0];
        fix_alpha(&mut px, true);
        assert_eq!(px, vec![100, 20, 30, 100, 0, 0, 0, 0]);
        // 24-bit bitmaps have no alpha byte worth reading.
        let mut px = vec![1, 2, 3, 77];
        fix_alpha(&mut px, false);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn a_picture_needs_matching_pixels() {
        assert!(Picture::new(2, 2, vec![0; 16], false).is_some());
        assert!(Picture::new(2, 2, vec![0; 15], false).is_none());
        assert!(Picture::new(0, 2, vec![], false).is_none());
        let p = Picture::new(1, 1, vec![1, 2, 3, 4], true).unwrap();
        assert!(p.is_icon() && !p.is_lost());
        assert_eq!(p.take_pixels(), Some(vec![1, 2, 3, 4]));
        assert_eq!(p.take_pixels(), None, "handed over once");
        p.give_back(vec![1, 2, 3, 4]);
        assert_eq!(p.take_pixels(), Some(vec![1, 2, 3, 4]), "and back when the renderer couldn't use them");
        let q = Picture::new(1, 1, vec![0; 4], false).unwrap();
        assert_ne!(p.id(), q.id());
    }

    #[test]
    fn orientation_turns() {
        assert_eq!(transform_for(1), WICBitmapTransformRotate0);
        assert_eq!(transform_for(0), WICBitmapTransformRotate0);
        assert_eq!(transform_for(6), WICBitmapTransformRotate90);
        assert_eq!(transform_for(8), WICBitmapTransformRotate270);
        assert_eq!(transform_for(3), WICBitmapTransformRotate180);
    }

    #[test]
    fn versions_change_with_the_file() {
        assert_ne!(version_of(100, 5), version_of(101, 5));
        assert_ne!(version_of(100, 5), version_of(100, 6));
    }
}
