//! Looking inside .zip and .7z files as if they were folders (read-only), unpacking them, and
//! making new ones.
//!
//! A path like `C:\x\a.zip\sub\b.txt` is an archive file plus a path inside it ([`split`]). [`read`]
//! lists one folder inside, [`extract`] copies items out to a real folder, [`extract_all`] unpacks a
//! whole archive (Extract here), and [`pack`] makes a new one (Compress to zip / 7z) from what
//! [`plan_pack`] found. Nothing is kept between calls: each one opens the archive again. That's cheap, because the list of what's inside sits in
//! one place (a zip's central directory at the end, a 7z's header), so listing never unpacks any
//! file data.
//!
//! Names inside an archive are written by whoever made it, so they're cleaned before use: `/` and
//! `\` both separate folders, and a name that climbs out (`..`) or names a drive (`C:`) is left
//! out altogether, so nothing can be written outside the folder it's extracted to. Damaged
//! archives come back as plain-words errors.
//!
//! Unpacking and packing report how far they are to the copy queue (copyqueue.rs) when they run in
//! it: its list then shows them with Pause and Cancel.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime};

use crate::folder::{self, Entry, EntryKind, Folder};
use crate::icons;

const ATTR_DIRECTORY: u32 = 0x10;

/// The id 7z gives its AES coder: a block packed with it needs a password.
const SEVENZ_AES: [u8; 4] = [0x06, 0xF1, 0x07, 0x01];

const PASSWORD: &str = "This archive is locked with a password, and Glint can't open password-protected archives yet.";
const DAMAGED_ZIP: &str = "This zip file is damaged or uses a kind of packing Glint can't read.";
const DAMAGED_7Z: &str = "This 7z file is damaged or uses a kind of packing Glint can't read.";
/// What unpacking or packing says when CANCEL was clicked in the copy queue's list.
const CANCELLED: &str = "Cancelled.";
/// How much a 7z packs into one solid block (more packs smaller; less unpacks one file faster).
const SOLID_BLOCK: u64 = 64 * 1024 * 1024;
/// Zip files bigger than about this need the zip64 extension.
const ZIP64_FROM: u64 = 0xFFFF_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Zip,
    SevenZ,
}

fn kind_of(name: &str) -> Option<Kind> {
    match folder::extension(name).as_str() {
        "zip" => Some(Kind::Zip),
        "7z" => Some(Kind::SevenZ),
        _ => None,
    }
}

/// Whether a file name has an archive extension Glint opens (zip, 7z).
pub fn is_archive_name(name: &str) -> bool {
    kind_of(name).is_some()
}

/// Splits a path like `C:\x\a.zip\sub\b.txt` into the archive file and the path inside it (`\`
/// separators, "" for the archive's root), when some part of the path is an existing file named
/// .zip or .7z. None for everything else. Ordinary paths cost only a look at the names: the file
/// system is asked only about parts with an archive extension.
pub fn split(path: &str) -> Option<(String, String)> {
    let mut start = 0;
    while start < path.len() {
        let end = path[start..].find('\\').map_or(path.len(), |i| start + i);
        if is_archive_name(&path[start..end]) && folder::is_file(&path[..end]) {
            let inner: Vec<&str> = path[end..].split('\\').filter(|p| !p.is_empty()).collect();
            return Some((path[..end].to_string(), inner.join("\\")));
        }
        start = end + 1;
    }
    None
}

/// One thing stored in an archive, with its name cleaned.
#[derive(Clone, Debug)]
struct Item {
    /// The path inside, `\` between parts, never empty, no `.`/`..` parts.
    path: String,
    is_dir: bool,
    size: u64,
    /// FILETIME, 0 when unknown.
    modified: u64,
    /// Needs a password to unpack.
    locked: bool,
    /// Where it is in the archive's own list (the zip index, the 7z file index).
    index: usize,
}

impl Item {
    fn parts(&self) -> std::str::Split<'_, char> {
        self.path.split('\\')
    }
}

/// An archive's name for something, as a safe relative path: None for names that climb out of
/// the folder (`..`) or are nothing but separators. Characters Windows doesn't allow in a name
/// (`:` from a Mac or Linux zip's "10:30.log", `?`, `*`...) become `_`, as 7-Zip does, so a drive
/// name (`C:/x`) stays inside the folder too. Parts that Windows takes for a device (`nul`,
/// `com1.txt`...) get a `_` in front, so they come out as files. Dots and spaces at the end of a
/// part go, as Windows drops them when writing it ("docs." is shown, opened and written as "docs");
/// a part of nothing else (`...`) can't be written, and is left out like `..`.
fn clean_name(raw: &str) -> Option<String> {
    let mut parts = Vec::new();
    for part in raw.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p => {
                let p: String = p.chars().map(|c| if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') || c < ' ' { '_' } else { c }).collect();
                let kept = p.trim_end_matches(['.', ' ']);
                if kept.is_empty() {
                    return None;
                }
                parts.push(if is_device_name(&p) { format!("_{p}") } else { kept.to_string() });
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\\"))
    }
}

/// Whether Windows opens a device for this name (CON, PRN, AUX, NUL, COM0-9, LPT0-9, CONIN$ or
/// CONOUT$, in any case, with or without an extension or trailing dots and spaces).
fn is_device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        s if s.starts_with("COM") || s.starts_with("LPT") => {
            let mut rest = s[3..].chars();
            matches!((rest.next(), rest.next()), (Some('0'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None))
        }
        _ => false,
    }
}

/// The parts of a path inside the archive as the caller gave it.
fn inner_parts(inner: &str) -> Vec<&str> {
    inner.split(['\\', '/']).filter(|p| !p.is_empty()).collect()
}

fn same_name(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

/// These parts as one path in lower case, the way `same_name` compares them: so many items can be
/// looked up by it at once.
fn folded(parts: &[&str]) -> String {
    parts.join("\\").chars().flat_map(char::to_lowercase).collect()
}

/// Whether `item` is `want` itself or something under it (names compared ignoring case, as
/// Windows does). Returns the item's parts.
fn under<'a>(item: &'a Item, want: &[&str]) -> Option<Vec<&'a str>> {
    let parts: Vec<&str> = item.parts().collect();
    if parts.len() < want.len() || !parts.iter().zip(want).all(|(a, b)| same_name(a, b)) {
        return None;
    }
    Some(parts)
}

// ---------------------------------------------------------------- listing

fn list(archive: &str) -> Result<(Kind, Vec<Item>), String> {
    match kind_of(archive) {
        Some(Kind::Zip) => list_zip(archive).map(|items| (Kind::Zip, items)),
        Some(Kind::SevenZ) => open_7z(archive).map(|(a, _)| (Kind::SevenZ, list_7z(&a))),
        None => Err("This isn't a zip or 7z file.".to_string()),
    }
}

fn open_zip(archive: &str) -> Result<zip::ZipArchive<BufReader<File>>, String> {
    let file = File::open(archive).map_err(|e| open_error(&e))?;
    zip::ZipArchive::new(BufReader::new(file)).map_err(|e| match e {
        zip::result::ZipError::Io(e) if e.kind() != io::ErrorKind::UnexpectedEof => open_error(&e),
        zip::result::ZipError::UnsupportedArchive(m) if m == zip::result::ZipError::PASSWORD_REQUIRED => PASSWORD.to_string(),
        _ => DAMAGED_ZIP.to_string(),
    })
}

fn list_zip(archive: &str) -> Result<Vec<Item>, String> {
    let zip = open_zip(archive)?;
    let mut items = Vec::with_capacity(zip.len());
    for index in 0..zip.len() {
        // The central directory's copy of the entry: no seeking to the data.
        let Ok(e) = zip.by_index_data(index) else { continue };
        let Some(path) = clean_name(&zip_name(e.name_raw())) else { continue };
        let is_dir = e.is_dir();
        items.push(Item {
            path,
            is_dir,
            size: if is_dir { 0 } else { e.size() },
            modified: e.last_modified().map_or(0, |t| local_to_filetime(t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second())),
            locked: e.encrypted(),
            index,
        });
    }
    Ok(items)
}

/// A zip entry's name: UTF-8 when it is (newer zips, and older ones with a Unicode name added),
/// otherwise in the PC's own old code page, as Windows' own zip folders and 7-Zip read it
/// (Russian, Japanese or Greek names from an older Windows zip).
fn zip_name(raw: &[u8]) -> String {
    if let Ok(name) = std::str::from_utf8(raw) {
        return name.to_string();
    }
    use windows::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
    // SAFETY: both buffers are slices the call only reads and writes within.
    let len = unsafe { MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, None) };
    let mut wide = vec![0u16; len.max(0) as usize];
    let len = unsafe { MultiByteToWideChar(CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), raw, Some(&mut wide)) };
    wide.truncate(len.max(0) as usize);
    String::from_utf16_lossy(&wide)
}

fn open_7z(archive: &str) -> Result<(sevenz_rust2::Archive, BufReader<File>), String> {
    let file = File::open(archive).map_err(|e| open_error(&e))?;
    let mut source = BufReader::new(file);
    let a = sevenz_rust2::Archive::read(&mut source, &sevenz_rust2::Password::empty()).map_err(|e| sevenz_error(&e))?;
    Ok((a, source))
}

fn list_7z(a: &sevenz_rust2::Archive) -> Vec<Item> {
    let mut items = Vec::with_capacity(a.files.len());
    for (index, f) in a.files.iter().enumerate() {
        if f.is_anti_item {
            continue;
        }
        let Some(path) = clean_name(&f.name) else { continue };
        let block = a.stream_map.file_block_index.get(index).copied().flatten().and_then(|b| a.blocks.get(b));
        items.push(Item {
            path,
            is_dir: f.is_directory,
            size: if f.is_directory { 0 } else { f.size },
            modified: if f.has_last_modified_date { u64::from(f.last_modified_date) } else { 0 },
            locked: block.is_some_and(|b| b.coders.iter().any(|c| c.encoder_method_id() == SEVENZ_AES)),
            index,
        });
    }
    items
}

/// Reads the folder `inner` of the archive (`\` separators, "" for its root) as a [`Folder`] whose
/// path is `full_path`. Folders that only show up in the names of the files in them are listed too
/// (many zips have no entries for folders). Never panics; problems end up in `Folder::error`.
pub fn read(archive: &str, inner: &str, full_path: &str) -> Folder {
    let start = Instant::now();
    let mut f = Folder { path: full_path.to_string(), ..Default::default() };
    match list(archive).and_then(|(_, items)| children(&items, inner)) {
        Ok(entries) => f.entries = entries,
        Err(msg) => f.error = Some(msg),
    }
    f.read_ms = start.elapsed().as_secs_f64() * 1000.0;
    f
}

/// The direct children of `inner` among the archive's items.
fn children(items: &[Item], inner: &str) -> Result<Vec<Entry>, String> {
    let want = inner_parts(inner);
    let n = want.len();
    let mut found = n == 0;
    let mut is_a_file = false;
    let mut entries: Vec<Entry> = Vec::new();
    // (lower-case name, is folder) -> where in `entries`.
    let mut seen: HashMap<(String, bool), usize> = HashMap::new();
    let mut types: HashMap<String, Arc<str>> = HashMap::new();
    for item in items {
        let Some(parts) = under(item, &want) else { continue };
        if parts.len() == n {
            if item.is_dir {
                found = true;
            } else {
                is_a_file = true;
            }
            continue;
        }
        found = true;
        let name = parts[n];
        let direct = parts.len() == n + 1;
        let is_dir = !direct || item.is_dir;
        let key = (name.to_lowercase(), is_dir);
        match seen.get(&key) {
            // A folder seen before through a file inside it, now with its own entry: take its time.
            Some(&i) if direct => {
                entries[i].modified = item.modified;
                entries[i].size = if is_dir { 0 } else { item.size };
            }
            Some(_) => {}
            None => {
                let (size, modified) = if direct { (if is_dir { 0 } else { item.size }, item.modified) } else { (0, 0) };
                seen.insert(key, entries.len());
                entries.push(entry(name, is_dir, size, modified, &mut types));
            }
        }
    }
    if found {
        Ok(entries)
    } else if is_a_file {
        Err(folder::NOT_A_FOLDER.to_string())
    } else {
        Err(format!("There's no folder called \u{201c}{}\u{201d} in this archive.", want.join("\\")))
    }
}

fn entry(name: &str, is_dir: bool, size: u64, modified: u64, types: &mut HashMap<String, Arc<str>>) -> Entry {
    let ext = if is_dir { String::new() } else { folder::extension(name) };
    let type_name = types.entry(if is_dir { "\\dir".to_string() } else { ext.clone() }).or_insert_with(|| icons::type_name(&ext, is_dir)).clone();
    Entry {
        name: name.to_string(),
        lower: name.to_lowercase(),
        kind: if is_dir { EntryKind::Folder } else { EntryKind::File },
        size,
        modified,
        created: 0,
        attributes: if is_dir { ATTR_DIRECTORY } else { 0 },
        ext,
        type_name,
        target: None,
        free: 0,
    }
}

// ---------------------------------------------------------------- extracting

/// Extracts these items of the archive (paths inside it; a folder brings everything under it)
/// into the real folder `dest_dir`, laid out as they are under their parent inside the archive.
/// Files already there are replaced. Returns the full paths of the top-level items written.
/// (Used for Glint's temporary folder: to open, copy or drag items out of an archive.)
pub fn extract(archive: &str, inner_items: &[String], dest_dir: &str) -> Result<Vec<String>, String> {
    let (kind, items) = list(archive)?;
    let wants: Vec<Vec<&str>> = inner_items.iter().map(|w| inner_parts(w)).collect();
    // The items each wanted path has (it and what's under it), found in one go through the archive:
    // a go for each one asked for took minutes for thousands of them.
    let mut found: HashMap<String, Vec<usize>> = wants.iter().map(|w| (folded(w), Vec::new())).collect();
    let depths: BTreeSet<usize> = wants.iter().map(Vec::len).collect();
    for (i, item) in items.iter().enumerate() {
        let parts: Vec<&str> = item.parts().collect();
        for &n in depths.iter().take_while(|&&n| n <= parts.len()) {
            if let Some(those) = found.get_mut(&folded(&parts[..n])) {
                those.push(i);
            }
        }
    }
    // (item index in `items`, where it goes under dest_dir)
    let mut plan: Vec<(usize, PathBuf)> = Vec::new();
    let mut planned = BTreeSet::new();
    let mut tops: Vec<String> = Vec::new();
    let mut top_names: HashSet<String> = HashSet::new();
    for want in &wants {
        // Everything keeps its path from the wanted item's parent down; for the root, everything.
        let keep_from = want.len().saturating_sub(1);
        let those = &found[&folded(want)];
        if those.is_empty() {
            return Err(format!("\u{201c}{}\u{201d} isn't in this archive.", want.join("\\")));
        }
        for &i in those {
            let parts: Vec<&str> = items[i].parts().collect();
            let top = folder::join(dest_dir, parts[keep_from]);
            if top_names.insert(folded(&[&top])) {
                tops.push(top);
            }
            if planned.insert(i) {
                plan.push((i, inside(dest_dir, &parts[keep_from..])?));
            }
        }
    }
    unpack(archive, kind, &items, &plan, true, &Meter::new(0))?;
    Ok(tops)
}

/// How many bytes taking these items out with [`extract`] unpacks: every file under a wanted
/// folder (the list shows folders as 0 bytes), and in a 7z also what's packed before them in their
/// block, which is unpacked (and thrown away) to get to them. So a big take-out can be told apart
/// before it starts.
pub fn unpack_size(archive: &str, inner_items: &[String]) -> Result<u64, String> {
    let wants: Vec<Vec<&str>> = inner_items.iter().map(|w| inner_parts(w)).collect();
    // Each item's path is looked up among the wanted ones (as in `extract`), not compared with each.
    let paths: HashSet<String> = wants.iter().map(|w| folded(w)).collect();
    let depths: BTreeSet<usize> = wants.iter().map(Vec::len).collect();
    let wanted = |item: &Item| {
        let parts: Vec<&str> = item.parts().collect();
        !item.is_dir && depths.iter().take_while(|&&n| n <= parts.len()).any(|&n| paths.contains(&folded(&parts[..n])))
    };
    if kind_of(archive) != Some(Kind::SevenZ) {
        let (_, items) = list(archive)?;
        return Ok(items.iter().filter(|i| wanted(i)).map(|i| i.size).sum());
    }
    let (a, _) = open_7z(archive)?;
    let block_of = |index: usize| a.stream_map.file_block_index.get(index).copied().flatten();
    // Each block that holds a wanted file, with the last wanted file in it (as in extract_7z).
    let mut blocks: HashMap<usize, usize> = HashMap::new();
    for item in list_7z(&a).iter().filter(|i| wanted(i)) {
        if let Some(b) = block_of(item.index) {
            let last = blocks.entry(b).or_insert(item.index);
            *last = (*last).max(item.index);
        }
    }
    // A block's files come one after another from its first: all of them up to that last one.
    let before_last = |index: usize| block_of(index).and_then(|b| blocks.get(&b)).is_some_and(|&last| index <= last);
    Ok(a.files.iter().enumerate().filter(|&(i, _)| before_last(i)).map(|(_, f)| f.size).sum())
}

/// Unpacks the whole archive into the folder `dest_dir` (Extract here), replacing nothing: a top
/// item whose name is taken there gets the next free one, as Windows names them ("notes (2).txt",
/// "docs (2)"). The full paths of the top items it makes go in `made` before they are made, so
/// they can be cleaned up when it stops partway (a cancel). Reports to the copy queue.
pub fn extract_all(archive: &str, dest_dir: &str, made: &mut Vec<String>) -> Result<(), String> {
    let (kind, items) = list(archive)?;
    let meter = Meter::new(items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum());
    // The archive's top names (lower case) and the names they get in `dest_dir`.
    let mut renamed: HashMap<String, String> = HashMap::new();
    let mut given: HashSet<String> = HashSet::new();
    // Where each item goes (lower case) and its place in the plan, so an archive listing the same
    // name twice writes it once: the last one, as its list shows it and a copy takes it out.
    let mut planned: HashMap<String, usize> = HashMap::new();
    let mut plan: Vec<(usize, PathBuf)> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let mut parts: Vec<&str> = item.parts().collect();
        let key = parts[0].to_lowercase();
        if !renamed.contains_key(&key) {
            let is_dir = item.is_dir || parts.len() > 1;
            let name = free_name(parts[0], is_dir, |n| given.contains(&n.to_lowercase()) || crate::fileops::exists(&folder::join(dest_dir, n)));
            given.insert(name.to_lowercase());
            made.push(folder::join(dest_dir, &name));
            renamed.insert(key.clone(), name);
        }
        parts[0] = &renamed[&key];
        let out = inside(dest_dir, &parts)?;
        match planned.get(&out.to_string_lossy().to_lowercase()) {
            Some(&at) => plan[at] = (i, out),
            None => {
                planned.insert(out.to_string_lossy().to_lowercase(), plan.len());
                plan.push((i, out));
            }
        }
    }
    unpack(archive, kind, &items, &plan, false, &meter)
}

/// Writes the planned items (index in `items`, where it goes): folders first, then the files.
/// `replace`: a file already there is replaced (otherwise that's an error, never an overwrite).
fn unpack(archive: &str, kind: Kind, items: &[Item], plan: &[(usize, PathBuf)], replace: bool, meter: &Meter) -> Result<(), String> {
    if let Some(&(i, _)) = plan.iter().find(|(i, _)| items[*i].locked && !items[*i].is_dir) {
        return Err(format!("\u{201c}{}\u{201d} is locked with a password, and Glint can't open password-protected archives yet.", folder::file_name(&items[i].path)));
    }
    // Folders first (empty ones too), and every file's folder.
    for (i, out) in plan {
        let dir = if items[*i].is_dir { Some(out.as_path()) } else { out.parent() };
        if let Some(dir) = dir {
            std::fs::create_dir_all(dir).map_err(|e| write_error(dir, &e))?;
        }
    }
    let files: Vec<(&Item, &Path)> = plan.iter().filter(|(i, _)| !items[*i].is_dir).map(|(i, out)| (&items[*i], out.as_path())).collect();
    let to = Out { replace, meter };
    match kind {
        Kind::Zip => extract_zip(archive, &files, &to)?,
        Kind::SevenZ => extract_7z(archive, &files, &to)?,
    }
    // Folders get the times they had in the archive, now that nothing more is written into them.
    for (i, out) in plan.iter().filter(|(i, _)| items[*i].is_dir) {
        if let Some(t) = filetime_to_system(items[*i].modified) {
            set_folder_time(out, t);
        }
    }
    Ok(())
}

/// How files are written out: replacing what's there or not, and where the progress goes.
struct Out<'a> {
    replace: bool,
    meter: &'a Meter,
}

/// The name a new item called `name` gets where `taken` names are in use: "notes (2).txt",
/// "docs (2)" (as Windows numbers them).
fn free_name(name: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => name.split_at(i),
        _ => (name, ""),
    };
    crate::fileops::unique_name(stem, ext, taken)
}

/// The folder Extract to puts an archive's contents in: the archive's name without .zip or .7z
/// ("Photos.zip" -> "Photos"), numbered when `taken` says it's in use ("Photos (2)").
pub fn extract_folder_name(archive_name: &str, taken: impl Fn(&str) -> bool) -> String {
    let stem = match archive_name.rfind('.') {
        Some(i) if i > 0 && is_archive_name(archive_name) => &archive_name[..i],
        _ => archive_name,
    };
    let stem = stem.trim().trim_end_matches(['.', ' ']);
    crate::fileops::unique_name(if stem.is_empty() { "Extracted" } else { stem }, "", taken)
}

/// `dest` + these (already cleaned) parts, refusing anything that isn't a plain name, so the
/// result can't point outside `dest`.
fn inside(dest: &str, parts: &[&str]) -> Result<PathBuf, String> {
    let mut out = PathBuf::from(dest);
    for part in parts {
        let mut c = Path::new(part).components();
        match (c.next(), c.next()) {
            (Some(Component::Normal(_)), None) => out.push(part),
            _ => return Err(format!("\u{201c}{part}\u{201d} isn't a name Glint can safely write.")),
        }
    }
    Ok(out)
}

fn extract_zip(archive: &str, files: &[(&Item, &Path)], to: &Out) -> Result<(), String> {
    let mut zip = open_zip(archive)?;
    for (item, out) in files {
        let name = folder::file_name(&item.path);
        let mut reader = zip.by_index(item.index).map_err(|e| match e {
            zip::result::ZipError::CompressionMethodNotSupported(_) => format!("\u{201c}{name}\u{201d} is packed in a way Glint can't unpack yet."),
            zip::result::ZipError::UnsupportedArchive(m) if m == zip::result::ZipError::PASSWORD_REQUIRED => PASSWORD.to_string(),
            zip::result::ZipError::InvalidPassword => PASSWORD.to_string(),
            zip::result::ZipError::Io(e) => format!("\u{201c}{name}\u{201d} couldn't be read from the archive ({e})."),
            _ => format!("\u{201c}{name}\u{201d} is damaged in this zip file."),
        })?;
        copy_out(&mut reader, out, item.modified, name, to)?;
    }
    Ok(())
}

fn extract_7z(archive: &str, files: &[(&Item, &Path)], to: &Out) -> Result<(), String> {
    let (a, mut source) = open_7z(archive)?;
    let mut wanted: HashMap<usize, (&Item, &Path)> = HashMap::new();
    // Each block that holds a wanted file, with the last wanted file in it.
    let mut blocks: BTreeMap<usize, usize> = BTreeMap::new();
    for &(item, out) in files {
        match a.stream_map.file_block_index.get(item.index).copied().flatten() {
            Some(b) if b < a.blocks.len() => {
                let last = blocks.entry(b).or_insert(item.index);
                *last = (*last).max(item.index);
                wanted.insert(item.index, (item, out));
            }
            // No data stored: an empty file.
            _ => copy_out(&mut io::empty(), out, item.modified, folder::file_name(&item.path), to)?,
        }
    }
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as u32;
    let password = sevenz_rust2::Password::empty();
    for (b, last_wanted) in blocks {
        // A block is one packed stream holding its files back to back: everything before a wanted
        // file has to be unpacked (and thrown away) to get to it.
        let first = a.stream_map.block_first_file_index.get(b).copied().unwrap_or(0);
        let mut index = first;
        let mut failed: Option<String> = None;
        let decoder = sevenz_rust2::BlockDecoder::new(threads, b, &a, &password, &mut source);
        let result = decoder.for_each_entries(&mut |_entry, reader: &mut dyn Read| {
            let this = index;
            index += 1;
            match wanted.get(&this) {
                Some((item, out)) => {
                    if let Err(e) = copy_out(reader, out, item.modified, folder::file_name(&item.path), to) {
                        failed = Some(e);
                        return Ok(false);
                    }
                }
                None => {
                    if let Err(e) = io::copy(reader, &mut io::sink()) {
                        failed = Some(read_error_7z(&e));
                        return Ok(false);
                    }
                }
            }
            Ok(this < last_wanted)
        });
        if let Some(msg) = failed {
            return Err(msg);
        }
        result.map_err(|e| sevenz_error(&e))?;
        // (The block ended before a wanted file: never say it's out when it isn't.)
        if index <= last_wanted {
            return Err("This 7z file is laid out in a way Glint can't read yet.".to_string());
        }
    }
    Ok(())
}

/// Copies one file's data out of the archive to `out`, then gives it the time it had in the
/// archive. A half-written file is removed again.
fn copy_out(reader: &mut dyn Read, out: &Path, modified: u64, name: &str, to: &Out) -> Result<(), String> {
    let made = if to.replace { File::create(out) } else { File::create_new(out) };
    let mut file = made.map_err(|e| write_error(out, &e))?;
    let mut buf = vec![0u8; 256 * 1024];
    let result = loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => break Err(unpack_error(name, &e)),
        };
        if let Err(e) = file.write_all(&buf[..n]) {
            break Err(write_error(out, &e));
        }
        if !to.meter.add(n as u64) {
            break Err(CANCELLED.to_string());
        }
    };
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(out);
        return result;
    }
    if let Some(t) = filetime_to_system(modified) {
        let _ = file.set_modified(t);
    }
    Ok(())
}

// ---------------------------------------------------------------- making archives

/// The kinds of archive Glint makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// Deflate, at zlib's usual level: opens everywhere, even in Windows itself.
    Zip,
    /// LZMA2, at its usual level: smaller, for 7-Zip and Glint.
    SevenZ,
}

impl Format {
    /// ".zip" or ".7z".
    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::SevenZ => ".7z",
        }
    }
}

/// Everything that goes into a new archive, found by [`plan_pack`].
pub struct Packing {
    items: Vec<Packed>,
    /// How many bytes the files hold together.
    pub total: u64,
}

/// One file or folder to pack.
struct Packed {
    path: PathBuf,
    /// Its name in the archive, `/` between parts.
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
}

/// The name a new archive of `paths` gets, without .zip / .7z: one file's name without its
/// extension ("report.pdf" -> "report"), one folder's name, or for several items the name of the
/// folder they're in ("Archive" when that's a drive).
pub fn pack_name(paths: &[String]) -> String {
    let name = match paths {
        [one] if !crate::fileops::is_folder(one) => {
            let name = folder::file_name(one);
            match name.rfind('.') {
                Some(i) if i > 0 => name[..i].to_string(),
                _ => name.to_string(),
            }
        }
        [one] => folder::file_name(one).to_string(),
        _ => paths.first().and_then(|p| folder::parent(p)).map(|p| folder::file_name(&p).to_string()).unwrap_or_default(),
    };
    let name = name.trim().trim_end_matches(['.', ' ']);
    if name.is_empty() || name.contains([':', '\\']) {
        "Archive".to_string()
    } else {
        name.to_string()
    }
}

/// Lists what an archive of `paths` holds: each item under its own name (two of the same name get
/// "(2)"), and folders with everything in them. Links to folders inside are left out (they can
/// lead round in circles). None when `stop` was set meanwhile (CANCEL).
pub fn plan_pack(paths: &[String], stop: &AtomicBool) -> Result<Option<Packing>, String> {
    let mut plan = Packing { items: Vec::new(), total: 0 };
    let mut taken: HashSet<String> = HashSet::new();
    for p in paths {
        let path = Path::new(p);
        let meta = std::fs::metadata(path).map_err(|e| read_error(path, &e))?;
        let name = free_name(folder::file_name(p), meta.is_dir(), |n| taken.contains(&n.to_lowercase()));
        taken.insert(name.to_lowercase());
        plan.add(path, name.clone(), &meta);
        if meta.is_dir() && !walk(path, &name, &mut plan, stop)? {
            return Ok(None);
        }
    }
    Ok(Some(plan))
}

impl Packing {
    fn add(&mut self, path: &Path, name: String, meta: &std::fs::Metadata) {
        let size = if meta.is_dir() { 0 } else { meta.len() };
        self.total += size;
        self.items.push(Packed { path: path.to_path_buf(), name, is_dir: meta.is_dir(), size, modified: meta.modified().ok() });
    }
}

/// Adds everything inside the folder `dir` (named `name` in the archive). False: stopped.
fn walk(dir: &Path, name: &str, plan: &mut Packing, stop: &AtomicBool) -> Result<bool, String> {
    let mut todo = vec![(dir.to_path_buf(), name.to_string())];
    while let Some((dir, name)) = todo.pop() {
        if stop.load(Ordering::SeqCst) {
            return Ok(false);
        }
        for entry in std::fs::read_dir(&dir).map_err(|e| read_error(&dir, &e))? {
            let entry = entry.map_err(|e| read_error(&dir, &e))?;
            let path = entry.path();
            let inner = format!("{name}/{}", entry.file_name().to_string_lossy());
            let kind = entry.file_type().map_err(|e| read_error(&path, &e))?;
            let meta = if kind.is_symlink() {
                // A link to a file brings the file; a link to a folder is left out.
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_file() => m,
                    _ => continue,
                }
            } else {
                entry.metadata().map_err(|e| read_error(&path, &e))?
            };
            plan.add(&path, inner.clone(), &meta);
            if meta.is_dir() {
                todo.push((path, inner));
            }
        }
    }
    Ok(true)
}

/// Makes the archive `out` (a new file: nothing is ever replaced) from `plan`, with each file's
/// time. Reports to the copy queue as it goes. When it fails or is cancelled, the half-made file
/// is removed again.
pub fn pack(plan: &Packing, out: &str, format: Format) -> Result<(), String> {
    let path = Path::new(out);
    let file = File::create_new(path).map_err(|e| write_error(path, &e))?;
    let meter = Meter::new(plan.total);
    let result = match format {
        Format::Zip => pack_zip(plan, file, path, &meter),
        Format::SevenZ => pack_7z(plan, file, path, &meter),
    };
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn pack_zip(plan: &Packing, file: File, out: &Path, meter: &Meter) -> Result<(), String> {
    use zip::write::SimpleFileOptions;
    let zip_error = |e: zip::result::ZipError| match e {
        zip::result::ZipError::Io(e) => write_error(out, &e),
        e => format!("\u{201c}{}\u{201d} couldn't be written ({e}).", out.display()),
    };
    let mut zip = zip::ZipWriter::new(BufWriter::new(file));
    let mut buf = vec![0u8; 256 * 1024];
    for p in &plan.items {
        let mut opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).compression_level(Some(6));
        if let Some(t) = p.modified.and_then(dos_time) {
            opts = opts.last_modified_time(t);
        }
        if p.is_dir {
            zip.add_directory(p.name.as_str(), opts).map_err(zip_error)?;
            continue;
        }
        zip.start_file(p.name.as_str(), opts.large_file(p.size >= ZIP64_FROM)).map_err(zip_error)?;
        let mut feed = Feed::new(&p.path, meter);
        loop {
            let n = match feed.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => return Err(meter.problem.take().unwrap_or_else(|| read_error(&p.path, &io::Error::other("read failed")))),
            };
            zip.write_all(&buf[..n]).map_err(|e| write_error(out, &e))?;
        }
    }
    let mut w = zip.finish().map_err(zip_error)?;
    w.flush().map_err(|e| write_error(out, &e))
}

fn pack_7z(plan: &Packing, file: File, out: &Path, meter: &Meter) -> Result<(), String> {
    use sevenz_rust2::encoder_options::Lzma2Options;
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderConfiguration};
    let seven_error = |e: sevenz_rust2::Error| meter.problem.take().unwrap_or_else(|| format!("\u{201c}{}\u{201d} couldn't be written ({e}).", out.display()));
    let mut w = ArchiveWriter::new(BufWriter::new(file)).map_err(seven_error)?;
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(4)) as u32;
    // (Chunk size 0: the smallest the level allows, so each thread holds little.)
    w.set_content_methods(vec![EncoderConfiguration::from(Lzma2Options::from_level_mt(6, threads, 0))]);
    // Files go in solid blocks of up to SOLID_BLOCK bytes, packed together.
    let mut block: Vec<&Packed> = Vec::new();
    let mut bytes = 0;
    for p in &plan.items {
        if p.is_dir || p.size == 0 {
            w.push_archive_entry::<&[u8]>(ArchiveEntry::from_path(&p.path, p.name.clone()), None).map_err(seven_error)?;
            continue;
        }
        block.push(p);
        bytes += p.size;
        if bytes >= SOLID_BLOCK {
            push_block(&mut w, &mut block, meter).map_err(seven_error)?;
            bytes = 0;
        }
    }
    push_block(&mut w, &mut block, meter).map_err(seven_error)?;
    let mut f = w.finish().map_err(|e| meter.problem.take().unwrap_or_else(|| write_error(out, &e)))?;
    f.flush().map_err(|e| write_error(out, &e))
}

/// Packs these files together as one solid block of the 7z.
fn push_block<W: Write + io::Seek>(w: &mut sevenz_rust2::ArchiveWriter<W>, block: &mut Vec<&Packed>, meter: &Meter) -> Result<(), sevenz_rust2::Error> {
    if block.is_empty() {
        return Ok(());
    }
    let entries = block.iter().map(|p| sevenz_rust2::ArchiveEntry::from_path(&p.path, p.name.clone())).collect();
    let feeds = block.iter().map(|p| sevenz_rust2::SourceReader::new(Feed::new(&p.path, meter))).collect();
    block.clear();
    w.push_archive_entries(entries, feeds).map(|_| ())
}

/// How far unpacking or packing is, for the copy queue's list (copyqueue.rs) - and whether it was
/// cancelled. Outside the queue (extracting to Glint's temporary folder) it never stops anything.
struct Meter {
    /// Bytes to go through in all (0: unknown).
    total: u64,
    done: Cell<u64>,
    /// What went wrong reading a file, in plain words (the packers' own errors say less).
    problem: RefCell<Option<String>>,
}

impl Meter {
    fn new(total: u64) -> Meter {
        Meter { total, done: Cell::new(0), problem: RefCell::new(None) }
    }

    /// `n` more bytes are done. Holds here while paused. False: cancelled.
    fn add(&self, n: u64) -> bool {
        let done = self.done.get() + n;
        self.done.set(done);
        if self.total == 0 {
            return crate::copyqueue::between_items();
        }
        crate::copyqueue::progress(1_000_000, (done.min(self.total) as u128 * 1_000_000 / self.total as u128) as u32)
    }
}

/// A file being packed: opened when first read (a block can hold thousands), closed when read to
/// its end. Read problems and CANCEL are kept in the meter, in plain words.
struct Feed<'a> {
    path: &'a Path,
    file: Option<File>,
    ended: bool,
    meter: &'a Meter,
}

impl<'a> Feed<'a> {
    fn new(path: &'a Path, meter: &'a Meter) -> Feed<'a> {
        Feed { path, file: None, ended: false, meter }
    }

    fn fail(&self, why: String) -> io::Error {
        self.meter.problem.replace(Some(why));
        io::Error::other("stopped")
    }
}

impl Read for Feed<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.ended {
            return Ok(0);
        }
        if self.file.is_none() {
            match File::open(self.path) {
                Ok(f) => self.file = Some(f),
                Err(e) => return Err(self.fail(read_error(self.path, &e))),
            }
        }
        let n = match self.file.as_mut().map(|f| f.read(buf)) {
            Some(Ok(n)) => n,
            Some(Err(e)) => return Err(self.fail(read_error(self.path, &e))),
            None => 0,
        };
        if n == 0 {
            self.ended = true;
            self.file = None;
        } else if !self.meter.add(n as u64) {
            return Err(self.fail(CANCELLED.to_string()));
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------- temp folder

/// A new, empty folder for one take-out from this archive (to open its items, copy them or drag
/// them out). It's numbered (1, 2...) inside the archive's own folder in %TEMP%\Glint\archives,
/// named after the archive's path and when it last changed. Each take-out has its own, so two
/// folders of the same name from different places in the archive aren't mixed together, and a
/// file still open from an earlier take-out isn't in the way.
pub fn temp_dir_for(archive: &str) -> Result<String, String> {
    let meta = std::fs::metadata(archive).map_err(|e| open_error(&e))?;
    let changed = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
    // FNV-1a: tiny, and the same on every run (unlike std's hasher).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in archive.to_lowercase().bytes().chain(changed.to_le_bytes()) {
        hash = (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3);
    }
    let dir = std::env::temp_dir().join("Glint").join("archives").join(format!("{:012x}", hash >> 16));
    std::fs::create_dir_all(&dir).map_err(|e| write_error(&dir, &e))?;
    // The earlier take-outs stay (Windows clears %TEMP% in time), so counting starts past them.
    let mut n = std::fs::read_dir(&dir).map_or(0, |d| d.count()) + 1;
    loop {
        let fresh = dir.join(n.to_string());
        match std::fs::create_dir(&fresh) {
            Ok(()) => return Ok(fresh.to_string_lossy().into_owned()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(write_error(&fresh, &e)),
        }
    }
}

// ---------------------------------------------------------------- times and errors

/// A zip's time (local time, two-second steps, as MS-DOS kept it) as a FILETIME; 0 if it's not a
/// real date.
fn local_to_filetime(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> u64 {
    if !(1980..=2107).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return 0;
    }
    let local = SYSTEMTIME { wYear: year, wMonth: month as u16, wDay: day as u16, wHour: hour as u16, wMinute: minute as u16, wSecond: second as u16, ..Default::default() };
    let mut utc = SYSTEMTIME::default();
    let mut ft = FILETIME::default();
    // SAFETY: plain value structs, all valid for the calls.
    unsafe {
        if TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).is_err() || SystemTimeToFileTime(&utc, &mut ft).is_err() {
            return 0;
        }
    }
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// 1601 to 1970 in 100 ns steps (FILETIME counts from 1601).
const UNIX_START: u64 = 116_444_736_000_000_000;

fn filetime_to_system(ft: u64) -> Option<SystemTime> {
    let since = ft.checked_sub(UNIX_START)?;
    UNIX_EPOCH.checked_add(Duration::from_nanos(since.checked_mul(100)?))
}

/// A file's time as a zip stores it (local time, two-second steps); None before 1980 or after 2107.
fn dos_time(t: SystemTime) -> Option<zip::DateTime> {
    let ft = u64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_nanos() / 100).ok()?.checked_add(UNIX_START)?;
    let ft = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: plain value structs, all valid for the calls.
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    let small = |v: u16| u8::try_from(v).ok();
    zip::DateTime::from_date_and_time(local.wYear, small(local.wMonth)?, small(local.wDay)?, small(local.wHour)?, small(local.wMinute)?, small(local.wSecond)?).ok()
}

/// Gives a folder a time (folders open only with "backup semantics" on Windows).
fn set_folder_time(dir: &Path, t: SystemTime) {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    if let Ok(f) = std::fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir) {
        let _ = f.set_modified(t);
    }
}

fn open_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "This archive isn't there any more.".to_string(),
        io::ErrorKind::PermissionDenied => "Glint isn't allowed to open this archive.".to_string(),
        _ => format!("This archive couldn't be opened ({e})."),
    }
}

fn read_error(path: &Path, e: &io::Error) -> String {
    let path = path.display();
    match e.kind() {
        io::ErrorKind::NotFound => format!("\u{201c}{path}\u{201d} isn't there any more."),
        io::ErrorKind::PermissionDenied => format!("Glint isn't allowed to read \u{201c}{path}\u{201d}."),
        _ => format!("\u{201c}{path}\u{201d} couldn't be read ({e})."),
    }
}

fn write_error(path: &Path, e: &io::Error) -> String {
    let path = path.display();
    match e.kind() {
        io::ErrorKind::PermissionDenied => format!("Glint isn't allowed to write \u{201c}{path}\u{201d}."),
        io::ErrorKind::StorageFull => format!("There's no room left to write \u{201c}{path}\u{201d}."),
        _ => format!("\u{201c}{path}\u{201d} couldn't be written ({e})."),
    }
}

/// Reading a file's data failed partway: the archive is damaged (or, for a 7z, locked).
fn unpack_error(name: &str, e: &io::Error) -> String {
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<sevenz_rust2::Error>()) {
        return sevenz_error(inner);
    }
    format!("\u{201c}{name}\u{201d} is damaged in this archive and couldn't be unpacked.")
}

fn read_error_7z(e: &io::Error) -> String {
    match e.get_ref().and_then(|i| i.downcast_ref::<sevenz_rust2::Error>()) {
        Some(inner) => sevenz_error(inner),
        None => DAMAGED_7Z.to_string(),
    }
}

fn sevenz_error(e: &sevenz_rust2::Error) -> String {
    use sevenz_rust2::Error as E;
    match e {
        E::PasswordRequired | E::MaybeBadPassword(_) => PASSWORD.to_string(),
        E::UnsupportedCompressionMethod(m) if m.to_ascii_lowercase().contains("aes") => PASSWORD.to_string(),
        E::UnsupportedCompressionMethod(_) | E::ExternalUnsupported | E::Unsupported(_) => "This 7z file uses a kind of packing Glint can't read yet.".to_string(),
        E::BadSignature(_) => "This isn't really a 7z file, or it's damaged.".to_string(),
        E::FileOpen(io, _) => open_error(io),
        E::MaxMemLimited { .. } => "This 7z file needs more memory to unpack than Glint can give it.".to_string(),
        _ => DAMAGED_7Z.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("glint-archive-tests").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    fn make_zip(path: &Path) {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(File::create(path).expect("create"));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.start_file("readme.txt", opts).expect("start");
        w.write_all(b"hello readme").expect("write");
        w.add_directory("docs/", opts).expect("dir");
        w.start_file("docs/a.txt", opts).expect("start");
        w.write_all(&b"aaaa".repeat(1000)).expect("write");
        w.add_directory("empty/", opts).expect("dir");
        // Folders only implied by this name: src, src\main.
        w.start_file("src/main/b.rs", opts).expect("start");
        w.write_all(b"fn main() {}").expect("write");
        w.start_file("../evil.txt", opts).expect("start");
        w.write_all(b"evil").expect("write");
        w.start_file("C:/abs.txt", opts).expect("start");
        w.write_all(b"evil").expect("write");
        w.finish().expect("finish");
    }

    fn make_7z(path: &Path) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader};
        let mut w = ArchiveWriter::create(path).expect("create");
        w.push_archive_entry(ArchiveEntry::new_file("readme.txt"), Some(&b"hello seven"[..])).expect("file");
        w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory("docs"), None).expect("dir");
        w.push_archive_entry(ArchiveEntry::new_file("docs/a.txt"), Some(&b"aaaa"[..])).expect("file");
        w.push_archive_entry(ArchiveEntry::new_file("docs/zero.txt"), Some(&b""[..])).expect("file");
        // Solid: three files in one block, the middle one implying deep\er.
        w.push_archive_entries(
            vec![ArchiveEntry::new_file("deep/x.txt"), ArchiveEntry::new_file("deep/er/y.txt"), ArchiveEntry::new_file("deep/z.txt")],
            vec![SourceReader::new(&b"xx"[..]), SourceReader::new(&b"yyy"[..]), SourceReader::new(&b"zzzz"[..])],
        )
        .expect("solid");
        w.finish().expect("finish");
    }

    fn names(f: &Folder) -> Vec<String> {
        let mut v: Vec<String> = f.entries.iter().map(|e| format!("{}{}", e.name, if e.kind == EntryKind::Folder { "\\" } else { "" })).collect();
        v.sort();
        v
    }

    #[test]
    fn archive_split() {
        let dir = scratch("split");
        let zip = dir.join("A.ZIP");
        make_zip(&zip);
        let z = s(&zip);
        assert_eq!(split(&z), Some((z.clone(), String::new())));
        assert_eq!(split(&format!("{z}\\docs\\a.txt")), Some((z.clone(), "docs\\a.txt".to_string())));
        assert_eq!(split(&format!("{z}\\docs\\")), Some((z.clone(), "docs".to_string())));
        assert_eq!(split(&s(&dir)), None);
        assert_eq!(split("C:\\Windows\\notepad.exe"), None);
        // A folder named like an archive is just a folder.
        std::fs::create_dir_all(dir.join("folder.7z")).expect("mkdir");
        assert_eq!(split(&s(&dir.join("folder.7z").join("x"))), None);
        assert!(is_archive_name("x.Zip") && is_archive_name("y.7Z") && !is_archive_name("zip") && !is_archive_name("a.rar"));
    }

    #[test]
    fn archive_zip_listing() {
        let dir = scratch("ziplist");
        let zip = dir.join("t.zip");
        make_zip(&zip);
        let z = s(&zip);
        let root = read(&z, "", &z);
        assert_eq!(root.error, None);
        // "../evil.txt" is left out; "C:/abs.txt" stays inside the folder, as C_\abs.txt.
        assert_eq!(names(&root), ["C_\\", "docs\\", "empty\\", "readme.txt", "src\\"]);
        let readme = root.entries.iter().find(|e| e.name == "readme.txt").expect("readme");
        assert_eq!(readme.size, 12);
        assert!(readme.modified > 0);
        assert_eq!(names(&read(&z, "docs", "")), ["a.txt"]);
        assert_eq!(read(&z, "DOCS", "").entries[0].size, 4000);
        assert_eq!(names(&read(&z, "src", "")), ["main\\"]);
        assert_eq!(names(&read(&z, "src\\main", "")), ["b.rs"]);
        assert!(read(&z, "empty", "").entries.is_empty() && read(&z, "empty", "").error.is_none());
        assert!(read(&z, "nope", "").error.is_some());
        assert_eq!(read(&z, "readme.txt", "").error.as_deref(), Some(folder::NOT_A_FOLDER));
    }

    #[test]
    fn archive_zip_extract() {
        let dir = scratch("zipx");
        let zip = dir.join("t.zip");
        make_zip(&zip);
        let z = s(&zip);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("mkdir");
        let o = s(&out);
        let tops = extract(&z, &["docs\\a.txt".to_string()], &o).expect("extract file");
        assert_eq!(tops, [s(&out.join("a.txt"))]);
        assert_eq!(std::fs::read(out.join("a.txt")).expect("read"), b"aaaa".repeat(1000));
        let tops = extract(&z, &["src".to_string(), "empty".to_string()], &o).expect("extract folder");
        assert_eq!(tops, [s(&out.join("src")), s(&out.join("empty"))]);
        assert_eq!(std::fs::read(out.join("src").join("main").join("b.rs")).expect("read"), b"fn main() {}");
        assert!(out.join("empty").is_dir());
        assert_eq!(unpack_size(&z, &["src".to_string(), "docs".to_string()]), Ok(12 + 4000));
        // The names that climb out can't be asked for, and nothing landed outside.
        assert!(extract(&z, &["..\\evil.txt".to_string()], &o).is_err());
        assert!(extract(&z, &["C:\\abs.txt".to_string()], &o).is_err());
        assert!(!dir.join("evil.txt").exists());
        // The whole archive (C:/abs.txt comes out as C_\abs.txt).
        let all = dir.join("all");
        std::fs::create_dir_all(&all).expect("mkdir");
        let tops = extract(&z, &[String::new()], &s(&all)).expect("extract all");
        assert_eq!(tops.len(), 5);
        assert!(all.join("readme.txt").is_file() && !dir.join("evil.txt").exists());
    }

    #[test]
    fn archive_zip_password_and_damage() {
        let dir = scratch("zipbad");
        let zip = dir.join("t.zip");
        make_zip(&zip);
        let bytes = std::fs::read(&zip).expect("read");
        // Mark every entry as encrypted (bit 0 of the flags, in both the local and central headers).
        let mut locked = bytes.clone();
        for i in 0..locked.len().saturating_sub(10) {
            if locked[i..i + 4] == *b"PK\x03\x04" {
                locked[i + 6] |= 1;
            } else if locked[i..i + 4] == *b"PK\x01\x02" {
                locked[i + 8] |= 1;
            }
        }
        let lz = dir.join("locked.zip");
        std::fs::write(&lz, &locked).expect("write");
        let l = s(&lz);
        assert_eq!(names(&read(&l, "", "")).len(), 5);
        let err = extract(&l, &["readme.txt".to_string()], &s(&dir)).expect_err("needs a password");
        assert!(err.contains("password"), "{err}");

        // Cut in half, garbage, empty, and the data of one file scrambled.
        for (name, data) in [("half.zip", bytes[..bytes.len() / 2].to_vec()), ("junk.zip", vec![0x55; 3000]), ("empty.zip", Vec::new())] {
            let p = dir.join(name);
            std::fs::write(&p, data).expect("write");
            assert_eq!(read(&s(&p), "", "").error.as_deref(), Some(DAMAGED_ZIP), "{name}");
            assert!(extract(&s(&p), &["readme.txt".to_string()], &s(&dir)).is_err());
        }
        let mut scrambled = bytes.clone();
        let at = scrambled.windows(4).position(|w| w == b"PK\x03\x04").expect("header") + 30 + "readme.txt".len();
        for b in &mut scrambled[at..at + 8] {
            *b ^= 0xA5;
        }
        let p = dir.join("scrambled.zip");
        std::fs::write(&p, scrambled).expect("write");
        assert!(extract(&s(&p), &["readme.txt".to_string()], &s(&dir)).is_err());
        assert!(!dir.join("readme.txt").exists(), "a half-written file is removed");
        assert!(read(&s(&dir.join("missing.zip")), "", "").error.is_some());
    }

    #[test]
    fn archive_7z() {
        let dir = scratch("seven");
        let p = dir.join("t.7z");
        make_7z(&p);
        let a = s(&p);
        let root = read(&a, "", &a);
        assert_eq!(root.error, None);
        assert_eq!(names(&root), ["deep\\", "docs\\", "readme.txt"]);
        assert_eq!(names(&read(&a, "docs", "")), ["a.txt", "zero.txt"]);
        assert_eq!(names(&read(&a, "deep", "")), ["er\\", "x.txt", "z.txt"]);
        assert_eq!(names(&read(&a, "deep\\er", "")), ["y.txt"]);
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("mkdir");
        let o = s(&out);
        extract(&a, &["readme.txt".to_string()], &o).expect("file");
        assert_eq!(std::fs::read(out.join("readme.txt")).expect("read"), b"hello seven");
        extract(&a, &["deep\\er".to_string(), "docs".to_string()], &o).expect("folders");
        assert_eq!(std::fs::read(out.join("er").join("y.txt")).expect("read"), b"yyy");
        assert_eq!(std::fs::read(out.join("docs").join("a.txt")).expect("read"), b"aaaa");
        assert_eq!(std::fs::read(out.join("docs").join("zero.txt")).expect("read"), b"");
        extract(&a, &["deep\\z.txt".to_string()], &o).expect("last in a solid block");
        assert_eq!(std::fs::read(out.join("z.txt")).expect("read"), b"zzzz");
        // What a take-out unpacks: a folder's files, and in a solid block what comes before them.
        assert_eq!(unpack_size(&a, &["docs".to_string()]), Ok(4));
        assert_eq!(unpack_size(&a, &["deep\\er".to_string()]), Ok(2 + 3));
        assert_eq!(unpack_size(&a, &["deep\\z.txt".to_string(), "readme.txt".to_string()]), Ok(2 + 3 + 4 + 11));

        let bytes = std::fs::read(&p).expect("read");
        for (name, data) in [("half.7z", bytes[..bytes.len() / 2].to_vec()), ("junk.7z", vec![0x55; 3000]), ("tail.7z", { let mut b = bytes.clone(); let n = b.len(); for x in &mut b[n - 40..] { *x ^= 0x5A; } b })] {
            let q = dir.join(name);
            std::fs::write(&q, data).expect("write");
            assert!(read(&s(&q), "", "").error.is_some(), "{name}");
        }
        // Scrambled packed data: listing works (the header is fine), unpacking fails cleanly.
        let mut packed = bytes.clone();
        for b in &mut packed[32..40] {
            *b ^= 0xA5;
        }
        let q = dir.join("packed.7z");
        std::fs::write(&q, packed).expect("write");
        if read(&s(&q), "", "").error.is_none() {
            assert!(extract(&s(&q), &["readme.txt".to_string()], &s(&dir)).is_err());
        }
    }

    #[test]
    fn archive_device_names() {
        assert_eq!(clean_name("docs/NUL").as_deref(), Some("docs\\_NUL"));
        assert_eq!(clean_name("com1.txt").as_deref(), Some("_com1.txt"));
        assert_eq!(clean_name("aux .tar.gz").as_deref(), Some("_aux .tar.gz"));
        assert_eq!(clean_name("con./x").as_deref(), Some("_con.\\x"));
        assert_eq!(clean_name("lpt\u{b9}").as_deref(), Some("_lpt\u{b9}"));
        assert_eq!(clean_name("nul/conout$").as_deref(), Some("_nul\\_conout$"));
        for name in ["null.txt", "com10", "comx", "lpt", "console", "a/nul_b", "auxiliary.c", "conin"] {
            assert_eq!(clean_name(name).as_deref(), Some(name.replace('/', "\\").as_str()), "{name}");
        }
    }

    #[test]
    fn archive_names_windows_does_not_allow() {
        assert_eq!(clean_name("logs/2024-05-01 10:30.log").as_deref(), Some("logs\\2024-05-01 10_30.log"));
        assert_eq!(clean_name("C:/abs.txt").as_deref(), Some("C_\\abs.txt"));
        assert_eq!(clean_name("what?<now>|\"*.txt").as_deref(), Some("what__now____.txt"));
        assert_eq!(clean_name("a\u{1}b\0").as_deref(), Some("a_b_"));
        assert_eq!(clean_name("x/../y"), None);
        assert_eq!(zip_name("caf\u{e9}/\u{65e5}.txt".as_bytes()), "caf\u{e9}/\u{65e5}.txt");
        // Not UTF-8: read in the PC's old code page, so nothing is lost.
        let old = zip_name(b"\x80bc.txt");
        assert!(old.ends_with("bc.txt") && old.chars().count() == 7, "{old}");
        // (Unless that code page is UTF-8 itself, as Windows' "use Unicode UTF-8" option and Wine make it.)
        // SAFETY: plain call, no arguments.
        if unsafe { windows::Win32::Globalization::GetOEMCP() } != windows::Win32::Globalization::CP_UTF8 {
            assert!(!old.contains('\u{fffd}'), "{old}");
        }
    }

    /// A small folder tree to pack: text, a bigger file, an empty file, an empty folder, a
    /// folder in a folder, and an old date.
    fn make_tree(root: &Path) -> PathBuf {
        let top = root.join("Stuff");
        std::fs::create_dir_all(top.join("sub").join("deeper")).expect("mkdir");
        std::fs::create_dir_all(top.join("empty")).expect("mkdir");
        std::fs::write(top.join("a.txt"), b"hello archive").expect("write");
        std::fs::write(top.join("sub").join("big.bin"), (0..200_000u32).flat_map(|i| (i % 251).to_le_bytes()).collect::<Vec<u8>>()).expect("write");
        std::fs::write(top.join("sub").join("deeper").join("zero.txt"), b"").expect("write");
        let old = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        File::options().write(true).open(top.join("a.txt")).expect("open").set_modified(old).expect("time");
        top
    }

    /// Every file and folder under `dir` as (path inside, contents; None for folders), sorted.
    fn tree(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in std::fs::read_dir(&d).expect("read dir") {
                let p = e.expect("entry").path();
                let rel = s(p.strip_prefix(dir).expect("inside"));
                if p.is_dir() {
                    out.push((rel, None));
                    todo.push(p);
                } else {
                    out.push((rel, Some(std::fs::read(&p).expect("read"))));
                }
            }
        }
        out.sort();
        out
    }

    fn modified(p: &Path) -> u64 {
        std::fs::metadata(p).expect("meta").modified().expect("time").duration_since(UNIX_EPOCH).expect("after 1970").as_secs()
    }

    #[test]
    fn archive_pack_and_unpack_round_trip() {
        let dir = scratch("roundtrip");
        let top = make_tree(&dir);
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
            assert_eq!(plan.total, 13 + 800_000);
            let out = dir.join(format!("Stuff{}", format.ext()));
            pack(&plan, &s(&out), format).expect("pack");
            // Nothing is replaced: packing to the same name again fails, and leaves the archive.
            assert!(pack(&plan, &s(&out), format).is_err());
            assert!(out.is_file());
            // Glint reads it like any other.
            assert_eq!(names(&read(&s(&out), "Stuff", "")), ["a.txt", "empty\\", "sub\\"]);
            let back = dir.join(format!("back{}", format.ext().replace('.', "-")));
            std::fs::create_dir_all(&back).expect("mkdir");
            let mut made = Vec::new();
            extract_all(&s(&out), &s(&back), &mut made).expect("extract");
            assert_eq!(made, [s(&back.join("Stuff"))]);
            assert_eq!(tree(&back.join("Stuff")), tree(&top), "{format:?}");
            // The file's date came along (a zip keeps it to two seconds).
            assert!(modified(&back.join("Stuff").join("a.txt")).abs_diff(1_500_000_000) <= 2, "{format:?}");
        }
    }

    #[test]
    fn archive_names_are_numbered_never_replaced() {
        let dir = scratch("numbered");
        let top = make_tree(&dir);
        std::fs::write(dir.join("note.txt"), b"one").expect("write");
        let never = AtomicBool::new(false);
        let plan = plan_pack(&[s(&top), s(&dir.join("note.txt"))], &never).expect("plan").expect("not stopped");
        let zip = dir.join("x.zip");
        pack(&plan, &s(&zip), Format::Zip).expect("pack");
        let out = dir.join("out");
        std::fs::create_dir_all(&out).expect("mkdir");
        std::fs::write(out.join("note.txt"), b"already here").expect("write");
        for _ in 0..2 {
            extract_all(&s(&zip), &s(&out), &mut Vec::new()).expect("extract");
        }
        assert_eq!(std::fs::read(out.join("note.txt")).expect("read"), b"already here", "not replaced");
        assert_eq!(std::fs::read(out.join("note (2).txt")).expect("read"), b"one");
        assert_eq!(std::fs::read(out.join("note (3).txt")).expect("read"), b"one");
        assert!(out.join("Stuff").join("sub").join("big.bin").is_file() && out.join("Stuff (2)").join("a.txt").is_file());

        // Two items of the same name (from different folders) both go in.
        std::fs::create_dir_all(dir.join("other")).expect("mkdir");
        std::fs::write(dir.join("other").join("note.txt"), b"two").expect("write");
        let plan = plan_pack(&[s(&dir.join("note.txt")), s(&dir.join("other").join("note.txt"))], &never).expect("plan").expect("not stopped");
        let names: Vec<&str> = plan.items.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["note.txt", "note (2).txt"]);

        let taken = |n: &str| ["Photos", "Photos (2)"].contains(&n);
        assert_eq!(extract_folder_name("Photos.zip", taken), "Photos (3)");
        assert_eq!(extract_folder_name("my.backup.7Z", |_| false), "my.backup");
        assert_eq!(extract_folder_name(".zip", |_| false), ".zip");
        assert_eq!(free_name("a.tar.gz", false, |n| n == "a.tar.gz"), "a.tar (2).gz");
        assert_eq!(free_name("v1.2", true, |n| n == "v1.2"), "v1.2 (2)");
        assert_eq!(pack_name(&[s(&dir.join("note.txt"))]), "note");
        assert_eq!(pack_name(&[s(&top)]), "Stuff");
        assert_eq!(pack_name(&[s(&top), s(&dir.join("note.txt"))]), "numbered");
        assert_eq!(pack_name(&["C:\\a.txt".to_string(), "C:\\b.txt".to_string()]), "Archive");
    }

    #[test]
    fn archive_a_cancelled_pack_leaves_nothing() {
        let dir = scratch("cancel");
        let top = make_tree(&dir);
        let never = AtomicBool::new(false);
        let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
        let stop = AtomicBool::new(true);
        assert!(plan_pack(&[s(&top)], &stop).expect("plan").is_none(), "stopped while looking");
        let label = "archive test: cancelled pack".to_string();
        let ticket = crate::copyqueue::Ticket::join(label.clone());
        assert!(ticket.wait_turn());
        let id = crate::copyqueue::shown().into_iter().find(|t| t.label == label).expect("listed").id;
        crate::copyqueue::cancel(id);
        for format in [Format::Zip, Format::SevenZ] {
            let out = dir.join(format!("gone{}", format.ext()));
            assert_eq!(pack(&plan, &s(&out), format).expect_err("cancelled"), CANCELLED);
            assert!(!out.exists(), "the half-made file is removed");
        }
        drop(ticket);
    }

    #[test]
    fn archive_temp_dir() {
        let dir = scratch("temp");
        let p = dir.join("t.zip");
        make_zip(&p);
        let t = temp_dir_for(&s(&p)).expect("temp");
        assert!(Path::new(&t).is_dir());
        // Each take-out gets a new, empty folder, next to the earlier ones.
        std::fs::write(Path::new(&t).join("a.txt"), b"a").expect("write");
        let again = temp_dir_for(&s(&p)).expect("again");
        assert_ne!(t, again);
        assert_eq!(Path::new(&t).parent(), Path::new(&again).parent());
        assert!(std::fs::read_dir(&again).expect("list").next().is_none());
        assert!(t.contains("Glint\\archives\\"));
    }

    // ---------------------------------------------------------------- awkward archives

    /// A fresh folder for one test under the temporary folder (with the process id in its name),
    /// removed again at the end.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("glint-archive-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Scratch(dir)
        }

        fn join(&self, part: &str) -> PathBuf {
            self.0.join(part)
        }

        /// A new, empty folder in it.
        fn dir(&self, part: &str) -> PathBuf {
            let dir = self.0.join(part);
            std::fs::create_dir_all(&dir).expect("mkdir");
            dir
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A zip of these entries, named exactly so (a name ending in `/` or `\` is a folder).
    fn zip_with(path: &Path, entries: &[(&str, &[u8])]) {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(File::create(path).expect("create"));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            if name.ends_with(['/', '\\']) {
                w.add_directory(*name, opts).expect("dir");
            } else {
                w.start_file(*name, opts).expect("start");
                w.write_all(data).expect("write");
            }
        }
        w.finish().expect("finish");
    }

    /// A 7z of these entries, named exactly so (a name ending in `/` is a folder), each file
    /// packed on its own.
    fn seven_with(path: &Path, entries: &[(&str, &[u8])]) {
        use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
        let mut w = ArchiveWriter::create(path).expect("create");
        for (name, data) in entries {
            match name.strip_suffix('/') {
                Some(dir) => {
                    w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory(dir), None).expect("dir");
                }
                None => {
                    w.push_archive_entry(ArchiveEntry::new_file(name), Some(*data)).expect("file");
                }
            }
        }
        w.finish().expect("finish");
    }

    /// Either kind of archive of these entries.
    fn archive_with(path: &Path, entries: &[(&str, &[u8])]) {
        match kind_of(&s(path)) {
            Some(Kind::Zip) => zip_with(path, entries),
            _ => seven_with(path, entries),
        }
    }

    /// Replaces every `from` in the file with `to` (as long), as a hex editor would.
    fn patch(path: &Path, from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len());
        let mut bytes = std::fs::read(path).expect("read");
        let mut i = 0;
        let mut patched = 0;
        while i + from.len() <= bytes.len() {
            if &bytes[i..i + from.len()] == from {
                bytes[i..i + from.len()].copy_from_slice(to);
                patched += 1;
            }
            i += 1;
        }
        assert!(patched > 0, "nothing to patch");
        std::fs::write(path, bytes).expect("write");
    }

    /// What `tree` should find: (path inside, contents; None for folders), sorted.
    fn expected(items: &[(&str, Option<&[u8]>)]) -> Vec<(String, Option<Vec<u8>>)> {
        let mut v: Vec<(String, Option<Vec<u8>>)> = items.iter().map(|(p, d)| (p.to_string(), d.map(<[u8]>::to_vec))).collect();
        v.sort();
        v
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).expect("list").map(|e| e.expect("entry").file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn a_zip_named_with_backslashes_opens_like_one_named_with_slashes() {
        // Old Windows zip tools wrote `\` between folders, and folders as "docs\empty\".
        let t = Scratch::new("backslash");
        let zip = t.join("old.zip");
        zip_with(&zip, &[("docs\\a.txt", b"a"), ("docs\\sub\\b.txt", b"bb"), ("docs\\empty\\", b""), ("top.txt", b"top")]);
        let z = s(&zip);
        assert_eq!(names(&read(&z, "", "")), ["docs\\", "top.txt"]);
        assert_eq!(names(&read(&z, "docs", "")), ["a.txt", "empty\\", "sub\\"]);
        let empty = read(&z, "docs\\empty", "");
        assert!(empty.error.is_none() && empty.entries.is_empty(), "{:?}", empty.error);
        let out = t.dir("out");
        extract_all(&z, &s(&out), &mut Vec::new()).expect("extract");
        let want = expected(&[
            ("docs", None),
            ("docs\\a.txt", Some(b"a")),
            ("docs\\empty", None),
            ("docs\\sub", None),
            ("docs\\sub\\b.txt", Some(b"bb")),
            ("top.txt", Some(b"top")),
        ]);
        assert_eq!(tree(&out), want);
    }

    #[test]
    fn names_that_climb_out_or_point_elsewhere_never_land_outside() {
        let t = Scratch::new("slip");
        let tricks: &[(&str, &[u8])] = &[
            ("../up1.txt", b"x"),
            ("..\\up2.txt", b"x"),
            ("a/../../up3.txt", b"x"),
            ("a\\..\\..\\up4.txt", b"x"),
            ("/abs.txt", b"1"),
            ("\\\\server\\share\\unc.txt", b"2"),
            ("C:\\Windows\\drive.txt", b"3"),
            ("D:rel.txt", b"4"),
            ("\\\\?\\C:\\long.txt", b"5"),
            ("./dot.txt", b"6"),
            ("x/./y.txt", b"7"),
            ("ads.txt::$DATA", b"8"),
        ];
        // What stays: every name inside the folder it's taken out to.
        let want = expected(&[
            ("abs.txt", Some(b"1")),
            ("server", None),
            ("server\\share", None),
            ("server\\share\\unc.txt", Some(b"2")),
            ("C_", None),
            ("C_\\Windows", None),
            ("C_\\Windows\\drive.txt", Some(b"3")),
            ("D_rel.txt", Some(b"4")),
            ("_", None),
            ("_\\C_", None),
            ("_\\C_\\long.txt", Some(b"5")),
            ("dot.txt", Some(b"6")),
            ("x", None),
            ("x\\y.txt", Some(b"7")),
            ("ads.txt__$DATA", Some(b"8")),
        ]);
        for ext in [".zip", ".7z"] {
            let archive = t.join(&format!("tricks{ext}"));
            archive_with(&archive, tricks);
            let a = s(&archive);
            assert!(read(&a, "", "").entries.iter().all(|e| e.name != ".." && !e.name.contains(['/', '\\', ':'])), "{ext}");
            // Extract here, and everything taken out at once (open, copy or drag).
            let here = t.dir(&format!("here{ext}"));
            let dest = here.join("dest");
            std::fs::create_dir(&dest).expect("mkdir");
            extract_all(&a, &s(&dest), &mut Vec::new()).expect("extract here");
            assert_eq!(tree(&dest), want, "{ext}");
            assert_eq!(names_in(&here), ["dest"], "nothing beside it ({ext})");
            let taken = t.dir(&format!("taken{ext}"));
            let dest = taken.join("dest");
            std::fs::create_dir(&dest).expect("mkdir");
            extract(&a, &[String::new()], &s(&dest)).expect("take out");
            assert_eq!(tree(&dest), want, "{ext}");
            assert_eq!(names_in(&taken), ["dest"], "nothing beside it ({ext})");
        }
        // Nothing climbed up to the test's own folder either.
        assert_eq!(names_in(&t.0), ["here.7z", "here.zip", "taken.7z", "taken.zip", "tricks.7z", "tricks.zip"]);
    }

    #[test]
    fn names_ending_in_dots_or_spaces_are_named_as_windows_writes_them() {
        // A Mac or Linux zip can have "more " or "docs." for a folder: Windows drops the dots and
        // spaces at the end of a name, so the list shows (and opens) it as it will be written.
        let t = Scratch::new("trailing");
        let zip = t.join("odd.zip");
        zip_with(&zip, &[("report.txt ", b"space"), ("docs./a.txt", b"a"), ("more /b.txt", b"b"), ("vol. 2 . . /c.txt", b"c")]);
        let z = s(&zip);
        assert_eq!(names(&read(&z, "", "")), ["docs\\", "more\\", "report.txt", "vol. 2\\"]);
        assert_eq!(names(&read(&z, "docs", "")), ["a.txt"]);
        assert_eq!(names(&read(&z, "more", "")), ["b.txt"]);
        let out = t.dir("out");
        extract_all(&z, &s(&out), &mut Vec::new()).expect("extract");
        let want = expected(&[
            ("docs", None),
            ("docs\\a.txt", Some(b"a")),
            ("more", None),
            ("more\\b.txt", Some(b"b")),
            ("report.txt", Some(b"space")),
            ("vol. 2", None),
            ("vol. 2\\c.txt", Some(b"c")),
        ]);
        assert_eq!(tree(&out), want);
        // A name that is nothing but dots and spaces can't be written at all: it's left out, like "..".
        assert_eq!(clean_name("x/.../y.txt"), None);
        assert_eq!(clean_name(".. /z.txt"), None);
        assert_eq!(clean_name(". ."), None);
        assert_eq!(clean_name("a. b").as_deref(), Some("a. b"));
    }

    #[test]
    fn the_same_name_twice_comes_out_as_the_list_shows_it() {
        // Names that differ only in capitals (from a Linux zip), and one name twice: Windows can keep
        // one of each, so it's the one the list shows (the last), whichever way it's taken out.
        let t = Scratch::new("twice");
        let zip = t.join("case.zip");
        zip_with(&zip, &[("Same.txt", b"first"), ("same.TXT", b"second!")]);
        let dup = t.join("dup.zip");
        zip_with(&dup, &[("a.txt", b"one"), ("b.txt", b"two!")]);
        patch(&dup, b"b.txt", b"a.txt");
        let seven = t.join("dup.7z");
        seven_with(&seven, &[("a.txt", b"one"), ("a.txt", b"two!")]);
        for (archive, name, last) in [(&zip, "Same.txt", &b"second!"[..]), (&dup, "a.txt", b"two!"), (&seven, "a.txt", b"two!")] {
            let a = s(archive);
            let listed = read(&a, "", "");
            assert_eq!(listed.entries.len(), 1, "{a}");
            assert_eq!(listed.entries[0].size, last.len() as u64, "{a}");
            let taken = t.dir(&format!("taken-{}", folder::file_name(&a)));
            extract(&a, &[name.to_string()], &s(&taken)).expect("take out");
            assert_eq!(std::fs::read(taken.join(name)).expect("read"), last, "{a}");
            let here = t.dir(&format!("here-{}", folder::file_name(&a)));
            extract_all(&a, &s(&here), &mut Vec::new()).expect("extract here");
            assert_eq!(names_in(&here).len(), 1, "{a}");
            assert_eq!(std::fs::read(here.join(name)).expect("read"), last, "{a}");
        }
    }

    #[test]
    fn folders_named_alike_but_for_capitals_are_one_folder() {
        let t = Scratch::new("capitals");
        let zip = t.join("caps.zip");
        zip_with(&zip, &[("Docs/a.txt", b"a"), ("DOCS/b.txt", b"b"), ("docs/Sub/c.txt", b"c"), ("docs/SUB/d.txt", b"d")]);
        let z = s(&zip);
        assert_eq!(names(&read(&z, "", "")), ["Docs\\"]);
        assert_eq!(names(&read(&z, "dOcS", "")), ["Sub\\", "a.txt", "b.txt"]);
        assert_eq!(names(&read(&z, "DOCS\\sub", "")), ["c.txt", "d.txt"]);
        let want = expected(&[
            ("Docs", None),
            ("Docs\\Sub", None),
            ("Docs\\Sub\\c.txt", Some(b"c")),
            ("Docs\\Sub\\d.txt", Some(b"d")),
            ("Docs\\a.txt", Some(b"a")),
            ("Docs\\b.txt", Some(b"b")),
        ]);
        let taken = t.dir("taken");
        assert_eq!(extract(&z, &["docs".to_string()], &s(&taken)), Ok(vec![s(&taken.join("Docs"))]));
        assert_eq!(tree(&taken), want);
        let here = t.dir("here");
        let mut made = Vec::new();
        extract_all(&z, &s(&here), &mut made).expect("extract here");
        assert_eq!(made, [s(&here.join("Docs"))]);
        assert_eq!(tree(&here), want);
    }

    #[test]
    fn empty_archives_open_as_empty_folders() {
        let t = Scratch::new("emptyarch");
        for ext in [".zip", ".7z"] {
            let archive = t.join(&format!("nothing{ext}"));
            archive_with(&archive, &[]);
            let a = s(&archive);
            let root = read(&a, "", "");
            assert_eq!((root.error.as_deref(), root.entries.len()), (None, 0), "{ext}");
            let out = t.dir(&format!("out{ext}"));
            let mut made = Vec::new();
            assert_eq!(extract_all(&a, &s(&out), &mut made), Ok(()), "{ext}");
            assert!(made.is_empty() && names_in(&out).is_empty(), "{ext}");
            // Only an empty folder and an empty file: both come out.
            let hollow = t.join(&format!("hollow{ext}"));
            archive_with(&hollow, &[("empty/", b""), ("zero.txt", b"")]);
            let out = t.dir(&format!("hollow-out{ext}"));
            let mut made = Vec::new();
            extract_all(&s(&hollow), &s(&out), &mut made).expect("extract");
            assert_eq!(tree(&out), expected(&[("empty", None), ("zero.txt", Some(b""))]), "{ext}");
            assert_eq!(made.len(), 2, "{ext}");
        }
        // A file of no bytes at all isn't an archive: said in words, for both kinds.
        for name in ["blank.zip", "blank.7z"] {
            let p = t.join(name);
            std::fs::write(&p, b"").expect("write");
            let error = read(&s(&p), "", "").error.expect("an error");
            assert!(error.contains("damaged"), "{name}: {error}");
        }
    }

    #[test]
    fn a_damaged_file_stops_extract_here_without_a_half_written_one() {
        let t = Scratch::new("midbad");
        // Big enough that its packed data is long, and doesn't pack to nothing.
        let noise: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        let zip = t.join("bad.zip");
        zip_with(&zip, &[("good.txt", b"good"), ("bad.bin", &noise), ("after.txt", b"after")]);
        let mut bytes = std::fs::read(&zip).expect("read");
        let header = bytes.windows(4).enumerate().filter(|(_, w)| *w == b"PK\x03\x04").map(|(i, _)| i).nth(1).expect("second header");
        let name_len = u16::from_le_bytes([bytes[header + 26], bytes[header + 27]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[header + 28], bytes[header + 29]]) as usize;
        let data = header + 30 + name_len + extra_len;
        for b in &mut bytes[data + 5_000..data + 5_064] {
            *b ^= 0x5A;
        }
        std::fs::write(&zip, bytes).expect("write");
        let out = t.dir("out");
        let mut made = Vec::new();
        let err = extract_all(&s(&zip), &s(&out), &mut made).expect_err("damaged");
        assert!(err.contains("bad.bin"), "{err}");
        // What came before is whole; the damaged one isn't left half-written; nothing after it.
        assert_eq!(tree(&out), expected(&[("good.txt", Some(b"good"))]));
        assert_eq!(made.len(), 3, "every top item is known, to be cleaned up");
    }

    #[test]
    fn one_locked_entry_stops_extract_here_before_anything_is_written() {
        let t = Scratch::new("onelocked");
        let zip = t.join("half.zip");
        zip_with(&zip, &[("plain.txt", b"plain"), ("secret.txt", b"secret")]);
        // Marks only secret.txt as needing a password (bit 0 of its flags, in both of its headers).
        let mut bytes = std::fs::read(&zip).expect("read");
        for i in 0..bytes.len().saturating_sub(50) {
            let name_at = match &bytes[i..i + 4] {
                b"PK\x03\x04" => i + 30,
                b"PK\x01\x02" => i + 46,
                _ => continue,
            };
            if bytes[name_at..].starts_with(b"secret.txt") {
                let flags = if bytes[i + 2] == 3 { i + 6 } else { i + 8 };
                bytes[flags] |= 1;
            }
        }
        std::fs::write(&zip, bytes).expect("write");
        let z = s(&zip);
        assert_eq!(names(&read(&z, "", "")), ["plain.txt", "secret.txt"]);
        let taken = t.dir("taken");
        extract(&z, &["plain.txt".to_string()], &s(&taken)).expect("the other one comes out");
        assert_eq!(std::fs::read(taken.join("plain.txt")).expect("read"), b"plain");
        let err = extract(&z, &["secret.txt".to_string()], &s(&taken)).expect_err("locked");
        assert!(err.contains("password"), "{err}");
        let here = t.dir("here");
        let err = extract_all(&z, &s(&here), &mut Vec::new()).expect_err("locked");
        assert!(err.contains("password"), "{err}");
        assert!(names_in(&here).is_empty(), "nothing written");
    }

    #[test]
    fn taking_thousands_of_items_out_at_once_stays_quick() {
        // Every file of a big zip folder selected and dragged or copied out: each one asked for
        // mustn't mean a pass through the whole archive (that held the window for minutes).
        let t = Scratch::new("thousands");
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for i in 0..10_000 {
            w.start_file(format!("many/file {i:05}.txt"), opts).expect("start");
            w.write_all(i.to_string().as_bytes()).expect("write");
        }
        let zip = t.join("many.zip");
        std::fs::write(&zip, w.finish().expect("finish").into_inner()).expect("write");
        let z = s(&zip);
        let wanted: Vec<String> = (0..3_000).map(|i| format!("many\\file {i:05}.txt")).collect();
        let started = Instant::now();
        let bytes: u64 = (0..3_000u32).map(|i| i.to_string().len() as u64).sum();
        assert_eq!(unpack_size(&z, &wanted), Ok(bytes));
        assert!(started.elapsed() < Duration::from_secs(3), "sizing up took {:?}", started.elapsed());
        let out = t.dir("out");
        let some: Vec<String> = (0..300).map(|i| format!("many\\FILE {:05}.txt", i * 33)).collect();
        let started = Instant::now();
        let tops = extract(&z, &some, &s(&out)).expect("take out");
        assert!(started.elapsed() < Duration::from_secs(5), "taking out took {:?}", started.elapsed());
        // In the order asked for, each with its own content.
        assert_eq!(tops.len(), 300);
        assert_eq!(tops[1], s(&out.join("file 00033.txt")));
        assert_eq!(std::fs::read(out.join("file 09867.txt")).expect("read"), b"9867");
        assert_eq!(names_in(&out).len(), 300);
    }

    #[test]
    fn a_cancelled_extract_leaves_no_file_that_looks_finished() {
        let t = Scratch::new("cancelx");
        let big: Vec<u8> = (0..600_000u32).map(|i| (i % 251) as u8).collect();
        for ext in [".zip", ".7z"] {
            let archive = t.join(&format!("big{ext}"));
            archive_with(&archive, &[("big.bin", &big), ("sub/small.txt", b"small"), ("sub/later.bin", &big)]);
            let dest = t.dir(&format!("dest{ext}"));
            let label = format!("archive test: cancelled extract {ext} {}", std::process::id());
            let ticket = crate::copyqueue::Ticket::join(label.clone());
            assert!(ticket.wait_turn());
            let id = crate::copyqueue::shown().into_iter().find(|t| t.label == label).expect("listed").id;
            crate::copyqueue::cancel(id);
            let mut made = Vec::new();
            assert_eq!(extract_all(&s(&archive), &s(&dest), &mut made), Err(CANCELLED.to_string()), "{ext}");
            drop(ticket);
            // No file is left holding only part of what it should.
            assert!(tree(&dest).iter().all(|(_, data)| data.is_none()), "{ext}: {:?}", names_in(&dest));
            // Every top item it made is known, so the job takes them all away again.
            assert_eq!(made, [s(&dest.join("big.bin")), s(&dest.join("sub"))], "{ext}");
        }
    }

    #[test]
    fn extract_here_numbers_what_is_in_the_way_and_leaves_it_alone() {
        let t = Scratch::new("inway");
        let zip = t.join("x.zip");
        zip_with(&zip, &[("docs/a.txt", b"new a"), ("note.txt", b"new note"), ("Photos/", b""), ("ro.txt", b"new ro")]);
        let dest = t.dir("dest");
        // A file named like the archive's folder, a folder named like its file, a read-only file,
        // and a folder in other capitals.
        std::fs::write(dest.join("docs"), b"my docs").expect("write");
        std::fs::create_dir(dest.join("note.txt")).expect("mkdir");
        std::fs::write(dest.join("note.txt").join("inside.txt"), b"mine").expect("write");
        std::fs::write(dest.join("RO.TXT"), b"read-only").expect("write");
        let mut p = std::fs::metadata(dest.join("RO.TXT")).expect("meta").permissions();
        p.set_readonly(true);
        std::fs::set_permissions(dest.join("RO.TXT"), p.clone()).expect("read-only");
        std::fs::create_dir(dest.join("photos")).expect("mkdir");
        let mut made = Vec::new();
        extract_all(&s(&zip), &s(&dest), &mut made).expect("extract");
        assert_eq!(made, ["docs (2)", "note (2).txt", "Photos (2)", "ro (2).txt"].map(|n| s(&dest.join(n))));
        assert_eq!(std::fs::read(dest.join("docs")).expect("read"), b"my docs");
        assert_eq!(std::fs::read(dest.join("note.txt").join("inside.txt")).expect("read"), b"mine");
        assert_eq!(std::fs::read(dest.join("RO.TXT")).expect("read"), b"read-only");
        assert_eq!(std::fs::read(dest.join("docs (2)").join("a.txt")).expect("read"), b"new a");
        assert_eq!(std::fs::read(dest.join("note (2).txt")).expect("read"), b"new note");
        assert_eq!(std::fs::read(dest.join("ro (2).txt")).expect("read"), b"new ro");
        assert!(dest.join("Photos (2)").is_dir() && names_in(&dest.join("photos")).is_empty());
        p.set_readonly(false);
        let _ = std::fs::set_permissions(dest.join("RO.TXT"), p);
    }

    #[test]
    fn odd_names_go_in_and_come_out_exactly() {
        let t = Scratch::new("oddnames");
        let mut names_used = vec!["100% #done.md", "spaces  inside .txt", "x.tar.gz", ".starts-with-a-dot", "UPPER.TXT", "a;b,c=d+e[1]{2}~.txt"];
        let mut top_name = "Odd #1".to_string();
        // Accents, other alphabets and emoji, where the disk can take them (not under Wine run
        // without a UTF-8 locale).
        let beyond_ascii = crate::search::odd_names_work(&t.0);
        if beyond_ascii {
            names_used.extend([
                "na\u{ef}ve r\u{e9}sum\u{e9}.txt",
                "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30d5}\u{30a1}\u{30a4}\u{30eb}.txt",
                "emoji \u{1f600}\u{1f389}.txt",
                "\u{395}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac} \u{3a3}\u{3a3}.txt",
                "stra\u{df}e.txt",
            ]);
            top_name.push_str(" caf\u{e9}");
        }
        let top = t.dir(&top_name);
        for (i, n) in names_used.iter().enumerate() {
            std::fs::write(top.join(n), format!("file {i}")).expect("write");
        }
        let deep = top.join("a#b#c").join(if beyond_ascii { "\u{1f4c1} folder" } else { "#folder" });
        std::fs::create_dir_all(&deep).expect("mkdir");
        std::fs::write(deep.join("x#y.txt"), b"deep").expect("write");
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
            let out = t.join(&format!("odd{}", format.ext()));
            pack(&plan, &s(&out), format).expect("pack");
            let inside = read(&s(&out), &top_name, "");
            let mut want: Vec<String> = names_used.iter().map(|n| n.to_string()).chain(["a#b#c\\".to_string()]).collect();
            want.sort();
            assert_eq!(names(&inside), want, "{format:?}");
            let back = t.dir(&format!("back{}", format.ext()));
            extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
            assert_eq!(tree(&back.join(&top_name)), tree(&top), "{format:?}");
        }
    }

    #[test]
    fn paths_longer_than_260_characters_go_in_and_come_out() {
        let t = Scratch::new("long");
        // Eight folders of 40 characters and a 60-character name: far past the old limit.
        let mut deep = t.join("Deep");
        for i in 0..8 {
            deep = deep.join(format!("{i}-{}", "f".repeat(38)));
        }
        std::fs::create_dir_all(&deep).expect("mkdir");
        let name = format!("{}.txt", "n".repeat(56));
        std::fs::write(deep.join(&name), b"far down").expect("write");
        assert!(s(&deep.join(&name)).len() > 400);
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            let plan = plan_pack(&[s(&t.join("Deep"))], &never).expect("plan").expect("not stopped");
            // The archive itself sits deep down too, and is read there like any other.
            let out = deep.join(format!("deep{}", format.ext()));
            pack(&plan, &s(&out), format).expect("pack");
            let inner: Vec<String> = (0..8).map(|i| format!("{i}-{}", "f".repeat(38))).collect();
            let at = format!("{}\\Deep\\{}", s(&out), inner.join("\\"));
            assert_eq!(split(&at), Some((s(&out), format!("Deep\\{}", inner.join("\\")))));
            assert_eq!(names(&read(&s(&out), &format!("Deep\\{}", inner.join("\\")), &at)), [name.clone()], "{format:?}");
            let back = t.dir(&format!("back{}", format.ext()));
            extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
            assert_eq!(std::fs::read(back.join("Deep").join(inner.iter().collect::<PathBuf>()).join(&name)).expect("read"), b"far down", "{format:?}");
            std::fs::remove_file(&out).expect("remove");
        }
        // A name longer than Windows allows in one folder: a plain error, and nothing half-written.
        let zip = t.join("toolong.zip");
        zip_with(&zip, &[("ok.txt", b"ok"), (&format!("{}.txt", "n".repeat(300)), b"too long")]);
        let out = t.dir("toolong");
        let err = extract_all(&s(&zip), &s(&out), &mut Vec::new()).expect_err("too long");
        assert!(err.contains("couldn't be written"), "{err}");
        assert_eq!(tree(&out), expected(&[("ok.txt", Some(b"ok"))]));
    }

    #[test]
    fn a_folder_packed_into_itself_doesnt_hold_its_own_archive() {
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            let t = Scratch::new(&format!("itself{}", format.ext()));
            let top = make_tree(&t.0);
            let before = tree(&top);
            // The archive goes inside the very folder being packed (as from search results).
            let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
            let out = top.join(format!("Stuff{}", format.ext()));
            pack(&plan, &s(&out), format).expect("pack");
            assert_eq!(names(&read(&s(&out), "Stuff", "")), ["a.txt", "empty\\", "sub\\"], "{format:?}");
            let back = t.dir("back");
            extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
            assert_eq!(tree(&back.join("Stuff")), before, "{format:?}");
            // Packed again: now the first archive is there to go in, whole.
            let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
            let again = top.join(format!("Stuff (2){}", format.ext()));
            pack(&plan, &s(&again), format).expect("pack again");
            let first = std::fs::read(&out).expect("read");
            let twice = t.dir("twice");
            extract_all(&s(&again), &s(&twice), &mut Vec::new()).expect("extract");
            assert_eq!(std::fs::read(twice.join("Stuff").join(format!("Stuff{}", format.ext()))).expect("read"), first, "{format:?}");
        }
    }

    #[test]
    fn a_locked_or_vanished_file_stops_the_pack_and_leaves_no_archive() {
        use std::os::windows::fs::OpenOptionsExt;
        let t = Scratch::new("lockpack");
        let top = make_tree(&t.0);
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
            // Another program has sub\big.bin open, sharing it with nobody.
            let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(top.join("sub").join("big.bin")).expect("lock");
            let out = t.join(&format!("locked{}", format.ext()));
            let err = pack(&plan, &s(&out), format).expect_err("locked");
            assert!(err.contains("big.bin") && err.contains("couldn't be read"), "{format:?}: {err}");
            assert!(!out.exists(), "{format:?}");
            drop(lock);
            // A file deleted since it was found.
            std::fs::rename(top.join("a.txt"), t.join("a.txt")).expect("move away");
            let out = t.join(&format!("vanished{}", format.ext()));
            let err = pack(&plan, &s(&out), format).expect_err("gone");
            assert!(err.contains("a.txt") && err.contains("isn't there any more"), "{format:?}: {err}");
            assert!(!out.exists(), "{format:?}");
            std::fs::rename(t.join("a.txt"), top.join("a.txt")).expect("move back");
        }
    }

    #[test]
    fn a_file_that_changed_size_since_it_was_found_is_packed_as_it_is_now() {
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            let t = Scratch::new(&format!("resized{}", format.ext()));
            let top = make_tree(&t.0);
            let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
            // Emptied, and cut short, after being found.
            std::fs::write(top.join("a.txt"), b"").expect("empty");
            std::fs::write(top.join("sub").join("big.bin"), b"ten bytes!").expect("shorten");
            let out = t.join(&format!("now{}", format.ext()));
            pack(&plan, &s(&out), format).expect("pack");
            let back = t.dir("back");
            extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
            assert_eq!(tree(&back.join("Stuff")), tree(&top), "{format:?}");
        }
    }

    #[test]
    fn packing_only_empty_things_makes_an_archive_that_opens() {
        let t = Scratch::new("emptypack");
        let nothing = t.dir("Nothing");
        let zero = t.join("zero.txt");
        std::fs::write(&zero, b"").expect("write");
        let never = AtomicBool::new(false);
        for format in [Format::Zip, Format::SevenZ] {
            for (i, what) in [vec![s(&nothing)], vec![s(&zero)], vec![s(&nothing), s(&zero)]].into_iter().enumerate() {
                let plan = plan_pack(&what, &never).expect("plan").expect("not stopped");
                let out = t.join(&format!("empty{i}{}", format.ext()));
                pack(&plan, &s(&out), format).expect("pack");
                let root = read(&s(&out), "", "");
                assert_eq!(root.error, None, "{format:?} {what:?}");
                let back = t.dir(&format!("back{i}{}", format.ext()));
                extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
                let want: Vec<_> = what.iter().map(|p| (folder::file_name(p).to_string(), if p.ends_with(".txt") { Some(Vec::new()) } else { None })).collect();
                let mut want = want;
                want.sort();
                assert_eq!(tree(&back), want, "{format:?} {what:?}");
            }
        }
    }

    #[test]
    fn an_old_dos_name_is_read_in_the_pcs_code_page_and_comes_out() {
        let t = Scratch::new("oem");
        let zip = t.join("old.zip");
        zip_with(&zip, &[("caf#.txt", b"coffee")]);
        // An "\u{e9}" as old DOS code page 437 wrote it, with no UTF-8 flag.
        patch(&zip, b"caf#.txt", b"caf\x82.txt");
        let z = s(&zip);
        let listed = read(&z, "", "");
        assert_eq!(listed.entries.len(), 1);
        let name = listed.entries[0].name.clone();
        assert!(name.starts_with("caf") && name.ends_with(".txt") && name.chars().count() == 8, "{name}");
        // (Taken out only where the disk can take such a name: not under Wine without a UTF-8 locale.)
        if !crate::search::odd_names_work(&t.0) {
            return;
        }
        let taken = t.dir("taken");
        extract(&z, &[name.clone()], &s(&taken)).expect("take out");
        assert_eq!(std::fs::read(taken.join(&name)).expect("read"), b"coffee");
        let here = t.dir("here");
        extract_all(&z, &s(&here), &mut Vec::new()).expect("extract");
        assert_eq!(tree(&here), vec![(name, Some(b"coffee".to_vec()))]);
    }
}
