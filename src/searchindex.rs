//! The search index: what a search of a whole drive saw, kept in a file, so that the next search of
//! that drive answers at once (search.rs).
//!
//! - Only a search with Subfolders on, from a drive's root or from This PC, uses and makes one, and
//!   only for a local fixed drive with NTFS or ReFS on it (`place_for`): not removable, network or
//!   CD drives, nor a drive some other program makes up (cloud drives show other file systems).
//!   Nothing runs while nobody is searching: there is no service and no watching of the disk.
//! - The file is `search-index-C.dat` (one per drive letter) right in Glint's cache folder
//!   (settings.rs `search_index_file`), so Settings' Clear deletes it and it moves with Portable.
//! - A search reads it straight from the disk, item by item, and keeps only the matches; nothing of
//!   it stays in memory between searches. The usual walk through the drive still runs afterwards
//!   and brings the results up to date (search.rs), and writes a new file as it goes
//!   ([`Recorder`]): under a name of its own first, renamed over the old one only when the walk got
//!   through the whole drive. A walk cancelled or stopped at the result limit leaves the old file.
//! - The format: a header (`GLINTIDX`, the version, the drive's serial number and its root), then
//!   each folder the walk went into, with its path from the root and its items (name, attributes,
//!   size and Date modified), then the counts and a checksum of everything before it. Numbers are
//!   stored in as few bytes as they need. A file that ends early, doesn't add up, is of another
//!   version or belongs to another drive counts as no index.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW, WIN32_FIND_DATAW};

use crate::folder::{self, Entry, NameFilter};
use crate::shell::{self, ComGuard};

const MAGIC: &[u8; 8] = b"GLINTIDX";
/// Goes up whenever the format changes: a file of another version counts as no index.
const VERSION: u16 = 1;
/// What comes next in the file: a folder and its items, or the end.
const FOLDER: u8 = 1;
const END: u8 = 0;
/// Folders a walk may have listed before the file is written, at most (then the walk waits).
const QUEUE: usize = 256;
const ATTR_DIRECTORY: u32 = 0x10;
/// Windows' `DRIVE_FIXED`.
const FIXED_DRIVE: u32 = 3;

/// A drive that gets an index: its root ("C:\"), its serial number, and its index file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub root: String,
    pub volume: u32,
    pub file: PathBuf,
}

/// One item of a folder, as the index keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub name: String,
    pub attributes: u32,
    pub size: u64,
    /// Date modified, as a FILETIME.
    pub modified: u64,
}

/// One folder as a walk listed it: its full path, and every item in it.
pub struct Block {
    pub dir: String,
    pub items: Vec<Item>,
}

/// The index for a folder a search starts in: only a drive's root ("C:\") on a local fixed drive
/// with NTFS or ReFS. Touches the drive: call it on a background thread.
pub fn place_for(root: &str) -> Option<Place> {
    #[cfg(test)]
    if let Some(p) = TEST_PLACES.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|p| p.root == root) {
        return Some(p.clone());
    }
    let b = root.as_bytes();
    if !(b.len() == 3 && b[0].is_ascii_alphabetic() && &b[1..] == b":\\") {
        return None;
    }
    let w = folder::wide(root);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    if unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) } != FIXED_DRIVE {
        return None;
    }
    let mut serial = 0u32;
    let mut system = [0u16; 32];
    // SAFETY: `w` outlives the call; valid out-pointer and buffer.
    unsafe { GetVolumeInformationW(PCWSTR(w.as_ptr()), None, Some(&mut serial), None, None, Some(&mut system)) }.ok()?;
    let len = system.iter().position(|&c| c == 0).unwrap_or(system.len());
    let system = String::from_utf16_lossy(&system[..len]);
    if !(system.eq_ignore_ascii_case("NTFS") || system.eq_ignore_ascii_case("ReFS")) {
        return None;
    }
    let letter = b[0].to_ascii_uppercase() as char;
    Some(Place { root: root.to_string(), volume: serial, file: crate::settings::search_index_file(&crate::settings::folders().cache, letter) })
}

/// Places for test folders (`place_for` gives them for their root), so tests can use an index
/// without a drive of their own.
#[cfg(test)]
pub static TEST_PLACES: std::sync::Mutex<Vec<Place>> = std::sync::Mutex::new(Vec::new());

/// Whether `dir` is `root` or inside it.
fn under(dir: &str, root: &str) -> bool {
    dir.strip_prefix(root).is_some_and(|rest| rest.is_empty() || root.ends_with('\\') || rest.starts_with('\\'))
}

// ---------------------------------------------------------------- reading

/// The index's answer for a search: the items of `place` the filter keeps, by the same rules as
/// the walk (hidden items only with `show_hidden`, nothing inside folders that don't show), at
/// most `limit`. None when there is no index that can be trusted (or the search was cancelled).
/// Words in quotes aren't looked for here (search.rs leaves the index out then).
pub fn answer(place: &Place, filter: &NameFilter, show_hidden: bool, show_system: bool, cancel: &AtomicBool, limit: usize) -> Option<Vec<Entry>> {
    // Type names ("Text Document") come from the shell, which needs COM.
    let _com = ComGuard::init();
    let mut types: HashMap<String, Arc<str>> = HashMap::new();
    let mut found = Vec::new();
    // The folders that don't show (and everything in them): a folder comes after the one it's in.
    let mut unseen: HashSet<String> = HashSet::new();
    read(place, cancel, &mut |dir, items| {
        let hidden_here = unseen.contains(dir);
        for it in items {
            let is_dir = it.attributes & ATTR_DIRECTORY != 0;
            if hidden_here || !folder::visible(it.attributes, show_hidden, show_system) {
                if is_dir {
                    unseen.insert(folder::join(dir, &it.name));
                }
                continue;
            }
            if found.len() >= limit {
                continue;
            }
            let lower = it.name.to_lowercase();
            if !filter.keeps(&lower, || folder::join(dir, &it.name)) {
                continue;
            }
            let Some(mut e) = entry_of(it, &mut types) else { continue };
            if filter.fits(&e) {
                e.target = Some(folder::join(dir, &it.name));
                found.push(e);
            }
        }
    })?;
    Some(found)
}

/// An item of the index as the list shows it (as folder.rs `entry_from` makes it from a listing).
fn entry_of(it: &Item, types: &mut HashMap<String, Arc<str>>) -> Option<Entry> {
    let mut data = WIN32_FIND_DATAW { dwFileAttributes: it.attributes, nFileSizeHigh: (it.size >> 32) as u32, nFileSizeLow: it.size as u32, ..Default::default() };
    data.ftLastWriteTime = FILETIME { dwLowDateTime: it.modified as u32, dwHighDateTime: (it.modified >> 32) as u32 };
    let name: Vec<u16> = it.name.encode_utf16().collect();
    // Room for the name and its NUL (a longer one can't be on the disk).
    if name.len() >= data.cFileName.len() {
        return None;
    }
    data.cFileName[..name.len()].copy_from_slice(&name);
    folder::entry_from(&data, types)
}

/// Goes through the index file of `place`, calling `each` with every folder's full path and its
/// items, in the order the walk listed them (a folder after the one it is in). None when there is
/// no file, it is damaged, of another version or another drive's, or the search was cancelled -
/// also when that only shows at the end, so the caller keeps what it found until then.
pub fn read(place: &Place, cancel: &AtomicBool, each: &mut dyn FnMut(&str, &[Item])) -> Option<()> {
    let file = File::open(&place.file).ok()?;
    let mut r = Reader { inner: BufReader::with_capacity(1 << 18, file), hash: Hash::new() };
    let mut magic = [0u8; 8];
    r.bytes(&mut magic)?;
    if &magic != MAGIC || r.u16()? != VERSION || r.u32()? != place.volume || r.text()? != place.root {
        return None;
    }
    let (mut folders, mut items) = (0u64, 0u64);
    let mut list = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match r.u8()? {
            FOLDER => {
                let rel = r.text()?;
                let count = r.number()?;
                list.clear();
                for _ in 0..count {
                    let name = r.text()?;
                    if name.is_empty() || name.contains('\\') {
                        return None;
                    }
                    let attributes = u32::try_from(r.number()?).ok()?;
                    let size = r.number()?;
                    let modified = r.u64()?;
                    list.push(Item { name, attributes, size, modified });
                }
                let dir = if rel.is_empty() { place.root.clone() } else { folder::join(&place.root, &rel) };
                each(&dir, &list);
                folders += 1;
                items += count;
            }
            END => {
                let ok = r.number()? == folders && r.number()? == items;
                let sum = r.hash.0;
                // The checksum, and nothing after it.
                return (ok && r.u64()? == sum && r.inner.read(&mut [0u8; 1]).ok()? == 0).then_some(());
            }
            _ => return None,
        }
    }
}

/// Reads the index's numbers and text, adding every byte to the checksum.
struct Reader {
    inner: BufReader<File>,
    hash: Hash,
}

impl Reader {
    fn bytes(&mut self, buf: &mut [u8]) -> Option<()> {
        self.inner.read_exact(buf).ok()?;
        self.hash.add(buf);
        Some(())
    }
    fn u8(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        self.bytes(&mut b)?;
        Some(b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        let mut b = [0u8; 2];
        self.bytes(&mut b)?;
        Some(u16::from_le_bytes(b))
    }
    fn u32(&mut self) -> Option<u32> {
        let mut b = [0u8; 4];
        self.bytes(&mut b)?;
        Some(u32::from_le_bytes(b))
    }
    fn u64(&mut self) -> Option<u64> {
        let mut b = [0u8; 8];
        self.bytes(&mut b)?;
        Some(u64::from_le_bytes(b))
    }
    /// A number written by `put_number`.
    fn number(&mut self) -> Option<u64> {
        let mut n = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            n |= u64::from(b & 0x7F).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(n);
            }
        }
        None
    }
    /// Text written by `put_text`: no longer than a path can be.
    fn text(&mut self) -> Option<String> {
        let len = usize::try_from(self.number()?).ok().filter(|&n| n <= 32_767 * 3)?;
        let mut buf = vec![0u8; len];
        self.bytes(&mut buf)?;
        String::from_utf8(buf).ok()
    }
}

/// FNV-1a: a quick checksum, to tell a damaged file.
struct Hash(u64);

impl Hash {
    fn new() -> Hash {
        Hash(0xCBF2_9CE4_8422_2325)
    }
    fn add(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01B3);
        }
    }
}

// ---------------------------------------------------------------- writing

/// Writes new index files while a walk goes through drives: the walk's threads hand it every
/// folder they listed ([`Tap`]), and [`Recorder::finish`] says whether the walk got through.
pub struct Recorder {
    tap: Tap,
    keep: Arc<AtomicBool>,
}

/// What a walk's threads hand their folders to. Cloned for each thread.
#[derive(Clone)]
pub struct Tap {
    tx: SyncSender<Block>,
    roots: Arc<[String]>,
}

impl Tap {
    /// Whether the folder is on a drive that gets an index (only then are its items gathered).
    pub fn wants(&self, dir: &str) -> bool {
        self.roots.iter().any(|r| under(dir, r))
    }

    /// A folder and all its items, once it has been listed to the end.
    pub fn send(&self, block: Block) {
        let _ = self.tx.send(block);
    }
}

impl Recorder {
    /// Starts writing new index files for `places` on a thread of its own. None for no places
    /// (or no thread).
    pub fn start(places: Vec<Place>) -> Option<Recorder> {
        if places.is_empty() {
            return None;
        }
        let (tx, rx) = sync_channel(QUEUE);
        let keep = Arc::new(AtomicBool::new(false));
        let roots: Arc<[String]> = places.iter().map(|p| p.root.clone()).collect();
        let k = keep.clone();
        // The new files are made here, so they're there before the walk starts.
        let outs = places.iter().map(Out::create).collect();
        shell::background("search index", move || write_all(&places, outs, rx, &k)).then_some(Recorder { tap: Tap { tx, roots }, keep })
    }

    pub fn tap(&self) -> &Tap {
        &self.tap
    }

    /// The walk is over: `keep` when it went through everything, and the new files take the old
    /// ones' place; otherwise they're thrown away and the old ones stay. (The files are finished
    /// once the walk's threads have all stopped.)
    pub fn finish(self, keep: bool) {
        self.keep.store(keep, Ordering::SeqCst);
    }
}

/// The writing thread: every folder into its drive's new file, then (all walk threads done) each
/// file renamed into place, or thrown away.
fn write_all(places: &[Place], mut outs: Vec<Option<Out>>, rx: Receiver<Block>, keep: &AtomicBool) {
    for block in rx {
        let Some(i) = places.iter().position(|p| under(&block.dir, &p.root)) else { continue };
        let ok = outs[i].as_mut().is_some_and(|out| out.folder(&places[i].root, &block).is_ok());
        if !ok {
            // Couldn't write (a full disk): no new index for this drive this time.
            if let Some(out) = outs[i].take() {
                out.discard();
            }
        }
    }
    let keep = keep.load(Ordering::SeqCst);
    for (out, place) in outs.into_iter().zip(places) {
        if let Some(out) = out {
            if keep {
                out.commit(&place.file);
            } else {
                out.discard();
            }
        }
    }
}

/// A new index file being written, under a name of its own next to where it goes.
struct Out {
    temp: PathBuf,
    w: BufWriter<File>,
    hash: Hash,
    folders: u64,
    items: u64,
}

/// Tells apart the files of searches that write at the same time.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Deletes the half-written files (`stem.<pid>-<n>.tmp`) a Glint that ended during a walk left in
/// `dir`: those of a Glint that isn't running any more, or a day old (its number may be another
/// program's by now). Another Glint's walk going on now keeps its own.
fn remove_left_behind(dir: &Path, stem: &str) {
    let Ok(listing) = std::fs::read_dir(dir) else { return };
    let mine = std::process::id();
    let day = std::time::Duration::from_secs(24 * 3600);
    for entry in listing.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix(stem).and_then(|r| r.strip_prefix('.')).and_then(|r| r.strip_suffix(".tmp")).and_then(|r| r.split_once('-')).and_then(|(pid, _)| pid.parse::<u32>().ok()) else {
            continue;
        };
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > day);
        if pid != mine && (old || !running(pid)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Whether a program with this process number is running.
fn running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: a query on a process handle we open and close here.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let alive = GetExitCodeProcess(process, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(process);
        alive
    }
}

impl Out {
    fn create(place: &Place) -> Option<Out> {
        let dir = place.file.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        let stem = place.file.file_name()?.to_string_lossy();
        remove_left_behind(dir, &stem);
        let temp = dir.join(format!("{stem}.{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let file = File::create(&temp).ok()?;
        let mut out = Out { temp, w: BufWriter::with_capacity(1 << 18, file), hash: Hash::new(), folders: 0, items: 0 };
        let made = out.put(MAGIC).and_then(|_| out.put(&VERSION.to_le_bytes())).and_then(|_| out.put(&place.volume.to_le_bytes())).and_then(|_| out.put_text(&place.root));
        if made.is_err() {
            out.discard();
            return None;
        }
        Some(out)
    }

    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.hash.add(bytes);
        self.w.write_all(bytes)
    }

    /// A number in as few bytes as it needs: 7 bits a byte, the top bit set while more follow.
    fn put_number(&mut self, mut n: u64) -> std::io::Result<()> {
        let mut buf = [0u8; 10];
        let mut len = 0;
        loop {
            let b = (n & 0x7F) as u8;
            n >>= 7;
            buf[len] = if n == 0 { b } else { b | 0x80 };
            len += 1;
            if n == 0 {
                break;
            }
        }
        self.put(&buf[..len])
    }

    fn put_text(&mut self, text: &str) -> std::io::Result<()> {
        self.put_number(text.len() as u64)?;
        self.put(text.as_bytes())
    }

    /// A folder (its path from `root`) and its items.
    fn folder(&mut self, root: &str, block: &Block) -> std::io::Result<()> {
        let rel = block.dir.strip_prefix(root).unwrap_or(&block.dir).trim_start_matches('\\');
        self.put(&[FOLDER])?;
        self.put_text(rel)?;
        self.put_number(block.items.len() as u64)?;
        for it in &block.items {
            self.put_text(&it.name)?;
            self.put_number(u64::from(it.attributes))?;
            self.put_number(it.size)?;
            self.put(&it.modified.to_le_bytes())?;
        }
        self.folders += 1;
        self.items += block.items.len() as u64;
        Ok(())
    }

    /// Ends the file and puts it in place of the old one (thrown away if that fails).
    fn commit(mut self, to: &Path) {
        let ended = (|| {
            self.put(&[END])?;
            self.put_number(self.folders)?;
            self.put_number(self.items)?;
            let sum = self.hash.0;
            self.w.write_all(&sum.to_le_bytes())?;
            self.w.flush()
        })();
        let Out { temp, w, .. } = self;
        drop(w);
        // Another search may be reading the old file just then (the reader lets it be replaced,
        // but an older Windows may not): tried again for up to half a minute, here on the writer's
        // own thread, so a long read of a big drive's index doesn't cost the new one.
        let mut tries = 0;
        let placed = ended.is_ok()
            && loop {
                if std::fs::rename(&temp, to).is_ok() {
                    break true;
                }
                tries += 1;
                if tries == 600 {
                    break false;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            };
        if !placed {
            let _ = std::fs::remove_file(&temp);
        }
    }

    fn discard(self) {
        let Out { temp, w, .. } = self;
        drop(w);
        let _ = std::fs::remove_file(&temp);
    }
}

/// Writes an index of `blocks` for `place` at once (for tests here and in search.rs).
#[cfg(test)]
pub fn write_now(place: &Place, blocks: Vec<Block>, keep: bool) {
    let (tx, rx) = sync_channel(QUEUE);
    let flag = AtomicBool::new(keep);
    let writer = {
        let places = vec![place.clone()];
        let outs = places.iter().map(Out::create).collect();
        std::thread::spawn(move || write_all(&places, outs, rx, &flag))
    };
    for b in blocks {
        tx.send(b).unwrap();
    }
    drop(tx);
    writer.join().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder under the temporary folder for one test, removed afterwards: `root` is the "drive",
    /// `cache` holds its index.
    struct Spot {
        dir: PathBuf,
        place: Place,
    }

    impl Spot {
        fn new(name: &str) -> Spot {
            let dir = std::env::temp_dir().join(format!("glint-index-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("cache")).unwrap();
            let root = dir.join("root").to_string_lossy().replace('/', "\\");
            let file = crate::settings::search_index_file(&dir.join("cache"), 'T');
            Spot { place: Place { root, volume: 0x1234_5678, file }, dir }
        }
        fn files_in_cache(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.dir.join("cache")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        }
    }

    impl Drop for Spot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn item(name: &str, attributes: u32, size: u64) -> Item {
        Item { name: name.to_string(), attributes, size, modified: 133_000_000_000_000_000u64.wrapping_add(size) }
    }

    fn all(place: &Place) -> Option<Vec<(String, Vec<Item>)>> {
        let mut v = Vec::new();
        read(place, &AtomicBool::new(false), &mut |dir, items| v.push((dir.to_string(), items.to_vec())))?;
        Some(v)
    }

    fn odd_blocks(root: &str) -> Vec<Block> {
        let long = "ñ".repeat(150); // 300 characters
        vec![
            Block { dir: root.to_string(), items: vec![item("İstanbul 🌉 #1.txt", 0x20, 12), item("sub", ATTR_DIRECTORY, 0), item(&long, 0x20, u64::MAX)] },
            Block { dir: format!("{root}\\sub"), items: vec![] },
            Block { dir: format!("{root}\\sub\\{long}"), items: vec![item("a#b", 0x2 | 0x20, 1 << 40)] },
        ]
    }

    #[test]
    fn writes_and_reads_back_odd_names_and_long_paths() {
        let s = Spot::new("odd");
        let root = s.place.root.clone();
        write_now(&s.place, odd_blocks(&root), true);
        let back = all(&s.place).expect("an index");
        let long = "ñ".repeat(150);
        assert_eq!(back.len(), 3);
        assert_eq!(back[0].0, root);
        assert_eq!(back[0].1, odd_blocks(&root)[0].items);
        assert_eq!(back[2].0, format!("{root}\\sub\\{long}"), "300 characters and more");
        assert_eq!(back[2].1, vec![item("a#b", 0x22, 1 << 40)]);
        assert_eq!(s.files_in_cache(), ["search-index-T.dat"], "no temporary file left");
        // Written again: replaced, the same.
        write_now(&s.place, odd_blocks(&root), true);
        assert_eq!(all(&s.place).unwrap().len(), 3);
        assert_eq!(s.files_in_cache(), ["search-index-T.dat"]);
    }

    #[test]
    fn two_hundred_thousand_items_stay_compact() {
        let s = Spot::new("many");
        let root = s.place.root.clone();
        let blocks: Vec<Block> = (0..200)
            .map(|f| Block { dir: format!("{root}\\folder {f}"), items: (0..1000).map(|i| item(&format!("file {i} of folder {f}.txt"), 0x20, i * 1000)).collect() })
            .collect();
        write_now(&s.place, blocks, true);
        let len = std::fs::metadata(&s.place.file).unwrap().len();
        assert!(len < 200_000 * 45, "{len} bytes for 200,000 items");
        let mut count = 0;
        read(&s.place, &AtomicBool::new(false), &mut |_, items| count += items.len()).expect("an index");
        assert_eq!(count, 200_000);
        // A search through it keeps at most the limit.
        let f = NameFilter::new("file");
        assert_eq!(answer(&s.place, &f, false, false, &AtomicBool::new(false), 500).unwrap().len(), 500);
        let f = NameFilter::new("regex:^file\\s999\\sof\\sfolder\\s1\\d\\.txt$");
        assert_eq!(answer(&s.place, &f, false, false, &AtomicBool::new(false), 20_000).unwrap().len(), 10);
    }

    #[test]
    fn a_damaged_or_foreign_file_is_no_index() {
        let s = Spot::new("damaged");
        let root = s.place.root.clone();
        write_now(&s.place, odd_blocks(&root), true);
        let good = std::fs::read(&s.place.file).unwrap();
        assert!(all(&s.place).is_some());
        // Cut short anywhere: no index.
        for cut in [0, 5, 8, 20, good.len() / 2, good.len() - 9, good.len() - 1] {
            std::fs::write(&s.place.file, &good[..cut]).unwrap();
            assert!(all(&s.place).is_none(), "cut at {cut}");
        }
        // One byte changed, or one more at the end.
        let mut bad = good.clone();
        bad[good.len() / 2] ^= 0x01;
        std::fs::write(&s.place.file, &bad).unwrap();
        assert!(all(&s.place).is_none());
        let mut longer = good.clone();
        longer.push(0);
        std::fs::write(&s.place.file, &longer).unwrap();
        assert!(all(&s.place).is_none());
        // Another version.
        let mut old = good.clone();
        old[8] = 0;
        std::fs::write(&s.place.file, &old).unwrap();
        assert!(all(&s.place).is_none());
        // Another drive: another serial number or root.
        std::fs::write(&s.place.file, &good).unwrap();
        assert!(all(&Place { volume: 1, ..s.place.clone() }).is_none());
        assert!(all(&Place { root: "D:\\".into(), ..s.place.clone() }).is_none());
        // Garbage, an empty file, no file.
        std::fs::write(&s.place.file, b"GLINTIDX\x01\x00 hello").unwrap();
        assert!(all(&s.place).is_none());
        std::fs::write(&s.place.file, b"").unwrap();
        assert!(all(&s.place).is_none());
        std::fs::remove_file(&s.place.file).unwrap();
        assert!(all(&s.place).is_none());
        // A cancelled read is no answer either.
        std::fs::write(&s.place.file, &good).unwrap();
        assert!(read(&s.place, &AtomicBool::new(true), &mut |_, _| {}).is_none());
    }

    #[test]
    fn a_walk_that_doesnt_finish_leaves_the_old_index() {
        let s = Spot::new("cancel");
        let root = s.place.root.clone();
        write_now(&s.place, odd_blocks(&root), true);
        let before = std::fs::read(&s.place.file).unwrap();
        // Half a walk, then cancelled: the old file stays as it was, and no temporary file.
        write_now(&s.place, vec![Block { dir: root.clone(), items: vec![item("new.txt", 0x20, 1)] }], false);
        assert_eq!(std::fs::read(&s.place.file).unwrap(), before);
        assert_eq!(s.files_in_cache(), ["search-index-T.dat"]);
        // Through the Recorder, as search.rs uses it.
        let r = Recorder::start(vec![s.place.clone()]).unwrap();
        assert_eq!(s.files_in_cache().len(), 2, "the new file, under a name of its own");
        let tap = r.tap().clone();
        assert!(tap.wants(&root) && tap.wants(&format!("{root}\\x")) && !tap.wants(&format!("{root}x")) && !tap.wants("C:\\other"));
        tap.send(Block { dir: root.clone(), items: vec![item("new.txt", 0x20, 1)] });
        r.finish(false);
        drop(tap);
        let wait = std::time::Instant::now();
        while s.files_in_cache().len() > 1 && wait.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(std::fs::read(&s.place.file).unwrap(), before);
        assert_eq!(s.files_in_cache(), ["search-index-T.dat"]);
        // A finished one takes its place.
        let r = Recorder::start(vec![s.place.clone()]).unwrap();
        r.tap().send(Block { dir: root.clone(), items: vec![item("new.txt", 0x20, 1)] });
        r.finish(true);
        let wait = std::time::Instant::now();
        while std::fs::read(&s.place.file).unwrap() == before && wait.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let back = all(&s.place).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].1[0].name, "new.txt");
    }

    #[test]
    fn answers_follow_the_rules_of_the_walk() {
        let s = Spot::new("rules");
        let root = s.place.root.clone();
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        let blocks = vec![
            Block { dir: root.clone(), items: vec![item("report.txt", 0x20, 10), item("secret report.txt", HIDDEN, 10), item("Hidden", ATTR_DIRECTORY | HIDDEN, 0), item("os report", ATTR_DIRECTORY | HIDDEN | SYSTEM, 0)] },
            Block { dir: format!("{root}\\Hidden"), items: vec![item("report inside.txt", 0x20, 10), item("deeper", ATTR_DIRECTORY, 0)] },
            Block { dir: format!("{root}\\Hidden\\deeper"), items: vec![item("report deep.txt", 0x20, 5000)] },
        ];
        write_now(&s.place, blocks, true);
        let names = |text: &str, hidden: bool, system: bool| {
            let mut v: Vec<String> = answer(&s.place, &NameFilter::new(text), hidden, system, &AtomicBool::new(false), 100).unwrap().into_iter().map(|e| e.target.unwrap()).collect();
            v.sort();
            v
        };
        assert_eq!(names("report", false, false), [format!("{root}\\report.txt")], "nothing hidden, nor inside a hidden folder");
        assert_eq!(names("report", true, false), [format!("{root}\\Hidden\\deeper\\report deep.txt"), format!("{root}\\Hidden\\report inside.txt"), format!("{root}\\report.txt"), format!("{root}\\secret report.txt")]);
        assert_eq!(names("report", true, true).len(), 5, "protected files only with both");
        assert_eq!(names("report size:>1kb", true, false), [format!("{root}\\Hidden\\deeper\\report deep.txt")]);
        assert_eq!(names("*.txt -secret -deep", true, false), [format!("{root}\\Hidden\\report inside.txt"), format!("{root}\\report.txt")]);
        let e = &answer(&s.place, &NameFilter::new("report.txt"), false, false, &AtomicBool::new(false), 10).unwrap()[0];
        assert_eq!((e.name.as_str(), e.size, e.modified, e.kind), ("report.txt", 10, 133_000_000_000_000_010, folder::EntryKind::File));
    }

    /// A half-written index a Glint that isn't running any more left goes when the next walk
    /// starts one; this Glint's own, the index itself and other files stay.
    #[test]
    fn half_written_files_left_behind_are_deleted() {
        let dir = std::env::temp_dir().join(format!("glint-index-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let stem = "search-index-T.dat";
        let mine = format!("{stem}.{}-0.tmp", std::process::id());
        let names = [stem.to_string(), mine.clone(), format!("{stem}.4000000000-7.tmp"), "search-index-U.dat.4000000000-1.tmp".to_string(), "notes.tmp".to_string()];
        for n in &names {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        remove_left_behind(&dir, stem);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        let mut want = vec![stem.to_string(), mine, "search-index-U.dat.4000000000-1.tmp".to_string(), "notes.tmp".to_string()];
        want.sort();
        assert_eq!(left, want, "only the other Glint's half-written file of this drive goes");
        assert!(running(std::process::id()) && !running(4_000_000_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_drive_root_gets_an_index() {
        assert!(place_for("C:\\Users").is_none() && place_for("\\\\server\\share").is_none() && place_for("").is_none() && place_for("C:").is_none());
        assert!(under("C:\\a", "C:\\") && under("C:\\", "C:\\") && !under("D:\\a", "C:\\"));
        if let Some(p) = place_for("C:\\") {
            assert_eq!(p.file, crate::settings::search_index_file(&crate::settings::folders().cache, 'C'));
        }
    }
}
