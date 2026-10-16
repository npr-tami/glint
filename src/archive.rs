//! Looking inside .zip and .7z files as if they were folders (read-only), unpacking them, and
//! making new ones.
//!
//! A path like `C:\x\a.zip\sub\b.txt` is an archive file plus a path inside it ([`split`]). [`read`]
//! lists one folder inside, [`extract`] copies items out to a real folder, [`extract_all`] unpacks a
//! whole archive (Extract here), and [`pack`] makes a new one (Compress to zip / 7z) from what
//! [`plan_pack`] found. Nothing is kept between calls: each one opens the archive again. That's cheap, because the list of what's inside sits in
//! one place (a zip's central directory at the end, a 7z's header), so listing never unpacks any
//! file data. The one exception is an archive inside the archive (`a.zip\sub\b.zip\docs`): it's
//! taken out to Glint's temporary folder the first time it's looked into, and read from there
//! ([`resolve`]), while its path stays the one through the outer archive.
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
use std::io::{self, BufReader, BufWriter, Read, Seek, Write};
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
    /// A zip entry the zip library has no index for (its name comes twice): where it is instead.
    hidden: Option<Hidden>,
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
    fold(&parts.join("\\"))
}

/// A path in lower case, as `same_name` compares names.
fn fold(path: &str) -> String {
    path.chars().flat_map(char::to_lowercase).collect()
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

/// An archive file opened and read once: what [`list`] made of it, kept so that unpacking from it
/// doesn't read its central directory or header again.
enum Opened {
    Zip(zip::ZipArchive<BufReader<File>>),
    SevenZ(sevenz_rust2::Archive, BufReader<File>),
}

fn list(archive: &str) -> Result<(Opened, Vec<Item>), String> {
    match kind_of(archive) {
        Some(Kind::Zip) => list_zip(archive).map(|(zip, items)| (Opened::Zip(zip), items)),
        Some(Kind::SevenZ) => open_7z(archive).map(|(a, source)| {
            let items = list_7z(&a);
            (Opened::SevenZ(a, source), items)
        }),
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

fn list_zip(archive: &str) -> Result<(zip::ZipArchive<BufReader<File>>, Vec<Item>), String> {
    let zip = open_zip(archive)?;
    let mut items = Vec::with_capacity(zip.len());
    // Where each entry the zip library knows starts, and the item for it.
    let mut known: HashMap<u64, Item> = HashMap::new();
    for index in 0..zip.len() {
        // The central directory's copy of the entry: no seeking to the data.
        let Ok(e) = zip.by_index_data(index) else { continue };
        let Some(path) = clean_name(&zip_name(e.name_raw())) else { continue };
        let is_dir = e.is_dir();
        let item = Item {
            path,
            is_dir,
            size: if is_dir { 0 } else { e.size() },
            modified: e.last_modified().map_or(0, |t| local_to_filetime(t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second())),
            locked: e.encrypted(),
            index,
            hidden: None,
        };
        known.insert(e.header_start(), item.clone());
        items.push(item);
    }
    // The zip library keeps one entry of each name (the last), so an entry whose name comes again
    // isn't among its entries at all: the central directory is gone through again for those, in
    // its own order (so the first of a name stays first).
    if let Some(records) = central_records(archive, &zip, &known) {
        items = records
            .into_iter()
            .filter_map(|r| match r {
                Record::Known(at) => known.remove(&at),
                Record::Hidden(item) => Some(item),
            })
            .collect();
    }
    number_alike(&mut items);
    Ok((zip, items))
}

/// An entry of a zip's central directory: one the zip library has (where it starts), or one it
/// doesn't, with what's needed to list and unpack it.
enum Record {
    Known(u64),
    Hidden(Item),
}

/// Where an entry the zip library can't reach by its index is, read from the central directory
/// (which holds the right sizes and checksum even when the entry's own header leaves them out).
#[derive(Clone, Copy, Debug)]
struct Hidden {
    /// Where its local header starts in the file.
    header: u64,
    compressed: u64,
    size: u64,
    crc: u32,
}

/// Every entry of the zip's central directory in order, when some aren't among the zip library's
/// (`known`, by where they start). None when there are none such, or the directory doesn't read
/// as expected (then the library's list is used as it is).
fn central_records(archive: &str, zip: &zip::ZipArchive<BufReader<File>>, known: &HashMap<u64, Item>) -> Option<Vec<Record>> {
    let mut file = BufReader::new(File::open(archive).ok()?);
    let offset = zip.offset();
    // The directory starts here (counted from the file's start, or from where the zip starts in it).
    let start = [zip.central_directory_start(), zip.central_directory_start() + offset].into_iter().find(|&at| {
        let mut sig = [0u8; 4];
        file.seek(io::SeekFrom::Start(at)).is_ok() && file.read_exact(&mut sig).is_ok() && sig == *b"PK\x01\x02"
    })?;
    file.seek(io::SeekFrom::Start(start)).ok()?;
    let mut records = Vec::new();
    let mut found = 0;
    let mut hidden = false;
    loop {
        let mut fixed = [0u8; 46];
        if file.read_exact(&mut fixed).is_err() || fixed[..4] != *b"PK\x01\x02" {
            break;
        }
        let u16_at = |i: usize| u16::from_le_bytes([fixed[i], fixed[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([fixed[i], fixed[i + 1], fixed[i + 2], fixed[i + 3]]);
        let (flags, time, date, crc) = (u16_at(8), u16_at(12), u16_at(14), u32_at(16));
        let (mut compressed, mut size, mut header) = (u64::from(u32_at(20)), u64::from(u32_at(24)), u64::from(u32_at(42)));
        let mut name = vec![0u8; usize::from(u16_at(28))];
        let mut extra = vec![0u8; usize::from(u16_at(30))];
        let mut comment = vec![0u8; usize::from(u16_at(32))];
        file.read_exact(&mut name).ok()?;
        file.read_exact(&mut extra).ok()?;
        file.read_exact(&mut comment).ok()?;
        let mut unicode: Option<Vec<u8>> = None;
        let mut rest = extra.as_slice();
        while rest.len() >= 4 {
            let (id, len) = (u16::from_le_bytes([rest[0], rest[1]]), usize::from(u16::from_le_bytes([rest[2], rest[3]])));
            let Some(data) = rest.get(4..4 + len) else { break };
            match id {
                // Zip64: the 8-byte sizes and start of the fields that didn't fit, in this order.
                0x0001 => {
                    let mut wide = data.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap_or([0; 8])));
                    for field in [&mut size, &mut compressed, &mut header] {
                        if *field == 0xFFFF_FFFF {
                            *field = wide.next()?;
                        }
                    }
                }
                // The name in UTF-8, added by an older tool beside its own code page's.
                0x7075 if data.len() > 5 => unicode = Some(data[5..].to_vec()),
                _ => {}
            }
            rest = &rest[4 + len..];
        }
        let header = header.checked_add(offset)?;
        if known.contains_key(&header) {
            found += 1;
            records.push(Record::Known(header));
            continue;
        }
        hidden = true;
        let name = unicode.unwrap_or(name);
        let is_dir = name.ends_with(b"/") || name.ends_with(b"\\");
        let Some(path) = clean_name(&zip_name(&name)) else { continue };
        let modified = local_to_filetime((date >> 9) + 1980, ((date >> 5) & 15) as u8, (date & 31) as u8, (time >> 11) as u8, ((time >> 5) & 63) as u8, ((time & 31) * 2) as u8);
        let at = Hidden { header, compressed, size, crc };
        records.push(Record::Hidden(Item { path, is_dir, size: if is_dir { 0 } else { size }, modified, locked: flags & 1 != 0, index: usize::MAX, hidden: Some(at) }));
    }
    // Every entry the library has must be there (once), or this isn't the directory it read.
    (hidden && found == known.len()).then_some(records)
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
            hidden: None,
        });
    }
    number_alike(&mut items);
    items
}

/// Gives every file a path of its own. An archive can hold the same name twice in one folder (a
/// file added again, or names that differ only in capitals, from a Linux zip), and a file can
/// have the name of a folder there. Windows keeps only one of each, so the first keeps its name
/// and the later ones are numbered as Windows numbers copies ("notes (2).txt", "notes (3).txt"),
/// never taking a name another item already has. The list then shows each of them, and opening,
/// copying or extracting one takes out its own data. Folders listed twice stay one folder.
fn number_alike(items: &mut [Item]) {
    // Every folder, named or only implied by the paths under it.
    let mut dirs: HashSet<String> = HashSet::new();
    for item in items.iter() {
        let parts: Vec<&str> = item.parts().collect();
        let implied = if item.is_dir { parts.len() } else { parts.len() - 1 };
        for n in 1..=implied {
            dirs.insert(folded(&parts[..n]));
        }
    }
    // Every file's own name, so a number given to one never takes a name that comes later.
    let mut taken: HashSet<String> = items.iter().filter(|i| !i.is_dir).map(|i| fold(&i.path)).collect();
    let mut kept: HashSet<String> = HashSet::new();
    for item in items.iter_mut().filter(|i| !i.is_dir) {
        let key = fold(&item.path);
        if !dirs.contains(&key) && kept.insert(key) {
            continue;
        }
        let (parent, name) = match item.path.rfind('\\') {
            Some(i) => (&item.path[..=i], &item.path[i + 1..]),
            None => ("", item.path.as_str()),
        };
        let name = free_name(name, false, |n| {
            let k = fold(&format!("{parent}{n}"));
            taken.contains(&k) || dirs.contains(&k)
        });
        item.path = format!("{parent}{name}");
        taken.insert(fold(&item.path));
        kept.insert(fold(&item.path));
    }
}

/// Reads the folder `inner` of the archive (`\` separators, "" for its root) as a [`Folder`] whose
/// path is `full_path`. Folders that only show up in the names of the files in them are listed too
/// (many zips have no entries for folders). Never panics; problems end up in `Folder::error`.
/// An archive inside the archive (`outer.zip\sub\inner.zip\docs`) is read like any other folder of
/// it: see [`resolve`].
pub fn read(archive: &str, inner: &str, full_path: &str) -> Folder {
    let start = Instant::now();
    let mut f = Folder { path: full_path.to_string(), ..Default::default() };
    let listed = resolve(archive, inner).and_then(|(archive, inner)| list(&archive).and_then(|(_, items)| children(&items, &inner)));
    match listed {
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

// ---------------------------------------------------------------- archives inside archives

/// Where a path inside an archive really is when it goes through another archive kept in it
/// (`sub\inner.zip\docs` in outer.zip): the archive file to open and the path inside that one.
/// Each archive on the way is taken out to Glint's temporary folder ([`nested_copy`]), as deep as
/// the path goes; Up, Back and the path bar keep the whole path, so going up from inner.zip lands
/// back in outer.zip's `sub`. A path that ends at an archive means the inside of it (to list it;
/// [`by_archive`] looks up an item's folder, so copying one takes the file itself). A plain path
/// comes back as it is.
fn resolve(archive: &str, inner: &str) -> Result<(String, String), String> {
    let mut archive = archive.to_string();
    let mut parts: Vec<String> = inner_parts(inner).into_iter().map(str::to_string).collect();
    'deeper: loop {
        // Only parts named like an archive can be one: a plain path costs no listing.
        let named: Vec<usize> = (1..=parts.len()).filter(|&n| is_archive_name(&parts[n - 1])).collect();
        if named.is_empty() {
            break;
        }
        let (mut opened, items) = list(&archive)?;
        for n in named {
            let key = fold(&parts[..n].join("\\"));
            // (A folder named "x.zip" is just a folder.)
            if let Some(at) = items.iter().position(|i| !i.is_dir && fold(&i.path) == key) {
                archive = nested_copy(&archive, &mut opened, &items, at)?;
                parts.drain(..n);
                continue 'deeper;
            }
        }
        break;
    }
    Ok((archive, parts.join("\\")))
}

/// The archive `items[at]` of `archive`, taken out to Glint's temporary folder to be looked into:
/// once, as later visits find it there (it's kept with the archive's other take-outs, under a name
/// made from its path inside, so a changed outer archive gets a new copy). It's unpacked to a
/// folder of its own first and then moved into place, so a copy that's there is always whole.
fn nested_copy(archive: &str, opened: &mut Opened, items: &[Item], at: usize) -> Result<String, String> {
    let item = &items[at];
    let name = folder::file_name(&item.path);
    let home = temp_home(archive)?.join("inside").join(format!("{:012x}", fnv(fold(&item.path).bytes()) >> 16));
    let copy = home.join(name);
    let whole = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == item.size);
    if whole(&copy) {
        return Ok(copy.to_string_lossy().into_owned());
    }
    let fresh = temp_dir_for(archive)?;
    let out = Path::new(&fresh).join(name);
    let made = unpack(archive, opened, items, &[(at, out.clone())], &Meter::new(0));
    let moved = made.and_then(|()| {
        std::fs::create_dir_all(&home).map_err(|e| write_error(&home, &e))?;
        // (Another window may have put it there meanwhile: then that one is used.)
        match std::fs::rename(&out, &copy) {
            Ok(()) => Ok(()),
            Err(_) if whole(&copy) => Ok(()),
            Err(e) => Err(write_error(&copy, &e)),
        }
    });
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_dir(&fresh);
    moved.map(|()| copy.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------- extracting

/// Extracts these items of the archive (paths inside it; a folder brings everything under it)
/// into the real folder `dest_dir`, laid out as they are under their parent inside the archive.
/// Nothing already there is replaced (that's an error instead). Returns the full paths of the
/// top-level items written. Items inside an archive in the archive are taken from that one
/// ([`resolve`]). (Used for Glint's temporary folder: to open, copy or drag items out of an
/// archive.)
pub fn extract(archive: &str, inner_items: &[String], dest_dir: &str) -> Result<Vec<String>, String> {
    let mut tops = Vec::new();
    for (real, those) in by_archive(archive, inner_items)? {
        tops.extend(extract_from(&real, &those, dest_dir)?);
    }
    Ok(tops)
}

/// Paths inside `archive`, by the archive file each is really in ([`resolve`]) - the same one
/// for all, as a rule (they're from one folder) - in the order asked for.
fn by_archive(archive: &str, inner_items: &[String]) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    // Each folder is looked up once, not once for each of thousands of items in it.
    let mut folders: HashMap<String, (String, String)> = HashMap::new();
    for item in inner_items {
        let mut parts = inner_parts(item);
        let name = parts.pop().unwrap_or("");
        let parent = parts.join("\\");
        let (real, at) = match folders.get(&fold(&parent)) {
            Some(known) => known.clone(),
            None => {
                let found = resolve(archive, &parent)?;
                folders.insert(fold(&parent), found.clone());
                found
            }
        };
        let inner = if at.is_empty() { name.to_string() } else { format!("{at}\\{name}") };
        match groups.iter_mut().find(|(a, _)| *a == real) {
            Some((_, those)) => those.push(inner),
            None => groups.push((real, vec![inner])),
        }
    }
    Ok(groups)
}

/// [`extract`] from one archive file.
fn extract_from(archive: &str, inner_items: &[String], dest_dir: &str) -> Result<Vec<String>, String> {
    let (mut opened, items) = list(archive)?;
    extract_listed(archive, &mut opened, &items, inner_items, dest_dir)
}

/// [`extract_from`] an archive already listed.
fn extract_listed(archive: &str, opened: &mut Opened, items: &[Item], inner_items: &[String], dest_dir: &str) -> Result<Vec<String>, String> {
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
    unpack(archive, opened, items, &plan, &Meter::new(0))?;
    Ok(tops)
}

/// How many bytes taking these items out with [`extract`] unpacks: every file under a wanted
/// folder (the list shows folders as 0 bytes), and in a 7z also what's packed before them in their
/// block, which is unpacked (and thrown away) to get to them. So a big take-out can be told apart
/// before it starts. (Glint itself asks [`extract_at_most`], which reads each archive once.)
#[cfg(test)]
fn unpack_size(archive: &str, inner_items: &[String]) -> Result<u64, String> {
    let mut size = 0;
    for (real, those) in by_archive(archive, inner_items)? {
        let (opened, items) = list(&real)?;
        size += unpack_size_in(&opened, &items, &those);
    }
    Ok(size)
}

/// [`extract`] to a new folder of Glint's temporary folder ([`temp_dir_for`]), but only when that
/// unpacks no more than `most` bytes (counted as `unpack_size` counts them): otherwise `too_much` is the error, and
/// nothing is made. Each archive is read once for both. Returns the folder and the top items.
pub fn extract_at_most(archive: &str, inner_items: &[String], most: u64, too_much: &str) -> Result<(String, Vec<String>), String> {
    let mut listed = Vec::new();
    let mut size = 0;
    for (real, those) in by_archive(archive, inner_items)? {
        let (opened, items) = list(&real)?;
        size += unpack_size_in(&opened, &items, &those);
        listed.push((real, opened, items, those));
    }
    if size > most {
        return Err(too_much.to_string());
    }
    let dir = temp_dir_for(archive)?;
    let mut tops = Vec::new();
    for (real, mut opened, items, those) in listed {
        tops.extend(extract_listed(&real, &mut opened, &items, &those, &dir)?);
    }
    Ok((dir, tops))
}

/// [`unpack_size`] in one archive file, already listed.
fn unpack_size_in(opened: &Opened, items: &[Item], inner_items: &[String]) -> u64 {
    let wants: Vec<Vec<&str>> = inner_items.iter().map(|w| inner_parts(w)).collect();
    // Each item's path is looked up among the wanted ones (as in `extract`), not compared with each.
    let paths: HashSet<String> = wants.iter().map(|w| folded(w)).collect();
    let depths: BTreeSet<usize> = wants.iter().map(Vec::len).collect();
    let wanted = |item: &Item| {
        let parts: Vec<&str> = item.parts().collect();
        !item.is_dir && depths.iter().take_while(|&&n| n <= parts.len()).any(|&n| paths.contains(&folded(&parts[..n])))
    };
    let Opened::SevenZ(a, _) = opened else {
        return items.iter().filter(|i| wanted(i)).map(|i| i.size).sum();
    };
    let block_of = |index: usize| a.stream_map.file_block_index.get(index).copied().flatten();
    // Each block that holds a wanted file, with the last wanted file in it (as in extract_7z).
    let mut blocks: HashMap<usize, usize> = HashMap::new();
    for item in items.iter().filter(|i| wanted(i)) {
        if let Some(b) = block_of(item.index) {
            let last = blocks.entry(b).or_insert(item.index);
            *last = (*last).max(item.index);
        }
    }
    // A block's files come one after another from its first: all of them up to that last one.
    let before_last = |index: usize| block_of(index).and_then(|b| blocks.get(&b)).is_some_and(|&last| index <= last);
    a.files.iter().enumerate().filter(|&(i, _)| before_last(i)).map(|(_, f)| f.size).sum()
}

/// Unpacks the whole archive into the folder `dest_dir` (Extract here), replacing nothing: a top
/// item whose name is taken there gets the next free one, as Windows names them ("notes (2).txt",
/// "docs (2)"). The full paths of the top items it makes go in `made` before they are made, so
/// they can be cleaned up when it stops partway (a cancel). Reports to the copy queue.
pub fn extract_all(archive: &str, dest_dir: &str, made: &mut Vec<String>) -> Result<(), String> {
    let (mut opened, items) = list(archive)?;
    let meter = Meter::new(items.iter().filter(|i| !i.is_dir).map(|i| i.size).sum());
    // The archive's top names (lower case) and the names they get in `dest_dir`.
    let mut renamed: HashMap<String, String> = HashMap::new();
    let mut given: HashSet<String> = HashSet::new();
    // Where each item goes (lower case) and its place in the plan, so a folder the archive lists
    // twice is made once. (Files never meet here: each has a path of its own, `number_alike`.)
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
    unpack(archive, &mut opened, &items, &plan, &meter)
}

/// Writes the planned items (index in `items`, where it goes): folders first, then the files. A
/// file already there is never replaced: that's an error instead.
fn unpack(archive: &str, opened: &mut Opened, items: &[Item], plan: &[(usize, PathBuf)], meter: &Meter) -> Result<(), String> {
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
    match opened {
        Opened::Zip(zip) => extract_zip(archive, zip, &files, meter)?,
        Opened::SevenZ(a, source) => extract_7z(a, source, &files, meter)?,
    }
    // Folders get the times they had in the archive, now that nothing more is written into them.
    for (i, out) in plan.iter().filter(|(i, _)| items[*i].is_dir) {
        if let Some(t) = filetime_to_system(items[*i].modified) {
            set_folder_time(out, t);
        }
    }
    Ok(())
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

fn extract_zip(archive: &str, zip: &mut zip::ZipArchive<BufReader<File>>, files: &[(&Item, &Path)], meter: &Meter) -> Result<(), String> {
    // For entries the library has no index for: the file, read from each one's own header.
    let mut direct: Option<BufReader<File>> = None;
    for (item, out) in files {
        let name = folder::file_name(&item.path);
        let unreadable = |e: zip::result::ZipError| match e {
            zip::result::ZipError::CompressionMethodNotSupported(_) => format!("\u{201c}{name}\u{201d} is packed in a way Glint can't unpack yet."),
            zip::result::ZipError::UnsupportedArchive(m) if m == zip::result::ZipError::PASSWORD_REQUIRED => PASSWORD.to_string(),
            zip::result::ZipError::InvalidPassword => PASSWORD.to_string(),
            zip::result::ZipError::Io(e) => format!("\u{201c}{name}\u{201d} couldn't be read from the archive ({e})."),
            _ => format!("\u{201c}{name}\u{201d} is damaged in this zip file."),
        };
        let Some(at) = item.hidden else {
            let mut reader = zip.by_index(item.index).map_err(unreadable)?;
            copy_out(&mut reader, out, item.modified, name, meter)?;
            continue;
        };
        if direct.is_none() {
            direct = Some(BufReader::new(File::open(archive).map_err(|e| open_error(&e))?));
        }
        let Some(source) = direct.as_mut() else { continue };
        source.seek(io::SeekFrom::Start(at.header)).map_err(|e| unreadable(e.into()))?;
        // (The central directory's sizes and checksum: right even when the header leaves them out.)
        let options = zip::ZipReadOptions::new().override_compressed_size(at.compressed).override_uncompressed_size(at.size).override_crc(at.crc);
        let mut reader = zip::read::read_zipfile_from_stream_with_options(source, options).map_err(unreadable)?.ok_or_else(|| format!("\u{201c}{name}\u{201d} is damaged in this zip file."))?;
        copy_out(&mut reader, out, item.modified, name, meter)?;
    }
    Ok(())
}

fn extract_7z(a: &sevenz_rust2::Archive, source: &mut BufReader<File>, files: &[(&Item, &Path)], meter: &Meter) -> Result<(), String> {
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
            _ => copy_out(&mut io::empty(), out, item.modified, folder::file_name(&item.path), meter)?,
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
        let decoder = sevenz_rust2::BlockDecoder::new(threads, b, a, &password, source);
        let result = decoder.for_each_entries(&mut |_entry, reader: &mut dyn Read| {
            let this = index;
            index += 1;
            match wanted.get(&this) {
                Some((item, out)) => {
                    if let Err(e) = copy_out(reader, out, item.modified, folder::file_name(&item.path), meter) {
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
fn copy_out(reader: &mut dyn Read, out: &Path, modified: u64, name: &str, meter: &Meter) -> Result<(), String> {
    let made = File::create_new(out);
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
        if !meter.add(n as u64) {
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
    created: Option<SystemTime>,
    accessed: Option<SystemTime>,
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
        self.items.push(Packed { path: path.to_path_buf(), name, is_dir: meta.is_dir(), size, modified: meta.modified().ok(), created: meta.created().ok(), accessed: meta.accessed().ok() });
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
            // A folder's own times: in its parent's listing Windows can still show older ones
            // after files were added inside it. (One more look per folder; folders are few.)
            let meta = if meta.is_dir() { std::fs::metadata(&path).unwrap_or(meta) } else { meta };
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
    // The packers' tables and buffers are all freed by now: the heap's free space goes back to
    // Windows rather than staying with Glint after the job.
    // SAFETY: plain call on our own process's default heap.
    unsafe {
        use windows::Win32::System::Memory::{GetProcessHeap, HeapCompact, HEAP_FLAGS};
        if let Ok(heap) = GetProcessHeap() {
            HeapCompact(heap, HEAP_FLAGS(0));
        }
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
    use sevenz_rust2::ArchiveWriter;
    let seven_error = |e: sevenz_rust2::Error| meter.problem.take().unwrap_or_else(|| format!("\u{201c}{}\u{201d} couldn't be written ({e}).", out.display()));
    let mut w = ArchiveWriter::new(BufWriter::new(file)).map_err(seven_error)?;
    // Files go in solid blocks of up to SOLID_BLOCK bytes, packed together. Those already packed
    // (photos, videos, archives...), which LZMA2 would only spend its time on, are stored as they
    // are, in blocks of their own: (files, bytes) for each kind.
    let mut packed: (Vec<&Packed>, u64) = (Vec::new(), 0);
    let mut stored: (Vec<&Packed>, u64) = (Vec::new(), 0);
    for p in &plan.items {
        if p.is_dir || p.size == 0 {
            w.push_archive_entry::<&[u8]>(sevenz_entry(p), None).map_err(seven_error)?;
            continue;
        }
        let store = already_packed(p);
        let block = if store { &mut stored } else { &mut packed };
        block.0.push(p);
        block.1 += p.size;
        if block.1 >= SOLID_BLOCK {
            push_block(&mut w, block, store, meter).map_err(seven_error)?;
        }
    }
    push_block(&mut w, &mut packed, false, meter).map_err(seven_error)?;
    push_block(&mut w, &mut stored, true, meter).map_err(seven_error)?;
    let mut f = w.finish().map_err(|e| meter.problem.take().unwrap_or_else(|| write_error(out, &e)))?;
    f.flush().map_err(|e| write_error(out, &e))
}

/// The 7z entry for `p`: what sevenz_rust2's `ArchiveEntry::from_path` makes, but from what the
/// walk already found (from_path asks the disk again, which opens each file three more times).
fn sevenz_entry(p: &Packed) -> sevenz_rust2::ArchiveEntry {
    use sevenz_rust2::NtTime;
    // (A time before 1601 isn't kept, as from_path keeps none.)
    let time = |t: Option<SystemTime>| t.and_then(|t| NtTime::try_from(t).ok()).filter(|&t| u64::from(t) > 0);
    let mut entry = sevenz_rust2::ArchiveEntry { name: p.name.clone(), has_stream: !p.is_dir, is_directory: p.is_dir, ..Default::default() };
    if let Some(t) = time(p.modified) {
        entry.last_modified_date = t;
        entry.has_last_modified_date = true;
    }
    if let Some(t) = time(p.created) {
        entry.creation_date = t;
        entry.has_creation_date = true;
    }
    if let Some(t) = time(p.accessed) {
        entry.access_date = t;
        entry.has_access_date = true;
    }
    entry
}

/// Kinds of file that are packed already: LZMA2 makes them a percent or two smaller at most, at
/// its full (slow) speed.
const PACKED_KINDS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "avif", "jxl", "mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "mp3", "m4a", "aac", "ogg", "opus", "flac", "wma", "zip", "7z", "rar", "gz", "tgz", "bz2", "xz", "zst", "lz4", "lzma", "cab", "jar", "apk", "docx", "xlsx", "pptx", "odt", "ods", "odp", "epub",
];
/// From this size a file of another kind is tried first ([`barely_shrinks`]): smaller ones take
/// little time anyway.
const TRY_FROM: u64 = 1 << 20;

/// Whether `p` goes into the 7z stored as it is: a kind that's packed already, or a big file
/// whose data doesn't shrink (random-looking: packed or encrypted data with another name).
fn already_packed(p: &Packed) -> bool {
    let name = p.name.rsplit('/').next().unwrap_or("");
    let kind = match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_ascii_lowercase(),
        _ => String::new(),
    };
    PACKED_KINDS.contains(&kind.as_str()) || (p.size >= TRY_FROM && barely_shrinks(&p.path, p.size))
}

/// Whether three samples of this file (its start, middle and end) shrink by less than 2% with a
/// quick Deflate. False when it can't be read: then packing it says why.
fn barely_shrinks(path: &Path, size: u64) -> bool {
    const SAMPLE: u64 = 64 * 1024;
    /// Counts what's written to it.
    struct Count(u64);
    impl Write for Count {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0 += buf.len() as u64;
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let Ok(mut file) = File::open(path) else { return false };
    let mut deflate = flate2::write::DeflateEncoder::new(Count(0), flate2::Compression::fast());
    let mut buf = vec![0u8; SAMPLE as usize];
    let mut read = 0u64;
    for at in [0, (size / 2).saturating_sub(SAMPLE / 2), size.saturating_sub(SAMPLE)] {
        if file.seek(io::SeekFrom::Start(at)).is_err() {
            return false;
        }
        let mut n = 0;
        while n < buf.len() {
            match file.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(m) => n += m,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        // (Each sample apart: the next one's start shouldn't look like a repeat of this one.)
        if deflate.write_all(&buf[..n]).and_then(|()| deflate.flush()).is_err() {
            return false;
        }
        read += n as u64;
    }
    let Ok(Count(out)) = deflate.finish() else { return false };
    // (A file that's shorter now than it was is packed as it is.)
    read >= SAMPLE && out * 100 >= read * 98
}

/// Packs these files (`block.1` bytes in all) together as one solid block of the 7z: stored as
/// they are, or with LZMA2 sized for them ([`lzma2_setup`]). Leaves `block` empty.
fn push_block<W: Write + io::Seek>(w: &mut sevenz_rust2::ArchiveWriter<W>, block: &mut (Vec<&Packed>, u64), store: bool, meter: &Meter) -> Result<(), sevenz_rust2::Error> {
    use sevenz_rust2::encoder_options::Lzma2Options;
    use sevenz_rust2::{EncoderConfiguration, EncoderMethod};
    let (files, bytes) = (std::mem::take(&mut block.0), std::mem::take(&mut block.1));
    if files.is_empty() {
        return Ok(());
    }
    if store {
        w.set_content_methods(vec![EncoderConfiguration::new(EncoderMethod::COPY)]);
    } else {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let (dict, threads) = lzma2_setup(bytes, cores.min(packers_with_room()));
        let mut options = if threads > 1 {
            // (Chunk size 0: one dictionary's worth for each thread, the smallest allowed.)
            Lzma2Options::from_level_mt(PACK_LEVEL, threads, 0)
        } else {
            Lzma2Options::from_level(PACK_LEVEL)
        };
        options.set_dictionary_size(dict);
        w.set_content_methods(vec![EncoderConfiguration::from(options)]);
    }
    let entries = files.iter().map(|p| sevenz_entry(p)).collect();
    let feeds = files.iter().map(|p| sevenz_rust2::SourceReader::new(Feed::new(&p.path, meter))).collect();
    w.push_archive_entries(entries, feeds).map(|_| ())
}

/// The LZMA2 level Glint packs 7z files at (7-Zip's own "normal").
const PACK_LEVEL: u32 = 6;
/// The dictionary that level has: the most a block is given.
const LEVEL_DICT: u32 = 8 << 20;
/// The least a block is given.
const MIN_DICT: u32 = 64 << 10;
/// The most packers that run at once.
const MOST_THREADS: usize = 8;

/// How the 7z packer is set up for a solid block of `bytes`, on a PC with `cores`: (dictionary
/// size, threads). The packer's memory goes with its dictionary - its match tables are about
/// eleven times as big, whatever it's given to pack, so level 6's 8 MB took about 95 MB for each
/// thread even for one small file - and a dictionary bigger than the data finds nothing more. So
/// it's the next power of two up from the data (64 KB at least, 8 MB at most). Threads each pack a
/// dictionary's worth of their own (and hold a packer each), so there's one for each such piece,
/// up to 8 (a block is 8 pieces): a block smaller than one piece is packed by one packer, without
/// a thread pool.
fn lzma2_setup(bytes: u64, cores: usize) -> (u32, u32) {
    let dict = bytes.clamp(1, u64::from(LEVEL_DICT)).next_power_of_two().max(u64::from(MIN_DICT)) as u32;
    let pieces = bytes.div_ceil(u64::from(dict)).max(1);
    let threads = (cores.clamp(1, MOST_THREADS) as u64).min(pieces) as u32;
    (dict, threads)
}

/// How many full-size packers (about 100 MB each) fit in a quarter of the memory free now, one
/// at least: on a PC short of memory a big 7z is packed by fewer threads rather than pushing
/// everything else out to the page file.
fn packers_with_room() -> usize {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: a plain query filling our own struct, its size given.
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
        return MOST_THREADS;
    }
    ((status.ullAvailPhys / 4 / (12 * u64::from(LEVEL_DICT))) as usize).max(1)
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
    let dir = temp_home(archive)?;
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

/// The archive's own folder in %TEMP%\Glint\archives (made if need be), named after the archive's
/// path and when it last changed.
fn temp_home(archive: &str) -> Result<PathBuf, String> {
    let meta = std::fs::metadata(archive).map_err(|e| open_error(&e))?;
    let changed = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
    let hash = fnv(archive.to_lowercase().bytes().chain(changed.to_le_bytes()));
    let dir = std::env::temp_dir().join("Glint").join("archives").join(format!("{:012x}", hash >> 16));
    std::fs::create_dir_all(&dir).map_err(|e| write_error(&dir, &e))?;
    Ok(dir)
}

/// FNV-1a: tiny, and the same on every run (unlike std's hasher).
fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash = (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3);
    }
    hash
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
    fn the_same_name_twice_shows_twice_and_each_comes_out_with_its_own_data() {
        // One name three times (added again and again), and names that differ only in capitals
        // (from a Linux zip): Windows keeps one of each name, so the later ones are numbered, in the
        // list and wherever they're taken out to, each with its own data.
        let t = Scratch::new("twice");
        let dup = t.join("dup.zip");
        zip_with(&dup, &[("a.txt", b"one"), ("b.txt", b"two!"), ("c.txt", b"three"), ("A.TXT", b"CAPS"), ("a (2).txt", b"really (2)"), ("docs/Same.txt", b"first"), ("docs/same.TXT", b"second!")]);
        patch(&dup, b"b.txt", b"a.txt");
        patch(&dup, b"c.txt", b"a.txt");
        let seven = t.join("dup.7z");
        seven_with(&seven, &[("a.txt", b"one"), ("a.txt", b"two!"), ("a.txt", b"three"), ("A.TXT", b"CAPS"), ("a (2).txt", b"really (2)"), ("docs/Same.txt", b"first"), ("docs/same.TXT", b"second!")]);
        // The first keeps its name; the others take the next numbers free, never one the archive
        // already uses ("a (2).txt" is a real one here).
        let want: &[(&str, &[u8])] = &[
            ("a.txt", b"one"),
            ("a (3).txt", b"two!"),
            ("a (4).txt", b"three"),
            ("A (5).TXT", b"CAPS"),
            ("a (2).txt", b"really (2)"),
            ("docs\\Same.txt", b"first"),
            ("docs\\same (2).TXT", b"second!"),
        ];
        for archive in [&dup, &seven] {
            let a = s(archive);
            let mut listed: Vec<(String, u64)> = read(&a, "", "").entries.iter().map(|e| (e.name.clone(), e.size)).collect();
            listed.sort();
            let mut top: Vec<(String, u64)> = want.iter().filter(|(p, _)| !p.contains('\\')).map(|(p, d)| (p.to_string(), d.len() as u64)).chain([("docs".to_string(), 0)]).collect();
            top.sort();
            assert_eq!(listed, top, "{a}");
            assert_eq!(names(&read(&a, "docs", "")), ["Same.txt", "same (2).TXT"], "{a}");
            // Each one taken out on its own (open, copy, drag) brings its own data.
            for (i, (path, data)) in want.iter().enumerate() {
                let taken = t.dir(&format!("one-{}-{i}", folder::file_name(&a)));
                let tops = extract(&a, &[path.to_string()], &s(&taken)).expect("take out");
                assert_eq!(tops, [s(&taken.join(folder::file_name(path)))], "{a} {path}");
                assert_eq!(std::fs::read(&tops[0]).expect("read"), *data, "{a} {path}");
            }
            // All of them at once, to one folder: none is written over another.
            let together = t.dir(&format!("together-{}", folder::file_name(&a)));
            let all: Vec<String> = want.iter().map(|(p, _)| p.to_string()).collect();
            extract(&a, &all, &s(&together)).expect("take out all");
            assert_eq!(names_in(&together).len(), want.len(), "{a}");
            assert_eq!(unpack_size(&a, &all), Ok(want.iter().map(|(_, d)| d.len() as u64).sum()), "{a}");
            // Extract here: every one, as listed.
            let here = t.dir(&format!("here-{}", folder::file_name(&a)));
            extract_all(&a, &s(&here), &mut Vec::new()).expect("extract here");
            let mut expect: Vec<(String, Option<Vec<u8>>)> = want.iter().map(|(p, d)| (p.to_string(), Some(d.to_vec()))).chain([("docs".to_string(), None)]).collect();
            expect.sort();
            assert_eq!(tree(&here), expect, "{a}");
            // Taking one out where its name is already used is refused: what's there stays.
            let busy = t.dir(&format!("busy-{}", folder::file_name(&a)));
            std::fs::write(busy.join("a (3).txt"), b"mine").expect("write");
            assert!(extract(&a, &["a (3).txt".to_string()], &s(&busy)).is_err(), "{a}");
            assert_eq!(std::fs::read(busy.join("a (3).txt")).expect("read"), b"mine", "{a}");
        }
    }

    #[test]
    fn the_same_name_twice_in_a_streamed_or_stored_zip_comes_out_too() {
        // A zip written as a stream keeps its sizes after the data, not in the entry's header; and
        // one stored without packing. The second of a name is read from where its header is.
        let t = Scratch::new("twicestream");
        for (name, method) in [("stream.zip", zip::CompressionMethod::Deflated), ("stored.zip", zip::CompressionMethod::Stored)] {
            let mut w = zip::ZipWriter::new_stream(Vec::new());
            let opts = zip::write::SimpleFileOptions::default().compression_method(method);
            for (n, data) in [("x.txt", &b"first one"[..]), ("y.txt", &b"second, longer one"[..])] {
                w.start_file(n, opts).expect("start");
                w.write_all(data).expect("write");
            }
            let path = t.join(name);
            std::fs::write(&path, w.finish().expect("finish").into_inner()).expect("write");
            patch(&path, b"y.txt", b"x.txt");
            let a = s(&path);
            assert_eq!(names(&read(&a, "", "")), ["x (2).txt", "x.txt"], "{name}");
            let out = t.dir(&format!("out-{name}"));
            extract(&a, &["x.txt".to_string(), "x (2).txt".to_string()], &s(&out)).expect("take out");
            assert_eq!(tree(&out), expected(&[("x (2).txt", Some(b"second, longer one")), ("x.txt", Some(b"first one"))]), "{name}");
        }
    }

    #[test]
    fn a_file_named_like_a_folder_beside_it_gets_its_own_name() {
        // A zip can hold a file "docs" and a folder "docs": on disk they can't both be "docs".
        let t = Scratch::new("filefolder");
        let zip = t.join("both.zip");
        zip_with(&zip, &[("docs", b"a file"), ("docs/in.txt", b"inside")]);
        let z = s(&zip);
        assert_eq!(names(&read(&z, "", "")), ["docs (2)", "docs\\"]);
        let here = t.dir("here");
        extract_all(&z, &s(&here), &mut Vec::new()).expect("extract");
        assert_eq!(tree(&here), expected(&[("docs", None), ("docs (2)", Some(b"a file")), ("docs\\in.txt", Some(b"inside"))]));
    }

    /// A zip of these entries, as bytes (to go inside another one).
    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            w.start_file(*name, opts).expect("start");
            w.write_all(data).expect("write");
        }
        w.finish().expect("finish").into_inner()
    }

    /// outer.zip > sub > mid.zip > level2 > deep.zip > deep.txt (names with # and, where the disk
    /// takes them, emoji): (the outer zip, the mid and deep zips' names, the deep zip's bytes).
    fn three_deep(t: &Scratch) -> (PathBuf, String, String, Vec<u8>) {
        let fancy = crate::search::odd_names_work(&t.0);
        let deep_name = if fancy { "deep \u{1f600} #3.zip" } else { "deep #3.zip" }.to_string();
        let mid_name = "mid #2.zip".to_string();
        let deep = zip_bytes(&[("deep.txt", b"at the bottom"), ("more/x.txt", b"x")]);
        let mid = zip_bytes(&[(&format!("level2/{deep_name}"), &deep), ("two.txt", b"two")]);
        let outer = t.join("outer #1.zip");
        zip_with(&outer, &[(&format!("sub/{mid_name}"), &mid), ("one.txt", b"one")]);
        (outer, mid_name, deep_name, deep)
    }

    #[test]
    fn an_archive_inside_an_archive_is_read_at_its_path_there() {
        let t = Scratch::new("nested");
        let (outer, mid, deep, deep_bytes) = three_deep(&t);
        let o = s(&outer);
        let deepest = format!("{o}\\sub\\{mid}\\level2\\{deep}");
        // Every level, by its whole path: the outer zip is the archive, the rest is inside it.
        assert_eq!(split(&deepest), Some((o.clone(), format!("sub\\{mid}\\level2\\{deep}"))));
        assert_eq!(names(&folder::read(&deepest)), ["deep.txt", "more\\"]);
        assert_eq!(names(&folder::read(&format!("{deepest}\\MORE"))), ["x.txt"]);
        assert_eq!(folder::read(&deepest).path, deepest);
        // Up from each level, and what's there.
        let mut at = format!("{deepest}\\more");
        let mut seen = Vec::new();
        while at != s(&t.0) {
            let listed = folder::read(&at);
            assert_eq!(listed.error, None, "{at}");
            seen.push(names(&listed));
            at = folder::parent(&at).expect("a parent");
        }
        let want: Vec<Vec<String>> = vec![
            vec!["x.txt".into()],
            vec!["deep.txt".into(), "more\\".into()],
            vec![deep.clone()],
            vec!["level2\\".into(), "two.txt".into()],
            vec![mid.clone()],
            vec!["one.txt".into(), "sub\\".into()],
        ];
        assert_eq!(seen, want);
        // Up from the top of the outer zip is the folder the zip is in.
        assert_eq!(folder::parent(&o), Some(s(&t.0)));
        // A file in the deepest one is a file, not a folder.
        assert_eq!(folder::read(&format!("{deepest}\\deep.txt")).error.as_deref(), Some(folder::NOT_A_FOLDER));
        assert_eq!(folder::read(&format!("{o}\\sub\\{mid}\\two.txt")).error.as_deref(), Some(folder::NOT_A_FOLDER));
        // Taking things out of the deepest one: a file, a folder, and the deep zip itself whole.
        let out = t.dir("out");
        let tops = extract(&o, &[format!("sub\\{mid}\\level2\\{deep}\\deep.txt"), format!("sub\\{mid}\\level2\\{deep}\\more")], &s(&out)).expect("take out");
        assert_eq!(tops, [s(&out.join("deep.txt")), s(&out.join("more"))]);
        assert_eq!(std::fs::read(out.join("deep.txt")).expect("read"), b"at the bottom");
        assert_eq!(std::fs::read(out.join("more").join("x.txt")).expect("read"), b"x");
        let whole = t.dir("whole");
        extract(&o, &[format!("sub\\{mid}\\level2\\{deep}")], &s(&whole)).expect("take out the zip");
        assert_eq!(std::fs::read(whole.join(&deep)).expect("read"), deep_bytes);
        assert_eq!(unpack_size(&o, &[format!("sub\\{mid}\\level2\\{deep}\\deep.txt")]), Ok(13));
        // Read again (as Back and Forward do): the copy taken out the first time is used.
        assert_eq!(names(&folder::read(&deepest)), ["deep.txt", "more\\"]);
        // A name that only looks like an archive, and one that isn't there.
        assert!(folder::read(&format!("{o}\\sub\\nope.zip")).error.is_some());
        assert!(folder::read(&format!("{o}\\sub\\{mid}\\nope.7z\\x")).error.is_some());
    }

    #[test]
    fn a_changed_outer_archive_shows_its_new_inner_archive() {
        let t = Scratch::new("nestedchange");
        let outer = t.join("o.zip");
        zip_with(&outer, &[("in.zip", &zip_bytes(&[("old.txt", b"old")]))]);
        let o = s(&outer);
        assert_eq!(names(&folder::read(&format!("{o}\\in.zip"))), ["old.txt"]);
        std::fs::remove_file(&outer).expect("remove");
        zip_with(&outer, &[("in.zip", &zip_bytes(&[("new.txt", b"newer")]))]);
        File::options().write(true).open(&outer).expect("open").set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000)).expect("time");
        assert_eq!(names(&folder::read(&format!("{o}\\in.zip"))), ["new.txt"]);
    }

    #[test]
    fn a_7z_inside_a_zip_inside_a_7z_opens_too() {
        let t = Scratch::new("nestedmix");
        let seven_inner = t.join("inner.7z");
        seven_with(&seven_inner, &[("x/y.txt", b"why")]);
        let zip = zip_bytes(&[("inner.7z", &std::fs::read(&seven_inner).expect("read"))]);
        let outer = t.join("outer.7z");
        seven_with(&outer, &[("dir/", b""), ("dir/mid.zip", &zip)]);
        let o = s(&outer);
        let at = format!("{o}\\dir\\mid.zip\\inner.7z\\x");
        assert_eq!(names(&folder::read(&at)), ["y.txt"]);
        let out = t.dir("out");
        extract(&o, &["dir\\mid.zip\\inner.7z\\x\\y.txt".to_string()], &s(&out)).expect("take out");
        assert_eq!(std::fs::read(out.join("y.txt")).expect("read"), b"why");
    }

    #[test]
    fn the_7z_packer_is_sized_by_what_it_packs() {
        const K: u64 = 1024;
        const M: u64 = 1024 * K;
        // Small: the next power of two up, 64 KB at least, one packer and no thread pool.
        assert_eq!(lzma2_setup(0, 8), (64 * 1024, 1));
        assert_eq!(lzma2_setup(1, 8), (64 * 1024, 1));
        assert_eq!(lzma2_setup(64 * K, 8), (64 * 1024, 1));
        assert_eq!(lzma2_setup(64 * K + 1, 8), (128 * 1024, 1));
        assert_eq!(lzma2_setup(3 * M, 8), (4 << 20, 1));
        assert_eq!(lzma2_setup(8 * M, 8), (8 << 20, 1));
        // Big: level 6's own 8 MB at most, and a thread for each 8 MB piece, up to 8 (and the cores).
        assert_eq!(lzma2_setup(8 * M + 1, 8), (8 << 20, 2));
        assert_eq!(lzma2_setup(20 * M, 8), (8 << 20, 3));
        assert_eq!(lzma2_setup(64 * M, 8), (8 << 20, 8));
        assert_eq!(lzma2_setup(64 * M, 32), (8 << 20, 8));
        assert_eq!(lzma2_setup(u64::MAX, 8), (8 << 20, 8));
        assert_eq!(lzma2_setup(64 * M, 4), (8 << 20, 4));
        assert_eq!(lzma2_setup(64 * M, 2), (8 << 20, 2));
        assert_eq!(lzma2_setup(64 * M, 1), (8 << 20, 1));
        assert_eq!(lzma2_setup(64 * M, 0), (8 << 20, 1));

        // And it's what a packed 7z says it was packed with: one byte, an empty folder, a few KB.
        let t = Scratch::new("dict");
        std::fs::write(t.join("one.txt"), b"1").expect("write");
        let empty = t.dir("Empty");
        let small = t.dir("Small #1");
        std::fs::write(small.join("a.txt"), "words ".repeat(20_000)).expect("write");
        let never = AtomicBool::new(false);
        for (name, what, dict) in [("one", s(&t.join("one.txt")), Some(64 * 1024)), ("empty", s(&empty), None), ("small", s(&small), Some(128 * 1024))] {
            let plan = plan_pack(std::slice::from_ref(&what), &never).expect("plan").expect("not stopped");
            let out = t.join(&format!("{name}.7z"));
            pack(&plan, &s(&out), Format::SevenZ).expect("pack");
            let (a, _) = open_7z(&s(&out)).expect("open");
            let dicts: Vec<u32> = a.blocks.iter().flat_map(|b| &b.coders).map(|c| lzma2_dict(c.properties())).collect();
            assert_eq!(dicts, dict.into_iter().collect::<Vec<u32>>(), "{name}");
            let back = t.dir(&format!("back-{name}"));
            extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
            if name == "one" {
                assert_eq!(tree(&back), expected(&[("one.txt", Some(b"1"))]));
            } else {
                assert_eq!(tree(&back.join(folder::file_name(&what))), tree(Path::new(&what)), "{name}");
            }
        }
    }

    /// The dictionary size an LZMA2 coder's one property byte gives.
    fn lzma2_dict(props: &[u8]) -> u32 {
        let p = u32::from(props[0]);
        (2 | (p & 1)) << (p / 2 + 11)
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

    /// `n` bytes that look random (and don't shrink), the same each time for one `seed`.
    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(n + 8);
        while out.len() < n {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(n);
        out
    }

    /// The 7z coder each file of `a` is packed with: "copy", "lzma2", or "" for no data.
    fn coders(a: &sevenz_rust2::Archive) -> Vec<(String, &'static str)> {
        let mut out: Vec<(String, &'static str)> = a
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let block = a.stream_map.file_block_index.get(i).copied().flatten().map(|b| &a.blocks[b]);
                let how = match block.map(|b| b.coders[0].encoder_method_id().to_vec()) {
                    None => "",
                    Some(id) if id == [0x00] => "copy",
                    Some(id) if id == [0x21] => "lzma2",
                    Some(_) => "other",
                };
                (f.name.clone(), how)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn photos_videos_and_data_that_doesnt_shrink_are_stored_in_a_7z_and_come_out_exactly() {
        let t = Scratch::new("stored");
        // (Emoji where the disk takes them: not under Wine without a UTF-8 locale.)
        let fancy = crate::search::odd_names_work(&t.0);
        let d = if fancy { "Mixed \u{1f4f7}" } else { "Mixed #1" };
        let clip = if fancy { "\u{1f600} clip.mp4" } else { "a clip.mp4" };
        let top = t.dir(d);
        std::fs::write(top.join("PHOTO.JPG"), noise(300_000, 1)).expect("write");
        std::fs::write(top.join(clip), noise(2_000_000, 2)).expect("write");
        std::fs::write(top.join("inner.zip"), zip_bytes(&[("a.txt", b"inside")])).expect("write");
        // Random data with a name that says nothing, big enough to be tried, and text named the same.
        std::fs::write(top.join("noise.bin"), noise(3_000_000, 3)).expect("write");
        std::fs::write(top.join("words.bin"), "plain words again and again ".repeat(100_000)).expect("write");
        // Text first, random after: the samples see both, and it shrinks.
        let mut half = "half text ".repeat(150_000).into_bytes();
        half.extend(noise(1_500_000, 4));
        std::fs::write(top.join("half.dat"), &half).expect("write");
        // Small text, a file only named ".jpg", and nothing at all.
        std::fs::write(top.join("notes.txt"), b"notes").expect("write");
        std::fs::write(top.join(".jpg"), "not a photo ".repeat(100)).expect("write");
        std::fs::write(top.join("empty.png"), b"").expect("write");
        let never = AtomicBool::new(false);
        let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
        let out = t.join("mixed.7z");
        pack(&plan, &s(&out), Format::SevenZ).expect("pack");
        let (a, _) = open_7z(&s(&out)).expect("open");
        let want: Vec<(String, &str)> = vec![
            (d.to_string(), ""),
            (format!("{d}/.jpg"), "lzma2"),
            (format!("{d}/PHOTO.JPG"), "copy"),
            (format!("{d}/empty.png"), ""),
            (format!("{d}/half.dat"), "lzma2"),
            (format!("{d}/inner.zip"), "copy"),
            (format!("{d}/noise.bin"), "copy"),
            (format!("{d}/notes.txt"), "lzma2"),
            (format!("{d}/words.bin"), "lzma2"),
            (format!("{d}/{clip}"), "copy"),
        ];
        let mut want = want;
        want.sort();
        assert_eq!(coders(&a), want);
        // Stored ones take no more room than they hold, and everything comes out as it went in.
        assert!(std::fs::metadata(&out).expect("size").len() < 300_000 + 2_000_000 + 3_000_000 + 1_600_000 + 10_000);
        let back = t.dir("back");
        extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
        assert_eq!(tree(&back.join(d)), tree(&top));
        // One out of the middle of a stored block, and the zip inside opened where it is.
        let one = t.dir("one");
        extract(&s(&out), &[format!("{d}\\noise.bin")], &s(&one)).expect("take out");
        assert_eq!(std::fs::read(one.join("noise.bin")).expect("read"), noise(3_000_000, 3));
        assert_eq!(names(&read(&s(&out), &format!("{d}\\inner.zip"), "")), ["a.txt"]);
    }

    #[test]
    fn the_quick_try_tells_data_that_shrinks_from_data_that_doesnt() {
        let t = Scratch::new("try");
        let file = |name: &str, data: &[u8]| {
            let p = t.join(name);
            std::fs::write(&p, data).expect("write");
            let size = data.len() as u64;
            (p, size)
        };
        let (p, n) = file("noise", &noise(2 << 20, 9));
        assert!(barely_shrinks(&p, n));
        let (p, n) = file("zeros", &vec![0u8; 2 << 20]);
        assert!(!barely_shrinks(&p, n));
        // Random but for a repeated middle: one sample shrinks, so all of it is packed.
        let mut mid = noise(2 << 20, 10);
        mid[(1 << 20) - 40_000..(1 << 20) + 40_000].fill(b'x');
        let (p, n) = file("middle", &mid);
        assert!(!barely_shrinks(&p, n));
        // Gone, or shorter than it was: never stored on a guess.
        assert!(!barely_shrinks(&t.join("nothing here"), 2 << 20));
        let (p, _) = file("short", b"tiny");
        assert!(!barely_shrinks(&p, 2 << 20));
        let (p, _) = file("empty", b"");
        assert!(!barely_shrinks(&p, 2 << 20));
    }

    #[test]
    fn a_7z_entry_has_the_times_the_disk_has_without_asking_it_again() {
        let t = Scratch::new("times");
        let top = make_tree(&t.0);
        let never = AtomicBool::new(false);
        let plan = plan_pack(&[s(&top)], &never).expect("plan").expect("not stopped");
        assert!(plan.items.len() >= 6);
        for p in &plan.items {
            let mine = sevenz_entry(p);
            let theirs = sevenz_rust2::ArchiveEntry::from_path(&p.path, p.name.clone());
            let fields = |e: &sevenz_rust2::ArchiveEntry| {
                (e.name.clone(), e.has_stream, e.is_directory, e.has_last_modified_date, u64::from(e.last_modified_date), e.has_creation_date, u64::from(e.creation_date), e.has_access_date)
            };
            assert_eq!(fields(&mine), fields(&theirs), "{}", p.name);
            // (Reading the disk again may have moved the access time on.)
            assert!(!mine.has_access_date || u64::from(mine.access_date) > 0, "{}", p.name);
        }
        // And the packed archive says so: a.txt's old time.
        let out = t.join("times.7z");
        pack(&plan, &s(&out), Format::SevenZ).expect("pack");
        let (a, _) = open_7z(&s(&out)).expect("open");
        let f = a.files.iter().find(|f| f.name == "Stuff/a.txt").expect("a.txt");
        assert!(f.has_last_modified_date && f.has_creation_date);
        assert_eq!(SystemTime::from(f.last_modified_date), UNIX_EPOCH + Duration::from_secs(1_500_000_000));
        let d = a.files.iter().find(|f| f.name == "Stuff/empty").expect("empty");
        assert!(d.is_directory && !d.has_stream && d.has_last_modified_date);
    }

    #[test]
    fn dragging_out_checks_the_size_first_and_makes_nothing_when_it_is_too_much() {
        let t = Scratch::new("atmost");
        let smile = if crate::search::odd_names_work(&t.0) { "\u{1f600}.txt" } else { "smile.txt" };
        for ext in ["zip", "7z"] {
            let path = t.join(&format!("a.{ext}"));
            archive_with(&path, &[("docs/a.txt", b"aaaa"), ("docs/b.txt", b"bb"), ("big.txt", &[b'x'; 5000]), (smile, b"smile")]);
            let a = s(&path);
            let home = temp_home(&a).expect("home");
            let before = std::fs::read_dir(&home).map_or(0, |d| d.count());
            let err = extract_at_most(&a, &["big.txt".to_string()], 4999, "Too much.").expect_err("too big");
            assert_eq!(err, "Too much.");
            assert_eq!(std::fs::read_dir(&home).map_or(0, |d| d.count()), before, "{ext}: no folder made");
            let err = extract_at_most(&a, &["nope.txt".to_string()], u64::MAX, "Too much.").expect_err("missing");
            assert!(err.contains("nope.txt"), "{err}");
            let (dir, tops) = extract_at_most(&a, &["docs".to_string(), smile.to_string()], 4 + 2 + 5, "Too much.").expect("fits");
            assert_eq!(tops, [folder::join(&dir, "docs"), folder::join(&dir, smile)]);
            assert_eq!(std::fs::read(Path::new(&dir).join("docs").join("b.txt")).expect("read"), b"bb");
            // Again: a new folder, nothing replaced.
            let (again, _) = extract_at_most(&a, &["docs".to_string()], 6, "Too much.").expect("again");
            assert_ne!(again, dir);
        }
        // From an archive inside one: counted and taken out of the inner one.
        let (outer, mid, deep, _) = three_deep(&t);
        let o = s(&outer);
        let item = format!("sub\\{mid}\\level2\\{deep}\\deep.txt");
        let (dir, tops) = extract_at_most(&o, std::slice::from_ref(&item), 13, "Too much.").expect("deep");
        assert_eq!(std::fs::read(&tops[0]).expect("read"), b"at the bottom");
        assert!(tops[0].starts_with(&dir));
        assert_eq!(extract_at_most(&o, &[item], 12, "Too much."), Err("Too much.".to_string()));
    }

    /// How long packing and unpacking a fixed set takes (not run by default): 50 MB of text and
    /// 100 MB of random bytes, made in the temporary folder. Run on a real PC with
    /// `cargo test --release archive_timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn archive_timing_of_a_fixed_set() {
        let t = Scratch::new("timing");
        let src = t.dir("set");
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let words = ["the", "file", "folder", "window", "glint", "archive", "packed", "small", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "explorer", "copy", "paste", "rename", "delete", "photo", "music", "report", "notes", "draft", "final", "version", "2026", "settings", "theme"];
        for n in 0..10 {
            let mut text = String::with_capacity(5 << 20);
            while text.len() < 5 << 20 {
                let r = next();
                text.push_str(words[(r % words.len() as u64) as usize]);
                text.push(if r >> 60 == 0 { '\n' } else { ' ' });
            }
            std::fs::write(src.join(format!("text{n}.txt")), text).expect("write");
        }
        for n in 0..4 {
            let bytes: Vec<u8> = (0..(25 << 20) / 8).flat_map(|_| next().to_le_bytes()).collect();
            std::fs::write(src.join(format!("random{n}.bin")), bytes).expect("write");
        }
        let never = AtomicBool::new(false);
        let plan = plan_pack(&[s(&src)], &never).expect("plan").expect("not stopped");
        for format in [Format::Zip, Format::SevenZ] {
            let out = t.join(&format!("set{}", format.ext()));
            let start = Instant::now();
            pack(&plan, &s(&out), format).expect("pack");
            let packed = start.elapsed();
            let size = std::fs::metadata(&out).map_or(0, |m| m.len());
            let back = t.dir(&format!("back{}", format.ext()));
            let start = Instant::now();
            extract_all(&s(&out), &s(&back), &mut Vec::new()).expect("extract");
            eprintln!("{format:?}: {} MB packed in {packed:?} to {} MB, unpacked in {:?}", plan.total >> 20, size >> 20, start.elapsed());
        }
    }
}
