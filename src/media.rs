//! Picture sizes and lengths for the file list's "Picture size" and "Length" columns (both off until
//! ticked in the column titles' right-click menu, and then only shown where something can have them,
//! [`here`]): how many pixels wide and tall a picture or a video is, and how long a video or a song
//! plays. They come from Windows' property system, the
//! same values File Explorer's columns show (and, for a picture Windows says nothing about, from the
//! picture's header, read with WIC, as the details panel does).
//!
//! - **What is read.** Only files whose extension can have a value for a column that is on
//!   (pictures and videos for Picture size, videos and songs for Length). Never folders, nothing
//!   inside a .zip or .7z, no cloud file that isn't on this PC (reading it would download it), and
//!   nothing at all while both columns are off.
//! - **In what order.** The rows on screen first: every frame, each list says which of them it
//!   still needs ([`want`]). Then the rest of the folder, a batch at a time, so sorting by a column
//!   soon has every value. On a USB stick or a network drive the rest is only read while the list is
//!   sorted by one of the columns (the rows on screen always are). A view without columns (List,
//!   the icons) has only the rest read, and only while sorted by one of them.
//! - **Where.** On one thread of its own, with COM, one file at a time. It repaints the window as
//!   values come in, and ends after a few idle seconds or when the window is hidden. Nothing on the
//!   UI thread waits for it: a slow drive only means the values come later.
//! - **Kept** by listing (the folder a tab shows, with its search results or the folders open in its
//!   tree) and then by file, with the file's last change: a file that changed is read again. The
//!   values of a listing no tab shows any more are forgotten ([`keep`]), and all of them are when
//!   both columns go off ([`clear`]).
//! - **Sorting** by a column ([`sort_view`]) uses what is known so far; files without a value go
//!   last. The list sorts again as more values come in (list.rs, the way folder sizes do in app.rs).

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Imaging::IWICImagingFactory;
use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

use crate::folder::{self, Entry, EntryKind, SortKey};
use crate::shell::{self, ComGuard};
use crate::thumbs;

/// The thread ends after waiting this long with nothing to do.
const IDLE_END: Duration = Duration::from_secs(3);
/// While values stream in, the window is repainted at most this often.
const REPAINT_EVERY: Duration = Duration::from_millis(100);
/// How many of the rest of a folder's files the list hands over at a time (it hands over more when
/// fewer than a quarter of them are still waiting).
pub const REST_BATCH: usize = 128;

/// Songs, by extension (lower case, no dot): they have a length. Pictures are thumbs.rs's, videos
/// video.rs's.
const SONGS: &[&str] = &["mp3", "m4a", "m4b", "aac", "wav", "wma", "flac", "ogg", "opus", "aif", "aiff", "ac3", "amr", "mka"];

// ---------------------------------------------------------------- what the columns show

/// What the columns show about one file. Anything unknown is None.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Width x height in pixels: a picture's, or a video's frames.
    pub dims: Option<(u32, u32)>,
    /// How long it plays, in 100-nanosecond steps.
    pub length: Option<u64>,
}

/// Which of the two columns show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Columns {
    /// Picture size.
    pub dims: bool,
    pub length: bool,
}

impl Columns {
    pub fn any(self) -> bool {
        self.dims || self.length
    }
}

/// Whether a file with this extension (lower case, no dot) can have a value for a column that shows.
pub fn can_have(ext: &str, cols: Columns) -> bool {
    let video = crate::video::is_video(ext);
    (cols.dims && (video || thumbs::is_picture(ext))) || (cols.length && (video || SONGS.contains(&ext)))
}

/// Whether the list asks about an entry at all: a file that can have a value for a column that
/// shows, and that is on this PC (a cloud file that isn't would be downloaded to be read).
pub fn worth_asking(e: &Entry, cols: Columns) -> bool {
    e.kind == EntryKind::File && e.attributes & crate::peek::NOT_HERE == 0 && can_have(&e.ext, cols)
}

/// Which columns anything in a view (rows, as indexes into `entries`) can fill (`can_have`): the
/// columns only show where this says so (list.rs), so a folder of folders or of documents has
/// neither. Nothing at all inside a .zip or .7z (nothing there is read) or in a listing that isn't a
/// folder.
pub fn here(entries: &[Entry], view: &[u32], listing: &str) -> Columns {
    let mut here = Columns::default();
    if listing.is_empty() || crate::archive::split(listing).is_some() {
        return here;
    }
    for e in view.iter().map(|&i| &entries[i as usize]).filter(|e| e.kind == EntryKind::File) {
        here.dims |= can_have(&e.ext, Columns { dims: true, length: false });
        here.length |= can_have(&e.ext, Columns { dims: false, length: true });
        if here.dims && here.length {
            break;
        }
    }
    here
}

/// A Picture size cell: "1920 × 1080" ("" while unknown, or for a file that has none).
pub fn size_text(facts: Option<&Facts>) -> String {
    match facts.and_then(|f| f.dims) {
        Some((w, h)) => format!("{w} \u{d7} {h}"),
        None => String::new(),
    }
}

/// A Length cell: "3:42", or "1:02:03" from an hour on ("" while unknown, or for a file that has
/// none).
pub fn length_text(facts: Option<&Facts>) -> String {
    facts.and_then(|f| f.length).map(crate::details::duration_text).unwrap_or_default()
}

// ---------------------------------------------------------------- asking

/// A file the list wants read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// How it's kept in its listing (`key_of`).
    key: String,
    path: String,
    /// Its last change (`Entry::modified`): the values are kept with it.
    modified: u64,
    /// A picture: WIC reads its size from its header when Windows' property system has none.
    picture: bool,
}

impl Ask {
    /// For an entry of the listing at `folder`.
    pub fn of(folder: &str, e: &Entry) -> Ask {
        Ask { key: key_of(e).into_owned(), path: e.path_in(folder), modified: e.modified, picture: thumbs::is_picture(&e.ext) }
    }
}

/// How an entry is kept in its listing: by its name in lower case (`Entry::lower`), or, for items
/// from other folders (search results, the tree view's open folders), by their full path in lower
/// case.
fn key_of(e: &Entry) -> Cow<'_, str> {
    match &e.target {
        Some(t) => Cow::Owned(t.to_lowercase()),
        None => Cow::Borrowed(&e.lower),
    }
}

/// What one list wants read.
struct Wish {
    /// Which list (list.rs gives each its own number: split view has two).
    list: u32,
    /// The listing it's for, in lower case.
    listing: String,
    /// Its rows on screen that aren't known yet, top first (replaced every frame).
    screen: VecDeque<Ask>,
    /// The rest of the listing, as the list hands it over.
    rest: VecDeque<Ask>,
}

/// A file's values, and its last change when they were read.
struct Value {
    modified: u64,
    facts: Facts,
}

/// A file the thread reads next, and the listing it's for.
struct Job {
    listing: String,
    ask: Ask,
}

/// What the lists, the sorting and the thread share.
#[derive(Default)]
struct State {
    wishes: Vec<Wish>,
    /// What was read, by listing (lower case), then by entry (`key_of`).
    kept: HashMap<String, HashMap<String, Value>>,
    /// What the thread is reading now (listing, key): not handed to it again meanwhile.
    reading: Option<(String, String)>,
    /// The thread is there (waiting for work, or working).
    running: bool,
}

impl State {
    /// The next file to read: the rows on screen first (every list's), then the rest. None while
    /// the window is hidden (Glint in the background: nobody looks).
    fn next(&mut self) -> Option<Job> {
        if !window_shown() {
            return None;
        }
        let State { wishes, kept, reading, .. } = self;
        for on_screen in [true, false] {
            for w in wishes.iter_mut() {
                let queue = if on_screen { &mut w.screen } else { &mut w.rest };
                while let Some(ask) = queue.pop_front() {
                    let known = kept.get(&w.listing).and_then(|k| k.get(&ask.key)).is_some_and(|v| v.modified == ask.modified);
                    if !known {
                        *reading = Some((w.listing.clone(), ask.key.clone()));
                        return Some(Job { listing: w.listing.clone(), ask });
                    }
                }
            }
        }
        None
    }

    /// Whether anything waits to be read (and may be, the window being on screen).
    fn has_work(&self) -> bool {
        window_shown() && self.wishes.iter().any(|w| !w.screen.is_empty() || !w.rest.is_empty())
    }
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
/// Wakes the thread when there is something to read.
static READY: Condvar = Condvar::new();
/// The window to repaint as values come in (0 until a list asks).
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Goes up with every value read (see [`arrived`]).
static ARRIVED: AtomicU64 = AtomicU64::new(0);

fn state() -> MutexGuard<'static, State> {
    STATE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether the window is on screen (no window yet, as in the tests, counts as shown).
fn window_shown() -> bool {
    let h = WINDOW.load(Ordering::SeqCst);
    // SAFETY: a plain query about a window handle; a stale one just answers false.
    h == 0 || unsafe { IsWindowVisible(HWND(h as *mut _)) }.as_bool()
}

/// What is known about these entries of the listing at `folder`, in their order (None: not read
/// yet, or read before the file last changed).
pub fn known<'a>(folder: &str, entries: impl IntoIterator<Item = &'a Entry>) -> Vec<Option<Facts>> {
    let s = state();
    let kept = s.kept.get(&folder.to_lowercase());
    entries.into_iter().map(|e| kept.and_then(|k| lookup(k, e))).collect()
}

fn lookup(kept: &HashMap<String, Value>, e: &Entry) -> Option<Facts> {
    kept.get(key_of(e).as_ref()).filter(|v| v.modified == e.modified).map(|v| v.facts)
}

/// What list `list` wants read for the listing at `folder`: its rows on screen that aren't known
/// yet (replacing what it said before), and more of the rest of the listing (`restart`: what it
/// handed over before was for an older listing, or other columns). `hwnd` is repainted as values
/// come in. Returns how many of the rest are still waiting, so the list hands over more in time.
pub fn want(list: u32, folder: &str, screen: Vec<Ask>, rest: Vec<Ask>, restart: bool, hwnd: HWND) -> usize {
    WINDOW.store(hwnd.0 as isize, Ordering::SeqCst);
    let listing = folder.to_lowercase();
    let mut s = state();
    let at = match s.wishes.iter().position(|w| w.list == list) {
        Some(at) => at,
        None => {
            s.wishes.push(Wish { list, listing: listing.clone(), screen: VecDeque::new(), rest: VecDeque::new() });
            s.wishes.len() - 1
        }
    };
    let reading = s.reading.clone();
    let w = &mut s.wishes[at];
    if restart || w.listing != listing {
        w.rest = VecDeque::new();
    }
    // (The one the thread is reading now comes in a moment.)
    w.screen = screen.into_iter().filter(|a| reading.as_ref().is_none_or(|(l, k)| *l != listing || *k != a.key)).collect();
    w.listing = listing;
    w.rest.extend(rest);
    let waiting = w.rest.len();
    let work = !w.screen.is_empty() || !w.rest.is_empty();
    if work && !s.running {
        s.running = true;
        if !shell::background("picture sizes and lengths", work_through) {
            // No thread to be had right now: what is on screen is asked for again next frame.
            s.running = false;
            s.wishes.retain(|w| w.list != list);
            return 0;
        }
    } else if work {
        READY.notify_one();
    }
    waiting
}

/// List `list` shows no rows these columns are about any more (another view, another place, the
/// columns off): what it asked for is dropped.
pub fn stop(list: u32) {
    state().wishes.retain(|w| w.list != list);
}

/// Forgets the values of every listing but these folders' (the ones the tabs show).
/// Tests that read the values (or tick a list, which forgets them) take turns: the values are
/// shared by the whole program.
#[cfg(test)]
pub(crate) fn test_turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn keep<'a>(folders: impl IntoIterator<Item = &'a str>) {
    let open: Vec<String> = folders.into_iter().map(str::to_lowercase).collect();
    state().kept.retain(|listing, _| open.contains(listing));
}

/// Both columns are off: nothing more is read, and everything read is forgotten (its memory given
/// back). Cheap when there is nothing.
pub fn clear() {
    let mut s = state();
    if !s.wishes.is_empty() || !s.kept.is_empty() {
        s.wishes.clear();
        s.kept = HashMap::new();
    }
}

/// A number that goes up whenever values come in: the list sorts again when it changes.
pub fn arrived() -> u64 {
    ARRIVED.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------- sorting

/// Sorts `view` (indexes into `entries`, the listing at `folder`) by Picture size (all its pixels,
/// then its width) or Length, with what is known so far: folders first as usual (`folders_first`),
/// files without a value last whichever way, ties A to Z (folder.rs `sort_view_by`).
pub fn sort_view(entries: &[Entry], view: &mut [u32], folder: &str, key: SortKey, descending: bool, folders_first: bool) {
    let mut values: Vec<Option<(u64, u64)>> = Vec::new();
    {
        let s = state();
        if let Some(kept) = s.kept.get(&folder.to_lowercase()) {
            values = vec![None; entries.len()];
            for &i in view.iter() {
                if let Some(e) = entries.get(i as usize).filter(|e| e.kind == EntryKind::File) {
                    values[i as usize] = lookup(kept, e).and_then(|f| sort_value(key, &f));
                }
            }
        }
    }
    folder::sort_view_by(entries, view, descending, folders_first, &values);
}

/// What sorting by `key` goes by: (pixels in all, width) for Picture size, the length for Length.
fn sort_value(key: SortKey, f: &Facts) -> Option<(u64, u64)> {
    match key {
        SortKey::Dimensions => f.dims.map(|(w, h)| (u64::from(w) * u64::from(h), u64::from(w))),
        SortKey::Length => f.length.map(|t| (t, 0)),
        _ => None,
    }
}

// ---------------------------------------------------------------- the thread

/// The thread: reads the next file (the rows on screen first), keeps its values and repaints the
/// window, until there has been nothing to do for IDLE_END.
fn work_through() {
    let _com = ComGuard::init();
    let wic = thumbs::wic_factory();
    let mut last_repaint: Option<Instant> = None;
    loop {
        let job = {
            let mut s = state();
            loop {
                if let Some(job) = s.next() {
                    break job;
                }
                let (guard, wait) = READY.wait_timeout(s, IDLE_END).unwrap_or_else(|e| e.into_inner());
                s = guard;
                if wait.timed_out() && !s.has_work() {
                    // Decided under the lock, so a list asking right now starts a new thread.
                    s.running = false;
                    return;
                }
            }
        };
        let facts = read(&job.ask.path, job.ask.picture, wic.as_ref());
        let more = {
            let mut s = state();
            s.reading = None;
            // (Not kept when its listing went meanwhile: no tab shows it any more.)
            if s.kept.contains_key(&job.listing) || s.wishes.iter().any(|w| w.listing == job.listing) {
                s.kept.entry(job.listing).or_default().insert(job.ask.key, Value { modified: job.ask.modified, facts });
                ARRIVED.fetch_add(1, Ordering::SeqCst);
            }
            s.has_work()
        };
        if !more || last_repaint.is_none_or(|t| t.elapsed() >= REPAINT_EVERY) {
            last_repaint = Some(Instant::now());
            thumbs::repaint(&WINDOW);
        }
    }
}

/// What Windows' property system knows about a file: its picture or frame size, and how long it
/// plays. For a picture it has no size for, WIC reads the header (peek.rs, as the details panel).
fn read(path: &str, picture: bool, wic: Option<&IWICImagingFactory>) -> Facts {
    let (dims, length) = crate::peek::size_and_length_of(path, picture, wic);
    Facts { dims, length }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn entry(name: &str, kind: EntryKind, modified: u64) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind,
            size: 1,
            modified,
            created: 0,
            attributes: 0,
            ext: if kind == EntryKind::File { folder::extension(name) } else { String::new() },
            type_name: Arc::from("x"),
            target: None,
            free: 0,
        }
    }

    #[test]
    fn sizes_and_lengths_read_plainly() {
        let both = Facts { dims: Some((1920, 1080)), length: Some(222 * 10_000_000) };
        assert_eq!(size_text(Some(&both)), "1920 \u{d7} 1080");
        assert_eq!(length_text(Some(&both)), "3:42");
        let long = Facts { dims: None, length: Some(3723 * 10_000_000) };
        assert_eq!(length_text(Some(&long)), "1:02:03");
        // A song has no picture size, a photo no length; unknown is empty.
        assert_eq!(size_text(Some(&long)), "");
        assert_eq!((size_text(None), length_text(None)), (String::new(), String::new()));
    }

    #[test]
    fn only_files_that_can_have_them_are_read() {
        let (dims, length) = (Columns { dims: true, length: false }, Columns { dims: false, length: true });
        assert!(can_have("jpg", dims) && can_have("mp4", dims) && !can_have("mp3", dims));
        assert!(can_have("mp3", length) && can_have("mkv", length) && !can_have("png", length));
        assert!(!can_have("txt", Columns { dims: true, length: true }) && !can_have("mp4", Columns::default()));
        assert!(worth_asking(&entry("a.flac", EntryKind::File, 0), length));
        // Never a folder (whatever its name), nor a cloud file that isn't on this PC.
        assert!(!worth_asking(&entry("trip.mp4", EntryKind::Folder, 0), length));
        let cloud = Entry { attributes: 0x400000, ..entry("film.mp4", EntryKind::File, 0) };
        assert!(!worth_asking(&cloud, length));
    }

    #[test]
    fn the_columns_only_show_where_something_can_have_them() {
        let all = |names: &[(&str, EntryKind)], listing: &str| {
            let entries: Vec<Entry> = names.iter().map(|&(n, k)| entry(n, k, 0)).collect();
            let view: Vec<u32> = (0..entries.len() as u32).collect();
            here(&entries, &view, listing)
        };
        let (folder, file) = (EntryKind::Folder, EntryKind::File);
        // Folders only (even one named like a video), or documents: neither column.
        assert_eq!(all(&[("Holiday", folder), ("trip.mp4", folder)], r"C:\x"), Columns::default());
        assert_eq!(all(&[("notes.txt", file), ("plan.pdf", file)], r"C:\x"), Columns::default());
        // A picture: Picture size. A song: Length. A video: both.
        assert_eq!(all(&[("notes.txt", file), ("cat.jpg", file)], r"C:\x"), Columns { dims: true, length: false });
        assert_eq!(all(&[("Music", folder), ("song.mp3", file)], r"C:\x"), Columns { dims: false, length: true });
        assert_eq!(all(&[("film.mkv", file)], r"C:\x"), Columns { dims: true, length: true });
        // Only what the view shows counts (a filter that hides the picture hides the column).
        let entries = vec![entry("notes.txt", file, 0), entry("cat.jpg", file, 0)];
        assert_eq!(here(&entries, &[0], r"C:\x"), Columns::default());
        // Nothing inside a zip is read, so nothing shows there.
        let dir = std::env::temp_dir().join(format!("glint-media-here-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let zip = dir.join("pack.zip");
        std::fs::write(&zip, b"").expect("write");
        assert_eq!(all(&[("cat.jpg", file)], &format!("{}\\pics", zip.display())), Columns::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_values_sort_last_whichever_way() {
        let entries = vec![
            entry("b.jpg", EntryKind::File, 0),
            entry("folder", EntryKind::Folder, 0),
            entry("a.jpg", EntryKind::File, 0),
            entry("big.jpg", EntryKind::File, 0),
            entry("small.jpg", EntryKind::File, 0),
            entry("notes.txt", EntryKind::File, 0),
        ];
        // Picture size by pixels in all: big 4000 x 3000, small 640 x 480; a, b and the text file unknown.
        let pixels = |w: u64, h: u64| Some((w * h, w));
        let values = vec![None, None, None, pixels(4000, 3000), pixels(640, 480), None];
        let sorted = |descending: bool, folders_first: bool| {
            let mut view: Vec<u32> = (0..entries.len() as u32).collect();
            folder::sort_view_by(&entries, &mut view, descending, folders_first, &values);
            view.iter().map(|&i| entries[i as usize].name.as_str()).collect::<Vec<_>>()
        };
        assert_eq!(sorted(true, true), vec!["folder", "big.jpg", "small.jpg", "a.jpg", "b.jpg", "notes.txt"]);
        assert_eq!(sorted(false, true), vec!["folder", "small.jpg", "big.jpg", "a.jpg", "b.jpg", "notes.txt"]);
        // Folders mixed in have no value either.
        assert_eq!(sorted(false, false), vec!["small.jpg", "big.jpg", "a.jpg", "b.jpg", "folder", "notes.txt"]);
        // Ties stay A to Z; a length sorts by itself; nothing known at all is A to Z.
        let same = vec![pixels(10, 10), None, pixels(10, 10), None, None, None];
        let mut view: Vec<u32> = vec![0, 2];
        folder::sort_view_by(&entries, &mut view, true, true, &same);
        assert_eq!(view, vec![2, 0]);
        assert_eq!(sort_value(SortKey::Length, &Facts { dims: Some((1, 1)), length: Some(5) }), Some((5, 0)));
        assert_eq!(sort_value(SortKey::Dimensions, &Facts { dims: None, length: Some(5) }), None);
        let mut view: Vec<u32> = (0..entries.len() as u32).collect();
        sort_view(&entries, &mut view, "C:\\nothing read here", SortKey::Length, true, false);
        assert_eq!(view.iter().map(|&i| entries[i as usize].name.as_str()).collect::<Vec<_>>(), vec!["a.jpg", "b.jpg", "big.jpg", "folder", "notes.txt", "small.jpg"]);
    }

    /// A tiny 24-bit BMP of `w` x `h` pixels.
    fn bmp(w: u32, h: u32) -> Vec<u8> {
        let row = (w * 3).div_ceil(4) * 4;
        let pixels = row * h;
        let mut b = Vec::new();
        b.extend_from_slice(b"BM");
        b.extend_from_slice(&(54 + pixels).to_le_bytes());
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(&54u32.to_le_bytes());
        b.extend_from_slice(&40u32.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24u16.to_le_bytes());
        b.extend_from_slice(&[0; 24]);
        b.extend(std::iter::repeat_n(0x80u8, pixels as usize));
        b
    }

    /// What the thread's `read` finds for one file.
    fn facts_of(path: &std::path::Path) -> Facts {
        let _com = ComGuard::init();
        let wic = thumbs::wic_factory();
        let picture = thumbs::is_picture(&folder::extension(&path.file_name().unwrap().to_string_lossy()));
        read(&path.to_string_lossy(), picture, wic.as_ref())
    }

    #[test]
    fn broken_empty_and_locked_files_have_no_size_and_do_not_crash() {
        let dir = std::env::temp_dir().join(format!("glint-media-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good = bmp(7, 5);
        let write = |name: &str, bytes: &[u8]| {
            let p = dir.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        };
        // Zero bytes, cut off in the header, plain junk, and gone.
        for (name, bytes) in [("zero.bmp", &[][..]), ("cut.bmp", &good[..20]), ("junk.png", b"this is not a picture at all"), ("junk.mp3", b"nope"), ("junk.mp4", &[0u8; 64][..])] {
            let f = facts_of(&write(name, bytes));
            assert_eq!(f.dims, None, "{name}");
            assert_eq!(f.length, None, "{name}");
        }
        assert_eq!(facts_of(&dir.join("gone.jpg")), Facts::default());
        // A header that claims 65535 x 65535 over a few bytes, and one that claims 0 x 0: no crash,
        // and a size of 0 is never shown.
        let mut huge = bmp(2, 2);
        huge[18..22].copy_from_slice(&65535u32.to_le_bytes());
        huge[22..26].copy_from_slice(&65535u32.to_le_bytes());
        let f = facts_of(&write("huge.bmp", &huge));
        assert!(f.dims.is_none_or(|(w, h)| w > 0 && h > 0));
        let mut zero = bmp(2, 2);
        zero[18..26].copy_from_slice(&[0; 8]);
        assert!(facts_of(&write("zero-size.bmp", &zero)).dims.is_none_or(|(w, h)| w > 0 && h > 0));
        // A wide one is shown as it is.
        let wide = facts_of(&write("wide.bmp", &bmp(2000, 1)));
        assert_eq!(wide.dims, Some((2000, 1)));
        assert_eq!(size_text(Some(&wide)), "2000 \u{d7} 1");
        // A picture with the wrong extension is still read as what it is (WIC looks at the bytes).
        assert_eq!(facts_of(&write("really-a-bmp.png", &good)).dims, Some((7, 5)));
        // A picture nobody may open: unknown, not a crash.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let p = write("locked.bmp", &good);
            let _lock = std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&p).unwrap();
            assert_eq!(facts_of(&p).dims, None);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_that_are_not_media_stay_empty_in_both_columns() {
        let both = Columns { dims: true, length: true };
        for name in ["notes.txt", "a.pdf", "b.exe", "noext", "archive.zip", "x.bmp.txt", "pic.", "dir.jpg.bak"] {
            assert!(!worth_asking(&entry(name, EntryKind::File, 0), both), "{name}");
        }
        // Extension case counts as the same.
        assert!(worth_asking(&entry("CAT.JPG", EntryKind::File, 0), Columns { dims: true, length: false }));
        assert!(worth_asking(&entry("song.MP3", EntryKind::File, 0), Columns { dims: false, length: true }));
        // Odd values.
        assert_eq!(length_text(Some(&Facts { dims: None, length: Some(0) })), "0:00");
        assert_eq!(length_text(Some(&Facts { dims: None, length: Some(u64::MAX) })), crate::details::duration_text(u64::MAX));
        assert_eq!(size_text(Some(&Facts { dims: Some((65535, 65535)), length: None })), "65535 \u{d7} 65535");
    }

    #[test]
    fn the_thread_reads_what_is_asked_and_keeps_it_by_last_change() {
        let _turn = test_turn();
        let dir = std::env::temp_dir().join(format!("glint-media-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("photo.bmp"), bmp(7, 5)).unwrap();
        let folder = dir.to_string_lossy().replace('/', "\\");
        let photo = entry("photo.bmp", EntryKind::File, 42);
        let ask = Ask::of(&folder, &photo);
        assert_eq!(ask.path, format!("{folder}\\photo.bmp"));
        want(9_001, &folder, vec![ask], Vec::new(), true, HWND::default());
        let started = Instant::now();
        while known(&folder, [&photo])[0].is_none() && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(20));
        }
        // (WIC reads the size from the header where the property system has none, as under Wine.)
        assert_eq!(known(&folder, [&photo])[0].and_then(|f| f.dims), Some((7, 5)));
        // Kept with the file's last change: once it changed, it's unknown until read again.
        let changed = Entry { modified: 43, ..photo.clone() };
        assert_eq!(known(&folder, [&changed]), vec![None]);
        // A folder no tab shows any more is forgotten.
        stop(9_001);
        keep(["C:\\somewhere else"]);
        assert_eq!(known(&folder, [&photo]), vec![None]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
