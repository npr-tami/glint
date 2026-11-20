//! Reading folders and drives, sorting, and the small path helpers.
//!
//! [`read`] lists one folder with FindFirstFileExW in its fastest mode (basic info only, large
//! fetches). It runs on a background thread (see app.rs) and never panics: a folder that can't be
//! read (no permission, gone, an empty DVD drive) comes back with `error` set and no entries, and
//! the list shows the message instead.
//!
//! The path "" means "This PC": its entries are the drives.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::mpsc::channel;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, LPARAM, SYSTEMTIME};
use windows::Win32::Globalization::{
    GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_SHORTDATE, ENUM_DATE_FORMATS_FLAGS, LOCALE_SDECIMAL, LOCALE_STHOUSAND,
    LCMapStringEx, LCMAP_SORTKEY, LINGUISTIC_IGNORECASE, SORT_DIGITSASNUMBERS, TIME_NOSECONDS,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetFileAttributesW, GetFullPathNameW, GetLogicalDrives, GetVolumeInformationW,
    FIND_FIRST_EX_LARGE_FETCH, INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime};

use crate::icons;
use crate::recycle;
use crate::settings::{DateFormat, SizeFormat};

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_HIDDEN: u32 = 0x2;
const ATTR_SYSTEM: u32 = 0x4;

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
    /// A drive on This PC.
    Drive,
}

/// One item in a folder.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// The name in lower case, for sorting, filtering and type-to-jump.
    pub lower: String,
    pub kind: EntryKind,
    /// Bytes (for drives: total size).
    pub size: u64,
    /// Last change, as a FILETIME (100 ns steps since 1601, UTC); 0 when unknown.
    pub modified: u64,
    /// When it was made, the same way; 0 when unknown (drives).
    pub created: u64,
    pub attributes: u32,
    /// The extension in lower case without the dot ("" for none and for folders).
    pub ext: String,
    /// What Windows calls this kind of file ("Text Document", "File folder").
    pub type_name: Arc<str>,
    /// Where opening it goes when that isn't folder + name: drives ("C:\"), and search results,
    /// which come from the folder's subfolders (their full path; see search.rs).
    pub target: Option<String>,
    /// Drives only: free bytes.
    pub free: u64,
}

impl Entry {
    /// Folders and drives: things you go into.
    pub fn is_container(&self) -> bool {
        self.kind != EntryKind::File
    }
    pub fn is_hidden(&self) -> bool {
        self.attributes & ATTR_HIDDEN != 0
    }
    /// Whether it shows, Explorer's way: hidden items only with `show_hidden`, protected
    /// operating-system files only with `show_system` as well.
    pub fn is_visible(&self, show_hidden: bool, show_system: bool) -> bool {
        visible(self.attributes, show_hidden, show_system)
    }
    /// The full path of this entry inside `folder`.
    pub fn path_in(&self, folder: &str) -> String {
        match &self.target {
            Some(t) => t.clone(),
            None => join(folder, &self.name),
        }
    }
    /// What tells this entry apart from the others in its listing, to find it again in a new read
    /// of the folder: its target (a drive's root, where a Recycle Bin item is kept) or else its
    /// name, in lower case. Names alone aren't enough in the Recycle Bin, which can hold three
    /// "notes.txt" deleted from three folders.
    pub fn key(&self) -> String {
        self.target.as_deref().map_or_else(|| self.lower.clone(), str::to_lowercase)
    }
}

/// Whether an item with these attributes shows (the same rule as `Entry::is_visible`, for the
/// subfolder search, which decides before it makes an entry).
pub fn visible(attributes: u32, show_hidden: bool, show_system: bool) -> bool {
    let hidden = attributes & ATTR_HIDDEN != 0;
    // Protected operating-system files are hidden and system, like desktop.ini or pagefile.sys.
    let protected = attributes & (ATTR_HIDDEN | ATTR_SYSTEM) == ATTR_HIDDEN | ATTR_SYSTEM;
    !hidden || (show_hidden && (show_system || !protected))
}

/// Free space on the drive a folder is on.
#[derive(Clone, Debug, Default)]
pub struct Space {
    /// "C:" (or the share for network paths).
    pub drive: String,
    pub free: u64,
}

/// A folder as read from disk.
#[derive(Clone, Debug, Default)]
pub struct Folder {
    pub path: String,
    pub entries: Vec<Entry>,
    /// Why it couldn't be read, in plain words.
    pub error: Option<String>,
    pub space: Option<Space>,
    /// How long reading took (file system only), in milliseconds.
    pub read_ms: f64,
    /// The Recycle Bin only: where each entry came from and where it is kept (recycle.rs).
    pub bin: Option<Arc<recycle::Listing>>,
    /// Home only: where its parts start (recent.rs).
    pub home: Option<Box<crate::recent::Parts>>,
}

/// A drive for This PC and the sidebar.
#[derive(Clone, Debug)]
pub struct Drive {
    /// "C:\"
    pub root: String,
    /// What Explorer shows: "Local Disk (C:)", "Backup (D:)", "DVD Drive (E:)".
    pub name: String,
    /// "Local Disk", "Removable Disk", "CD Drive", "Network Drive".
    pub kind_name: &'static str,
    pub total: u64,
    pub free: u64,
    /// False for an empty card reader or DVD drive.
    pub ready: bool,
}

/// Which column a list is sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortKey {
    Name,
    /// Date modified.
    Date,
    Type,
    Size,
    /// Date created.
    Created,
    /// The folder a search result is in (only while search results are shown).
    Folder,
    /// Image dimensions: how many pixels a picture or a video has (media.rs reads it).
    Dimensions,
    /// How long a video or a song plays (media.rs reads it).
    Length,
}

/// "This is a file, not a folder.": what reading a path that is a file says. Going to a file
/// opens it instead (app.rs, `navigate`).
pub const NOT_A_FOLDER: &str = "This is a file, not a folder.";

/// Whether `path` is one of Glint's places that aren't folders: the Recycle Bin or Recent. Nothing
/// can be made, pasted or dropped there.
pub fn is_place(path: &str) -> bool {
    recycle::is_bin(path) || crate::recent::is_recent(path) || crate::diskspace::is_disk(path)
}

/// Reads one folder. Never panics; problems end up in `Folder::error`.
pub fn read(path: &str) -> Folder {
    let start = Instant::now();
    let mut folder = if recycle::is_bin(path) {
        recycle::read()
    } else if crate::recent::is_recent(path) {
        crate::recent::read()
    } else if crate::diskspace::is_disk(path) {
        // Disk space draws its own page (diskpage.rs) from its look (app/disk.rs).
        Folder { path: path.to_string(), ..Default::default() }
    } else if path.is_empty() {
        this_pc()
    } else if !is_absolute(path) {
        // A bare name typed while on This PC: there's no folder to start from (and the process's
        // current folder has nothing to do with what's on screen).
        let error = format!("\u{201c}{path}\u{201d} isn't a full path. Type one like C:\\Users.");
        Folder { path: path.to_string(), error: Some(error), ..Default::default() }
    } else if let Some((archive, inner)) = crate::archive::split(path) {
        // Inside a .zip or .7z: read from the archive (archive.rs).
        crate::archive::read(&archive, &inner, path)
    } else {
        let mut f = Folder { path: path.to_string(), ..Default::default() };
        match list_dir(path) {
            Ok(entries) => f.entries = entries,
            Err(msg) => f.error = Some(msg),
        }
        f.space = space_for(path);
        f
    };
    folder.read_ms = start.elapsed().as_secs_f64() * 1000.0;
    folder
}

fn list_dir(path: &str) -> Result<Vec<Entry>, String> {
    let pattern = wide_path(&join(path, "*"));
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL-terminated and outlives the call; `data` is a valid WIN32_FIND_DATAW.
    let handle = unsafe {
        FindFirstFileExW(
            PCWSTR(pattern.as_ptr()),
            FindExInfoBasic,
            &mut data as *mut _ as *mut _,
            FindExSearchNameMatch,
            None,
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };
    let handle = match handle {
        Ok(h) => h,
        Err(e) => {
            // A drive root with nothing on it has no "." entry, so "*" finds nothing: that's just empty.
            if e.code().0 as u32 == 0x8007_0002 && is_dir(path) {
                return Ok(Vec::new());
            }
            // A file's path (typed into the path bar): Windows says "not found", "invalid folder
            // name" or "invalid name" depending on the version; whatever it says, it's a file.
            if is_file(path) {
                return Err(NOT_A_FOLDER.to_string());
            }
            return Err(explain(e.code().0 as u32, &e.message()));
        }
    };
    let mut entries = Vec::with_capacity(256);
    let mut types: HashMap<String, Arc<str>> = HashMap::new();
    let mut failed = None;
    loop {
        if let Some(e) = entry_from(&data, &mut types) {
            entries.push(e);
        }
        // SAFETY: `handle` is the live search handle from above; `data` is valid.
        if let Err(e) = unsafe { FindNextFileW(handle, &mut data) } {
            // A drive or network share that stopped answering part way (pulled out, the network
            // gone) is an error, not a shorter list. (Other errors end the list as before: some
            // drives end it their own way.)
            let code = e.code().0 as u32;
            if code >> 16 == 0x8007 && matches!(code & 0xFFFF, 21 | 31 | 53 | 59 | 64 | 121 | 1117 | 1167 | 1231 | 1236) {
                failed = Some(explain(code, &e.message()));
            }
            break;
        }
    }
    // SAFETY: closes the handle we opened; it is not used afterwards.
    unsafe {
        let _ = FindClose(handle);
    }
    match failed {
        Some(msg) => Err(msg),
        None => Ok(entries),
    }
}

/// One item from a FindFirstFile / FindNextFile result (None for "." and ".."). `types` caches the
/// type names by extension for one read. The subfolder search uses it too (search.rs).
pub fn entry_from(data: &WIN32_FIND_DATAW, types: &mut HashMap<String, Arc<str>>) -> Option<Entry> {
    let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
    let name = String::from_utf16_lossy(&data.cFileName[..len]);
    if name == "." || name == ".." {
        return None;
    }
    let attrs = data.dwFileAttributes;
    // Hidden files and protected operating-system files are listed too; the tab decides whether
    // to show them (app.rs, `Tab::rebuild_view`), so switching them on needs no new read.
    let is_dir = attrs & ATTR_DIRECTORY != 0;
    let ext = if is_dir { String::new() } else { extension(&name) };
    let type_name = types.entry(if is_dir { "\\dir".to_string() } else { ext.clone() }).or_insert_with(|| icons::type_name(&ext, is_dir)).clone();
    Some(Entry {
        lower: name.to_lowercase(),
        name,
        kind: if is_dir { EntryKind::Folder } else { EntryKind::File },
        size: if is_dir { 0 } else { ((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64 },
        modified: ((data.ftLastWriteTime.dwHighDateTime as u64) << 32) | data.ftLastWriteTime.dwLowDateTime as u64,
        created: ((data.ftCreationTime.dwHighDateTime as u64) << 32) | data.ftCreationTime.dwLowDateTime as u64,
        attributes: attrs,
        ext,
        type_name,
        target: None,
        free: 0,
    })
}

/// "This PC": the drives as entries.
fn this_pc() -> Folder {
    drives_folder(read_drives())
}

/// "This PC" made from a drive list that was already read (when drives come and go, app.rs
/// updates an open This PC with the new list instead of reading the drives a second time).
pub fn drives_folder(drives: Vec<Drive>) -> Folder {
    let entries = drives
        .into_iter()
        .map(|d| Entry {
            lower: d.name.to_lowercase(),
            name: d.name,
            kind: EntryKind::Drive,
            size: d.total,
            modified: 0,
            created: 0,
            attributes: ATTR_DIRECTORY,
            ext: String::new(),
            type_name: Arc::from(d.kind_name),
            target: Some(d.root),
            free: d.free,
        })
        .collect();
    Folder { path: String::new(), entries, ..Default::default() }
}

/// How long the drive list waits for a slow drive (a sleeping network share, a DVD drive spinning
/// up) before showing it without its label and sizes.
const DRIVE_WAIT: Duration = Duration::from_millis(1500);

/// All drives with their names and space. Asking a drive for its label and size can be slow (network
/// drives, sleeping discs), so every drive is asked at the same time on its own little thread, and a
/// drive that hasn't answered within DRIVE_WAIT is listed without them. Call this from a background
/// thread (app.rs does).
pub fn read_drives() -> Vec<Drive> {
    // SAFETY: no arguments; returns a bit mask.
    let mask = unsafe { GetLogicalDrives() };
    let mut drives: Vec<Drive> = (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let letter = (b'A' + i) as char;
            let root = format!("{letter}:\\");
            let root_w = wide(&root);
            // SAFETY: `root_w` is NUL-terminated and outlives the call. This one is always quick.
            let kind_name = match unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) } {
                2 => "Removable Disk",
                4 => "Network Drive",
                5 => "CD Drive",
                6 => "RAM Disk",
                _ => "Local Disk",
            };
            Drive { name: format!("{kind_name} ({letter}:)"), root, kind_name, total: 0, free: 0, ready: false }
        })
        .collect();

    let (tx, rx) = channel();
    for (i, d) in drives.iter().enumerate() {
        let (tx, root) = (tx.clone(), d.root.clone());
        // If no thread can be had, that drive just stays without its details.
        let _ = std::thread::Builder::new().name("drive info".into()).spawn(move || {
            let _ = tx.send((i, volume_info(&root)));
        });
    }
    drop(tx);
    let deadline = Instant::now() + DRIVE_WAIT;
    // Ends when every drive has answered (all senders gone) or the time is up.
    while let Ok((i, info)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let (Some(d), Some((label, total, free))) = (drives.get_mut(i), info) else { continue };
        if !label.is_empty() {
            d.name = format!("{label} ({})", &d.root[..2]);
        }
        (d.total, d.free, d.ready) = (total, free, true);
    }
    drives
}

/// A drive's label, total and free bytes - or None when it isn't ready (an empty DVD drive).
fn volume_info(root: &str) -> Option<(String, u64, u64)> {
    let root_w = wide(root);
    let mut label = [0u16; 261];
    // SAFETY: `root_w` outlives the call; `label` is a writable buffer of the given length.
    unsafe { GetVolumeInformationW(PCWSTR(root_w.as_ptr()), Some(&mut label), None, None, None, None) }.ok()?;
    let len = label.iter().position(|&c| c == 0).unwrap_or(0);
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: valid out-pointers for the two numbers we ask for.
    let _ = unsafe { GetDiskFreeSpaceExW(PCWSTR(root_w.as_ptr()), Some(&mut free), Some(&mut total), None) };
    Some((String::from_utf16_lossy(&label[..len]), total, free))
}

fn space_for(path: &str) -> Option<Space> {
    let w = wide_path(path);
    let mut free = 0u64;
    // SAFETY: `w` is NUL-terminated and outlives the call; valid out-pointer.
    unsafe { GetDiskFreeSpaceExW(PCWSTR(w.as_ptr()), Some(&mut free), None, None) }.ok()?;
    let drive = if path.as_bytes().get(1) == Some(&b':') { path[..2].to_uppercase() } else { root_of(path) };
    Some(Space { drive, free })
}

/// The root of a UNC path ("\\server\share"), or the path itself.
fn root_of(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("\\\\") {
        let parts: Vec<&str> = rest.splitn(3, '\\').collect();
        if parts.len() >= 2 {
            return format!("\\\\{}\\{}", parts[0], parts[1]);
        }
    }
    path.to_string()
}

fn is_dir(path: &str) -> bool {
    let w = wide_path(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & ATTR_DIRECTORY != 0
}

pub(crate) fn is_file(path: &str) -> bool {
    let w = wide_path(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & ATTR_DIRECTORY == 0
}

/// Turns a Windows error into a sentence a person understands.
fn explain(code: u32, system: &str) -> String {
    match code & 0xFFFF {
        5 => "You don't have permission to open this folder.".into(),
        2 | 3 => "This folder doesn't exist.".into(),
        21 => "The drive isn't ready. Is there a disc or card in it?".into(),
        53 | 67 => "The network location can't be reached.".into(),
        206 => "The path is too long.".into(),
        267 => NOT_A_FOLDER.into(),
        1392 => "The disk is damaged or unreadable.".into(),
        _ => {
            let s = system.trim();
            if s.is_empty() {
                format!("Failed to open this folder (error {}).", code & 0xFFFF)
            } else {
                format!("Failed to open this folder: {s}")
            }
        }
    }
}

// ---------------------------------------------------------------- sorting

/// Compares two lower-case names the way Explorer does: "file2" before "file10", and letters by
/// the user's language rather than by their code ("\u{e4} umlaut" among the a's, before "item").
/// Windows' sort key for each name (CompareStringEx's order, worked out once per name and kept),
/// or, where Windows can't give keys that order names like that, accented Latin letters counted
/// as their base letter. Names that still tie go by their text, so the order is always the same.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let windows = *WINDOWS_KEYS.get_or_init(windows_keys_work);
    let order = SORT_KEYS.with(|keys| {
        let mut keys = keys.borrow_mut();
        if keys.len() > SORT_KEYS_KEPT {
            keys.clear();
        }
        for name in [a, b] {
            keep_key(&mut keys, name, windows);
        }
        let (ka, kb) = (&keys[a], &keys[b]);
        key_cmp(ka, kb, windows)
    });
    order.then_with(|| natural_cmp_bytes(a.as_bytes(), b.as_bytes(), false))
}

/// Works out and keeps `name`'s sort key, if it isn't kept yet.
fn keep_key(keys: &mut HashMap<Box<str>, Box<[u8]>>, name: &str, windows: bool) {
    if !keys.contains_key(name) {
        let key = if windows { windows_sort_key(name).unwrap_or_default() } else { folded(name).into_bytes() };
        keys.insert(name.into(), key.into_boxed_slice());
    }
}

fn key_cmp(ka: &[u8], kb: &[u8], windows: bool) -> Ordering {
    if windows { ka.cmp(kb) } else { natural_cmp_bytes(ka, kb, false) }
}

/// The sort keys of the entries a sort goes through, looked up once each (see [`with_name_keys`])
/// instead of on every compare.
pub struct NameKeys<'a> {
    entries: &'a [Entry],
    /// By entry index; empty for entries not in the view.
    keys: Vec<&'a [u8]>,
    windows: bool,
}

impl NameKeys<'_> {
    /// [`natural_cmp`] of the names of entries `x` and `y`.
    pub fn cmp(&self, x: u32, y: u32) -> Ordering {
        let (a, b) = (&self.entries[x as usize].lower, &self.entries[y as usize].lower);
        if a == b {
            return Ordering::Equal;
        }
        key_cmp(self.keys[x as usize], self.keys[y as usize], self.windows).then_with(|| natural_cmp_bytes(a.as_bytes(), b.as_bytes(), false))
    }

    /// [`name_order`] of entries `x` and `y`.
    pub fn name_order(&self, x: u32, y: u32) -> Ordering {
        let (a, b) = (&self.entries[x as usize], &self.entries[y as usize]);
        self.cmp(x, y).then_with(|| a.name.cmp(&b.name)).then_with(|| natural_cmp_ignore_case(result_folder(a), result_folder(b)))
    }
}

/// Calls `f` with the sort keys of the entries in `view`. While `f` runs the kept keys are
/// borrowed: it mustn't call [`natural_cmp`] (NameKeys::cmp does the same).
pub fn with_name_keys<R>(entries: &[Entry], view: &[u32], f: impl FnOnce(&NameKeys) -> R) -> R {
    let windows = *WINDOWS_KEYS.get_or_init(windows_keys_work);
    SORT_KEYS.with(|cell| {
        {
            let mut keys = cell.borrow_mut();
            if keys.len() > SORT_KEYS_KEPT {
                keys.clear();
            }
            for &i in view {
                keep_key(&mut keys, &entries[i as usize].lower, windows);
            }
        }
        let kept = cell.borrow();
        let mut keys: Vec<&[u8]> = vec![&[]; entries.len()];
        for &i in view {
            keys[i as usize] = &kept[entries[i as usize].lower.as_str()];
        }
        f(&NameKeys { entries, keys, windows })
    })
}

/// Whether this Windows gives sort keys that order names as Explorer does (Wine may not): asked
/// once, so one way of ordering is used throughout.
static WINDOWS_KEYS: OnceLock<bool> = OnceLock::new();

thread_local! {
    /// Names' sort keys (Windows' or `folded`), worked out once per name: sorting 20,000 names
    /// compares them some 300,000 times. Emptied when it grows past SORT_KEYS_KEPT.
    static SORT_KEYS: std::cell::RefCell<HashMap<Box<str>, Box<[u8]>>> = std::cell::RefCell::new(HashMap::new());
}
const SORT_KEYS_KEPT: usize = 200_000;

fn windows_keys_work() -> bool {
    let ordered = |names: &[&str]| {
        let keys: Option<Vec<Vec<u8>>> = names.iter().map(|n| windows_sort_key(n)).collect();
        keys.is_some_and(|k| k.windows(2).all(|w| w[0] < w[1]))
    };
    ordered(&["a", "\u{e4}", "b"]) && ordered(&["\u{e4} umlaut", "item"]) && ordered(&["file2", "file10", "filea"])
}

/// The user's language's sort key for `name`: case ignored, numbers by their value (the order
/// CompareStringEx gives with LINGUISTIC_IGNORECASE | SORT_DIGITSASNUMBERS). None if Windows says no.
fn windows_sort_key(name: &str) -> Option<Vec<u8>> {
    let wide: Vec<u16> = name.encode_utf16().collect();
    if wide.is_empty() {
        return Some(Vec::new());
    }
    let flags = LCMAP_SORTKEY | LINGUISTIC_IGNORECASE.0 | SORT_DIGITSASNUMBERS.0;
    // A sort key's length is in bytes, the buffer's too (windows-rs passes the slice's length).
    let len = unsafe { LCMapStringEx(PCWSTR::null(), flags, &wide, None, None, None, LPARAM(0)) };
    if len <= 0 {
        return None;
    }
    let mut key = vec![0u8; len as usize + 1];
    // (Half of `key`'s length in u16s, said to Windows as bytes: within the buffer either way.)
    let buf = unsafe { std::slice::from_raw_parts_mut(key.as_mut_ptr().cast::<u16>(), len as usize) };
    let wrote = unsafe { LCMapStringEx(PCWSTR::null(), flags, &wide, Some(buf), None, None, LPARAM(0)) };
    if wrote <= 0 || wrote > len {
        return None;
    }
    key.truncate(wrote as usize);
    Some(key)
}

/// `name` (lower case) with accented Latin letters as their base letter ("\u{e4}" as "a", "\u{df}"
/// as "ss"), for comparing with natural_cmp_bytes where Windows' sort keys can't be had.
fn folded(name: &str) -> String {
    // U+00C0 to U+00FF, then U+0100 to U+017F, a letter each; '.' leaves the letter as it is.
    const LATIN1: &str = "aaaaaa.ceeeeiiiidnooooo.ouuuuy..aaaaaa.ceeeeiiiidnooooo.ouuuuy.y";
    const EXTENDED_A: &str =
        "aaaaaaccccccccddddeeeeeeeeeegggggggghhhhiiiiiiiiii..jjkkkllllllllllnnnnnnnnnoooooo..rrrrrrssssssssttttttuuuuuuuuuuuuwwyyyzzzzzzs";
    const _: () = assert!(LATIN1.len() == 64 && EXTENDED_A.len() == 128);
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let two = match c {
            '\u{c6}' | '\u{e6}' => "ae",
            '\u{152}' | '\u{153}' => "oe",
            '\u{132}' | '\u{133}' => "ij",
            '\u{df}' => "ss",
            _ => "",
        };
        let one = match c as u32 {
            n @ 0xC0..=0xFF => LATIN1.as_bytes()[n as usize - 0xC0],
            n @ 0x100..=0x17F => EXTENDED_A.as_bytes()[n as usize - 0x100],
            _ => b'.',
        };
        if !two.is_empty() {
            out.push_str(two);
        } else if one != b'.' {
            out.push(one as char);
        } else {
            out.push(c);
        }
    }
    out
}

/// The same for text that isn't lower case yet (A-Z count as a-z), without making copies: the
/// folders of search results are compared this way many thousand times while sorting.
fn natural_cmp_ignore_case(a: &str, b: &str) -> Ordering {
    natural_cmp_bytes(a.as_bytes(), b.as_bytes(), true)
}

/// `fold`: compare A-Z as a-z.
fn natural_cmp_bytes(a: &[u8], b: &[u8], fold: bool) -> Ordering {
    let byte = |s: &[u8], k: usize| if fold { s[k].to_ascii_lowercase() } else { s[k] };
    let (mut i, mut j) = (0, 0);
    let mut zeros_tiebreak = Ordering::Equal;
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            // Compare the whole run of digits as a number: skip leading zeros, then the longer run
            // is the bigger number, and equal lengths compare digit by digit.
            let (za, zb) = (i, j);
            while i < a.len() && a[i] == b'0' {
                i += 1;
            }
            while j < b.len() && b[j] == b'0' {
                j += 1;
            }
            let (sa, sb) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let ord = (i - sa).cmp(&(j - sb)).then_with(|| a[sa..i].cmp(&b[sb..j]));
            if ord != Ordering::Equal {
                return ord;
            }
            if zeros_tiebreak == Ordering::Equal {
                // "01" and "1" are the same number; the one with fewer zeros goes first.
                zeros_tiebreak = (sa - za).cmp(&(sb - zb));
            }
        } else {
            let (x, y) = (byte(a, i), byte(b, j));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then(zeros_tiebreak)
}

/// Sorts `view` (indexes into `entries`): folders and drives first (unless `folders_first` is
/// off: the "FoldersFirst" setting), then by `key`. Items that tie (all folders when sorting by
/// size, say) stay in A-to-Z order whichever way the column is sorted, like in Explorer; search
/// results with the same name then go by their folder.
pub fn sort_view(entries: &[Entry], view: &mut [u32], key: SortKey, descending: bool, folders_first: bool) {
    let order: Vec<u32> = view.to_vec();
    with_name_keys(entries, &order, |names| view.sort_unstable_by(|&x, &y| {
        let (a, b) = (&entries[x as usize], &entries[y as usize]);
        if folders_first {
            let group = b.is_container().cmp(&a.is_container());
            if group != Ordering::Equal {
                return group;
            }
        }
        let by_name = || names.name_order(x, y);
        let flip = |o: Ordering| if descending { o.reverse() } else { o };
        match key {
            SortKey::Name => flip(by_name()),
            SortKey::Date => flip(a.modified.cmp(&b.modified)).then_with(by_name),
            SortKey::Created => flip(a.created.cmp(&b.created)).then_with(by_name),
            // (One Arc, as entries of one type share: the same text.)
            SortKey::Type if Arc::ptr_eq(&a.type_name, &b.type_name) => by_name(),
            SortKey::Type => flip(cmp_ignore_case(&a.type_name, &b.type_name)).then_with(by_name),
            SortKey::Size => flip(a.size.cmp(&b.size)).then_with(by_name),
            SortKey::Folder => flip(natural_cmp_ignore_case(result_folder(a), result_folder(b))).then_with(by_name),
            // (Their values aren't in the entries: `sort_view_by` sorts by them, with what media.rs
            // has read. Without them, A to Z.)
            SortKey::Dimensions | SortKey::Length => by_name(),
        }
    }));
}

/// Sorts `view` like [`sort_view`], by a value only some entries have (Image dimensions and Duration,
/// which media.rs reads in the background): `values` holds each entry's, by its index into
/// `entries`. Items without one go last whichever way the column is sorted; ties stay A to Z.
pub fn sort_view_by<K: Ord>(entries: &[Entry], view: &mut [u32], descending: bool, folders_first: bool, values: &[Option<K>]) {
    let order: Vec<u32> = view.to_vec();
    with_name_keys(entries, &order, |names| view.sort_unstable_by(|&x, &y| {
        let (a, b) = (&entries[x as usize], &entries[y as usize]);
        if folders_first {
            let group = b.is_container().cmp(&a.is_container());
            if group != Ordering::Equal {
                return group;
            }
        }
        match (values.get(x as usize).and_then(Option::as_ref), values.get(y as usize).and_then(Option::as_ref)) {
            (Some(p), Some(q)) => (if descending { q.cmp(p) } else { p.cmp(q) }).then_with(|| names.name_order(x, y)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => names.name_order(x, y),
        }
    }));
}

/// A to Z the way people count, then capitals, then (search results with the same name) by folder.
/// (What `NameKeys::name_order` does; the tests check one against the other.)
#[cfg(test)]
fn name_order(a: &Entry, b: &Entry) -> Ordering {
    natural_cmp(&a.lower, &b.lower).then_with(|| a.name.cmp(&b.name)).then_with(|| natural_cmp_ignore_case(result_folder(a), result_folder(b)))
}

/// The folder a search result is in (its full path without the name); "" for everything else.
pub fn result_folder(e: &Entry) -> &str {
    match &e.target {
        Some(t) if e.kind != EntryKind::Drive => t.rfind('\\').map_or("", |i| &t[..i]),
        _ => "",
    }
}

fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

// ---------------------------------------------------------------- formatting

/// One of the user's number settings (the thousands or the decimal separator), asked once.
fn locale_text(cell: &'static OnceLock<String>, what: u32, fallback: &str) -> &'static str {
    cell.get_or_init(|| {
        let mut buf = [0u16; 8];
        // SAFETY: `buf` is writable and its length passed; a null locale name means the user's own.
        let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), what, Some(&mut buf)) };
        if len > 1 { String::from_utf16_lossy(&buf[..len as usize - 1]) } else { fallback.to_string() }
    })
}

/// The digits of a whole number with `sep` between the thousands.
fn group_digits(digits: &str, sep: &str) -> String {
    let mut out = String::with_capacity(digits.len() + 4);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push_str(sep);
        }
        out.push(c);
    }
    out
}

/// A count with the user's thousands separator: "10,000" (or "10.000", "10 000"...).
pub fn format_count(n: usize) -> String {
    static SEPARATOR: OnceLock<String> = OnceLock::new();
    group_digits(&n.to_string(), locale_text(&SEPARATOR, LOCALE_STHOUSAND, ","))
}

/// A number with `decimals` places, written the user's way ("1,023.9", or "1.023,9" in Germany),
/// like "1,023.9" with one decimal.
fn format_number(v: f64, decimals: usize) -> String {
    static THOUSANDS: OnceLock<String> = OnceLock::new();
    static DECIMAL: OnceLock<String> = OnceLock::new();
    number_with(v, decimals, locale_text(&THOUSANDS, LOCALE_STHOUSAND, ","), locale_text(&DECIMAL, LOCALE_SDECIMAL, "."))
}

fn number_with(v: f64, decimals: usize, thousands: &str, decimal: &str) -> String {
    let text = format!("{:.*}", decimals, v.max(0.0));
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let mut out = group_digits(whole, thousands);
    if !fraction.is_empty() {
        out.push_str(decimal);
        out.push_str(fraction);
    }
    out
}

/// A size in the automatic style ("auto"): "512 B", "13 KB" (rounded up, like Explorer), "3.1 MB",
/// "41.7 GB", "1.82 TB". A size that would round to 1,024 of a unit is shown in the next one
/// ("1.0 GB", not "1,024.0 MB").
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    // Rounded the way they're shown, to pick the unit.
    let kb = (b / KB).ceil();
    let mb = (b / (KB * KB) * 10.0).round() / 10.0;
    let gb = (b / (KB * KB * KB) * 10.0).round() / 10.0;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if kb < KB {
        format!("{} KB", format_number(kb, 0))
    } else if mb < KB {
        format!("{} MB", format_number(b / (KB * KB), 1))
    } else if gb < KB {
        format!("{} GB", format_number(b / (KB * KB * KB), 1))
    } else {
        format!("{} TB", format_number(b / (KB * KB * KB * KB), 2))
    }
}

/// A file's size in the chosen style ("SizeFormat"): automatic, or always in KB like File
/// Explorer ("3,172 KB"; anything under 1 KB but empty is "1 KB").
pub fn format_size_as(bytes: u64, style: SizeFormat) -> String {
    match style {
        SizeFormat::Auto => format_size(bytes),
        SizeFormat::Kb => format!("{} KB", format_number((bytes as f64 / 1024.0).ceil(), 0)),
    }
}

/// A FILETIME as the user's short date and time ("08/10/2026 14:32"), in local time.
pub fn format_date(filetime: u64) -> String {
    format_date_as(filetime, DateFormat::Default)
}

/// A FILETIME in the chosen style ("DateFormat"), in local time:
/// - default: Windows' short date and time, "08/10/2026 14:32";
/// - relative: "Just now", "5 min ago", "Today, 3:02 PM", "Yesterday, 3:02 PM", "Monday, 3:02 PM"
///   within the last week, then the short date;
/// - iso: "2026-10-08 14:32".
pub fn format_date_as(filetime: u64, style: DateFormat) -> String {
    if filetime == 0 {
        return String::new();
    }
    let Some(local) = local_time(filetime) else { return String::new() };
    match style {
        DateFormat::Iso => format!("{:04}-{:02}-{:02} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute),
        DateFormat::Default => format!("{} {}", windows_date(&local, None), windows_time(&local)),
        DateFormat::Relative => {
            // SAFETY: no arguments; returns the time now.
            let now = unsafe { GetSystemTimeAsFileTime() };
            let now = ((now.dwHighDateTime as u64) << 32) | now.dwLowDateTime as u64;
            let Some(today) = local_time(now) else { return windows_date(&local, None) };
            match relative(filetime, now, day_number(&local), day_number(&today)) {
                Relative::JustNow => "Just now".into(),
                Relative::MinutesAgo(n) => format!("{n} min ago"),
                Relative::Today => format!("Today, {}", windows_time(&local)),
                Relative::Yesterday => format!("Yesterday, {}", windows_time(&local)),
                Relative::Weekday => format!("{}, {}", windows_date(&local, Some("dddd")), windows_time(&local)),
                Relative::Date => windows_date(&local, None),
            }
        }
    }
}

/// What a relative date says, worked out from the times (FILETIMEs) and the local days.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Relative {
    JustNow,
    MinutesAgo(u64),
    Today,
    Yesterday,
    /// Its weekday ("Monday"): within the last week.
    Weekday,
    /// Just the date.
    Date,
}

/// Relative dates: under a minute ago "Just now", under an hour "N min
/// ago", then today / yesterday / the weekday for the last week, then the date. A time in the
/// future goes by its day.
fn relative(time: u64, now: u64, day: i64, today: i64) -> Relative {
    const MINUTE: u64 = 60 * 10_000_000;
    if now >= time {
        let ago = now - time;
        if ago < MINUTE {
            return Relative::JustNow;
        }
        if ago < 60 * MINUTE {
            return Relative::MinutesAgo(ago / MINUTE);
        }
    }
    if day == today {
        Relative::Today
    } else if day == today - 1 {
        Relative::Yesterday
    } else if day > today - 7 && day < today {
        Relative::Weekday
    } else {
        Relative::Date
    }
}

/// A FILETIME (UTC) as the local date and time.
fn local_time(filetime: u64) -> Option<SYSTEMTIME> {
    let ft = FILETIME { dwLowDateTime: filetime as u32, dwHighDateTime: (filetime >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: valid in/out pointers to plain structs.
    let ok = unsafe { FileTimeToSystemTime(&ft, &mut utc).is_ok() && SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).is_ok() };
    ok.then_some(local)
}

/// Days since 1 March of year 0 for a date: consecutive days get consecutive numbers.
fn day_number(t: &SYSTEMTIME) -> i64 {
    let (y, m, d) = (t.wYear as i64, t.wMonth as i64, t.wDay as i64);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe
}

/// The date as Windows writes it for the user: the short date, or `format` ("dddd": the weekday).
fn windows_date(t: &SYSTEMTIME, format: Option<&str>) -> String {
    let mut buf = [0u16; 80];
    let format_w = format.map(wide);
    let (flags, pattern) = match &format_w {
        Some(f) => (ENUM_DATE_FORMATS_FLAGS(0), PCWSTR(f.as_ptr())),
        None => (DATE_SHORTDATE, PCWSTR::null()),
    };
    // SAFETY: `t` is a valid SYSTEMTIME; the buffer is writable and its length passed; `format_w`
    // outlives the call.
    let n = unsafe { GetDateFormatEx(PCWSTR::null(), flags, Some(t), pattern, Some(&mut buf), PCWSTR::null()) };
    String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1).min(buf.len())])
}

/// The time as Windows writes it for the user, without seconds ("3:02 PM", "15:02").
fn windows_time(t: &SYSTEMTIME) -> String {
    let mut buf = [0u16; 64];
    // SAFETY: `t` is a valid SYSTEMTIME; the buffer is writable and its length passed.
    let n = unsafe { GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, Some(t), PCWSTR::null(), Some(&mut buf)) };
    String::from_utf16_lossy(&buf[..(n.max(1) as usize - 1).min(buf.len())])
}

// ---------------------------------------------------------------- the filter

/// What the filter box keeps: with `*` or `?` in it, a wildcard
/// pattern for the whole name ("*.pdf", "report-??.txt"); otherwise every word must be somewhere
/// in the name, in any order ("tax 2024" finds "2024 Tax return.pdf"). Capitals don't matter.
/// `tag:red` keeps only the items with that colour tag, `tag:` alone every tagged item (tags.rs),
/// and the rest goes for the name as before: "tag:red invoice".
/// Words in quotes are looked for inside files (search.rs): `"hello world"` in text files,
/// `"hello world" *.md *.txt` only in those kinds of files. With quotes, each `*` or `?` word is a
/// pattern of its own (any of them will do) and the other words are for the name.
/// A few more words, in any capitals, go with all of that:
/// - `size:>10mb`, `size:<500kb`, `size:1gb..5gb` (b, kb, mb, gb; none is bytes; `size:0` is
///   exactly that): files only, as folders have no size here;
/// - `date:today`, `date:yesterday`, `date:2026`, `date:2026-01`, `date:2026-01-31`, and `>` or `<`
///   before a date for after or before it: by Date modified, in local time;
/// - a word or pattern with a minus in front leaves those names out (`-draft`, `-*.tmp`; a minus
///   alone is an ordinary word);
/// - `regex:` and a regular expression for the name (`regex:^img_\d{4}\.jpe?g$`).
///
/// A `size:`, `date:` or `regex:` word that doesn't make sense (`date:2026-13-40`, a pattern still
/// being typed) keeps nothing, as a word no name has; one with nothing after the colon doesn't
/// count yet. These can't clash with a real name, as Windows allows no `:` in names; a name that
/// starts with a minus is found with `*-draft*` or `regex:^-draft`.
#[derive(Clone, Debug)]
pub struct NameFilter {
    name: NameRule,
    tag: Option<crate::tags::TagFilter>,
    /// The words in quotes, in lower case: each must be inside the file.
    inside: Vec<String>,
    /// The words and patterns with a minus in front, in lower case: names with any of them go.
    without: Vec<NameRule>,
    /// The `regex:` patterns: the name must match each.
    regex: Vec<regex_lite::Regex>,
    /// `size:`: files of at least the first and at most the second number of bytes.
    size: Option<(u64, u64)>,
    /// `date:`: changed at or after the first FILETIME and before the second.
    date: Option<(u64, u64)>,
    /// A `size:`, `date:` or `regex:` word that doesn't make sense: nothing is kept.
    broken: bool,
}

/// The filter's part about the name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NameRule {
    /// Nothing typed: everything shows.
    All,
    Words(Vec<String>),
    /// A wildcard pattern, in lower case.
    Pattern(Vec<char>),
    /// With words in quotes: names matching any of the patterns (all names when there are none),
    /// with every word in them.
    Kinds { patterns: Vec<Vec<char>>, words: Vec<String> },
}

/// The words in quotes (lower case, not empty) and the rest of the text. A quote left open runs to
/// the end, so `"hello wor` already looks for "hello wor" while it's being typed.
pub fn split_quotes(text: &str) -> (Vec<String>, String) {
    let mut inside = Vec::new();
    let mut rest = String::new();
    let mut parts = text.split('"');
    if let Some(first) = parts.next() {
        rest.push_str(first);
    }
    for (n, part) in parts.enumerate() {
        if n % 2 == 0 {
            let p = part.trim();
            if !p.is_empty() {
                inside.push(p.to_string());
            }
        } else {
            rest.push(' ');
            rest.push_str(part);
        }
    }
    (inside, rest)
}

impl NameFilter {
    pub fn new(text: &str) -> NameFilter {
        NameFilter::with_tags(text, crate::tags::shown)
    }

    /// As `new`, with the tags `tags` gives (asked only for a `tag:` word).
    fn with_tags(text: &str, tags: impl FnOnce() -> Arc<crate::tags::Map>) -> NameFilter {
        // `regex:` words first, as typed: lower case would turn `\D` (not a digit) into `\d`.
        let (patterns, text) = take_regex(text.trim());
        let text = text.trim().to_lowercase();
        let (inside, text) = if text.contains('"') { split_quotes(&text) } else { (Vec::new(), text) };
        let (tag, text) = match crate::tags::TagFilter::take(&text, tags) {
            Some((tag, rest)) => (Some(tag), rest),
            None => (None, text),
        };
        let mut broken = false;
        let mut regex = Vec::new();
        for p in patterns {
            match pattern_regex(&p) {
                Some(r) => regex.push(r),
                None => broken = true,
            }
        }
        // The other new words; without any, the text goes on exactly as it was.
        let (mut size, mut date, mut without) = (None, None, Vec::new());
        let mut rest: Vec<&str> = Vec::new();
        let mut taken = false;
        for w in text.split_whitespace() {
            if let Some(v) = w.strip_prefix("size:") {
                taken = true;
                if !v.is_empty() {
                    match size_range(v) {
                        Some(r) => size = Some(narrow(size, r)),
                        None => broken = true,
                    }
                }
            } else if let Some(v) = w.strip_prefix("date:") {
                taken = true;
                if !v.is_empty() {
                    match date_range(v, today) {
                        Some(r) => date = Some(narrow(date, r)),
                        None => broken = true,
                    }
                }
            } else if let Some(v) = w.strip_prefix('-').filter(|v| !v.is_empty()) {
                taken = true;
                without.push(if v.contains(['*', '?']) { NameRule::Pattern(v.chars().collect()) } else { NameRule::Words(vec![v.to_string()]) });
            } else {
                rest.push(w);
            }
        }
        let text = if taken { rest.join(" ") } else { text.trim().to_string() };
        let name = if !inside.is_empty() {
            let (patterns, words): (Vec<&str>, Vec<&str>) = text.split_whitespace().partition(|w| w.contains(['*', '?']));
            NameRule::Kinds { patterns: patterns.iter().map(|p| p.chars().collect()).collect(), words: words.iter().map(|w| w.to_string()).collect() }
        } else if text.is_empty() {
            NameRule::All
        } else if text.contains(['*', '?']) {
            NameRule::Pattern(text.chars().collect())
        } else {
            NameRule::Words(text.split_whitespace().map(str::to_string).collect())
        };
        NameFilter { name, tag, inside, without, regex, size, date, broken }
    }

    /// The words in quotes, to look for inside files (none: only names count).
    pub fn inside(&self) -> &[String] {
        &self.inside
    }

    /// Whether the kinds of files to look inside were named (`*.pdf`): then they're read whatever
    /// is in them.
    pub fn reads_any(&self) -> bool {
        matches!(&self.name, NameRule::Kinds { patterns, .. } if !patterns.is_empty())
    }

    /// Whether a file (by its name in lower case) is one to look inside: one of the kinds asked for
    /// (`*.md`), or else a text file by its name.
    pub fn reads(&self, lower: &str) -> bool {
        if self.inside.is_empty() {
            return false;
        }
        match &self.name {
            NameRule::Kinds { patterns, .. } if !patterns.is_empty() => true, // (the name already matched one)
            _ => {
                let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
                crate::peek::is_text_file(ext) || ext == "glint"
            }
        }
    }

    /// Whether an item is kept: by its name in lower case and, for a `tag:` filter, by its tag
    /// (`path` gives its full path, asked only then).
    /// `size:` and `date:` words need the item itself: see `keeps_item`.
    pub fn keeps(&self, lower: &str, path: impl FnOnce() -> String) -> bool {
        !self.broken
            && self.name.matches(lower)
            && !self.without.iter().any(|w| w.matches(lower))
            && self.regex.iter().all(|r| r.is_match(lower))
            && self.tag.as_ref().is_none_or(|t| t.keeps(path))
    }

    /// As `keeps`, and by the item's size and date for `size:` and `date:` words.
    pub fn keeps_item(&self, e: &Entry, path: impl FnOnce() -> String) -> bool {
        self.keeps(&e.lower, path) && self.fits(e)
    }

    /// Whether an item's size and date fit the `size:` and `date:` words (all do without them).
    pub fn fits(&self, e: &Entry) -> bool {
        self.size.is_none_or(|(least, most)| !e.is_container() && least <= e.size && e.size <= most)
            && self.date.is_none_or(|(from, to)| e.modified != 0 && from <= e.modified && e.modified < to)
    }
}

impl NameRule {
    /// Whether the words or the pattern keep a name in lower case.
    fn matches(&self, lower: &str) -> bool {
        match self {
            NameRule::All => true,
            NameRule::Words(words) => words.iter().all(|w| lower.contains(w.as_str())),
            NameRule::Pattern(p) => {
                let name: Vec<char> = lower.chars().collect();
                wildcard(p, &name)
            }
            NameRule::Kinds { patterns, words } => {
                let name: Vec<char> = lower.chars().collect();
                (patterns.is_empty() || patterns.iter().any(|p| wildcard(p, &name))) && words.iter().all(|w| lower.contains(w.as_str()))
            }
        }
    }
}

/// Takes the `regex:` words (in any capitals, not in quotes) out of `text`: their patterns, and the
/// rest. Without any, the text comes back as it was.
fn take_regex(text: &str) -> (Vec<String>, String) {
    let mut patterns = Vec::new();
    let mut rest = String::new();
    let mut from = 0;
    for w in text.split_whitespace() {
        let at = w.as_ptr() as usize - text.as_ptr() as usize;
        let outside_quotes = (rest.matches('"').count() + text[from..at].matches('"').count()) % 2 == 0;
        if outside_quotes && w.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("regex:")) {
            patterns.push(w[6..].to_string());
            rest.push_str(&text[from..at]);
            from = at + w.len();
        }
    }
    if patterns.is_empty() {
        return (patterns, text.to_string());
    }
    rest.push_str(&text[from..]);
    patterns.retain(|p| !p.is_empty());
    (patterns, rest)
}

/// A `regex:` pattern made ready: None when it doesn't make sense. Names are matched in lower case,
/// so the pattern is too, but for the letter after a `\` (`\D` isn't `\d`). The limits keep a
/// typed pattern from taking much memory; regex-lite never takes long to match, whatever the pattern.
fn pattern_regex(pattern: &str) -> Option<regex_lite::Regex> {
    let mut lower = String::with_capacity(pattern.len());
    let mut escaped = false;
    for c in pattern.chars() {
        if escaped {
            lower.push(c);
        } else {
            lower.extend(c.to_lowercase());
        }
        escaped = !escaped && c == '\\';
    }
    regex_lite::RegexBuilder::new(&lower).case_insensitive(true).size_limit(1 << 20).nest_limit(64).build().ok()
}

/// Both ranges at once (`size:>1mb size:<5mb`); the first is None for the first word.
fn narrow(old: Option<(u64, u64)>, new: (u64, u64)) -> (u64, u64) {
    old.map_or(new, |(a, b)| (a.max(new.0), b.min(new.1)))
}

/// A number of bytes as typed after `size:`: `10mb`, `1.5gb`, `500` (bytes). None when it isn't one.
fn parse_bytes(text: &str) -> Option<u64> {
    let digits = text.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let times: u64 = match unit {
        "" | "b" => 1,
        "kb" => 1 << 10,
        "mb" => 1 << 20,
        "gb" => 1 << 30,
        _ => return None,
    };
    if number.is_empty() || number.starts_with('.') || number.ends_with('.') || number.matches('.').count() > 1 {
        return None;
    }
    if let Ok(n) = number.parse::<u64>() {
        return n.checked_mul(times);
    }
    let bytes = number.parse::<f64>().ok()? * times as f64;
    (bytes.is_finite() && bytes < u64::MAX as f64).then(|| bytes.round() as u64)
}

/// What `size:` keeps, as the least and most bytes: `>10mb`, `<500kb`, `1gb..5gb` (either way
/// round; a side left out has no limit), or exactly `0`.
fn size_range(text: &str) -> Option<(u64, u64)> {
    if let Some(v) = text.strip_prefix('>') {
        return Some((parse_bytes(v)?.checked_add(1)?, u64::MAX));
    }
    if let Some(v) = text.strip_prefix('<') {
        // Below 0 bytes: nothing (an empty range).
        return Some(parse_bytes(v)?.checked_sub(1).map_or((1, 0), |most| (0, most)));
    }
    if let Some((a, b)) = text.split_once("..") {
        let a = if a.is_empty() { 0 } else { parse_bytes(a)? };
        let b = if b.is_empty() { u64::MAX } else { parse_bytes(b)? };
        return Some((a.min(b), a.max(b)));
    }
    let n = parse_bytes(text)?;
    Some((n, n))
}

/// The days of a date as typed after `date:` (`2026`, `2026-01`, `2026-01-31`, `today`,
/// `yesterday`), as day numbers (`day_number`): the first, and the one after the last. `today`
/// gives today's day number, asked only for those two words.
fn date_days(text: &str, today: impl FnOnce() -> Option<i64>) -> Option<(i64, i64)> {
    match text {
        "today" => return today().map(|d| (d, d + 1)),
        "yesterday" => return today().map(|d| (d - 1, d)),
        _ => {}
    }
    let mut parts = text.split('-');
    let number = |part: Option<&str>, most: usize| part.filter(|p| !p.is_empty() && p.len() <= most && p.bytes().all(|b| b.is_ascii_digit())).and_then(|p| p.parse::<u16>().ok());
    let year = number(parts.next(), 4).filter(|y| (1601..=9999).contains(y))?;
    let day = |month: u16, d: u16| day_number(&SYSTEMTIME { wYear: year, wMonth: month, wDay: d, ..Default::default() });
    let Some(month) = parts.next() else { return Some((day(1, 1), day(1, 1) + if leap(year) { 366 } else { 365 })) };
    let month = number(Some(month), 2).filter(|m| (1..=12).contains(m))?;
    let days = [31, if leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][month as usize - 1];
    let Some(d) = parts.next() else { return Some((day(month, 1), day(month, 1) + days)) };
    let d = number(Some(d), 2).filter(|d| (1..=days).contains(&i64::from(*d)))?;
    parts.next().is_none().then(|| (day(month, d), day(month, d) + 1))
}

fn leap(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// What `date:` keeps, as FILETIMEs: from the first, up to (not with) the second. `>` before a
/// date is after it, `<` before it. `today` gives today's day number (`day_number`).
fn date_range(text: &str, today: impl FnOnce() -> Option<i64>) -> Option<(u64, u64)> {
    if let Some(v) = text.strip_prefix('>') {
        return Some((local_midnight(date_days(v, today)?.1)?, u64::MAX));
    }
    if let Some(v) = text.strip_prefix('<') {
        return Some((0, local_midnight(date_days(v, today)?.0)?));
    }
    let (first, after) = date_days(text, today)?;
    Some((local_midnight(first)?, local_midnight(after)?))
}

/// The start of a day (a `day_number`) in local time, as a FILETIME.
fn local_midnight(day: i64) -> Option<u64> {
    // The other way round from `day_number`.
    let era = day.div_euclid(146_097);
    let doe = day - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let local = SYSTEMTIME { wYear: u16::try_from(y).ok()?, wMonth: m as u16, wDay: d as u16, ..Default::default() };
    let mut utc = SYSTEMTIME::default();
    let mut ft = FILETIME::default();
    // SAFETY: valid in/out pointers to plain structs.
    unsafe { TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).ok()? };
    unsafe { SystemTimeToFileTime(&utc, &mut ft).ok()? };
    Some(((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64)
}

/// Today's `day_number`, in local time.
fn today() -> Option<i64> {
    // SAFETY: no arguments; returns the time now.
    let now = unsafe { GetSystemTimeAsFileTime() };
    local_time(((now.dwHighDateTime as u64) << 32) | now.dwLowDateTime as u64).map(|t| day_number(&t))
}

/// Whether `name` matches the whole pattern: `*` is any run of characters (also none), `?` is one.
/// Goes back at most to the last `*`, so it never takes long, whatever the pattern.
fn wildcard(pattern: &[char], name: &[char]) -> bool {
    let (mut p, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, n));
            p += 1;
        } else if let Some((sp, sn)) = star {
            // The last `*` takes one more character.
            p = sp + 1;
            n = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

// ---------------------------------------------------------------- paths

/// `dir` + `\` + `name`, without doubling the backslash after a drive root.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

/// The folder above: "C:\Users\YourName" -> "C:\Users" -> "C:\" -> "" (This PC) -> None.
pub fn parent(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let trimmed = path.trim_end_matches('\\');
    if trimmed.len() <= 2 && trimmed.ends_with(':') {
        return Some(String::new()); // a drive root's parent is This PC
    }
    if path.starts_with("\\\\") && root_of(path) == trimmed {
        return Some(String::new()); // \\server\share
    }
    match trimmed.rfind('\\') {
        Some(i) if i <= 2 && trimmed.as_bytes().get(1) == Some(&b':') => Some(trimmed[..=i].to_string()),
        Some(i) if i > 0 => Some(trimmed[..i].to_string()),
        _ => Some(String::new()),
    }
}

/// The last part of a path ("Downloads"); for a drive root the root itself.
pub fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('\\');
    match trimmed.rfind('\\') {
        Some(i) if i + 1 < trimmed.len() => &trimmed[i + 1..],
        _ => path,
    }
}

/// The lower-case extension of a file name without the dot ("" if none).
pub fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Whether a path is complete on its own: "C:\..." or a network path "\\server\share\...".
fn is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\") || (b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\'))
}

/// `path` as the disk spells it ("c:\USERS\ME\FILES" typed: "C:\Users\Me\files"), so the path
/// bar and the tab show the real names. Only on this PC's own drives and USB sticks (a network
/// drive that's away would hold things up); the path as it is when a part of it isn't found.
pub fn real_case(path: &str) -> String {
    let b = path.as_bytes();
    if !(b.len() >= 3 && b[1] == b':' && b[2] == b'\\' && b[0].is_ascii_alphabetic()) {
        return path.to_string();
    }
    let root = format!("{}:\\", (b[0] as char).to_ascii_uppercase());
    let w = wide(&root);
    // SAFETY: a NUL-terminated root path that outlives the call.
    let kind = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
    if !matches!(kind, 2 | 3) {
        // (DRIVE_REMOVABLE, DRIVE_FIXED only.)
        return path.to_string();
    }
    let mut out = root.trim_end_matches('\\').to_string();
    for part in path[3..].split('\\').filter(|p| !p.is_empty()) {
        let asked = format!("{out}\\{part}");
        // "." and "..", and wildcards, are left as they are.
        let name = if part == "." || part == ".." || part.contains(['*', '?']) { None } else { found_name(&asked) };
        match name {
            Some(n) if n.eq_ignore_ascii_case(part) || n.to_lowercase() == part.to_lowercase() => out = format!("{out}\\{n}"),
            _ => return path.to_string(),
        }
    }
    if out.len() == 2 {
        out.push('\\');
    }
    out
}

/// The name of the one item at `path`, as the disk spells it.
fn found_name(path: &str) -> Option<String> {
    let w = wide_path(path);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `w` is NUL-terminated and outlives the call; `data` is a valid WIN32_FIND_DATAW.
    let handle = unsafe {
        FindFirstFileExW(PCWSTR(w.as_ptr()), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, None, Default::default())
    }
    .ok()?;
    // SAFETY: the handle FindFirstFileExW just gave.
    let _ = unsafe { FindClose(handle) };
    let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
    Some(String::from_utf16_lossy(&data.cFileName[..len]))
}

/// What a person typed, as a full path, with `base` (the folder they're in) as the starting point
/// for a name or relative path: "src" and "..\Music" go where Explorer's address bar would take
/// them, not somewhere under the process's current folder (which has nothing to do with what is
/// on screen). "\Windows" is the root of `base`'s drive. Otherwise like [`normalize`].
pub fn resolve(input: &str, base: &str) -> String {
    let path = normalize(input);
    // Already complete, This PC, the Recycle Bin, or "D:something" (rare; left to Windows).
    if path.is_empty() || base.is_empty() || is_absolute(&path) || is_place(&path) || path.as_bytes().get(1) == Some(&b':') {
        return path;
    }
    if path.starts_with('\\') {
        return match base.as_bytes().get(1) {
            Some(b':') => normalize(&format!("{}{path}", &base[..2])),
            _ => path,
        };
    }
    normalize(&join(base, &path))
}

/// Cleans up what a person typed into a folder path: quotes, `/`, `~` (the home folder, as in
/// "~\Downloads"), `%VARIABLES%`, `..`, "c:", "This PC", "Recycle Bin", "Home". Returns "" for This
/// PC.
pub fn normalize(input: &str) -> String {
    let mut s = input.trim().trim_matches('"').trim().replace('/', "\\");
    if s.is_empty() || s.eq_ignore_ascii_case("this pc") || s.eq_ignore_ascii_case("computer") {
        return String::new();
    }
    if recycle::means_bin(&s) {
        return recycle::LOCATION.to_string();
    }
    if let Some(drive) = crate::diskspace::drive_of_location(&s) {
        return crate::diskspace::location(&drive);
    }
    if crate::recent::means_home(&s) {
        return crate::recent::LOCATION.to_string();
    }
    if s == "~" || s.starts_with("~\\") {
        s = format!("{}{}", crate::shell::home(), &s[1..]);
    }
    // (Not in the full path of something that's there, though: a folder really called %APPDATA% -
    // left by a program that didn't fill the name in - opens as it is.)
    if s.contains('%') && !(is_absolute(&s) && (is_dir(&s) || is_file(&s))) {
        let w = wide(&s);
        let mut buf = vec![0u16; 4096];
        // SAFETY: `w` is NUL-terminated; `buf` is writable and its length passed.
        let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
        if n > 0 && n <= buf.len() {
            s = String::from_utf16_lossy(&buf[..n - 1]);
        }
    }
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    // Resolve "." and ".." for absolute paths (never relative ones: those would depend on the
    // process's current directory).
    if is_absolute(&s) {
        let w = wide(&s);
        let mut buf = vec![0u16; 32768];
        // SAFETY: `w` is NUL-terminated; `buf` is writable and its length passed.
        let n = unsafe { GetFullPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf), None) } as usize;
        if n > 0 && n < buf.len() {
            s = String::from_utf16_lossy(&buf[..n]);
        }
    }
    // Drive letters in capitals, no trailing backslash except on a root.
    if s.as_bytes().get(1) == Some(&b':') {
        s = s[..1].to_uppercase() + &s[1..];
    }
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// A string as a NUL-terminated UTF-16 buffer for Windows calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A path as a NUL-terminated UTF-16 buffer for the file-system calls. A long path gets the "\\?\"
/// prefix, which lifts Windows' old 260-character limit for that call. (The manifest asks for long
/// paths too, but that only works once they're also switched on in the registry, which by default
/// they aren't.) Short paths go as they are.
pub fn wide_path(path: &str) -> Vec<u16> {
    // Comfortably under the limit even for "folder\*" and a file name on top.
    const LONG: usize = 240;
    if (path.len() < LONG && !odd_name_in(path)) || path.starts_with("\\\\?\\") || path.starts_with("\\\\.\\") || !is_absolute(path) {
        return wide(path);
    }
    match path.strip_prefix("\\\\") {
        Some(unc) => wide(&format!("\\\\?\\UNC\\{unc}")),
        None => wide(&format!("\\\\?\\{path}")),
    }
}

/// Whether a part of the full path `path` ends in a dot or a space ("notes." or "draft "): names
/// other systems (and archives) make, which Windows quietly trims (and then can't find) unless
/// the path goes with the "\\?\" prefix. A path with "/" or "." or ".." parts never counts: those
/// the prefix would take literally.
pub fn odd_name_in(path: &str) -> bool {
    let parts: Vec<&str> = path.split('\\').skip(1).filter(|p| !p.is_empty()).collect();
    !path.contains('/') && !parts.iter().any(|p| *p == "." || *p == "..") && parts.iter().any(|p| p.ends_with('.') || p.ends_with(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_ending_in_a_dot_or_space_go_with_the_prefix() {
        assert!(odd_name_in("X:\\files\\dot.end."));
        assert!(odd_name_in("X:\\folder \\a.txt"));
        assert!(!odd_name_in("X:\\files\\a.txt"));
        assert!(!odd_name_in("X:\\files\\..\\a."), "never with .. parts");
        assert!(!odd_name_in("X:\\"));
        assert_eq!(String::from_utf16_lossy(&wide_path("X:\\files\\dot.end.")), "\\\\?\\X:\\files\\dot.end.\0");
        assert_eq!(String::from_utf16_lossy(&wide_path("X:\\files\\a.txt")), "X:\\files\\a.txt\0");
    }

    /// A file for the sort tests: `name`, of type `kind`, `size` bytes.
    fn sort_entry(name: &str, kind: Arc<str>, size: u64) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: if kind.is_empty() { EntryKind::Folder } else { EntryKind::File },
            size,
            modified: size * 7 % 5,
            created: 0,
            attributes: 0,
            ext: extension(name),
            type_name: kind,
            target: None,
            free: 0,
        }
    }

    /// The sorts look each name's key up once; the order is what comparing with natural_cmp on
    /// every step gives, for every column, either way, folders first or not.
    #[test]
    fn sorting_with_keys_looked_up_once_orders_as_before() {
        let names = ["file10", "file2", "File2", "FILE2", "\u{e4} umlaut", "item", "a", "B", "b", "😀 smile", "😀", "", "x01", "x1", "x001", "Été", "ete", "straße", "strasse", "ΣΟΦΟΣ", "σοφος", "new folder (2)", "new folder (10)", "a.b.c", ".hidden"];
        let mut entries: Vec<Entry> = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let kind: Arc<str> = Arc::from(if i % 5 == 0 { "" } else { "File" });
            let mut e = sort_entry(n, kind, (i % 4) as u64);
            entries.push(e.clone());
            // The same name in two search folders.
            e.target = Some(format!("C:\\Z{}\\{n}", i % 3));
            entries.push(e.clone());
            e.target = Some(format!("C:\\a{}\\{n}", i % 2));
            entries.push(e);
        }
        let old_order = |key: SortKey, descending: bool, folders_first: bool, x: u32, y: u32| {
            let (a, b) = (&entries[x as usize], &entries[y as usize]);
            if folders_first {
                let group = b.is_container().cmp(&a.is_container());
                if group != Ordering::Equal {
                    return group;
                }
            }
            let by_name = || name_order(a, b);
            let flip = |o: Ordering| if descending { o.reverse() } else { o };
            match key {
                SortKey::Name => flip(by_name()),
                SortKey::Date => flip(a.modified.cmp(&b.modified)).then_with(by_name),
                SortKey::Created => flip(a.created.cmp(&b.created)).then_with(by_name),
                SortKey::Type => flip(cmp_ignore_case(&a.type_name, &b.type_name)).then_with(by_name),
                SortKey::Size => flip(a.size.cmp(&b.size)).then_with(by_name),
                SortKey::Folder => flip(natural_cmp_ignore_case(result_folder(a), result_folder(b))).then_with(by_name),
                SortKey::Dimensions | SortKey::Length => by_name(),
            }
        };
        let keys = [SortKey::Name, SortKey::Date, SortKey::Created, SortKey::Type, SortKey::Size, SortKey::Folder, SortKey::Dimensions];
        for key in keys {
            for (descending, folders_first) in [(false, true), (true, true), (false, false), (true, false)] {
                let mut view: Vec<u32> = (0..entries.len() as u32).rev().collect();
                // (Only some in view: the others have no key looked up.)
                view.retain(|i| i % 7 != 3);
                let mut want = view.clone();
                want.sort_by(|&x, &y| old_order(key, descending, folders_first, x, y));
                sort_view(&entries, &mut view, key, descending, folders_first);
                assert_eq!(view, want, "{key:?} descending {descending} folders first {folders_first}");
            }
        }
        // By a value only some have.
        let values: Vec<Option<u32>> = (0..entries.len()).map(|i| (i % 3 != 0).then_some((i % 4) as u32)).collect();
        for descending in [false, true] {
            let mut view: Vec<u32> = (0..entries.len() as u32).collect();
            let mut want = view.clone();
            want.sort_by(|&x, &y| {
                let (a, b) = (&entries[x as usize], &entries[y as usize]);
                b.is_container().cmp(&a.is_container()).then_with(|| match (values[x as usize], values[y as usize]) {
                    (Some(p), Some(q)) => (if descending { q.cmp(&p) } else { p.cmp(&q) }).then_with(|| name_order(a, b)),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => name_order(a, b),
                })
            });
            sort_view_by(&entries, &mut view, descending, true, &values);
            assert_eq!(view, want);
        }
        // Nothing stays borrowed: one-off compares still work, and an empty view sorts.
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        let mut none: Vec<u32> = Vec::new();
        sort_view(&entries, &mut none, SortKey::Name, false, true);
        assert!(none.is_empty());
    }

    /// Sorting by Type: entries sharing one type's Arc go just as those with copies of the text.
    #[test]
    fn by_type_shared_or_not_sorts_the_same() {
        let kinds = ["Text Document", "TEXT document", "PNG File", "ΣΟΦΟΣ", "σοφος", "😀 File", ""];
        let shared: Vec<Arc<str>> = kinds.iter().map(|k| Arc::from(*k)).collect();
        let make = |share: bool| -> Vec<Entry> {
            (0..80).map(|i| sort_entry(&format!("n{} {}", i % 9, i), if share { shared[i % kinds.len()].clone() } else { Arc::from(kinds[i % kinds.len()]) }, i as u64 % 4)).collect()
        };
        let (a, b) = (make(true), make(false));
        for (descending, folders_first) in [(false, true), (true, true), (false, false), (true, false)] {
            let mut va: Vec<u32> = (0..80).collect();
            let mut vb = va.clone();
            sort_view(&a, &mut va, SortKey::Type, descending, folders_first);
            sort_view(&b, &mut vb, SortKey::Type, descending, folders_first);
            assert_eq!(va, vb, "descending {descending}, folders first {folders_first}");
        }
    }

    #[test]
    fn accented_letters_sort_beside_their_letter() {
        // A name starting with an umlaut is among the a's, as in Explorer, not after the z's.
        let mut names = vec!["item", "\u{e4} umlaut", "zebra", "a", "b", "\u{e9}t\u{e9}", "eagle", "f"];
        names.sort_by(|a, b| natural_cmp(a, b));
        let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(at("a") < at("\u{e4} umlaut") && at("\u{e4} umlaut") < at("b"), "{names:?}");
        assert!(at("\u{e4} umlaut") < at("item"), "{names:?}");
        assert!(at("eagle") < at("\u{e9}t\u{e9}") && at("\u{e9}t\u{e9}") < at("f"), "{names:?}");
        // Numbers still count by their value.
        let mut numbered = vec!["\u{e4}10", "\u{e4}9", "a10", "a9"];
        numbered.sort_by(|a, b| natural_cmp(a, b));
        let at = |n: &str| numbered.iter().position(|x| *x == n).unwrap();
        assert!(at("a9") < at("a10") && at("\u{e4}9") < at("\u{e4}10"), "{numbered:?}");
        // Different names never tie, and either way round they agree.
        assert_ne!(natural_cmp("a", "\u{e4}"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "\u{e4}"), natural_cmp("\u{e4}", "a").reverse());
    }

    #[test]
    fn folding_without_windows_keys() {
        assert_eq!(folded("\u{e4} umlaut"), "a umlaut");
        assert_eq!(folded("stra\u{df}e \u{153}uvre \u{142}\u{f3}d\u{17a} \u{17f}"), "strasse oeuvre lodz s");
        assert_eq!(folded("\u{f7}\u{fe}\u{d7} \u{4e2d}"), "\u{f7}\u{fe}\u{d7} \u{4e2d}");
        assert_eq!(folded("\u{c0}\u{ff}\u{100}\u{17e}"), "ayaz");
        assert_eq!(folded("\u{12d}\u{131}\u{135}\u{138}\u{149}\u{14b}\u{177}\u{178}"), "iijknnyy");
        // Folded names sort as the a's do, numbers counting.
        let mut names = vec!["item", "\u{e4} umlaut", "a10", "\u{e4}9", "b"];
        names.sort_by(|a, b| natural_cmp_bytes(folded(a).as_bytes(), folded(b).as_bytes(), false));
        assert_eq!(names, vec!["\u{e4} umlaut", "\u{e4}9", "a10", "b", "item"]);
    }

    #[test]
    fn sorting_many_names_is_quick() {
        let names: Vec<String> =
            (0..20_000).map(|n| format!("{} photo {}", ["\u{e4}", "a", "item", "z\u{fc}rich"][n % 4], n * 7919 % 20_000)).collect();
        let mut sorted = names.clone();
        let start = Instant::now();
        sorted.sort_by(|a, b| natural_cmp(a, b));
        let took = start.elapsed();
        assert!(took < Duration::from_secs(3), "sorting 20,000 names took {took:?}");
        assert!(sorted.windows(2).all(|w| natural_cmp(&w[0], &w[1]) == Ordering::Less));
        let first = |prefix: &str| sorted.iter().position(|x| x.starts_with(prefix)).unwrap();
        assert!(first("item ") < first("z\u{fc}rich "));
        assert!(sorted.iter().position(|x| x.starts_with("\u{e4} ")).unwrap() < first("item "));
        let numbers: Vec<usize> =
            sorted.iter().filter_map(|x| x.strip_prefix("item photo ")).map(|n| n.parse().unwrap()).collect();
        assert!(numbers.len() == 5_000 && numbers.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "file2", "file1", "file02", "a", "file", "b1", "b01"];
        names.sort_by(|a, b| natural_cmp(a, b));
        // "2" and "02" (and "1" and "01") are the same number: Windows' sort keys decide which of
        // the two goes first.
        let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert_eq!(&names[..1], ["a"]);
        assert!(at("b1").max(at("b01")) < at("file"), "{names:?}");
        assert!(at("file") < at("file1") && at("file1") < at("file2").min(at("file02")), "{names:?}");
        assert!(at("file2").max(at("file02")) < at("file10"), "{names:?}");
        // Without them, the one with fewer zeros.
        let mut names = vec!["file10", "file2", "file1", "file02", "a", "file", "b1", "b01"];
        names.sort_by(|a, b| natural_cmp_bytes(a.as_bytes(), b.as_bytes(), false));
        assert_eq!(names, vec!["a", "b1", "b01", "file", "file1", "file2", "file02", "file10"]);
    }

    #[test]
    fn parents() {
        assert_eq!(parent("C:\\Users\\YourName").as_deref(), Some("C:\\Users"));
        assert_eq!(parent("C:\\Users").as_deref(), Some("C:\\"));
        assert_eq!(parent("C:\\").as_deref(), Some(""));
        assert_eq!(parent(""), None);
        assert_eq!(parent("\\\\server\\share").as_deref(), Some(""));
        assert_eq!(parent("\\\\server\\share\\x").as_deref(), Some("\\\\server\\share"));
    }

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind,
            size,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: extension(name),
            type_name: Arc::from("x"),
            target: None,
            free: 0,
        }
    }

    #[test]
    fn hidden_and_protected_files_show_like_in_explorer() {
        let mut plain = entry("a.txt", EntryKind::File, 1);
        let mut hidden = plain.clone();
        hidden.attributes = ATTR_HIDDEN;
        let mut protected = plain.clone();
        protected.attributes = ATTR_HIDDEN | ATTR_SYSTEM;
        plain.attributes = ATTR_SYSTEM; // system alone isn't hidden
        assert!(plain.is_visible(false, false));
        assert!(!hidden.is_visible(false, false) && hidden.is_visible(true, false));
        assert!(!protected.is_visible(true, false) && !protected.is_visible(false, true) && protected.is_visible(true, true));
    }

    #[test]
    fn ties_stay_a_to_z_whichever_way() {
        let entries = vec![
            entry("b", EntryKind::Folder, 0),
            entry("a", EntryKind::Folder, 0),
            entry("small.txt", EntryKind::File, 1),
            entry("big.txt", EntryKind::File, 9),
        ];
        let mut view: Vec<u32> = (0..4).collect();
        sort_view(&entries, &mut view, SortKey::Size, true, true);
        let names: Vec<&str> = view.iter().map(|&i| entries[i as usize].name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "big.txt", "small.txt"]);
        sort_view(&entries, &mut view, SortKey::Name, true, true);
        let names: Vec<&str> = view.iter().map(|&i| entries[i as usize].name.as_str()).collect();
        assert_eq!(names, vec!["b", "a", "small.txt", "big.txt"]);
    }

    #[test]
    fn typed_paths() {
        assert_eq!(resolve("src", "C:\\Users\\me"), "C:\\Users\\me\\src");
        assert_eq!(resolve("..", "C:\\Users\\me"), "C:\\Users");
        assert_eq!(resolve("\\Windows", "C:\\Users\\me"), "C:\\Windows");
        assert_eq!(resolve("d:/x/", "C:\\Users"), "D:\\x");
        assert_eq!(resolve("this pc", "C:\\Users"), "");
        assert_eq!(resolve("\\\\server\\share", "C:\\Users"), "\\\\server\\share");
        // What the path box shows on Home goes back there, from anywhere.
        assert_eq!(resolve("Home", crate::recent::LOCATION), crate::recent::LOCATION);
        assert_eq!(resolve(" home ", "C:\\Users"), crate::recent::LOCATION);
    }

    #[test]
    fn long_paths() {
        let short = "C:\\Users";
        assert_eq!(wide_path(short), wide(short));
        let long = format!("C:\\{}", "x".repeat(300));
        assert_eq!(String::from_utf16_lossy(&wide_path(&long)), format!("\\\\?\\{long}\0"));
        let unc = format!("\\\\server\\share\\{}", "y".repeat(300));
        assert!(String::from_utf16_lossy(&wide_path(&unc)).starts_with("\\\\?\\UNC\\server\\share\\"));
    }

    #[test]
    fn counts() {
        assert_eq!(format_count(999), "999");
        let big = format_count(1_234_567);
        assert_eq!(big.chars().filter(char::is_ascii_digit).collect::<String>(), "1234567");
        assert!(big.len() > 7, "a thousands separator was added: {big}");
    }

    fn digits(s: &str) -> String {
        s.chars().filter(char::is_ascii_digit).collect()
    }

    #[test]
    fn sizes_round_up_and_switch_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1 KB");
        assert_eq!(format_size(12_700), "13 KB", "KB are rounded up, as in Explorer");
        // (The separators are the user's own, so only the digits are compared.)
        let mb = format_size(3 * 1024 * 1024 + 100_000);
        assert!(mb.ends_with(" MB") && digits(&mb) == "31", "{mb}");
        let gb = format_size(5 * 1024 * 1024 * 1024 + 300 * 1024 * 1024);
        assert!(gb.ends_with(" GB") && digits(&gb) == "53", "{gb}");
        let tb = format_size(2 * 1024u64.pow(4));
        assert!(tb.ends_with(" TB") && digits(&tb) == "200", "{tb}");
        // Just under a unit is one of it, not 1,024 of the one below.
        for (bytes, unit, shown) in [(1024 * 1024 - 1, " MB", "10"), (1024 * 1024 * 1024 - 1, " GB", "10"), (1024u64.pow(4) - 1, " TB", "100")] {
            let s = format_size(bytes);
            assert!(s.ends_with(unit) && digits(&s) == shown, "{s}");
        }
        let mb = format_size(1_073_636_966);
        assert!(mb.ends_with(" MB") && digits(&mb) == "10239", "{mb}");
        // Always in KB.
        assert_eq!(format_size_as(512, SizeFormat::Kb), "1 KB");
        assert_eq!(format_size_as(0, SizeFormat::Kb), "0 KB");
        let kb = format_size_as(3_247_000, SizeFormat::Kb);
        assert!(kb.ends_with(" KB") && digits(&kb) == "3171", "{kb}");
        assert_eq!(format_size_as(512, SizeFormat::Auto), "512 B");
    }

    #[test]
    fn words_in_quotes_are_for_inside_files_and_the_rest_for_the_name() {
        let split = |t: &str| {
            let (inside, rest) = split_quotes(t);
            (inside, rest.split_whitespace().collect::<Vec<_>>().join(" "))
        };
        assert_eq!(split("\"hello world\" *.md"), (vec!["hello world".to_string()], "*.md".to_string()));
        assert_eq!(split("a \"b\" c \"d e"), (vec!["b".to_string(), "d e".to_string()], "a c".to_string()));
        assert_eq!(split("\"\" x"), (vec![], "x".to_string()));
        let f = NameFilter::new("\"Hello World\" *.md *.TXT");
        assert_eq!(f.inside(), ["hello world"]);
        assert!(f.keeps("notes.md", String::new) && f.keeps("a.txt", String::new) && !f.keeps("a.png", String::new));
        assert!(f.reads("notes.md") && f.reads_any());
        let plain = NameFilter::new("\"hello\"");
        assert!(plain.keeps("anything.png", String::new), "the name doesn't matter");
        assert!(plain.reads("a.md") && plain.reads("x.glint") && !plain.reads("a.png") && !plain.reads("noext") && !plain.reads_any());
        let named = NameFilter::new("\"hello\" report");
        assert!(named.keeps("report.txt", String::new) && !named.keeps("notes.txt", String::new));
        // Without quotes, nothing changes: no looking inside, and a pattern is still the whole text.
        let old = NameFilter::new("tax 2024");
        assert!(old.inside().is_empty() && !old.reads("a.txt"));
        assert!(NameFilter::new("*.md *.txt").keeps("a.md *.txt", String::new));
    }

    #[test]
    fn numbers_the_users_way() {
        assert_eq!(number_with(1023.94, 1, ",", "."), "1,023.9");
        assert_eq!(number_with(1023.94, 1, ".", ","), "1.023,9");
        assert_eq!(number_with(1_234_567.0, 0, "\u{a0}", ","), "1\u{a0}234\u{a0}567");
        assert_eq!(number_with(3.0, 2, ",", "."), "3.00");
        assert_eq!(number_with(-1.0, 0, ",", "."), "0");
    }

    #[test]
    fn relative_dates_say_how_long_ago() {
        const MIN: u64 = 60 * 10_000_000;
        const DAY: u64 = 24 * 60 * MIN;
        let now = 1000 * DAY;
        assert_eq!(relative(now - 10 * 10_000_000, now, 5, 5), Relative::JustNow);
        assert_eq!(relative(now - 5 * MIN, now, 5, 5), Relative::MinutesAgo(5));
        assert_eq!(relative(now - 60 * MIN + 1, now, 5, 5), Relative::MinutesAgo(59));
        assert_eq!(relative(now - 61 * MIN, now, 5, 5), Relative::Today);
        assert_eq!(relative(now - 61 * MIN, now, 4, 5), Relative::Yesterday);
        assert_eq!(relative(now - 3 * DAY, now, 2, 5), Relative::Weekday);
        assert_eq!(relative(now - 6 * DAY, now, -1, 5), Relative::Weekday);
        assert_eq!(relative(now - 7 * DAY, now, -2, 5), Relative::Date);
        // In the future (a camera with its clock wrong): by its day.
        assert_eq!(relative(now + MIN, now, 5, 5), Relative::Today);
        assert_eq!(relative(now + 2 * DAY, now, 7, 5), Relative::Date);
        // Days are counted across months and years.
        let st = |y, m, d| SYSTEMTIME { wYear: y, wMonth: m, wDay: d, ..Default::default() };
        assert_eq!(day_number(&st(2026, 3, 1)) - day_number(&st(2026, 2, 28)), 1);
        assert_eq!(day_number(&st(2024, 3, 1)) - day_number(&st(2024, 2, 29)), 1);
        assert_eq!(day_number(&st(2027, 1, 1)) - day_number(&st(2026, 12, 31)), 1);
        assert_eq!(day_number(&st(2026, 10, 9)) - day_number(&st(2026, 10, 2)), 7);
    }

    #[test]
    fn date_styles() {
        // 2026-10-09 12:00 UTC: the local time differs by zone, but the shapes don't.
        let ft = 134_360_208_000_000_000u64;
        let iso = format_date_as(ft, DateFormat::Iso);
        assert!(iso.len() == 16 && iso.starts_with("2026-10-") && iso.as_bytes()[10] == b' ' && iso.as_bytes()[13] == b':', "{iso}");
        assert!(!format_date_as(ft, DateFormat::Default).is_empty());
        assert!(!format_date_as(ft, DateFormat::Relative).is_empty());
        assert_eq!(format_date_as(0, DateFormat::Iso), "", "an unknown date stays empty");
    }

    #[test]
    fn the_filter_keeps_names_with_every_word_or_matching_the_pattern() {
        // Several words: all of them, anywhere, in any order.
        let f = NameFilter::new("Tax 2024");
        assert!(f.keeps("2024 tax return.pdf", String::new));
        assert!(!f.keeps("tax return.pdf", String::new));
        // A pattern: the whole name.
        let p = NameFilter::new("*.PDF");
        assert!(p.keeps("report.pdf", String::new) && !p.keeps("report.pdf.txt", String::new) && !p.keeps("pdf", String::new));
        let q = NameFilter::new("report-??.txt");
        assert!(q.keeps("report-12.txt", String::new) && !q.keeps("report-123.txt", String::new));
        assert!(NameFilter::new("a*b*c").keeps("axxbyyc", String::new) && !NameFilter::new("a*b*c").keeps("axxbyy", String::new));
        assert!(NameFilter::new("  ").keeps("anything", String::new));
        assert!(NameFilter::new("*").keeps("", String::new));
        // A long name against a pattern with many stars stays quick.
        let long = "a".repeat(5000);
        assert!(!NameFilter::new("*a*a*a*a*b").keeps(&long, String::new));
    }

    #[test]
    fn the_filter_keeps_tagged_items_for_tag_words() {
        use crate::tags::{map_of, Tag};
        let tags = map_of(&[("C:\\x\\Invoice 3.pdf", Tag::Red), ("C:\\x\\notes.txt", Tag::Red), ("C:\\x\\Invoice 4.pdf", Tag::Blue)]);
        let f = |text: &str| NameFilter::with_tags(text, || tags.clone());
        let keeps = |filter: &NameFilter, name: &str| filter.keeps(&name.to_lowercase(), || join("C:\\x", name));
        // tag:red (in any capitals): the red ones only.
        for red in [f("tag:red"), f("TAG:Red"), f("  tag:red  ")] {
            assert!(keeps(&red, "Invoice 3.pdf") && keeps(&red, "notes.txt"));
            assert!(!keeps(&red, "Invoice 4.pdf") && !keeps(&red, "untagged.pdf"));
        }
        // tag: alone: everything tagged.
        let any = f("tag:");
        assert!(keeps(&any, "Invoice 4.pdf") && keeps(&any, "notes.txt") && !keeps(&any, "untagged.pdf"));
        // With words, or a pattern: both must hold, in any order.
        for both in [f("tag:red invoice"), f("invoice tag:red")] {
            assert!(keeps(&both, "Invoice 3.pdf"));
            assert!(!keeps(&both, "notes.txt") && !keeps(&both, "Invoice 4.pdf"));
        }
        let pattern = f("tag: *.pdf");
        assert!(keeps(&pattern, "Invoice 3.pdf") && keeps(&pattern, "Invoice 4.pdf") && !keeps(&pattern, "notes.txt"));
        // A colour no item has, or none at all: nothing.
        assert!(!keeps(&f("tag:green"), "Invoice 3.pdf"));
        let none = NameFilter::with_tags("tag:red", Default::default);
        assert!(!keeps(&none, "Invoice 3.pdf"));
        // Without a tag word nothing changes, and the path isn't even asked for.
        assert!(f("invoice").keeps("invoice 4.pdf", || unreachable!()));
        assert!(f("").keeps("anything", || unreachable!()));
    }

    /// A file with this size, changed at this FILETIME.
    fn dated(name: &str, size: u64, modified: u64) -> Entry {
        Entry { modified, ..entry(name, EntryKind::File, size) }
    }

    /// The names of `items` the filter for `text` keeps, by name, size and date.
    fn kept<'a>(text: &str, items: &'a [Entry]) -> Vec<&'a str> {
        let f = NameFilter::with_tags(text, Default::default);
        items.iter().filter(|e| f.keeps_item(e, || join("C:\\t", &e.name))).map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn size_words_keep_files_of_that_size() {
        const MB: u64 = 1 << 20;
        let items = [
            entry("empty.txt", EntryKind::File, 0),
            entry("small.txt", EntryKind::File, 400 << 10),
            entry("exactly 500kb.txt", EntryKind::File, 500 << 10),
            entry("ten.bin", EntryKind::File, 10 * MB),
            entry("big.iso", EntryKind::File, 3 << 30),
            entry("huge.vhd", EntryKind::File, 6 << 30),
            entry("Folder", EntryKind::Folder, 0),
        ];
        assert_eq!(kept("size:>10mb", &items), ["big.iso", "huge.vhd"], "more than, not as much as");
        assert_eq!(kept("size:<500kb", &items), ["empty.txt", "small.txt"], "less than, and folders never");
        assert_eq!(kept("size:1gb..5gb", &items), ["big.iso"]);
        assert_eq!(kept("size:5gb..1gb", &items), ["big.iso"], "a range the wrong way round is the same range");
        assert_eq!(kept("size:500kb..10mb", &items), ["exactly 500kb.txt", "ten.bin"], "both ends count");
        assert_eq!(kept("size:0", &items), ["empty.txt"], "a number alone is exactly that");
        assert_eq!(kept("size:>10485759", &items), ["ten.bin", "big.iso", "huge.vhd"], "no unit is bytes");
        assert_eq!(kept("size:2gb..", &items), ["big.iso", "huge.vhd"]);
        assert_eq!(kept("size:..400kb", &items), ["empty.txt", "small.txt"]);
        assert_eq!(kept("size:>0.5gb", &items), ["big.iso", "huge.vhd"], "parts of a unit");
        assert_eq!(kept("size:<0", &items), Vec::<&str>::new(), "below nothing");
        // Any capitals, and with other words.
        for text in ["SIZE:>10MB", "Size:>10Mb", "  size:>10mb  "] {
            assert_eq!(kept(text, &items), ["big.iso", "huge.vhd"], "{text}");
        }
        assert_eq!(kept("size:>1mb size:<5gb", &items), ["ten.bin", "big.iso"], "two size words: both hold");
        assert_eq!(kept("size:>1mb iso", &items), ["big.iso"]);
        assert_eq!(kept("*.txt size:>100kb", &items), ["small.txt", "exactly 500kb.txt"]);
        // Numbers that aren't: nothing, as a word no name has; nothing after the colon: not yet a word.
        for bad in ["size:>", "size:<", "size:big", "size:>10tb", "size:>-5", "size:>1.", "size:>.5mb", "size:1..2..3", "size:>1.2.3mb", "size:>99999999999gb", "size:>=1mb", "size:mb"] {
            assert!(kept(bad, &items).is_empty(), "{bad}");
        }
        assert_eq!(kept("size:", &items).len(), items.len());
        // A huge number of bytes that still fits, and one that doesn't.
        assert_eq!(kept("size:<18446744073709551615", &items).len(), 6);
        assert!(kept("size:>18446744073709551615", &items).is_empty());
        assert!(kept("size:>18446744073709551616", &items).is_empty());
    }

    #[test]
    fn date_words_keep_items_changed_then() {
        let day = |y: u16, m: u16, d: u16| day_number(&SYSTEMTIME { wYear: y, wMonth: m, wDay: d, ..Default::default() });
        let hour = 36_000_000_000;
        // At noon local time on each day.
        let at = |y, m, d| local_midnight(day(y, m, d)).unwrap() + 12 * hour;
        let today = today().unwrap();
        let items = [
            dated("old.txt", 1, at(2025, 12, 31)),
            dated("new year.txt", 1, local_midnight(day(2026, 1, 1)).unwrap()),
            dated("mid jan.txt", 1, at(2026, 1, 15)),
            dated("last of jan.txt", 1, local_midnight(day(2026, 2, 1)).unwrap() - 1),
            dated("leap.txt", 1, at(2028, 2, 29)),
            dated("unknown.txt", 1, 0),
            dated("today.txt", 1, local_midnight(today).unwrap() + hour),
            dated("yesterday.txt", 1, local_midnight(today - 1).unwrap() + hour),
            Entry { modified: at(2026, 1, 31), ..entry("Folder", EntryKind::Folder, 0) },
        ];
        assert_eq!(kept("date:2026-01", &items), ["new year.txt", "mid jan.txt", "last of jan.txt", "Folder"], "folders have a date");
        assert_eq!(kept("date:2026-01-31", &items), ["last of jan.txt", "Folder"], "to the last moment of the day");
        assert_eq!(kept("date:2026-1-15", &items), ["mid jan.txt"]);
        assert_eq!(kept("date:2025", &items), ["old.txt"]);
        assert_eq!(kept("date:2028-02-29", &items), ["leap.txt"]);
        assert_eq!(kept("date:<2026-01-01", &items), ["old.txt"], "before, not on");
        assert_eq!(kept("date:>2026-01-31", &items).first(), Some(&"leap.txt"), "after, not on");
        assert_eq!(kept("date:<2026", &items), ["old.txt"]);
        assert_eq!(kept("date:today", &items), ["today.txt"]);
        assert_eq!(kept("date:yesterday", &items), ["yesterday.txt"]);
        assert_eq!(kept("DATE:Today", &items), ["today.txt"], "any capitals");
        assert_eq!(kept("date:>2026-01-01 date:<2026-01-31", &items), ["mid jan.txt"], "two date words: both hold");
        assert_eq!(kept("date:2026-01 size:0", &items), Vec::<&str>::new());
        assert_eq!(kept("date:2026-01 jan", &items), ["mid jan.txt", "last of jan.txt"]);
        // Dates that aren't: nothing.
        for bad in ["date:2026-13-40", "date:2026-13", "date:2026-02-30", "date:2025-02-29", "date:2026-00-10", "date:26", "date:20260", "date:2026-01-31-1", "date:2026--01", "date:tomorrow", "date:>", "date:1600", "date:>today2", "date:2026-1x"] {
            assert!(kept(bad, &items).is_empty(), "{bad}");
        }
        assert_eq!(kept("date:", &items).len(), items.len(), "not yet a word");
        assert!(kept("date:2026-01 date:2025", &items).is_empty(), "two that can't both hold");
    }

    #[test]
    fn a_minus_leaves_names_out() {
        let items = [entry("report.docx", EntryKind::File, 1), entry("report draft.docx", EntryKind::File, 1), entry("cache.tmp", EntryKind::File, 1), entry("-notes.txt", EntryKind::File, 1)];
        assert_eq!(kept("-draft", &items), ["report.docx", "cache.tmp", "-notes.txt"]);
        assert_eq!(kept("-DRAFT", &items), ["report.docx", "cache.tmp", "-notes.txt"]);
        assert_eq!(kept("report -draft", &items), ["report.docx"]);
        assert_eq!(kept("-draft report", &items), ["report.docx"], "in any order");
        assert_eq!(kept("-*.tmp", &items), ["report.docx", "report draft.docx", "-notes.txt"]);
        assert_eq!(kept("*.docx -*draft*", &items), ["report.docx"]);
        assert_eq!(kept("-draft -*.tmp", &items), ["report.docx", "-notes.txt"]);
        // A minus alone is an ordinary word; a name that starts with one is found another way.
        assert_eq!(kept("-", &items), ["-notes.txt"]);
        assert_eq!(kept("*-notes*", &items), ["-notes.txt"]);
        assert_eq!(kept("regex:^-notes", &items), ["-notes.txt"]);
        // A word in the middle of a name isn't touched.
        assert_eq!(kept("rt-dr", &[entry("rt-draft", EntryKind::File, 1)]), ["rt-draft"]);
    }

    #[test]
    fn regex_words_match_the_name_in_any_capitals() {
        let items = [
            entry("IMG_2024.jpg", EntryKind::File, 1),
            entry("img_1234.JPEG", EntryKind::File, 1),
            entry("IMG_12.jpg", EntryKind::File, 1),
            entry("photo IMG_2024.jpg", EntryKind::File, 1),
            entry("İstanbul 🌉 #1.png", EntryKind::File, 1),
            entry("Año.txt", EntryKind::File, 1),
        ];
        assert_eq!(kept(r"regex:^IMG_\d{4}\.jpe?g$", &items), ["IMG_2024.jpg", "img_1234.JPEG"]);
        assert_eq!(kept(r"REGEX:^img_\d{4}\.JPE?G$", &items), ["IMG_2024.jpg", "img_1234.JPEG"]);
        assert_eq!(kept(r"regex:^\D+$", &items), ["Año.txt"], "a capital after \\ keeps its meaning (not a digit)");
        assert_eq!(kept("regex:^İst", &items), ["İstanbul 🌉 #1.png"]);
        assert_eq!(kept("regex:🌉 regex:#1", &items), ["İstanbul 🌉 #1.png"], "each must match");
        assert_eq!(kept("regex:AÑO", &items), ["Año.txt"]);
        // With other words, patterns and tags.
        assert_eq!(kept(r"regex:\d{4} photo", &items), ["photo IMG_2024.jpg"]);
        assert_eq!(kept(r"*.jpg regex:_\d{4}", &items), ["IMG_2024.jpg", "photo IMG_2024.jpg"]);
        assert_eq!(kept(r"regex:_\d{4} -photo size:1", &items), ["IMG_2024.jpg", "img_1234.JPEG"]);
        // A broken or half-typed pattern keeps nothing, and doesn't panic; nothing after it isn't one yet.
        for broken in ["regex:(", "regex:[a-", r"regex:\", "regex:a{2,1}", "regex:(?<x", "regex:img regex:)"] {
            assert!(kept(broken, &items).is_empty(), "{broken}");
        }
        assert_eq!(kept("regex:", &items).len(), items.len());
        // A pattern too big to make is refused rather than taking the memory.
        assert!(kept("regex:((a{100}){100}){100}", &items).is_empty());
        let deep = format!("regex:{}a{}", "(".repeat(500), ")".repeat(500));
        assert!(kept(&deep, &items).is_empty());
        // In quotes it's text to look for inside files.
        let f = NameFilter::new("\"regex:x\" *.txt");
        assert_eq!(f.inside(), ["regex:x"]);
        assert!(f.keeps("a.txt", String::new));
        // A long name, and a long pattern, still answer at once.
        let long = "a".repeat(300);
        let start = Instant::now();
        assert!(NameFilter::new("regex:(a|aa)*b").keeps(&long, String::new) == false);
        assert!(NameFilter::new("regex:^(a*)*$").keeps(&long, String::new));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_new_words_go_with_tags_and_quotes() {
        use crate::tags::{map_of, Tag};
        let tags = map_of(&[("C:\\t\\big draft.iso", Tag::Red), ("C:\\t\\big.iso", Tag::Red), ("C:\\t\\small.iso", Tag::Red)]);
        let items = [entry("big draft.iso", EntryKind::File, 2 << 30), entry("big.iso", EntryKind::File, 2 << 30), entry("small.iso", EntryKind::File, 1), entry("other.iso", EntryKind::File, 2 << 30)];
        let f = NameFilter::with_tags("TAG:red size:>1gb -draft", || tags.clone());
        let kept: Vec<&str> = items.iter().filter(|e| f.keeps_item(e, || join("C:\\t", &e.name))).map(|e| e.name.as_str()).collect();
        assert_eq!(kept, ["big.iso"]);
        // Quotes: inside files; the rest still counts.
        let q = NameFilter::new("\"hello -draft size:>1gb\" size:<1kb -old *.txt");
        assert_eq!(q.inside(), ["hello -draft size:>1gb"]);
        assert!(q.keeps_item(&entry("a.txt", EntryKind::File, 10), String::new));
        assert!(!q.keeps_item(&entry("a.txt", EntryKind::File, 2000), String::new));
        assert!(!q.keeps_item(&entry("old a.txt", EntryKind::File, 10), String::new));
        assert!(!q.keeps_item(&entry("a.md", EntryKind::File, 10), String::new));
        let r = NameFilter::new(r#""x" regex:"\d""#);
        assert_eq!(r.inside(), ["x"], "a quote in a pattern doesn't start words for inside files");
    }

    #[test]
    fn old_searches_work_as_before() {
        // Without the new words the filter is made just as it was.
        for text in ["tax 2024", "*.PDF", "a*  b", "report-??.txt", "  ", "\"hello\" *.md", "rt-draft", "Ünïcödé 🌉 #1", "size", "date", "regex"] {
            let f = NameFilter::with_tags(text, Default::default);
            assert!(f.without.is_empty() && f.regex.is_empty() && f.size.is_none() && f.date.is_none() && !f.broken, "{text}");
        }
        assert_eq!(NameFilter::new("a*  b").name, NameRule::Pattern("a*  b".chars().collect()), "spaces in a pattern stay");
        assert_eq!(NameFilter::new("Tax 2024").name, NameRule::Words(vec!["tax".into(), "2024".into()]));
        // keeps_item and keeps agree when there are no size or date words, also for folders.
        let f = NameFilter::new("tax");
        assert!(f.keeps_item(&entry("Tax", EntryKind::Folder, 0), String::new));
        let long = "İ".repeat(300);
        assert!(NameFilter::new(&long).keeps_item(&entry(&long, EntryKind::File, 0), String::new));
    }

    #[test]
    fn folders_first_or_mixed() {
        let entries = vec![entry("zeta", EntryKind::Folder, 0), entry("alpha.txt", EntryKind::File, 1)];
        let mut view: Vec<u32> = (0..2).collect();
        let names = |view: &[u32]| view.iter().map(|&i| entries[i as usize].name.clone()).collect::<Vec<_>>();
        sort_view(&entries, &mut view, SortKey::Name, false, true);
        assert_eq!(names(&view), vec!["zeta", "alpha.txt"]);
        sort_view(&entries, &mut view, SortKey::Name, false, false);
        assert_eq!(names(&view), vec!["alpha.txt", "zeta"]);
    }

    #[test]
    fn search_results_sort_by_folder_and_keep_same_names_apart() {
        let result = |name: &str, path: &str| Entry { target: Some(path.to_string()), ..entry(name, EntryKind::File, 1) };
        let entries = vec![
            result("a.txt", "C:\\x\\Zed\\a.txt"),
            result("a.txt", "C:\\x\\alpha\\a.txt"),
            result("b.txt", "C:\\x\\b.txt"),
            result("a.txt", "C:\\x\\folder10\\a.txt"),
            result("a.txt", "C:\\x\\folder9\\a.txt"),
        ];
        let mut view: Vec<u32> = (0..5).collect();
        let paths = |view: &[u32]| view.iter().map(|&i| entries[i as usize].target.clone().unwrap_or_default()).collect::<Vec<_>>();
        sort_view(&entries, &mut view, SortKey::Folder, false, true);
        assert_eq!(paths(&view), vec!["C:\\x\\b.txt", "C:\\x\\alpha\\a.txt", "C:\\x\\folder9\\a.txt", "C:\\x\\folder10\\a.txt", "C:\\x\\Zed\\a.txt"]);
        // By name, the same names go by their folder.
        sort_view(&entries, &mut view, SortKey::Name, false, true);
        assert_eq!(paths(&view)[..4], ["C:\\x\\alpha\\a.txt", "C:\\x\\folder9\\a.txt", "C:\\x\\folder10\\a.txt", "C:\\x\\Zed\\a.txt"]);
        assert_eq!(result_folder(&entries[2]), "C:\\x");
        assert_eq!(result_folder(&entry("plain", EntryKind::File, 0)), "");
    }

    #[test]
    fn home_and_variables_in_typed_paths() {
        let home = crate::shell::home();
        assert_eq!(normalize("~"), normalize(&home));
        assert_eq!(normalize("~/Downloads"), normalize(&format!("{home}\\Downloads")));
        assert_eq!(normalize("~x"), "~x", "only ~ on its own or before a backslash");
        std::env::set_var("GLINT_TEST_PLACE", "C:\\Somewhere");
        assert_eq!(normalize("%GLINT_TEST_PLACE%\\x"), "C:\\Somewhere\\x");
        // A folder that is really called that keeps its name.
        let real = std::env::temp_dir().join(format!("glint-{}-%GLINT_TEST_PLACE%", std::process::id()));
        std::fs::create_dir_all(&real).unwrap();
        assert!(normalize(&real.to_string_lossy()).ends_with("-%GLINT_TEST_PLACE%"));
        let _ = std::fs::remove_dir(&real);
    }
}
