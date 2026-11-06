//! Disk space (Ctrl+Shift+U): what fills a drive.
//!
//! A look goes through every folder on the drive on a thread of its own and keeps:
//! - the folders big enough to matter, as a tree (the "Biggest" tab: each folder's biggest
//!   folders, with a bar), smaller ones folded into their parent's total;
//! - how much each kind of file takes (the "By type" tab: videos, games, pictures, ...);
//! - the space no folder shows: the Recycle Bin, the hibernation and page files, and the rest
//!   (restore points and other system files Glint isn't allowed to read).
//!
//! Each look is remembered in a small text file in the config folder, so the next look can say
//! which folders grew or shrank since then (the "Changes" tab).
//!
//! As everywhere else in Glint, every file counts (hidden or not), junctions and symbolic links are
//! never followed, and a folder that can't be read counts as empty.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, WIN32_FIND_DATAW};
use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};

use crate::folder;
use crate::search;

const ATTR_DIRECTORY: u32 = 0x10;
/// Folders smaller than this part of the drive's used space are folded into their parent (but
/// never less than `KEEP_AT_LEAST`), so a drive with a million folders keeps a few thousand.
const KEEP_PART: u64 = 10_000;
const KEEP_AT_LEAST: u64 = 32 * 1024 * 1024;
/// A look remembered for "Changes" is replaced only once it's this old, so looking twice in a
/// row still compares with yesterday's look, not the one from a minute ago.
const REMEMBER_FOR_SECS: u64 = 20 * 60 * 60;
/// "Changes" leaves out changes smaller than this.
const GREW_AT_LEAST: u64 = 100 * 1024 * 1024;
/// "Changes" shows this many folders at most.
const GREW_MAX: usize = 30;
/// The first line of a remembered look.
const FILE_HEAD: &str = "glint disk space 1";

/// One folder in a look: its name, everything inside it, and the folders inside it big enough to
/// keep (biggest first).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub name: String,
    pub size: u64,
    pub kids: Vec<Node>,
}

impl Node {
    /// What's in this folder outside the folders it kept: its own files and the small folders.
    pub fn rest(&self) -> u64 {
        self.size.saturating_sub(self.kids.iter().map(|k| k.size).sum())
    }

    /// The folder at `rel` inside this one ("Users\\me"; "" is this one), in any case.
    pub fn find(&self, rel: &str) -> Option<&Node> {
        let mut at = self;
        for part in rel.split('\\').filter(|p| !p.is_empty()) {
            at = at.kids.iter().find(|k| k.name.eq_ignore_ascii_case(part))?;
        }
        Some(at)
    }

    /// Every kept folder with its path inside this one, this one first ("").
    fn walk(&self, prefix: &str, each: &mut dyn FnMut(&str, &Node)) {
        each(prefix, self);
        for k in &self.kids {
            let rel = if prefix.is_empty() { k.name.clone() } else { format!("{prefix}\\{}", k.name) };
            k.walk(&rel, each);
        }
    }
}

/// The kinds of files the "By type" tab adds up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Videos,
    Games,
    Apps,
    Windows,
    Pictures,
    Music,
    Documents,
    Archives,
    Other,
}

impl Kind {
    pub const ALL: [Kind; 9] = [Kind::Videos, Kind::Games, Kind::Apps, Kind::Windows, Kind::Pictures, Kind::Music, Kind::Documents, Kind::Archives, Kind::Other];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Videos => "Videos",
            Kind::Games => "Games",
            Kind::Apps => "Apps",
            Kind::Windows => "Windows",
            Kind::Pictures => "Pictures",
            Kind::Music => "Music",
            Kind::Documents => "Documents",
            Kind::Archives => "Archives and disc images",
            Kind::Other => "Other",
        }
    }

    /// The small line under the name: what counts as this kind.
    pub fn examples(self) -> &'static str {
        match self {
            Kind::Videos => "mp4, mkv, mov",
            Kind::Games => "Steam, Epic, Xbox",
            Kind::Apps => "exe, dll, msi",
            Kind::Windows => "system files",
            Kind::Pictures => "jpg, png, gif",
            Kind::Music => "mp3, flac, wav",
            Kind::Documents => "pdf, Word, Excel, txt",
            Kind::Archives => "zip, 7z, iso",
            Kind::Other => "everything else",
        }
    }

    fn index(self) -> usize {
        Kind::ALL.iter().position(|&k| k == self).unwrap_or(Kind::ALL.len() - 1)
    }

    /// A file's kind by its extension (lower case, no dot), outside the Windows and game folders.
    pub fn of_extension(ext: &str) -> Kind {
        match ext {
            "mp4" | "mkv" | "mov" | "avi" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg" | "ts" | "m2ts" | "3gp" => Kind::Videos,
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" | "svg" | "ico" | "avif" => Kind::Pictures,
            "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "aiff" | "alac" | "mid" => Kind::Music,
            "exe" | "dll" | "msi" | "msix" | "appx" | "sys" | "pak" | "dat" | "bin" | "so" | "jar" | "pdb" | "lib" => Kind::Apps,
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "txt" | "rtf" | "md" | "csv" | "epub" => Kind::Documents,
            "zip" | "7z" | "rar" | "tar" | "gz" | "xz" | "bz2" | "zst" | "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "wim" | "esd" | "cab" => Kind::Archives,
            _ => Kind::Other,
        }
    }
}

/// Which part of the drive a folder is in: inside Windows' folder or a game library, everything
/// counts as that kind whatever its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Any,
    Windows,
    Games,
}

impl Zone {
    /// The zone of folder `name` inside a folder of zone `self`, `depth` folders below the root.
    fn inside(self, name: &str, depth: usize) -> Zone {
        if self != Zone::Any {
            return self;
        }
        let n = name.to_lowercase();
        if depth == 1 && n == "windows" {
            return Zone::Windows;
        }
        if matches!(n.as_str(), "steamapps" | "epic games" | "xboxgames" | "riot games" | "gog galaxy" | "ea games" | "ubisoft game launcher" | "battle.net") {
            return Zone::Games;
        }
        Zone::Any
    }
}

/// Space on the drive no folder shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    pub recycle_bin: u64,
    /// hiberfil.sys.
    pub hibernation: u64,
    /// pagefile.sys and swapfile.sys.
    pub page_file: u64,
    /// The rest: restore points and system files Glint can't read.
    pub other: u64,
}

impl Hidden {
    pub fn total(&self) -> u64 {
        self.recycle_bin + self.hibernation + self.page_file + self.other
    }
}

/// One look at a drive.
#[derive(Clone, Debug, Default)]
pub struct Look {
    /// "C:\\".
    pub root: String,
    pub tree: Node,
    /// Bytes per kind, in `Kind::ALL`'s order.
    pub kinds: [u64; 9],
    pub hidden: Hidden,
    pub total: u64,
    pub free: u64,
    /// When it was made (seconds since 1970).
    pub at: u64,
}

impl Look {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    /// The kinds with something in them, biggest first.
    pub fn kinds_by_size(&self) -> Vec<(Kind, u64)> {
        let mut v: Vec<(Kind, u64)> = Kind::ALL.iter().map(|&k| (k, self.kinds[k.index()])).filter(|&(_, b)| b > 0).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }
}

/// Seconds since 1970.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A drive's total and free bytes (0, 0 when it isn't ready).
fn space(root: &str) -> (u64, u64) {
    let w = folder::wide_path(root);
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: `w` is NUL-terminated and outlives the call; valid out-pointers.
    let _ = unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), Some(&mut total), None) };
    (total, free)
}

/// The used bytes of the drive at `root` (0 when it isn't ready). Touches the disk.
pub fn used_space(root: &str) -> u64 {
    let (total, free) = space(root);
    total.saturating_sub(free)
}

/// The bytes in the Recycle Bin of the drive at `root`.
fn bin_bytes(root: &str) -> u64 {
    let w: Vec<u16> = root.encode_utf16().chain([0]).collect();
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: a NUL-terminated root path and a struct with its size set, both alive for the call.
    unsafe { SHQueryRecycleBinW(PCWSTR(w.as_ptr()), &mut info) }.ok().map(|_| info.i64Size.max(0) as u64).unwrap_or(0)
}

/// A folder on the way down, while it is being added up.
struct Frame {
    path: String,
    node: Node,
    zone: Zone,
    depth: usize,
    /// Its folders still to go through.
    todo: Vec<String>,
}

/// Looks at the whole drive `root` ("C:\\"). `counted` goes up as files are added up (for "312 GB
/// counted so far"). None when cancelled.
pub fn look(root: &str, cancel: &AtomicBool, counted: &AtomicU64, tick: &mut dyn FnMut()) -> Option<Look> {
    let root = if root.ends_with('\\') { root.to_string() } else { format!("{root}\\") };
    let (total, free) = space(&root);
    let used = total.saturating_sub(free);
    let keep = (used / KEEP_PART).max(KEEP_AT_LEAST);
    let mut kinds = [0u64; 9];
    let mut hidden = Hidden { recycle_bin: bin_bytes(&root), ..Hidden::default() };
    let mut stack = vec![Frame { path: root.clone(), node: Node { name: root.trim_end_matches('\\').to_string(), ..Node::default() }, zone: Zone::Any, depth: 0, todo: Vec::new() }];
    let mut tree = None;
    // The root is listed first, then each folder as it comes up.
    let mut fresh = true;
    while let Some(top) = stack.last_mut() {
        if fresh {
            fresh = false;
            let (path, zone, depth) = (top.path.clone(), top.zone, top.depth);
            let mut size = 0u64;
            let mut todo = Vec::new();
            let listed = search::list(&path, cancel, &mut |data: &WIN32_FIND_DATAW| {
                let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
                let name = String::from_utf16_lossy(&data.cFileName[..len]);
                if data.dwFileAttributes & ATTR_DIRECTORY != 0 {
                    if search::is_link(data) || name == "." || name == ".." {
                        return true;
                    }
                    // The Recycle Bin is counted as a whole above (other people's parts can't be read).
                    if depth == 0 && name.eq_ignore_ascii_case("$Recycle.Bin") {
                        return true;
                    }
                    todo.push(name);
                    return true;
                }
                let bytes = ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64;
                if depth == 0 {
                    let lower = name.to_lowercase();
                    if lower == "hiberfil.sys" {
                        hidden.hibernation += bytes;
                        return true;
                    }
                    if lower == "pagefile.sys" || lower == "swapfile.sys" {
                        hidden.page_file += bytes;
                        return true;
                    }
                }
                let kind = match zone {
                    Zone::Windows => Kind::Windows,
                    Zone::Games => Kind::Games,
                    Zone::Any => Kind::of_extension(&folder::extension(&name)),
                };
                kinds[kind.index()] += bytes;
                size += bytes;
                counted.fetch_add(bytes, Ordering::Relaxed);
                true
            });
            tick();
            if !listed {
                return None;
            }
            let top = stack.last_mut().expect("the frame just listed");
            top.node.size += size;
            top.todo = todo;
            continue;
        }
        if let Some(name) = top.todo.pop() {
            let path = folder::join(&top.path, &name);
            let (zone, depth) = (top.zone.inside(&name, top.depth + 1), top.depth + 1);
            stack.push(Frame { path, node: Node { name, ..Node::default() }, zone, depth, todo: Vec::new() });
            fresh = true;
            continue;
        }
        // All its folders are added up: it is done.
        let mut done = stack.pop().expect("the frame on top");
        done.node.kids.retain(|k| k.size >= keep);
        done.node.kids.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
        match stack.last_mut() {
            Some(parent) => {
                parent.node.size += done.node.size;
                parent.node.kids.push(done.node);
            }
            None => tree = Some(done.node),
        }
    }
    let tree = tree?;
    hidden.other = used.saturating_sub(tree.size + hidden.recycle_bin + hidden.hibernation + hidden.page_file);
    Some(Look { root, tree, kinds, hidden, total, free, at: now() })
}

// ---------------------------------------------------------------- remembering looks

/// Where the looks at drive `root` are remembered: the newer one and the one before it.
fn files_for(root: &str) -> (PathBuf, PathBuf) {
    let letter: String = root.chars().take_while(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase();
    let letter = if letter.is_empty() { "drive".to_string() } else { letter };
    let dir = crate::settings::folders().config.join("disk space");
    (dir.join(format!("{letter}.txt")), dir.join(format!("{letter} before.txt")))
}

/// A remembered look: when, the used space then, and each kept folder's size by its path inside
/// the drive (lower case; "" is the whole drive).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remembered {
    pub at: u64,
    pub used: u64,
    pub sizes: HashMap<String, u64>,
}

/// A look as the text it's remembered as.
pub fn to_text(look: &Look) -> String {
    let mut out = format!("{FILE_HEAD}\nat {}\nused {}\n", look.at, look.used());
    look.tree.walk("", &mut |rel, node| {
        out.push_str(&format!("{}\t{}\n", node.size, rel));
    });
    out
}

/// A remembered look from its text; None for anything else.
pub fn from_text(text: &str) -> Option<Remembered> {
    let mut lines = text.lines();
    if lines.next()? != FILE_HEAD {
        return None;
    }
    let at = lines.next()?.strip_prefix("at ")?.trim().parse().ok()?;
    let used = lines.next()?.strip_prefix("used ")?.trim().parse().ok()?;
    let mut sizes = HashMap::new();
    for line in lines {
        let Some((size, rel)) = line.split_once('\t') else { continue };
        let Ok(size) = size.parse::<u64>() else { continue };
        sizes.insert(rel.to_lowercase(), size);
    }
    Some(Remembered { at, used, sizes })
}

/// The remembered look to compare a new one with: the newest that is old enough
/// (`REMEMBER_FOR_SECS`), or else the oldest there is. Touches the disk.
pub fn remembered(root: &str, now: u64) -> Option<Remembered> {
    let (newer, before) = files_for(root);
    let read = |p: &PathBuf| std::fs::read_to_string(p).ok().and_then(|t| from_text(&t));
    pick_remembered(read(&newer), read(&before), now)
}

fn pick_remembered(newer: Option<Remembered>, before: Option<Remembered>, now: u64) -> Option<Remembered> {
    match (newer, before) {
        (Some(n), _) if now.saturating_sub(n.at) >= REMEMBER_FOR_SECS => Some(n),
        (Some(n), None) => Some(n),
        (Some(_), Some(b)) => Some(b),
        (None, b) => b,
    }
}

/// Remembers `look`. The newer file moves to "before" only when it is old enough, so the one to
/// compare with stays at least a day old. Touches the disk. Errors are ignored: the next look just
/// can't say what grew.
pub fn remember(look: &Look) {
    let (newer, before) = files_for(&look.root);
    if let Some(dir) = newer.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let old_enough = std::fs::read_to_string(&newer).ok().and_then(|t| from_text(&t)).is_some_and(|r| look.at.saturating_sub(r.at) >= REMEMBER_FOR_SECS);
    if old_enough {
        let _ = std::fs::rename(&newer, &before);
    }
    let tmp = newer.with_extension("tmp");
    if std::fs::write(&tmp, to_text(look)).is_ok() {
        let _ = std::fs::rename(&tmp, &newer);
    }
}

/// A folder that grew (`change` > 0) or shrank since the remembered look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Its path inside the drive ("Users\\me\\Videos").
    pub rel: String,
    pub change: i64,
}

/// The folders that grew or shrank most since `then`, biggest change first. A folder is named
/// only when the change isn't mostly one of its folders' (then that folder is named instead), so
/// "Users", "Users\\me" and "Users\\me\\Videos" don't all show the same 160 GB.
pub fn grew(look: &Look, then: &Remembered) -> Vec<Change> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    fn visit(node: &Node, rel: &str, then: &Remembered, seen: &mut std::collections::HashSet<String>, out: &mut Vec<Change>) {
        let key = rel.to_lowercase();
        seen.insert(key.clone());
        let change = node.size as i64 - then.sizes.get(&key).copied().unwrap_or(0) as i64;
        let kid_rel = |k: &Node| if rel.is_empty() { k.name.clone() } else { format!("{rel}\\{}", k.name) };
        // Does one folder inside carry most of this change?
        let carried = node.kids.iter().any(|k| {
            let kc = k.size as i64 - then.sizes.get(&kid_rel(k).to_lowercase()).copied().unwrap_or(0) as i64;
            kc.signum() == change.signum() && kc.unsigned_abs() * 10 >= change.unsigned_abs() * 7
        });
        if !rel.is_empty() && change.unsigned_abs() >= GREW_AT_LEAST && !carried {
            out.push(Change { rel: rel.to_string(), change });
        }
        for k in &node.kids {
            visit(k, &kid_rel(k), then, seen, out);
        }
    }
    visit(&look.tree, "", then, &mut seen, &mut out);
    // Folders that were big then and aren't kept now: gone or shrunk (only the top one of each).
    let mut gone: Vec<(&String, &u64)> = then.sizes.iter().filter(|(k, &v)| !k.is_empty() && !seen.contains(*k) && v >= GREW_AT_LEAST).collect();
    gone.sort_by(|a, b| a.0.cmp(b.0));
    let mut tops: Vec<&String> = Vec::new();
    for (k, &v) in gone {
        if tops.iter().any(|t| k.starts_with(&format!("{t}\\"))) {
            continue;
        }
        tops.push(k);
        // Its real name isn't known any more (only the lower case): the path as remembered.
        out.push(Change { rel: k.clone(), change: -(v as i64) });
    }
    out.sort_by(|a, b| b.change.unsigned_abs().cmp(&a.change.unsigned_abs()).then_with(|| a.rel.cmp(&b.rel)));
    out.truncate(GREW_MAX);
    out
}

/// The start of Disk space's path in a tab: "::DiskSpace (C:)". It has no backslash, so Up goes to
/// This PC, and it can't be mistaken for a real folder.
const LOCATION_START: &str = "::DiskSpace (";

/// Disk space's path in a tab for `drive` ("C:\\" or "c:").
pub fn location(drive: &str) -> String {
    let letter = drive.chars().next().unwrap_or('C').to_ascii_uppercase();
    format!("{LOCATION_START}{letter}:)")
}

/// Whether a tab's path is Disk space.
pub fn is_disk(path: &str) -> bool {
    path.len() > LOCATION_START.len() && path.get(..LOCATION_START.len()).is_some_and(|s| s.eq_ignore_ascii_case(LOCATION_START)) && path.ends_with(":)")
}

/// The drive ("C:\\") of Disk space's path.
pub fn drive_of_location(path: &str) -> Option<String> {
    if !is_disk(path) {
        return None;
    }
    drive_of(&path[LOCATION_START.len()..])
}

/// What it's called on screen.
pub const NAME: &str = "Disk space";

/// "C:\\" from any path on that drive ("c:\\users" gives "C:\\"); None for paths that aren't on a
/// lettered drive.
pub fn drive_of(path: &str) -> Option<String> {
    let b = path.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| format!("{}:\\", (b[0] as char).to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str, size: u64, kids: Vec<Node>) -> Node {
        Node { name: name.into(), size, kids }
    }

    const GB: u64 = 1024 * 1024 * 1024;

    fn sample() -> Look {
        let tree = node("C:", 500 * GB, vec![
            node("Users", 300 * GB, vec![node("me", 290 * GB, vec![node("Videos", 200 * GB, vec![]), node("Downloads", 60 * GB, vec![])])]),
            node("Games", 150 * GB, vec![]),
        ]);
        Look { root: "C:\\".into(), tree, total: 1000 * GB, free: 400 * GB, at: 1_000_000, ..Look::default() }
    }

    #[test]
    fn a_look_reads_back_from_its_text() {
        let look = sample();
        let back = from_text(&to_text(&look)).unwrap();
        assert_eq!(back.at, 1_000_000);
        assert_eq!(back.used, 600 * GB);
        assert_eq!(back.sizes.get(""), Some(&(500 * GB)));
        assert_eq!(back.sizes.get("users\\me\\videos"), Some(&(200 * GB)));
        assert!(from_text("something else\nat 1\nused 2\n").is_none());
    }

    #[test]
    fn what_grew_names_the_folder_that_really_grew() {
        let look = sample();
        let mut then = from_text(&to_text(&look)).unwrap();
        // Videos grew by 160 GB (so did me, Users and the drive, through it); Games shrank by 20 GB.
        for (k, d) in [("", 140i64), ("users", 160), ("users\\me", 160), ("users\\me\\videos", 160), ("games", -20)] {
            let v = then.sizes.get_mut(k).unwrap();
            *v = (*v as i64 - d * GB as i64) as u64;
        }
        // A folder that was 30 GB then and is gone now.
        then.sizes.insert("old stuff".into(), 30 * GB);
        then.sizes.insert("old stuff\\inside".into(), 29 * GB);
        let changes = grew(&look, &then);
        assert_eq!(changes, vec![
            Change { rel: "Users\\me\\Videos".into(), change: 160 * GB as i64 },
            Change { rel: "old stuff".into(), change: -30 * GB as i64 },
            Change { rel: "Games".into(), change: -20 * GB as i64 },
        ]);
    }

    #[test]
    fn the_rest_is_what_no_kept_folder_holds() {
        let look = sample();
        assert_eq!(look.tree.rest(), 50 * GB);
        assert_eq!(look.tree.find("users\\ME").map(|n| n.size), Some(290 * GB));
        assert!(look.tree.find("nope").is_none());
        assert_eq!(look.tree.find("").map(|n| n.name.as_str()), Some("C:"));
    }

    #[test]
    fn the_older_look_is_compared_with_until_the_newer_one_is_a_day_old() {
        let r = |at| Some(Remembered { at, ..Remembered::default() });
        let day = REMEMBER_FOR_SECS;
        assert_eq!(pick_remembered(r(100), r(10), 100 + 60).map(|x| x.at), Some(10));
        assert_eq!(pick_remembered(r(100), r(10), 100 + day).map(|x| x.at), Some(100));
        assert_eq!(pick_remembered(r(100), None, 160).map(|x| x.at), Some(100));
        assert_eq!(pick_remembered(None, None, 160), None);
    }

    #[test]
    fn kinds_and_zones() {
        assert_eq!(Kind::of_extension("mkv"), Kind::Videos);
        assert_eq!(Kind::of_extension("iso"), Kind::Archives);
        assert_eq!(Kind::of_extension("whatever"), Kind::Other);
        assert_eq!(Zone::Any.inside("Windows", 1), Zone::Windows);
        assert_eq!(Zone::Any.inside("Windows", 3), Zone::Any);
        assert_eq!(Zone::Any.inside("steamapps", 4), Zone::Games);
        assert_eq!(Zone::Games.inside("bin", 5), Zone::Games);
        assert_eq!(drive_of("c:\\users"), Some("C:\\".into()));
        assert_eq!(drive_of("\\\\server\\share"), None);
    }
}
