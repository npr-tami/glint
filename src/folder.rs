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
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::{
    GetDateFormatEx, GetLocaleInfoEx, GetTimeFormatEx, DATE_SHORTDATE, ENUM_DATE_FORMATS_FLAGS, LOCALE_SDECIMAL, LOCALE_STHOUSAND,
    TIME_NOSECONDS,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, GetDiskFreeSpaceExW,
    GetDriveTypeW, GetFileAttributesW, GetFullPathNameW, GetLogicalDrives, GetVolumeInformationW,
    FIND_FIRST_EX_LARGE_FETCH, INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

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
    /// A protected operating-system file (hidden and system, like desktop.ini or pagefile.sys).
    pub fn is_protected(&self) -> bool {
        self.attributes & (ATTR_HIDDEN | ATTR_SYSTEM) == ATTR_HIDDEN | ATTR_SYSTEM
    }
    /// Whether it shows, Explorer's way: hidden items only with `show_hidden`, protected
    /// operating-system files only with `show_system` as well.
    pub fn is_visible(&self, show_hidden: bool, show_system: bool) -> bool {
        !self.is_hidden() || (show_hidden && (show_system || !self.is_protected()))
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
}

/// "This is a file, not a folder.": what reading a path that is a file says. Going to a file
/// opens it instead (app.rs, `navigate`).
pub const NOT_A_FOLDER: &str = "This is a file, not a folder.";

/// Reads one folder. Never panics; problems end up in `Folder::error`.
pub fn read(path: &str) -> Folder {
    let start = Instant::now();
    let mut folder = if recycle::is_bin(path) {
        recycle::read()
    } else if path.is_empty() {
        this_pc()
    } else if !is_absolute(path) {
        // A bare name typed while on This PC: there's no folder to start from (and the process's
        // current folder has nothing to do with what's on screen).
        let error = format!("\u{201c}{path}\u{201d} isn't a full path. Type one like C:\\Users.");
        Folder { path: path.to_string(), error: Some(error), ..Default::default() }
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
    loop {
        if let Some(e) = entry_from(&data, &mut types) {
            entries.push(e);
        }
        // SAFETY: `handle` is the live search handle from above; `data` is valid.
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    // SAFETY: closes the handle we opened; it is not used afterwards.
    unsafe {
        let _ = FindClose(handle);
    }
    Ok(entries)
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

fn is_file(path: &str) -> bool {
    let w = wide_path(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    attrs != INVALID_FILE_ATTRIBUTES && attrs & ATTR_DIRECTORY == 0
}

/// Turns a Windows error into a sentence a person understands.
fn explain(code: u32, system: &str) -> String {
    match code & 0xFFFF {
        5 => "You don't have permission to open this folder.".into(),
        2 | 3 => "This folder doesn't exist (any more).".into(),
        21 => "The drive isn't ready. Is there a disc or card in it?".into(),
        53 | 67 => "The network location can't be reached.".into(),
        206 => "The path is too long.".into(),
        267 => NOT_A_FOLDER.into(),
        1392 => "The disk is damaged or unreadable.".into(),
        _ => {
            let s = system.trim();
            if s.is_empty() {
                format!("Windows couldn't open this folder (error {}).", code & 0xFFFF)
            } else {
                format!("Windows couldn't open this folder: {s}")
            }
        }
    }
}

// ---------------------------------------------------------------- sorting

/// Compares two lower-case names the way people count: "file2" before "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_bytes(a.as_bytes(), b.as_bytes(), false)
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
    view.sort_unstable_by(|&x, &y| {
        let (a, b) = (&entries[x as usize], &entries[y as usize]);
        if folders_first {
            let group = b.is_container().cmp(&a.is_container());
            if group != Ordering::Equal {
                return group;
            }
        }
        let by_name = || {
            natural_cmp(&a.lower, &b.lower)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| natural_cmp_ignore_case(result_folder(a), result_folder(b)))
        };
        let flip = |o: Ordering| if descending { o.reverse() } else { o };
        match key {
            SortKey::Name => flip(by_name()),
            SortKey::Date => flip(a.modified.cmp(&b.modified)).then_with(by_name),
            SortKey::Created => flip(a.created.cmp(&b.created)).then_with(by_name),
            SortKey::Type => flip(cmp_ignore_case(&a.type_name, &b.type_name)).then_with(by_name),
            SortKey::Size => flip(a.size.cmp(&b.size)).then_with(by_name),
            SortKey::Folder => flip(natural_cmp_ignore_case(result_folder(a), result_folder(b))).then_with(by_name),
        }
    });
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
/// "41.7 GB", "1.82 TB".
pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", format_number((b / KB).ceil(), 0))
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{} MB", format_number(b / (KB * KB), 1))
    } else if bytes < 1024 * 1024 * 1024 * 1024 {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameFilter {
    /// Nothing typed: everything shows.
    All,
    Words(Vec<String>),
    /// A wildcard pattern, in lower case.
    Pattern(Vec<char>),
}

impl NameFilter {
    pub fn new(text: &str) -> NameFilter {
        let text = text.trim().to_lowercase();
        if text.is_empty() {
            NameFilter::All
        } else if text.contains(['*', '?']) {
            NameFilter::Pattern(text.chars().collect())
        } else {
            NameFilter::Words(text.split_whitespace().map(str::to_string).collect())
        }
    }

    /// Whether a name in lower case (`Entry::lower`) is kept.
    pub fn matches(&self, lower: &str) -> bool {
        match self {
            NameFilter::All => true,
            NameFilter::Words(words) => words.iter().all(|w| lower.contains(w.as_str())),
            NameFilter::Pattern(p) => {
                let name: Vec<char> = lower.chars().collect();
                wildcard(p, &name)
            }
        }
    }
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

/// The folder above: "C:\Users\pandora" -> "C:\Users" -> "C:\" -> "" (This PC) -> None.
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

/// What a person typed, as a full path, with `base` (the folder they're in) as the starting point
/// for a name or relative path: "src" and "..\Music" go where Explorer's address bar would take
/// them, not somewhere under the process's current folder (which has nothing to do with what is
/// on screen). "\Windows" is the root of `base`'s drive. Otherwise like [`normalize`].
pub fn resolve(input: &str, base: &str) -> String {
    let path = normalize(input);
    // Already complete, This PC, the Recycle Bin, or "D:something" (rare; left to Windows).
    if path.is_empty() || base.is_empty() || is_absolute(&path) || recycle::is_bin(&path) || path.as_bytes().get(1) == Some(&b':') {
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
/// "~\Downloads"), `%VARIABLES%`, `..`, "c:", "This PC", "Recycle Bin". Returns "" for This PC.
pub fn normalize(input: &str) -> String {
    let mut s = input.trim().trim_matches('"').trim().replace('/', "\\");
    if s.is_empty() || s.eq_ignore_ascii_case("this pc") || s.eq_ignore_ascii_case("computer") {
        return String::new();
    }
    if recycle::means_bin(&s) {
        return recycle::LOCATION.to_string();
    }
    if s == "~" || s.starts_with("~\\") {
        s = format!("{}{}", crate::shell::home(), &s[1..]);
    }
    if s.contains('%') {
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
    if path.len() < LONG || path.starts_with("\\\\?\\") || path.starts_with("\\\\.\\") || !is_absolute(path) {
        return wide(path);
    }
    match path.strip_prefix("\\\\") {
        Some(unc) => wide(&format!("\\\\?\\UNC\\{unc}")),
        None => wide(&format!("\\\\?\\{path}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "file2", "file1", "file02", "a", "file", "b1", "b01"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["a", "b1", "b01", "file", "file1", "file2", "file02", "file10"]);
    }

    #[test]
    fn parents() {
        assert_eq!(parent("C:\\Users\\pandora").as_deref(), Some("C:\\Users"));
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
        // Always in KB.
        assert_eq!(format_size_as(512, SizeFormat::Kb), "1 KB");
        assert_eq!(format_size_as(0, SizeFormat::Kb), "0 KB");
        let kb = format_size_as(3_247_000, SizeFormat::Kb);
        assert!(kb.ends_with(" KB") && digits(&kb) == "3171", "{kb}");
        assert_eq!(format_size_as(512, SizeFormat::Auto), "512 B");
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
        assert!(f.matches("2024 tax return.pdf"));
        assert!(!f.matches("tax return.pdf"));
        // A pattern: the whole name.
        let p = NameFilter::new("*.PDF");
        assert!(p.matches("report.pdf") && !p.matches("report.pdf.txt") && !p.matches("pdf"));
        let q = NameFilter::new("report-??.txt");
        assert!(q.matches("report-12.txt") && !q.matches("report-123.txt"));
        assert!(NameFilter::new("a*b*c").matches("axxbyyc") && !NameFilter::new("a*b*c").matches("axxbyy"));
        assert!(NameFilter::new("  ").matches("anything"));
        assert!(NameFilter::new("*").matches(""));
        // A long name against a pattern with many stars stays quick.
        let long = "a".repeat(5000);
        assert!(!NameFilter::new("*a*a*a*a*b").matches(&long));
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
    }
}
