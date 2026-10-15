//! Finding duplicate files: files under a folder (and its subfolders) with exactly the same
//! content, whatever their names. The rules for what is looked at are the subfolder search's:
//! - hidden items only when hidden files are shown, and nothing inside hidden folders then either;
//! - junctions and symbolic links are never followed, and files that are links or stand-ins for
//!   cloud files are left alone (reading one could download it);
//! - empty files aren't duplicates of anything worth showing, so they're skipped;
//! - files and folders that can't be read are skipped without a word.
//!
//! Comparing every file with every other would take forever, so it narrows down step by step, each
//! step only for what's left after the one before:
//! 1. the size: only files with a size twin can be duplicates (most files have none);
//! 2. a hash of the first 64 KB, which tells most different files of the same size apart quickly;
//! 3. a hash of the whole content;
//! 4. a byte-by-byte comparison, so two different files that happen to hash alike are never called
//!    duplicates.
//!
//! Two names of the same file (hard links) count once: deleting one frees no space.
//!
//! Memory stays small on a whole drive too. The folders are gone through twice: the first time
//! only notes which sizes turn up more than once (in a fixed 4 MB table), the second keeps just
//! the files with such a size, and at most [`MAX_CANDIDATES`] of them. If more files than that
//! have a size twin, the smallest ones are left out (the biggest waste space the most), and the
//! summary says from which size on files were compared.
//!
//! Each group of duplicates goes out as soon as it's confirmed, biggest files first. Call this
//! from a background thread with COM (shell::ComGuard), since the entries get their type names
//! from the shell.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::windows::io::AsRawHandle;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    FileIdInfo, FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, GetFileInformationByHandle, GetFileInformationByHandleEx,
    BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FIND_FIRST_EX_FLAGS, WIN32_FIND_DATAW,
};

use crate::folder::{self, Entry};
use crate::search;

/// Files with a size twin kept at most (about 70 bytes each, so a few MB).
pub const MAX_CANDIDATES: usize = 100_000;
/// How much of each file the quick hash reads.
const HEAD: usize = 64 * 1024;
/// Files are read this much at a time.
const CHUNK: usize = 256 * 1024;
/// Bits in each of the two size tables (2 MB each). A size that only looks like it was seen twice
/// (two sizes landing on the same bit) just costs a little memory in the second pass.
const SIZE_BITS: usize = 1 << 24;

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;

/// What a finished search found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Sets of identical files.
    pub groups: usize,
    /// Files in all of them.
    pub files: usize,
    /// What deleting all but one of each set would free.
    pub wasted_bytes: u64,
    /// Files looked at (not empty, not left alone for the reasons above).
    pub scanned_files: usize,
    /// Files smaller than this weren't compared, because too many files had a size twin (0 when
    /// all were).
    pub smallest_compared: u64,
}

/// A file with a size twin, waiting to be compared: just enough to find it again.
struct Candidate {
    /// Index into the folders of the second pass.
    dir: u32,
    name: Box<str>,
    size: u64,
}

/// Finds files under `root` with exactly the same content (see the top of this file). Calls
/// `group` with each set of two or more identical files as soon as it's confirmed: entries like
/// the subfolder search's (`target` is the full path), in path order. `group` returns false to
/// stop. Returns None when cancelled or stopped.
pub fn find(root: &str, show_hidden: bool, show_system: bool, cancel: &AtomicBool, group: &mut dyn FnMut(Vec<Entry>) -> bool) -> Option<Summary> {
    let mut summary = Summary::default();

    // First pass: which sizes turn up more than once, and roughly how many files have them.
    let mut once = vec![0u64; SIZE_BITS / 64];
    let mut twice = vec![0u64; SIZE_BITS / 64];
    // Files with a twin by size class (2^n bytes up to 2^(n+1)), to pick the smallest size compared.
    let mut by_class = [0usize; 64];
    walk(root, show_hidden, show_system, cancel, &mut |_, data| {
        let Some(size) = comparable(data) else { return };
        summary.scanned_files += 1;
        let (word, bit) = size_bit(size);
        let class = 63 - size.leading_zeros() as usize;
        if twice[word] & bit != 0 {
            by_class[class] += 1;
        } else if once[word] & bit != 0 {
            twice[word] |= bit;
            by_class[class] += 2;
        } else {
            once[word] |= bit;
        }
    })?;
    drop(once);
    let mut smallest = 0u64;
    let mut total = 0usize;
    for class in (0..64).rev() {
        total += by_class[class];
        if total > MAX_CANDIDATES {
            smallest = 1u64 << (class + 1).min(63);
            break;
        }
    }
    summary.smallest_compared = smallest;

    // Second pass: keep the files whose size was seen twice.
    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<Candidate> = Vec::new();
    walk(root, show_hidden, show_system, cancel, &mut |dir, data| {
        let Some(size) = comparable(data) else { return };
        let (word, bit) = size_bit(size);
        if size < smallest || twice[word] & bit == 0 || files.len() >= MAX_CANDIDATES {
            return;
        }
        if dirs.last().map(String::as_str) != Some(dir) {
            dirs.push(dir.to_string());
        }
        files.push(Candidate { dir: (dirs.len() - 1) as u32, name: name_of(data).into(), size });
    })?;
    drop(twice);

    // Biggest first; the same size together.
    files.sort_unstable_by_key(|f| std::cmp::Reverse(f.size));
    let mut buffers = Buffers { a: vec![0u8; CHUNK], b: vec![0u8; CHUNK] };
    let mut types = HashMap::new();
    let mut start = 0;
    while start < files.len() {
        let size = files[start].size;
        let end = start + files[start..].iter().take_while(|f| f.size == size).count();
        if end - start >= 2 {
            let paths: Vec<String> = files[start..end].iter().map(|f| folder::join(&dirs[f.dir as usize], &f.name)).collect();
            for same in same_content(paths, size, &mut buffers, cancel)? {
                let mut entries: Vec<Entry> = same.iter().filter_map(|p| entry_at(p, &mut types)).collect();
                if entries.len() < 2 {
                    continue;
                }
                entries.sort_by_cached_key(|e| e.target.as_deref().unwrap_or_default().to_lowercase());
                summary.groups += 1;
                summary.files += entries.len();
                summary.wasted_bytes += size * (entries.len() as u64 - 1);
                if cancel.load(Ordering::SeqCst) || !group(entries) {
                    return None;
                }
            }
        }
        start = end;
    }
    Some(summary)
}

/// The size of a file worth comparing, or None (a folder, empty, a link, a cloud stand-in).
fn comparable(data: &WIN32_FIND_DATAW) -> Option<u64> {
    let attrs = data.dwFileAttributes;
    if attrs & (ATTR_DIRECTORY | ATTR_REPARSE_POINT | crate::peek::NOT_HERE) != 0 {
        return None;
    }
    let size = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
    (size > 0).then_some(size)
}

/// Where a size lands in the size tables: (word, bit).
fn size_bit(size: u64) -> (usize, u64) {
    // Mixed, so sizes that are near each other (or all multiples of 4 KB) spread over the table.
    let mut h = size.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    let i = (h as usize) & (SIZE_BITS - 1);
    (i / 64, 1u64 << (i % 64))
}

fn name_of(data: &WIN32_FIND_DATAW) -> String {
    let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
    String::from_utf16_lossy(&data.cFileName[..len])
}

/// Goes through `root` and its subfolders (deep first, which keeps the list of folders still to
/// go short), calling `each` with every visible item and its folder. None when cancelled.
fn walk(root: &str, show_hidden: bool, show_system: bool, cancel: &AtomicBool, each: &mut dyn FnMut(&str, &WIN32_FIND_DATAW)) -> Option<()> {
    let mut folders = vec![root.to_string()];
    while let Some(dir) = folders.pop() {
        let mut subs = Vec::new();
        let listed = search::list(&dir, cancel, &mut |data: &WIN32_FIND_DATAW| {
            let attrs = data.dwFileAttributes;
            if !folder::visible(attrs, show_hidden, show_system) {
                return true;
            }
            if attrs & ATTR_DIRECTORY != 0 {
                let name = name_of(data);
                if !search::is_link(data) && name != "." && name != ".." {
                    subs.push(folder::join(&dir, &name));
                }
                return true;
            }
            each(&dir, data);
            true
        });
        if !listed {
            return None;
        }
        // Reversed, so they're gone through in the order they were listed.
        folders.extend(subs.into_iter().rev());
    }
    Some(())
}

/// Two read buffers, made once for the whole search.
struct Buffers {
    a: Vec<u8>,
    b: Vec<u8>,
}

/// Which files of `paths` (all `size` bytes) have the same content: the sets of two or more.
/// None when cancelled.
fn same_content(paths: Vec<String>, size: u64, buf: &mut Buffers, cancel: &AtomicBool) -> Option<Vec<Vec<String>>> {
    // The first 64 KB, and which file each one is (two names of one file count once).
    let mut heads: Vec<(u64, Option<(u64, u128)>, String)> = Vec::with_capacity(paths.len());
    for path in paths {
        let Ok(mut f) = File::open(&path) else { continue };
        let id = file_id(&f);
        let want = HEAD.min(size as usize);
        match fill(&mut f, &mut buf.a[..want], cancel)? {
            n if n == want => heads.push((hash(&buf.a[..want]), id, path)),
            _ => continue, // shorter than it was a moment ago
        }
    }
    heads.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase())));
    // Only the same content under the same known id is one file.
    heads.dedup_by(|b, a| a.0 == b.0 && a.1.is_some() && a.1 == b.1);

    let mut sets = Vec::new();
    for run in runs(&heads, |h| h.0) {
        if run.len() < 2 {
            continue;
        }
        let paths: Vec<&str> = run.iter().map(|h| h.2.as_str()).collect();
        // A small file's head is all of it; a bigger one gets a hash of the whole content.
        let mut whole: Vec<(u64, &str)> = Vec::with_capacity(paths.len());
        if size as usize <= HEAD {
            whole.extend(paths.iter().map(|p| (0, *p)));
        } else {
            for p in paths {
                if let Some(h) = hash_file(p, size, &mut buf.a, cancel)? {
                    whole.push((h, p));
                }
            }
            whole.sort_unstable_by_key(|w| w.0);
        }
        for run in runs(&whole, |w| w.0) {
            let mut left: Vec<&str> = run.iter().map(|w| w.1).collect();
            // Byte by byte against the first; any that differ (a hash that matched by chance)
            // are compared among themselves again.
            while left.len() >= 2 {
                let first = left[0];
                let mut same = vec![first.to_string()];
                let mut other = Vec::new();
                for &p in &left[1..] {
                    match equal(first, p, buf, cancel)? {
                        true => same.push(p.to_string()),
                        false => other.push(p),
                    }
                }
                if same.len() >= 2 {
                    // In path order, the same every time (which copy counts as first mustn't change).
                    same.sort_by_cached_key(|p| p.to_lowercase());
                    sets.push(same);
                }
                left = other;
            }
        }
    }
    Some(sets)
}

/// Runs of items with the same key in a sorted slice.
fn runs<T, K: PartialEq>(items: &[T], key: impl Fn(&T) -> K) -> Vec<&[T]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..=items.len() {
        if i == items.len() || key(&items[i]) != key(&items[start]) {
            out.push(&items[start..i]);
            start = i;
        }
    }
    out
}

/// Which file this is on which drive, to tell two names of the same file (hard links): the
/// 128-bit id where the drive has one (ReFS's shorter ids can repeat). None when the drive gives
/// no real id (all zeros, as some network drives do), so the file is never taken for another.
fn file_id(f: &File) -> Option<(u64, u128)> {
    let handle = HANDLE(f.as_raw_handle());
    let mut long = FILE_ID_INFO::default();
    // SAFETY: the handle belongs to `f`, which is open for the whole call; `long` is a buffer of
    // the size given.
    if unsafe { GetFileInformationByHandleEx(handle, FileIdInfo, &mut long as *mut _ as *mut _, std::mem::size_of::<FILE_ID_INFO>() as u32) }.is_ok() {
        let id = u128::from_le_bytes(long.FileId.Identifier);
        return (id != 0).then_some((long.VolumeSerialNumber, id));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: as above; `info` is a valid buffer.
    unsafe { GetFileInformationByHandle(handle, &mut info) }.ok()?;
    let id = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
    (id != 0).then_some((u64::from(info.dwVolumeSerialNumber), u128::from(id)))
}

/// Reads until `buf` is full or the file ends. The bytes read, or None when cancelled; a read
/// error counts as the end.
fn fill(f: &mut File, buf: &mut [u8], cancel: &AtomicBool) -> Option<usize> {
    let mut n = 0;
    while n < buf.len() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        match f.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    Some(n)
}

/// A hash of the whole file, or Some(None) if it can't be read or isn't `size` bytes any more.
/// None when cancelled.
fn hash_file(path: &str, size: u64, buf: &mut [u8], cancel: &AtomicBool) -> Option<Option<u64>> {
    let Ok(mut f) = File::open(path) else { return Some(None) };
    let mut h = Hasher::new();
    let mut read = 0u64;
    loop {
        let n = fill(&mut f, buf, cancel)?;
        h.feed(&buf[..n]);
        read += n as u64;
        if n < buf.len() {
            break;
        }
    }
    Some((read == size).then(|| h.finish(read)))
}

/// Whether two files have the same bytes (false if either can't be read through). None when
/// cancelled.
fn equal(a: &str, b: &str, buf: &mut Buffers, cancel: &AtomicBool) -> Option<bool> {
    let (Ok(mut fa), Ok(mut fb)) = (File::open(a), File::open(b)) else { return Some(false) };
    loop {
        let na = fill(&mut fa, &mut buf.a, cancel)?;
        let nb = fill(&mut fb, &mut buf.b, cancel)?;
        if na != nb || buf.a[..na] != buf.b[..nb] {
            return Some(false);
        }
        if na < buf.a.len() {
            return Some(true);
        }
    }
}

/// A hash of one buffer.
fn hash(bytes: &[u8]) -> u64 {
    let mut h = Hasher::new();
    h.feed(bytes);
    h.finish(bytes.len() as u64)
}

/// A quick 64-bit hash, eight bytes at a time; reading the disk takes far longer. It needn't be
/// hard to fool: matching hashes are always checked byte by byte. Every piece but the last must be
/// a multiple of 8 bytes long (the read buffers are).
struct Hasher(u64);

impl Hasher {
    fn new() -> Hasher {
        Hasher(0x243F_6A88_85A3_08D3)
    }

    fn feed(&mut self, bytes: &[u8]) {
        let mut words = bytes.chunks_exact(8);
        for w in &mut words {
            self.mix(u64::from_le_bytes([w[0], w[1], w[2], w[3], w[4], w[5], w[6], w[7]]));
        }
        let rest = words.remainder();
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.mix(u64::from_le_bytes(last));
        }
    }

    fn mix(&mut self, w: u64) {
        self.0 = (self.0 ^ w).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    }

    fn finish(mut self, len: u64) -> u64 {
        self.mix(len);
        let mut h = self.0;
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^ (h >> 33)
    }
}

/// The entry for one file found, read fresh (None if it's gone meanwhile).
fn entry_at(path: &str, types: &mut HashMap<String, Arc<str>>) -> Option<Entry> {
    let wide = folder::wide_path(path);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `data` is a valid WIN32_FIND_DATAW.
    let handle = unsafe {
        FindFirstFileExW(PCWSTR(wide.as_ptr()), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_FLAGS(0))
    }
    .ok()?;
    // SAFETY: closes the handle opened above; it is not used afterwards.
    unsafe {
        let _ = FindClose(handle);
    }
    let mut e = folder::entry_from(&data, types)?;
    e.target = Some(path.to_string());
    Some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small tree under the temporary folder, removed afterwards: (path, content) pairs.
    struct Tree(std::path::PathBuf);

    impl Tree {
        fn new(name: &str, files: &[(&str, &[u8])]) -> Tree {
            let root = std::env::temp_dir().join(format!("glint-dupes-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            for (f, bytes) in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, bytes).unwrap();
            }
            Tree(root)
        }
        fn path(&self) -> String {
            self.0.to_string_lossy().replace('/', "\\")
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn groups(root: &str) -> (Vec<Vec<String>>, Summary) {
        let never = AtomicBool::new(false);
        let mut found = Vec::new();
        let summary = find(root, false, false, &never, &mut |g| {
            found.push(g.iter().map(|e| e.target.clone().unwrap()[root.len()..].to_string()).collect());
            true
        })
        .expect("not cancelled");
        (found, summary)
    }

    #[test]
    fn identical_files_are_grouped_across_subfolders() {
        // Big enough to need the whole-content hash, and differing only after the first 64 KB.
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let mut big_other = big.clone();
        big_other[200_000] ^= 1;
        let t = Tree::new("group", &[
            ("a/photo.jpg", &big),
            ("b/c/photo copy.jpg", &big),
            ("b/almost.jpg", &big_other),
            ("x.txt", b"hello"),
            ("y/x again.txt", b"hello"),
            ("y/z/x once more.txt", b"hello"),
            ("same size.txt", b"world"),
            ("empty1", b""),
            ("y/empty2", b""),
        ]);
        let (found, summary) = groups(&t.path());
        assert_eq!(found, vec![
            vec!["\\a\\photo.jpg".to_string(), "\\b\\c\\photo copy.jpg".to_string()],
            vec!["\\x.txt".to_string(), "\\y\\x again.txt".to_string(), "\\y\\z\\x once more.txt".to_string()],
        ]);
        assert_eq!(summary.groups, 2);
        assert_eq!(summary.files, 5);
        assert_eq!(summary.wasted_bytes, 300_000 + 2 * 5);
        assert_eq!(summary.scanned_files, 7);
        assert_eq!(summary.smallest_compared, 0);
    }

    #[test]
    fn a_cancelled_search_finds_nothing() {
        let t = Tree::new("cancel", &[("a", b"same"), ("b", b"same")]);
        let cancelled = AtomicBool::new(true);
        assert_eq!(find(&t.path(), false, false, &cancelled, &mut |_| true), None);
        // Stopping from the group callback stops it too.
        let never = AtomicBool::new(false);
        assert_eq!(find(&t.path(), false, false, &never, &mut |_| false), None);
    }

    #[test]
    fn the_hash_doesnt_depend_on_how_the_bytes_come() {
        let bytes: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let mut h = Hasher::new();
        h.feed(&bytes[..512]);
        h.feed(&bytes[512..]);
        assert_eq!(h.finish(1000), hash(&bytes));
        assert_ne!(hash(&bytes[..999]), hash(&bytes));
    }

    #[test]
    fn empty_files_are_never_duplicates_of_each_other() {
        let t = Tree::new("empties", &[("a", b""), ("b", b""), ("sub/c", b""), ("d.txt", b"x")]);
        let (found, summary) = groups(&t.path());
        assert!(found.is_empty());
        assert_eq!((summary.groups, summary.files, summary.wasted_bytes, summary.scanned_files), (0, 0, 0, 1));
    }

    #[test]
    fn same_size_but_different_content_is_not_a_duplicate() {
        // Small ones, big ones that differ at the start, the middle and the very last byte.
        let base: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        let (mut first, mut mid, mut last) = (base.clone(), base.clone(), base.clone());
        first[0] ^= 1;
        mid[100_000] ^= 1;
        *last.last_mut().unwrap() ^= 1;
        let t = Tree::new("samesize", &[
            ("s1", b"abcde"),
            ("s2", b"abcdf"),
            ("big", &base),
            ("big-first", &first),
            ("big-mid", &mid),
            ("big-last", &last),
            ("copy/big again", &base),
        ]);
        let (found, summary) = groups(&t.path());
        assert_eq!(found, vec![vec!["\\big".to_string(), "\\copy\\big again".to_string()]]);
        assert_eq!((summary.groups, summary.files), (1, 2));
        assert_eq!(summary.wasted_bytes, 200_000);
    }

    #[test]
    fn a_missing_folder_has_no_duplicates() {
        let t = Tree::new("missing", &[("a", b"same")]);
        let (found, summary) = groups(&format!("{}\\nope", t.path()));
        assert!(found.is_empty());
        assert_eq!((summary.groups, summary.scanned_files), (0, 0));
        // A file given as the folder is the same as an empty one.
        let (found, _) = groups(&format!("{}\\a", t.path()));
        assert!(found.is_empty());
    }

    #[test]
    fn a_file_that_vanishes_or_shrinks_during_the_search_is_just_left_out() {
        let t = Tree::new("vanish", &[("a", b"hello"), ("b", b"hello"), ("c", b"hello"), ("d", b"hello")]);
        let p = |n: &str| format!("{}\\{n}", t.path());
        let mut buffers = Buffers { a: vec![0u8; CHUNK], b: vec![0u8; CHUNK] };
        let never = AtomicBool::new(false);
        // One was deleted after the folders were read: the others still make a set.
        std::fs::remove_file(p("c")).unwrap();
        let sets = same_content(vec![p("a"), p("b"), p("c")], 5, &mut buffers, &never).unwrap();
        assert_eq!(sets, vec![vec![p("a"), p("b")]]);
        // One shrank (it is shorter than the size the listing said): left out.
        std::fs::write(p("d"), b"hi").unwrap();
        let sets = same_content(vec![p("a"), p("b"), p("d")], 5, &mut buffers, &never).unwrap();
        assert_eq!(sets, vec![vec![p("a"), p("b")]]);
        // Only one left: no set at all. All gone: no set.
        assert!(same_content(vec![p("a"), p("c")], 5, &mut buffers, &never).unwrap().is_empty());
        assert!(same_content(vec![p("c"), p("zz")], 5, &mut buffers, &never).unwrap().is_empty());
        // Big ones: one grew after the head was read is still compared by its whole content.
        let big = vec![9u8; 150_000];
        std::fs::write(p("big1"), &big).unwrap();
        std::fs::write(p("big2"), &big).unwrap();
        std::fs::write(p("big3"), &big).unwrap();
        std::fs::remove_file(p("big3")).unwrap();
        let sets = same_content(vec![p("big1"), p("big2"), p("big3")], 150_000, &mut buffers, &never).unwrap();
        assert_eq!(sets, vec![vec![p("big1"), p("big2")]]);
        // Cancelled: nothing.
        assert!(same_content(vec![p("big1"), p("big2")], 150_000, &mut buffers, &AtomicBool::new(true)).is_none());
    }

    #[test]
    fn odd_names_and_capitals_are_grouped_and_listed_in_path_order() {
        if !crate::search::odd_names_work(&std::env::temp_dir()) {
            return;
        }
        let t = Tree::new("oddnames", &[
            ("\u{e9}t\u{e9} \u{1f600}.txt", b"same text"),
            ("#1/Z.TXT", b"same text"),
            ("#1/a.txt", b"same text"),
            ("dots. here/copy.txt", b"same text"),
        ]);
        let (found, summary) = groups(&t.path());
        assert_eq!(summary.groups, 1);
        assert_eq!(
            found[0],
            vec!["\\#1\\a.txt".to_string(), "\\#1\\Z.TXT".to_string(), "\\dots. here\\copy.txt".to_string(), "\\\u{e9}t\u{e9} \u{1f600}.txt".to_string()]
        );
    }

    #[test]
    fn a_hard_link_is_one_file_not_a_duplicate() {
        let t = Tree::new("links", &[("a", b"linked content"), ("x", b"linked content")]);
        if std::fs::hard_link(t.0.join("a"), t.0.join("a-link")).is_err() {
            return; // (this file system can't)
        }
        let (found, _) = groups(&t.path());
        // a and a-link are the same file: with x that is two files, not three.
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].len(), 2, "{found:?}");
    }

    #[test]
    fn asking_to_stop_after_the_first_set_ends_the_search() {
        let t = Tree::new("stopearly", &[("a", b"one one"), ("b", b"one one"), ("c", b"two two"), ("d", b"two two")]);
        let never = AtomicBool::new(false);
        let mut got = 0;
        let r = find(&t.path(), false, false, &never, &mut |_| {
            got += 1;
            false
        });
        assert!(r.is_none() && got == 1);
        // Cancelled before it starts: nothing, and no set.
        let mut got = 0;
        let r = find(&t.path(), false, false, &AtomicBool::new(true), &mut |_| {
            got += 1;
            true
        });
        assert!(r.is_none() && got == 0);
    }
}
