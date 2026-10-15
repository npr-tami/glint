//! Looking inside files for the details panel and the Space preview: a picture's size, how long a
//! song or video plays, how many items a folder holds, the first lines of a text file, and (for the
//! Space preview) a picture decoded at full size.
//!
//! All of it touches the disk or the shell's property handlers, so it runs on a thread of its own,
//! never on the UI thread. Each panel has its own [`Peeker`] with one waiting place: asking again
//! replaces what was waiting, so arrowing through a big folder only ever looks at the newest item
//! (and at most the one already under way). Answers carry the `key` they were asked with; the panel
//! keeps facts by key and drops a full picture that isn't for what it shows any more. The thread
//! repaints the window when an answer is ready, and ends after a few idle seconds.
//!
//! Nothing reads a cloud file that isn't on this PC (that would download it), and
//! folders on network drives aren't counted (a sleeping server would hold the thread).

use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::{HWND, PROPERTYKEY};
use windows::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetDriveTypeW, FIND_FIRST_EX_LARGE_FETCH,
    WIN32_FIND_DATAW,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};

use crate::folder::{join, visible, wide, wide_path};
use crate::shell::{self, ComGuard};
use crate::thumbs::{self, Picture};

/// The thread ends after waiting this long with nothing to do.
const IDLE_END: Duration = Duration::from_secs(3);
/// Folders are counted up to this many items ("10,000+ items").
pub const MAX_COUNT: usize = 10_000;
/// Files whose name doesn't say they're text are only looked at when they're this small.
const SNIFF_LIMIT: u64 = 1024 * 1024;

/// FILE_ATTRIBUTE_OFFLINE, RECALL_ON_OPEN, RECALL_ON_DATA_ACCESS: a cloud file that isn't on this PC.
/// Reading it would download it, so only the shell (which asks the cloud app) looks at it.
pub const NOT_HERE: u32 = 0x1000 | 0x40000 | 0x400000;

/// Files that are text whatever is in them (lower case, no dot).
pub const TEXT_FILES: &[&str] = &[
    "txt", "md", "markdown", "log", "ini", "json", "xml", "csv", "tsv", "cs", "js", "ts", "jsx", "tsx", "py", "ps1", "bat", "cmd", "sh", "css",
    "html", "htm", "yml", "yaml", "toml", "cfg", "conf", "c", "h", "cpp", "hpp", "java", "go", "rs", "sql", "srt", "reg", "gitignore",
    "editorconfig", "lua", "rb", "php", "kt", "swift", "vb", "nfo", "diff", "patch", "properties", "env",
];

/// Never worth reading as text, even when small.
const NOT_TEXT: &[&str] = &["exe", "dll", "sys", "msi", "lnk", "zip", "7z", "rar", "gz", "tar", "iso", "pdf", "jar", "docx", "xlsx", "pptx", "bin"];

/// Whether a file with this extension is text by its name.
pub fn is_text_file(ext: &str) -> bool {
    TEXT_FILES.contains(&ext)
}

/// What kind of item is looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    File,
    Folder,
    /// A drive (This PC's rows): everything worth saying is in its entry already.
    Drive,
}

/// How much of a text file to read and keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextLimits {
    /// At most this many bytes from its start.
    pub bytes: usize,
    pub lines: usize,
    /// Longer lines are cut (in characters).
    pub line_chars: usize,
}

/// The details panel's: a box's worth of lines (64 KB, 40 lines, 400 characters).
pub const PANEL_TEXT: TextLimits = TextLimits { bytes: 64 * 1024, lines: 40, line_chars: 400 };
/// The Space preview's: the first 200 KB.
pub const PREVIEW_TEXT: TextLimits = TextLimits { bytes: 200 * 1024, lines: 20_000, line_chars: 2_000 };

/// What to find out about one item.
#[derive(Clone, Debug)]
pub struct Ask {
    /// What the answers are for (the item and its version); they carry it back.
    pub key: String,
    /// What the shell opens (a full path, or a place's "::{…}" name).
    pub path: String,
    pub what: What,
    /// The extension of the name shown (lower case, no dot): in the Recycle Bin the file on disk is
    /// called $R…, but the name shown is the real one.
    pub ext: String,
    pub attributes: u32,
    pub size: u64,
    /// Counting a folder's items: hidden ones too, and protected system files too.
    pub show_hidden: bool,
    pub show_system: bool,
    /// Read the start of the file as text (when it is text).
    pub text: Option<TextLimits>,
    /// Decode the picture at full size, fitting a square of this many pixels and at most this many
    /// pixels in all (the Space preview).
    pub picture: Option<(u32, u64)>,
}

/// The start of a text file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Text {
    pub lines: Vec<String>,
    /// The file goes on after what was read.
    pub cut: bool,
}

/// What was found out about an item. Anything unknown is None.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// Width x height of a picture or a video.
    pub dims: Option<(u32, u32)>,
    /// How long a song or video plays, in 100-nanosecond steps.
    pub duration: Option<u64>,
    /// How many items a folder holds (stopping at MAX_COUNT: then the flag says there are more).
    pub items: Option<(usize, bool)>,
    /// The start of a text file.
    pub text: Option<Arc<Text>>,
    /// The file is text by its name (not just looked like text when sniffed).
    pub text_by_name: bool,
    /// EXIF orientation of a photo (1 = upright), when the shell knows it.
    pub orientation: u32,
    /// What the preview's info strip says about a picture or a video (see `media_lines`).
    pub media: Media,
}

/// Facts about a picture or a video for the preview's info strip. Anything unknown is None.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    /// The picture's format by WIC ("PNG", "JPEG").
    pub format: Option<String>,
    /// Bits per pixel.
    pub bits: Option<u32>,
    /// Dots per inch (across).
    pub dpi: Option<f64>,
    /// How many frames (more than 1: an animation).
    pub frames: Option<u32>,
    /// The camera ("Canon EOS 80D").
    pub camera: Option<String>,
    /// When the photo was taken (a FILETIME).
    pub taken: Option<u64>,
    /// A video's frames per second.
    pub fps: Option<f64>,
    /// A video's bits per second.
    pub bitrate: Option<u32>,
}

/// An answer from the thread.
pub enum Answer {
    Facts { key: String, facts: Facts },
    /// A picture decoded at full size (None: it couldn't be), and its size on disk.
    Picture { key: String, picture: Option<Arc<Picture>>, original: (u32, u32) },
    /// An animated GIF's frames (asked with a picture; only sent when it has more than one).
    Frames { key: String, frames: crate::anim::Frames },
}

/// Facts kept by key, the oldest forgotten first (a panel's memory of what it looked at).
pub struct FactsCache {
    map: std::collections::HashMap<String, Facts>,
    order: std::collections::VecDeque<String>,
    cap: usize,
}

impl FactsCache {
    pub fn new(cap: usize) -> FactsCache {
        FactsCache { map: std::collections::HashMap::new(), order: std::collections::VecDeque::new(), cap: cap.max(1) }
    }

    pub fn get(&self, key: &str) -> Option<&Facts> {
        self.map.get(key)
    }

    pub fn insert(&mut self, key: String, facts: Facts) {
        if self.map.insert(key.clone(), facts).is_none() {
            self.order.push_back(key);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }
}

/// One panel's way to the thread (see the top of this file).
pub struct Peeker {
    shared: Arc<Shared>,
    tx: Sender<Answer>,
    rx: Receiver<Answer>,
    name: &'static str,
}

struct Shared {
    slot: Mutex<Slot>,
    ready: Condvar,
    hwnd: AtomicIsize,
}

struct Slot {
    waiting: Option<Ask>,
    running: bool,
}

impl Peeker {
    /// `name` names its thread (for debuggers and crash reports).
    pub fn new(name: &'static str) -> Peeker {
        let (tx, rx) = channel();
        let shared = Arc::new(Shared { slot: Mutex::new(Slot { waiting: None, running: false }), ready: Condvar::new(), hwnd: AtomicIsize::new(0) });
        Peeker { shared, tx, rx, name }
    }

    /// Asks about an item, replacing whatever was still waiting. `hwnd` is repainted when answers
    /// are ready. False when it couldn't be asked (no thread to be had right now).
    pub fn ask(&self, ask: Ask, hwnd: HWND) -> bool {
        self.shared.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        let start = {
            let mut slot = self.shared.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            slot.waiting = Some(ask);
            !std::mem::replace(&mut slot.running, true)
        };
        if start {
            let (shared, tx) = (self.shared.clone(), self.tx.clone());
            if !shell::background(self.name, move || work(&shared, &tx)) {
                // No thread right now: the panel asks again the next time it's drawn.
                let mut slot = self.shared.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                slot.running = false;
                slot.waiting = None;
                return false;
            }
        } else {
            self.shared.ready.notify_one();
        }
        true
    }

    /// The answers that arrived since the last call.
    pub fn take(&self) -> Vec<Answer> {
        self.rx.try_iter().collect()
    }

    /// Forgets what is still waiting (the panel was closed).
    pub fn cancel(&self) {
        self.shared.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).waiting = None;
    }
}

fn work(shared: &Shared, tx: &Sender<Answer>) {
    let _com = ComGuard::init();
    let wic = thumbs::wic_factory();
    loop {
        let ask = {
            let mut slot = shared.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if let Some(a) = slot.waiting.take() {
                    break a;
                }
                let (guard, wait) = shared.ready.wait_timeout(slot, IDLE_END).unwrap_or_else(std::sync::PoisonError::into_inner);
                slot = guard;
                if wait.timed_out() && slot.waiting.is_none() {
                    // Decided under the lock, so a question asked right now starts a new thread.
                    slot.running = false;
                    return;
                }
            }
        };
        let facts = look(&ask, wic.as_ref());
        let orientation = facts.orientation;
        if tx.send(Answer::Facts { key: ask.key.clone(), facts }).is_err() {
            return; // the window is gone
        }
        thumbs::repaint(&shared.hwnd);
        let Some((max_side, max_pixels)) = ask.picture else { continue };
        // Something newer is waiting already: decoding this one would only make it wait.
        if shared.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).waiting.is_some() {
            continue;
        }
        let started = std::time::Instant::now();
        // A cloud file that isn't on this PC isn't read (that would download it): the preview shows
        // Windows' thumbnail of it instead.
        let here = ask.attributes & NOT_HERE == 0;
        let decoded = wic.as_ref().filter(|_| here).and_then(|w| thumbs::decode(w, &ask.path, max_side, max_pixels, orientation));
        if std::env::var_os("GLINT_THUMB_LOG").is_some() {
            crate::log::line(&format!(
                "full picture {}: {} in {:.1} ms",
                ask.path,
                decoded.as_ref().map_or("couldn't be decoded".to_string(), |d| format!("{} x {} (on disk {} x {})", d.width, d.height, d.original.0, d.original.1)),
                started.elapsed().as_secs_f64() * 1000.0
            ));
        }
        let (picture, original) = match decoded {
            Some(d) => (Picture::new(d.width, d.height, d.pixels, false), d.original),
            None => (None, (0, 0)),
        };
        if tx.send(Answer::Picture { key: ask.key.clone(), picture, original }).is_err() {
            return;
        }
        thumbs::repaint(&shared.hwnd);
        // An animated GIF: its frames follow the first picture (which shows meanwhile).
        if ask.ext == "gif" && here && shared.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).waiting.is_none() {
            if let Some(frames) = wic.as_ref().and_then(|w| crate::anim::decode_gif(w, &ask.path, max_side.min(2048))) {
                if tx.send(Answer::Frames { key: ask.key, frames }).is_err() {
                    return;
                }
                thumbs::repaint(&shared.hwnd);
            }
        }
    }
}

/// Finds out what can be found out about one item.
fn look(ask: &Ask, wic: Option<&windows::Win32::Graphics::Imaging::IWICImagingFactory>) -> Facts {
    let mut facts = Facts::default();
    let here = ask.attributes & NOT_HERE == 0;
    match ask.what {
        What::File => {
            shell_facts(&ask.path, &mut facts);
            if facts.dims.is_none() && here && thumbs::is_picture(&ask.ext) {
                facts.dims = wic.and_then(|w| thumbs::picture_size(w, &ask.path));
            }
            if here && thumbs::is_picture(&ask.ext) {
                if let Some(w) = wic {
                    picture_media(w, &ask.path, &mut facts.media);
                }
            }
            facts.text_by_name = is_text_file(&ask.ext);
            let worth_sniffing = ask.size <= SNIFF_LIMIT && !NOT_TEXT.contains(&ask.ext.as_str()) && !thumbs::is_picture(&ask.ext) && facts.duration.is_none();
            if let Some(limits) = ask.text.filter(|_| here && (facts.text_by_name || worth_sniffing)) {
                facts.text = read_text(&ask.path, limits, !facts.text_by_name).map(Arc::new);
            }
        }
        What::Folder => {
            if here && !on_network(&ask.path) {
                facts.items = count_items(&ask.path, ask.show_hidden, ask.show_system);
            }
        }
        What::Drive => {}
    }
    facts
}

// ---------------------------------------------------------------- the shell's properties

const fn key(fmtid: u128, pid: u32) -> PROPERTYKEY {
    PROPERTYKEY { fmtid: GUID::from_u128(fmtid), pid }
}

/// System.Image.HorizontalSize / VerticalSize.
const IMAGE_WIDTH: PROPERTYKEY = key(0x6444048f_4c8b_11d1_8b70_080036b11a03, 3);
const IMAGE_HEIGHT: PROPERTYKEY = key(0x6444048f_4c8b_11d1_8b70_080036b11a03, 4);
/// System.Image.Dimensions ("1600 x 1000").
const IMAGE_DIMENSIONS: PROPERTYKEY = key(0x6444048f_4c8b_11d1_8b70_080036b11a03, 13);
/// System.Video.FrameWidth / FrameHeight.
const VIDEO_WIDTH: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 3);
const VIDEO_HEIGHT: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 4);
/// System.Media.Duration, in 100-nanosecond steps.
const MEDIA_DURATION: PROPERTYKEY = key(0x64440490_4c8b_11d1_8b70_080036b11a03, 3);
/// System.Photo.Orientation (EXIF's 1-8).
const PHOTO_ORIENTATION: PROPERTYKEY = key(0x14b81da1_0135_4d31_96d9_6cbfc9671a99, 274);
/// System.Photo.CameraManufacturer / CameraModel / DateTaken.
const CAMERA_MAKER: PROPERTYKEY = key(0x14b81da1_0135_4d31_96d9_6cbfc9671a99, 271);
const CAMERA_MODEL: PROPERTYKEY = key(0x14b81da1_0135_4d31_96d9_6cbfc9671a99, 272);
const DATE_TAKEN: PROPERTYKEY = key(0x14b81da1_0135_4d31_96d9_6cbfc9671a99, 36867);
/// System.Video.FrameRate (frames per 1,000 seconds), EncodingBitrate and TotalBitrate (bits per second).
const VIDEO_FRAME_RATE: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 6);
const VIDEO_BITRATE: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 8);
const VIDEO_TOTAL_BITRATE: PROPERTYKEY = key(0x64440491_4c8b_11d1_8b70_080036b11a03, 43);

/// The item for `path` in Windows' property system; None if Windows can't make one.
fn shell_item(path: &str) -> Option<IShellItem2> {
    let w = wide_path(path);
    // SAFETY: `w` is NUL-terminated and outlives the call; the item is an owned COM pointer.
    unsafe { SHCreateItemFromParsingName::<_, _, IShellItem2>(PCWSTR(w.as_ptr()), None) }.ok()
}

/// A picture's or a video's size in pixels, and how long it plays (in 100-nanosecond steps), as
/// Windows' property handlers know them.
fn size_and_length(item: &IShellItem2) -> (Option<(u32, u32)>, Option<u64>) {
    let mut dims = None;
    // SAFETY: plain property reads on a live item; the keys are constants, and the one string
    // read is freed with the allocator it came from.
    unsafe {
        for (across, down) in [(IMAGE_WIDTH, IMAGE_HEIGHT), (VIDEO_WIDTH, VIDEO_HEIGHT)] {
            if let (Ok(width), Ok(height)) = (item.GetUInt32(&across), item.GetUInt32(&down)) {
                if width > 0 && height > 0 {
                    dims = Some((width, height));
                    break;
                }
            }
        }
        if dims.is_none() {
            if let Ok(text) = item.GetString(&IMAGE_DIMENSIONS) {
                if !text.is_null() {
                    dims = parse_dimensions(&text.to_string().unwrap_or_default());
                    CoTaskMemFree(Some(text.0 as *const _));
                }
            }
        }
        let length = item.GetUInt64(&MEDIA_DURATION).ok().filter(|&ticks| ticks > 0 && ticks < i64::MAX as u64);
        (dims, length)
    }
}

/// The same for a file on disk, with a picture's header read with WIC when Windows has no size for
/// it (as the details panel does): the list's Picture size and Length columns (media.rs).
pub fn size_and_length_of(path: &str, picture: bool, wic: Option<&windows::Win32::Graphics::Imaging::IWICImagingFactory>) -> (Option<(u32, u32)>, Option<u64>) {
    let (mut dims, length) = shell_item(path).map_or((None, None), |item| size_and_length(&item));
    if dims.is_none() && picture {
        dims = wic.and_then(|w| thumbs::picture_size(w, path));
    }
    (dims, length)
}

/// What Windows' property handlers know: picture and video size, length, a photo's orientation.
fn shell_facts(path: &str, facts: &mut Facts) {
    let Some(item) = shell_item(path) else { return };
    (facts.dims, facts.duration) = size_and_length(&item);
    // SAFETY: plain property reads on a live item; the keys are constants.
    unsafe {
        if let Ok(o) = item.GetUInt32(&PHOTO_ORIENTATION) {
            facts.orientation = o;
        }
        let m = &mut facts.media;
        let text = |k: &PROPERTYKEY| -> Option<String> {
            let p = item.GetString(k).ok()?;
            if p.is_null() {
                return None;
            }
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            let s = s.trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        m.camera = camera_name(text(&CAMERA_MAKER).as_deref(), text(&CAMERA_MODEL).as_deref());
        if let Ok(ft) = item.GetFileTime(&DATE_TAKEN) {
            let t = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
            m.taken = (t > 0).then_some(t);
        }
        if let Ok(r) = item.GetUInt32(&VIDEO_FRAME_RATE) {
            m.fps = (r > 0).then_some(r as f64 / 1000.0);
        }
        m.bitrate = [VIDEO_TOTAL_BITRATE, VIDEO_BITRATE].iter().find_map(|k| item.GetUInt32(k).ok().filter(|&b| b > 0));
    }
}

/// "Canon EOS 80D" from the maker "Canon" and the model "Canon EOS 80D" (models often start with
/// the maker's name already).
pub fn camera_name(maker: Option<&str>, model: Option<&str>) -> Option<String> {
    match (maker, model) {
        (Some(a), Some(b)) if b.to_lowercase().starts_with(&a.to_lowercase().split_whitespace().next().unwrap_or("").to_string()) => Some(b.to_string()),
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (None, Some(b)) => Some(b.to_string()),
        (Some(a), None) => Some(a.to_string()),
        (None, None) => None,
    }
}

/// A picture's format, bit depth, resolution and frame count, from WIC (headers only).
fn picture_media(wic: &windows::Win32::Graphics::Imaging::IWICImagingFactory, path: &str, m: &mut Media) {
    use windows::Win32::Graphics::Imaging::IWICPixelFormatInfo;
    use windows::core::Interface;
    let w = wide_path(path);
    // SAFETY: the name buffer outlives the call; WIC objects are owned wrappers.
    unsafe {
        let Ok(decoder) = wic.CreateDecoderFromFilename(PCWSTR(w.as_ptr()), None, windows::Win32::Foundation::GENERIC_READ, windows::Win32::Graphics::Imaging::WICDecodeMetadataCacheOnDemand) else { return };
        if let Ok(g) = decoder.GetContainerFormat() {
            m.format = format_name(&g).map(str::to_string);
        }
        m.frames = decoder.GetFrameCount().ok();
        let Ok(frame) = decoder.GetFrame(0) else { return };
        if let Ok(pf) = frame.GetPixelFormat() {
            m.bits = wic.CreateComponentInfo(&pf).ok().and_then(|i| i.cast::<IWICPixelFormatInfo>().ok()).and_then(|i| i.GetBitsPerPixel().ok()).filter(|&b| b > 0);
        }
        let (mut x, mut y) = (0.0f64, 0.0f64);
        if frame.GetResolution(&mut x, &mut y).is_ok() && x > 0.0 {
            m.dpi = Some(x);
        }
    }
}

/// The name of a WIC container format.
fn format_name(g: &GUID) -> Option<&'static str> {
    use windows::Win32::Graphics::Imaging::*;
    let names: [(GUID, &str); 10] = [
        (GUID_ContainerFormatPng, "PNG"),
        (GUID_ContainerFormatJpeg, "JPEG"),
        (GUID_ContainerFormatGif, "GIF"),
        (GUID_ContainerFormatBmp, "BMP"),
        (GUID_ContainerFormatTiff, "TIFF"),
        (GUID_ContainerFormatIco, "ICO"),
        (GUID_ContainerFormatWebp, "WebP"),
        (GUID_ContainerFormatHeif, "HEIF"),
        (GUID_ContainerFormatWmp, "JPEG XR"),
        (GUID_ContainerFormatDds, "DDS"),
    ];
    names.iter().find(|(id, _)| id == g).map(|(_, n)| *n)
}

/// "1600 x 1000" (System.Image.Dimensions; the "x" may be a "×", with or without spaces).
pub fn parse_dimensions(text: &str) -> Option<(u32, u32)> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace() && *c != '\u{200e}' && *c != '\u{202a}' && *c != '\u{202c}').collect();
    let (w, h) = cleaned.split_once(['x', 'X', '\u{d7}'])?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

// ---------------------------------------------------------------- folders

/// Network places can stall for a long time while disconnected: their folders aren't counted.
pub fn on_network(path: &str) -> bool {
    if path.starts_with("\\\\") {
        return true;
    }
    let Some(root) = path.get(..3).filter(|r| r.ends_with(":\\")) else { return true };
    let w = wide(root);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) == 4 /* DRIVE_REMOTE */ }
}

/// How many items a folder holds (as the list would show them), stopping at MAX_COUNT.
fn count_items(path: &str, show_hidden: bool, show_system: bool) -> Option<(usize, bool)> {
    let pattern = wide_path(&join(path, "*"));
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL-terminated and outlives the call; `data` is a valid WIN32_FIND_DATAW.
    let handle = unsafe { FindFirstFileExW(PCWSTR(pattern.as_ptr()), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_LARGE_FETCH) };
    let handle = match handle {
        Ok(h) => h,
        // A drive root with nothing on it finds nothing at all ("not found"). Any other failure
        // (no permission, as in System Volume Information) leaves it uncounted, not empty.
        Err(e) => return (matches!(e.code().0 as u32, 0x8007_0002 | 0x8007_0012) && std::path::Path::new(path).is_dir()).then_some((0, false)),
    };
    let mut n = 0usize;
    let mut more = false;
    loop {
        let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
        let name = &data.cFileName[..len];
        let dots = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
        if !dots && visible(data.dwFileAttributes, show_hidden, show_system) {
            n += 1;
            if n >= MAX_COUNT {
                more = true;
                break;
            }
        }
        // SAFETY: `handle` is the live search handle from above; `data` is valid.
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    // SAFETY: closes the handle we opened; it's not used afterwards.
    unsafe {
        let _ = FindClose(handle);
    }
    Some((n, more))
}

// ---------------------------------------------------------------- text

/// The start of a file as text, or None when it isn't text (or can't be read). `sniff`: the name
/// doesn't say it's text, so it must be small and look like text.
fn read_text(path: &str, limits: TextLimits, sniff: bool) -> Option<Text> {
    let mut file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if sniff && length > SNIFF_LIMIT {
        return None;
    }
    let mut buf = Vec::new();
    file.by_ref().take(limits.bytes as u64).read_to_end(&mut buf).ok()?;
    let whole = buf.len() as u64 >= length;
    decode_text(&buf, whole, sniff, limits)
}

/// Text from the first bytes of a file: UTF-8 or UTF-16 with a byte-order
/// mark; otherwise no zero bytes and few control characters, read as UTF-8 if it is, else as
/// Latin-1. A file cut short is read up to its last whole line (or character), so the cut doesn't
/// make it look like something else. Lines end in \r\n, \n or \r; tabs become four spaces; empty
/// lines at the end go. `whole`: `bytes` is the whole file.
pub fn decode_text(bytes: &[u8], whole: bool, sniff: bool, limits: TextLimits) -> Option<Text> {
    let n = bytes.len();
    let text: String = if n >= 3 && bytes[..3] == [0xEF, 0xBB, 0xBF] {
        let body = &bytes[3..];
        String::from_utf8_lossy(&body[..whole_utf8(body, whole)]).into_owned()
    } else if n >= 2 && (bytes[..2] == [0xFF, 0xFE] || bytes[..2] == [0xFE, 0xFF]) {
        let little = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|p| if little { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) }).collect();
        String::from_utf16_lossy(&units)
    } else {
        // No marker: zero bytes or many control characters mean it isn't text.
        let head = &bytes[..n.min(4096)];
        let mut control = 0usize;
        let mut high = 0usize;
        for &b in head {
            if b == 0 {
                return None;
            }
            if b < 32 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 27) {
                control += 1;
            }
            if b >= 128 {
                high += 1;
            }
        }
        if control > head.len() / 50 + 1 {
            return None;
        }
        let end = if whole || n == 0 {
            n
        } else {
            match bytes.iter().rposition(|&b| b == b'\n') {
                Some(nl) => nl + 1,
                None => whole_utf8(bytes, false),
            }
        };
        match std::str::from_utf8(&bytes[..end]) {
            Ok(s) => s.to_string(),
            Err(_) => {
                if sniff && high > head.len() * 3 / 10 {
                    return None; // mostly odd bytes: binary after all
                }
                bytes[..end].iter().map(|&b| b as char).collect() // Latin-1
            }
        }
    };
    let mut lines = Vec::new();
    let mut cut = !whole;
    let mut rest = text.as_str();
    loop {
        if lines.len() >= limits.lines {
            cut = cut || !rest.is_empty();
            break;
        }
        let (line, next) = match rest.find(['\r', '\n']) {
            Some(i) => {
                let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                (&rest[..i], Some(&rest[i + skip..]))
            }
            None => (rest, None),
        };
        let mut shown: String = line.chars().take(limits.line_chars).collect();
        if shown.contains('\t') {
            shown = shown.replace('\t', "    ");
        }
        lines.push(shown);
        match next {
            Some(n) => rest = n,
            None => break,
        }
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    Some(Text { lines, cut })
}

/// How many of `bytes` hold whole UTF-8 characters (the last one may have been cut off), or all of
/// them when `whole`.
fn whole_utf8(bytes: &[u8], whole: bool) -> usize {
    let n = bytes.len();
    if whole || n == 0 {
        return n;
    }
    let mut start = n - 1;
    while start > 0 && n - start < 4 && bytes[start] & 0xC0 == 0x80 {
        start -= 1; // back to the character's first byte
    }
    let lead = bytes[start];
    let need = if lead >= 0xF0 {
        4
    } else if lead >= 0xE0 {
        3
    } else if lead >= 0xC0 {
        2
    } else {
        1
    };
    if n - start < need {
        start
    } else {
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: TextLimits = TextLimits { bytes: 1 << 20, lines: 100, line_chars: 50 };

    #[test]
    fn plain_text_becomes_lines() {
        let t = decode_text(b"one\r\ntwo\nthree\rfour\n\n\n", true, false, L).unwrap();
        assert_eq!(t.lines, vec!["one", "two", "three", "four"]);
        assert!(!t.cut);
        let t = decode_text(b"a\tb", true, false, L).unwrap();
        assert_eq!(t.lines, vec!["a    b"], "tabs become spaces");
        assert_eq!(decode_text(b"", true, false, L).unwrap().lines, Vec::<String>::new(), "an empty file");
    }

    #[test]
    fn binary_is_not_text() {
        assert!(decode_text(b"MZ\x90\x00\x03", true, true, L).is_none(), "zero bytes");
        assert!(decode_text(&[1, 2, 3, 4, 5, 6, 7, 8, 65, 66], true, true, L).is_none(), "control characters");
    }

    #[test]
    fn encodings() {
        // UTF-8 with and without a marker, UTF-16 both ways, Latin-1 when it isn't UTF-8.
        assert_eq!(decode_text("caf\u{e9}".as_bytes(), true, false, L).unwrap().lines, vec!["caf\u{e9}"]);
        assert_eq!(decode_text(b"\xEF\xBB\xBFhi", true, false, L).unwrap().lines, vec!["hi"]);
        // With a marker and cut short in the middle of a character.
        assert_eq!(decode_text(b"\xEF\xBB\xBFab\xC3", false, false, L).unwrap().lines, vec!["ab"]);
        assert_eq!(decode_text(b"\xEF\xBB\xBF", false, false, L).unwrap().lines, Vec::<String>::new());
        assert_eq!(decode_text(b"\xFF\xFEh\x00i\x00", true, false, L).unwrap().lines, vec!["hi"]);
        assert_eq!(decode_text(b"\xFE\xFF\x00h\x00i", true, false, L).unwrap().lines, vec!["hi"]);
        assert_eq!(decode_text(b"caf\xE9", true, false, L).unwrap().lines, vec!["caf\u{e9}"]);
    }

    #[test]
    fn a_cut_file_stays_utf8() {
        // The file goes on after the first bytes read: it's read up to its last whole line, so a
        // character cut in half at the end doesn't turn it into Latin-1.
        let mut bytes = "first line\nsecond caf\u{e9}".as_bytes().to_vec();
        bytes.pop(); // half of the "é"
        let t = decode_text(&bytes, false, false, L).unwrap();
        assert_eq!(t.lines, vec!["first line"]);
        assert!(t.cut);
        // One long line cut in half: up to its last whole character.
        let mut one = "\u{e9}\u{e9}\u{e9}".as_bytes().to_vec();
        one.pop();
        assert_eq!(decode_text(&one, false, false, L).unwrap().lines, vec!["\u{e9}\u{e9}"]);
    }

    #[test]
    fn limits_hold() {
        let many: String = (0..10).map(|i| format!("line {i}\n")).collect();
        let t = decode_text(many.as_bytes(), true, false, TextLimits { bytes: 1000, lines: 3, line_chars: 4 }).unwrap();
        assert_eq!(t.lines, vec!["line", "line", "line"]);
        assert!(t.cut, "more lines than kept");
    }

    #[test]
    fn dimensions_from_the_shell() {
        assert_eq!(parse_dimensions("1600 x 1000"), Some((1600, 1000)));
        assert_eq!(parse_dimensions("\u{202a}800 \u{d7} 600\u{202c}"), Some((800, 600)));
        assert_eq!(parse_dimensions("800x600"), Some((800, 600)));
        assert_eq!(parse_dimensions("big"), None);
        assert_eq!(parse_dimensions("0 x 5"), None);
    }

    #[test]
    fn the_oldest_facts_go_first() {
        let mut c = FactsCache::new(2);
        c.insert("a".into(), Facts::default());
        c.insert("b".into(), Facts { orientation: 6, ..Facts::default() });
        c.insert("b".into(), Facts { orientation: 8, ..Facts::default() });
        assert_eq!(c.get("b").map(|f| f.orientation), Some(8), "replaced, not added twice");
        c.insert("c".into(), Facts::default());
        assert!(c.get("a").is_none() && c.get("b").is_some() && c.get("c").is_some());
    }

    #[test]
    fn text_files_by_name() {
        assert!(is_text_file("md") && is_text_file("rs") && is_text_file("txt"));
        assert!(!is_text_file("jpg") && !is_text_file(""));
    }
}
