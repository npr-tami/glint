//! The Recycle Bin as a place in Glint: listing what's in it, putting things back, deleting them for
//! good, emptying it, and noticing when it changes.
//!
//! **Where it is.** The bin has no folder of its own: every drive keeps your deleted items in
//! `X:\$Recycle.Bin\<your user id>\` as pairs - `$R…` is the item itself, `$I…` says where it came
//! from, how big it was and when it was deleted. Tabs and the sidebar use [`LOCATION`] (Windows' own
//! name for the bin, "::{645FF040-…}") as its path.
//!
//! **Listing** ([`read`], on the folder-reading thread): Windows' shell lists the bin (its
//! IShellFolder, the one File Explorer shows), so every drive's bin is in it, and network folders'
//! too. For each item Windows gives the path of its `$R…` file; the `$I…` file next to it gives the
//! exact original path, size and date deleted. Where there is no such pair (Wine's bin, which is
//! Linux's trash), the shell's own details are used: name, original folder, date deleted.
//!
//! **Restore and delete** run as file jobs (fileops.rs threads). Restore moves the `$R…` item back
//! where it was (making the folder again if it's gone); when that name is taken now, the item gets a
//! number ("notes (2).txt") - nothing is ever replaced. Delete permanently
//! removes the `$R…` item and its `$I…` file with plain file-system calls (never following a link
//! inside a deleted folder). Both only ever touch items directly inside a `$Recycle.Bin\<user>`
//! folder whose names start with `$R` / `$I`. Emptying the whole bin is Windows' own
//! `SHEmptyRecycleBin`, so Windows asks first, shows its progress and plays its sound.
//!
//! **Changes** ([`Watch`]): while a tab shows the bin, Windows tells Glint about changes to it
//! (SHChangeNotifyRegister) on a thread of its own that sleeps in GetMessage, so nothing polls.
//!
//! For testing, `GLINT_TEST_BIN=<folder>` makes Glint use a pretend bin: the `$R…` / `$I…` pairs in
//! that folder (in the Windows 10 format, as `info_bytes` in the tests writes them), instead of the
//! real one.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};

use windows::core::{w, Interface, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{FILETIME, HANDLE, HWND, LPARAM, LRESULT, PROPERTYKEY, WPARAM};
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, SetFileAttributesW, FILE_ATTRIBUTE_READONLY, FILE_FLAGS_AND_ATTRIBUTES, MOVEFILE_COPY_ALLOWED, MOVE_FILE_FLAGS,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Shell::Common::{ITEMIDLIST, SHELLDETAILS, STRRET};
use windows::Win32::UI::Shell::{
    IEnumIDList, IShellFolder, IShellFolder2, IShellItem2, SHChangeNotification_Lock, SHChangeNotification_Unlock, SHChangeNotify,
    SHChangeNotifyDeregister, SHChangeNotifyEntry, SHChangeNotifyRegister, SHCreateItemWithParent, SHEmptyRecycleBinW,
    SHGetDesktopFolder, SHGetKnownFolderIDList, SHQueryRecycleBinW, StrRetToStrW, FOLDERID_RecycleBinFolder, SHCNE_ALLEVENTS,
    SHCNE_CREATE, SHCNE_DELETE, SHCNE_ID, SHCNE_MKDIR, SHCNE_RMDIR, SHCNF_FLUSHNOWAIT, SHCNF_PATHW, SHCNRF_InterruptLevel,
    SHCNRF_NewDelivery, SHCNRF_ShellLevel, SHCONTF_FOLDERS, SHCONTF_INCLUDEHIDDEN, SHCONTF_NONFOLDERS, SHGDNF, SHGDN_FORPARSING,
    SHGDN_INFOLDER, SHQUERYRBINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, KillTimer, PeekMessageW, PostThreadMessageW,
    RegisterClassExW, SetTimer, HWND_MESSAGE, MSG, PM_NOREMOVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_QUIT, WM_TIMER, WNDCLASSEXW,
};

use crate::fileops::{self, attributes, exists, same_path, unique_name, Outcome};
use crate::folder::{extension, file_name, format_count, format_size, join, parent, wide, wide_path, Entry, EntryKind, Folder};
use crate::icons;
use crate::log;

/// The Recycle Bin's path in Glint (what tabs, the sidebar and settings.json use): Windows' own name
/// for it.
pub const LOCATION: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";
/// What it's called on screen.
pub const NAME: &str = "Recycle Bin";

/// Where an item came from (the "Original location" column) and when it was deleted, as two
/// properties of Windows' bin (PID_DISPLACED_FROM and PID_DISPLACED_DATE).
const DISPLACED: GUID = GUID::from_u128(0x9b174b33_40ff_11d2_a27e_00c04fc30871);
const DISPLACED_FROM: PROPERTYKEY = PROPERTYKEY { fmtid: DISPLACED, pid: 2 };
const DISPLACED_DATE: PROPERTYKEY = PROPERTYKEY { fmtid: DISPLACED, pid: 3 };
/// PKEY_Size.
const SIZE_KEY: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac), pid: 12 };

const ATTR_DIRECTORY: u32 = 0x10;
/// More items than this and the listing stops (a broken enumerator must not run for ever).
const MAX_ITEMS: usize = 1_000_000;

/// Whether `path` is the Recycle Bin (Glint's own path for it, in any case).
pub fn is_bin(path: &str) -> bool {
    path.eq_ignore_ascii_case(LOCATION)
}

/// Whether something typed into the path box means the Recycle Bin: its path, "shell:RecycleBinFolder"
/// (as in Explorer's address bar) or just "Recycle Bin".
pub fn means_bin(typed: &str) -> bool {
    let t = typed.trim();
    is_bin(t)
        || t.eq_ignore_ascii_case("shell:RecycleBinFolder")
        || t.strip_prefix("shell:").is_some_and(is_bin)
        || t.eq_ignore_ascii_case(NAME)
}

/// One item in the Recycle Bin, besides what its list entry shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BinItem {
    /// The full path it was deleted from ("C:\Users\me\Desktop\notes.txt"); "" when Windows
    /// didn't say.
    pub original: String,
    /// The folder it came from (the "Original location" column).
    pub location: String,
    /// Where it is kept now: its `$R…` item. None when Windows doesn't say (Wine): then it can't be
    /// restored or deleted by Glint.
    pub real: Option<String>,
    /// Its `$I…` file (where it came from, when it was deleted), deleted with it or after a restore.
    pub info: Option<String>,
    /// When it was deleted (a FILETIME; 0 when Windows didn't say). With `original`, it tells
    /// whether the `$R…` item is still the one that was listed (`still_listed`).
    pub deleted: u64,
}

/// What a listing of the bin holds besides its entries (`Folder::bin`).
#[derive(Debug, Default)]
pub struct Listing {
    /// One per entry of the listing, in the same order.
    pub items: Vec<BinItem>,
    /// How much it all takes, in bytes (the status bar shows it).
    pub total: u64,
}

impl Listing {
    /// The status bar's line about the bin: "1.2 GB in the Recycle Bin".
    pub fn size_text(&self) -> String {
        if self.items.is_empty() {
            "The Recycle Bin is empty".to_string()
        } else {
            format!("{} in the Recycle Bin", format_size(self.total))
        }
    }
}

/// The pretend bin for testing (`GLINT_TEST_BIN`), if set.
pub fn test_dir() -> Option<&'static str> {
    static DIR: OnceLock<Option<String>> = OnceLock::new();
    DIR.get_or_init(|| std::env::var("GLINT_TEST_BIN").ok().map(|d| d.trim().trim_end_matches('\\').to_string()).filter(|d| !d.is_empty()))
        .as_deref()
}

// ---------------------------------------------------------------- reading

/// Lists the Recycle Bin. Call it on a background thread with COM started (app.rs's folder reads
/// are). Never panics; problems end up in `Folder::error`.
pub fn read() -> Folder {
    let found = match test_dir() {
        Some(dir) => read_pairs(dir),
        None => read_shell(),
    };
    match found {
        Ok(pairs) => {
            saw_full(!pairs.is_empty());
            let mut entries = Vec::with_capacity(pairs.len());
            let mut items = Vec::with_capacity(pairs.len());
            let mut total = 0u64;
            for (e, item) in pairs {
                total = total.saturating_add(e.size);
                entries.push(e);
                items.push(item);
            }
            Folder { path: LOCATION.to_string(), entries, bin: Some(Arc::new(Listing { items, total })), ..Default::default() }
        }
        Err(error) => Folder { path: LOCATION.to_string(), error: Some(error), ..Default::default() },
    }
}

/// A list entry for a deleted item: its original name, the date deleted as its date.
fn entry_for(name: &str, is_dir: bool, size: u64, deleted: u64, attributes: u32, real: Option<&str>) -> Entry {
    let ext = if is_dir { String::new() } else { extension(name) };
    Entry {
        name: name.to_string(),
        lower: name.to_lowercase(),
        kind: if is_dir { EntryKind::Folder } else { EntryKind::File },
        size,
        modified: deleted,
        created: 0,
        attributes,
        type_name: icons::type_name(&ext, is_dir),
        ext,
        target: real.map(str::to_string),
        free: 0,
    }
}

/// The `$I…` file that belongs to a `$R…` item (None if `real` isn't one).
fn info_path_for(real: &str) -> Option<String> {
    let name = file_name(real);
    let rest = name.get(..2).filter(|p| p.eq_ignore_ascii_case("$r")).and_then(|_| name.get(2..))?;
    Some(join(&parent(real)?, &format!("$I{rest}")))
}

/// An item and its `$I…` file read from the disk: (entry, item), or None when either is missing or
/// the `$I…` file makes no sense.
fn read_pair(real: &str) -> Option<(Entry, BinItem)> {
    let info_path = info_path_for(real)?;
    let attributes = attributes(real)?;
    let info = parse_info(&std::fs::read(&info_path).ok()?)?;
    let name = original_name(&info.original);
    let is_dir = attributes & ATTR_DIRECTORY != 0;
    let location = parent(&info.original).unwrap_or_default();
    let e = entry_for(&name, is_dir, info.size, info.deleted, attributes, Some(real));
    Some((e, BinItem { original: info.original, location, real: Some(real.to_string()), info: Some(info_path), deleted: info.deleted }))
}

/// The name of the original item ("notes.txt"; a whole drive's root as it is).
fn original_name(original: &str) -> String {
    let name = file_name(original);
    if name.is_empty() { original.to_string() } else { name.to_string() }
}

/// The pretend bin (`GLINT_TEST_BIN`): every `$R…` item in `dir` that has its `$I…` file.
fn read_pairs(dir: &str) -> Result<Vec<(Entry, BinItem)>, String> {
    let list = std::fs::read_dir(dir).map_err(|e| format!("The test Recycle Bin ({dir}) can't be read: {e}"))?;
    let mut out = Vec::new();
    for item in list.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.get(..2).is_some_and(|p| p.eq_ignore_ascii_case("$r")) {
            if let Some(pair) = read_pair(&join(dir, &name)) {
                out.push(pair);
            }
        }
    }
    Ok(out)
}

/// An id list Windows made with CoTaskMemAlloc, freed when dropped.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Windows allocated it with CoTaskMemAlloc, and nobody else frees it.
            unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
        }
    }
}

/// The bin's shell folder (File Explorer's view of it).
struct BinFolder {
    pidl: Pidl,
    folder: IShellFolder,
    folder2: Option<IShellFolder2>,
}

impl BinFolder {
    fn open() -> Option<BinFolder> {
        // SAFETY: COM calls on this thread (which has COM started); the id list is owned by `Pidl`.
        unsafe {
            let pidl = Pidl(SHGetKnownFolderIDList(&FOLDERID_RecycleBinFolder, 0, None).ok()?);
            let desktop = SHGetDesktopFolder().ok()?;
            let folder: IShellFolder = desktop.BindToObject(pidl.0, None).ok()?;
            let folder2 = folder.cast::<IShellFolder2>().ok();
            Some(BinFolder { pidl, folder, folder2 })
        }
    }

    /// The text Windows gives for an item (`flags`: SHGDN_...).
    fn name_of(&self, child: *const ITEMIDLIST, flags: SHGDNF) -> Option<String> {
        let mut sr = STRRET::default();
        // SAFETY: `child` is a valid id list of this folder; the string is freed after copying.
        unsafe {
            self.folder.GetDisplayNameOf(child, flags, &mut sr).ok()?;
            strret_text(&mut sr, child)
        }
    }

    /// The column of the shell's own details view that shows `key` (Wine's bin answers only those).
    fn column_of(&self, key: &PROPERTYKEY) -> Option<u32> {
        let f2 = self.folder2.as_ref()?;
        (0..16u32).find(|&col| {
            let mut k = PROPERTYKEY::default();
            // SAFETY: a valid out-pointer; unknown columns just fail.
            unsafe { f2.MapColumnToSCID(col, &mut k).is_ok() && k.fmtid == key.fmtid && k.pid == key.pid }
        })
    }

    /// The text of the shell's details column `col` for an item.
    fn detail_text(&self, child: *const ITEMIDLIST, col: u32) -> Option<String> {
        let f2 = self.folder2.as_ref()?;
        let mut d = SHELLDETAILS::default();
        // SAFETY: `child` is a valid id list of this folder; SHELLDETAILS is packed, so its STRRET
        // is copied out before use.
        unsafe {
            f2.GetDetailsOf(child, col, &mut d).ok()?;
            let mut sr = d.str;
            strret_text(&mut sr, child)
        }
    }

    fn is_folder(&self, child: *const ITEMIDLIST) -> bool {
        let mut attrs = 0x2000_0000u32; // SFGAO_FOLDER
        // SAFETY: one valid child id list and a valid in/out value.
        unsafe { self.folder.GetAttributesOf(&[child], &mut attrs).is_ok() && attrs & 0x2000_0000 != 0 }
    }
}

/// A STRRET's text (and the memory it may own freed).
///
/// # Safety
/// `sr` must come from the folder that `child` belongs to.
unsafe fn strret_text(sr: &mut STRRET, child: *const ITEMIDLIST) -> Option<String> {
    let mut out = PWSTR::null();
    // SAFETY: as the caller promises; the returned string is copied, then freed with CoTaskMemFree.
    unsafe {
        StrRetToStrW(sr, Some(child), &mut out).ok()?;
        let s = out.to_string().ok();
        CoTaskMemFree(Some(out.0 as *const _));
        s
    }
}

/// A shell string (CoTaskMem) as a String, freed.
fn take_string(p: PWSTR) -> Option<String> {
    // SAFETY: `p` came from a shell call that hands its string over; it is freed once, here.
    unsafe {
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// The real Recycle Bin, as Windows lists it.
fn read_shell() -> Result<Vec<(Entry, BinItem)>, String> {
    let bin = BinFolder::open().ok_or_else(|| "Windows couldn't open the Recycle Bin.".to_string())?;
    let mut list: Option<IEnumIDList> = None;
    let flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0 | SHCONTF_INCLUDEHIDDEN.0) as u32;
    // SAFETY: a valid out-pointer; an empty bin may answer S_FALSE with no list at all.
    let hr = unsafe { bin.folder.EnumObjects(HWND::default(), flags, &mut list) };
    if hr.is_err() {
        return Err(format!("Windows couldn't list the Recycle Bin: {}", fileops::explain(hr)));
    }
    let Some(list) = list else { return Ok(Vec::new()) };
    let from_column = bin.column_of(&DISPLACED_FROM);
    let mut out = Vec::new();
    while out.len() < MAX_ITEMS {
        let mut one = [std::ptr::null_mut()];
        let mut got = 0u32;
        // SAFETY: room for one id list, which we then own (freed by `Pidl`).
        let hr = unsafe { list.Next(&mut one, Some(&mut got)) };
        if hr.is_err() || hr.0 == 1 || got == 0 || one[0].is_null() {
            break;
        }
        let child = Pidl(one[0]);
        // On Windows: the path of its $R… item, whose $I… file says the rest exactly.
        let parsing = bin.name_of(child.0, SHGDN_FORPARSING);
        if let Some(pair) = parsing.as_deref().and_then(read_pair) {
            out.push(pair);
            continue;
        }
        out.push(from_shell(&bin, child.0, parsing.as_deref(), from_column));
    }
    Ok(out)
}

/// An item described by Windows' own details (no $R…/$I… pair to read).
fn from_shell(bin: &BinFolder, child: *const ITEMIDLIST, parsing: Option<&str>, from_column: Option<u32>) -> (Entry, BinItem) {
    let name = bin.name_of(child, SHGDN_INFOLDER).unwrap_or_default();
    let (mut location, mut deleted, mut size) = (String::new(), 0u64, 0u64);
    // SAFETY: COM calls on objects of this thread; `child` is a valid id list of the bin.
    let item2 = unsafe { SHCreateItemWithParent::<_, IShellItem2>(Some(bin.pidl.0 as *const _), &bin.folder, child) }.ok();
    if let Some(i2) = &item2 {
        // SAFETY: as above; strings are freed by take_string.
        unsafe {
            location = i2.GetString(&DISPLACED_FROM).ok().and_then(take_string).unwrap_or_default();
            deleted = i2.GetFileTime(&DISPLACED_DATE).map(filetime_u64).unwrap_or(0);
            size = i2.GetUInt64(&SIZE_KEY).unwrap_or(0);
        }
    }
    if location.is_empty() {
        location = from_column.and_then(|c| bin.detail_text(child, c)).unwrap_or_default();
    }
    // A real item on disk Windows named (without its $I… file): it can still be put back.
    let real = parsing.filter(|p| is_full_path(p) && attributes(p).is_some()).map(str::to_string);
    let attrs = real.as_deref().and_then(attributes).unwrap_or(if bin.is_folder(child) { ATTR_DIRECTORY } else { 0 });
    let is_dir = attrs & ATTR_DIRECTORY != 0;
    let original = if location.is_empty() || name.is_empty() { String::new() } else { join(&location, &name) };
    let e = entry_for(&name, is_dir, size, deleted, attrs, real.as_deref());
    (e, BinItem { original, location, real, info: None, deleted })
}

fn filetime_u64(f: FILETIME) -> u64 {
    ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64
}

/// "C:\..." or "\\server\share\...".
fn is_full_path(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with("\\\\") || (b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\'))
}

// ---------------------------------------------------------------- the $I… files

/// What an `$I…` file says.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    /// The full path the item was deleted from.
    pub original: String,
    /// Its size in bytes (a folder: everything in it).
    pub size: u64,
    /// When it was deleted, as a FILETIME.
    pub deleted: u64,
}

/// Reads an `$I…` file. Version 1 (Windows Vista to 8) has a fixed 260-character path, version 2
/// (Windows 10 and 11) its length first.
pub fn parse_info(b: &[u8]) -> Option<Info> {
    let u64_at = |at: usize| b.get(at..at + 8).and_then(|s| s.try_into().ok()).map(u64::from_le_bytes);
    let (version, size, deleted) = (u64_at(0)?, u64_at(8)?, u64_at(16)?);
    let text = if version >= 2 {
        let chars = u32::from_le_bytes(b.get(24..28)?.try_into().ok()?) as usize;
        b.get(28..(28 + chars.saturating_mul(2)).min(b.len()))?
    } else {
        b.get(24..(24 + 520).min(b.len()))?
    };
    let units: Vec<u16> = text.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    let original = String::from_utf16_lossy(&units);
    (!original.is_empty()).then_some(Info { original, size, deleted })
}

/// An `$I…` file in the Windows 10/11 format (the tests make pretend bins with it).
#[cfg(test)]
pub fn info_bytes(original: &str, size: u64, deleted: u64) -> Vec<u8> {
    let path: Vec<u16> = original.encode_utf16().chain([0]).collect();
    let mut out = Vec::with_capacity(28 + path.len() * 2);
    out.extend(2u64.to_le_bytes());
    out.extend(size.to_le_bytes());
    out.extend(deleted.to_le_bytes());
    out.extend((path.len() as u32).to_le_bytes());
    out.extend(path.iter().flat_map(|u| u.to_le_bytes()));
    out
}

// ---------------------------------------------------------------- what can be touched

/// Whether `path` is something Glint may move out of or delete from the bin: a `$R…` / `$I…` item
/// (by `prefix`) directly inside a `$Recycle.Bin` folder - or inside the test bin. A belt-and-braces
/// check, so that no mix-up anywhere can ever make "Delete permanently" touch anything else.
fn in_storage(path: &str, prefix: &str, test: Option<&str>) -> bool {
    let name = file_name(path);
    if !name.get(..2).is_some_and(|p| p.eq_ignore_ascii_case(prefix)) || name.len() <= 2 || !is_full_path(path) {
        return false;
    }
    let Some(dir) = parent(path) else { return false };
    match test {
        Some(t) => same_path(&dir, t),
        None => parent(&dir).is_some_and(|bin| file_name(&bin).eq_ignore_ascii_case("$Recycle.Bin")),
    }
}

/// The item's `$R…` path, if Glint may touch it.
fn real_of<'a>(item: &'a BinItem, test: Option<&str>) -> Option<&'a str> {
    item.real.as_deref().filter(|r| in_storage(r, "$r", test))
}

/// The item's `$I…` path, if Glint may touch it.
fn info_of<'a>(item: &'a BinItem, test: Option<&str>) -> Option<&'a str> {
    item.info.as_deref().filter(|i| in_storage(i, "$i", test))
}

/// Whether the item is still the one that was listed: its `$I…` file still says it was deleted from
/// the same place at the same time. The listing on screen can be a while old, and meanwhile the
/// item may have been restored or deleted elsewhere (File Explorer, another Glint) - and its `$R…`
/// name given to something deleted later. A restore or "Delete permanently" picked from that
/// listing must then do nothing, rather than act on whatever has that name now. (An item listed
/// from Windows' own details, without an `$I…` file, has nothing to compare.)
fn still_listed(item: &BinItem, test: Option<&str>) -> bool {
    if item.info.is_none() {
        return true;
    }
    let Some(now) = info_of(item, test).and_then(|i| std::fs::read(i).ok()).and_then(|b| parse_info(&b)) else { return false };
    now.original == item.original && (item.deleted == 0 || now.deleted == item.deleted)
}

/// What a restore or delete says about an item that isn't the one listed any more.
const NOT_LISTED: &str = "it changed in the Recycle Bin since it was listed (press F5 and try again)";

// ---------------------------------------------------------------- jobs

/// Something to do in the bin, run on a file-job thread (fileops.rs).
#[derive(Clone, Debug)]
pub enum BinJob {
    /// Put these back where they came from.
    Restore(Vec<BinItem>),
    /// Delete these for good (Glint asked first).
    Delete(Vec<BinItem>),
    /// Empty the whole bin. Windows asks first (the owner window is its parent); the test bin is
    /// emptied after Glint's own question.
    Empty { owner: isize },
}

/// Does a bin job. Call it on a thread with COM started (fileops.rs's job threads are).
pub fn run(job: BinJob) -> Outcome {
    let test = test_dir();
    let mut out = match job {
        BinJob::Restore(items) => restore(&items, test),
        BinJob::Delete(items) => delete(&items, test),
        BinJob::Empty { owner } => empty(HWND(owner as *mut _), test),
    };
    if !out.changed.iter().any(|c| is_bin(c)) {
        out.changed.insert(0, LOCATION.to_string());
    }
    match test {
        Some(dir) => saw_full(read_pairs(dir).is_ok_and(|p| !p.is_empty())),
        None => {
            update_bin_icon();
            if let Some(n) = count() {
                saw_full(n > 0);
            }
        }
    }
    out
}

/// Puts back the items last deleted from these full paths (Ctrl+Z after a delete, Ctrl+Y after
/// undoing a copy or a new folder): for each path, the newest item in the bin that came from there.
/// Restored items are reported as made, so the list selects them. An item whose name is taken now
/// stays in the bin (not put back with a number, as the bin's own Restore does): the undo step
/// would then point at the other item.
pub fn restore_paths(paths: &[String]) -> Outcome {
    let test = test_dir();
    let listed = match test {
        Some(dir) => read_pairs(dir),
        None => read_shell(),
    };
    let pairs = match listed {
        Ok(p) => p,
        Err(why) => return Outcome { note: Some(why), ..Default::default() },
    };
    let mut items: Vec<BinItem> = Vec::new();
    let mut missing = 0;
    let mut taken: Vec<String> = Vec::new();
    for p in paths {
        if exists(p) {
            taken.push(p.clone());
            continue;
        }
        match pairs.iter().map(|(_, item)| item).filter(|item| same_path(&item.original, p)).max_by_key(|item| item.deleted) {
            Some(item) => items.push(item.clone()),
            None => missing += 1,
        }
    }
    let mut out = run(BinJob::Restore(items));
    if missing > 0 {
        out.ok = false;
        out.note = Some(if missing == paths.len() {
            "It isn't in the Recycle Bin any more.".to_string()
        } else {
            format!("{} of them aren't in the Recycle Bin any more.", format_count(missing))
        });
    }
    if !taken.is_empty() {
        out.ok = false;
        out.note = Some(format!("{} stayed in the Recycle Bin: something with that name is there now.", fileops::items_text(&taken)));
    }
    // Restored into one folder (nearly always): select them there.
    if let [dir] = out.changed.iter().filter(|c| !is_bin(c)).collect::<Vec<_>>().as_slice() {
        let names = paths
            .iter()
            .filter(|p| !taken.contains(p) && parent(p).is_some_and(|d| same_path(&d, dir)))
            .map(|p| file_name(p).to_string())
            .collect();
        out.created = Some((dir.to_string(), names));
    }
    out
}

/// Whether the bin held anything when Glint last looked (0: not looked yet, 1: empty, 2: full).
static FULL: AtomicU8 = AtomicU8::new(0);

/// Notes whether the bin holds anything (a listing, a job, something sent to it).
pub fn saw_full(full: bool) {
    FULL.store(if full { 2 } else { 1 }, Ordering::SeqCst);
}

/// Whether the bin held anything when Glint last looked (None: not looked yet). The sidebar shows
/// its full or empty icon by it.
pub fn seen_full() -> Option<bool> {
    match FULL.load(Ordering::SeqCst) {
        1 => Some(false),
        2 => Some(true),
        _ => None,
    }
}

/// Puts `items` back. Nothing is ever replaced: a name that's taken now gets a number.
fn restore(items: &[BinItem], test: Option<&str>) -> Outcome {
    let mut restored: Vec<String> = Vec::new();
    let mut renamed = 0;
    let mut problems: Vec<String> = Vec::new();
    let mut changed: Vec<String> = Vec::new();
    for item in items {
        let shown = original_name(&item.original);
        let Some(real) = real_of(item, test) else {
            problems.push(format!("\u{201c}{}\u{201d}: Windows didn't say where it's kept", if shown.is_empty() { "?" } else { &shown }));
            continue;
        };
        if !is_full_path(&item.original) || parent(&item.original).is_none_or(|d| d.is_empty()) {
            problems.push(format!("\u{201c}{shown}\u{201d}: Windows didn't say where it came from"));
            continue;
        }
        if !still_listed(item, test) {
            problems.push(format!("\u{201c}{shown}\u{201d}: {NOT_LISTED}"));
            continue;
        }
        match restore_one(real, &item.original) {
            Ok(target) => {
                if let Some(info) = info_of(item, test) {
                    let _ = std::fs::remove_file(info);
                }
                renamed += usize::from(!same_path(&target, &item.original));
                let dir = parent(&target).unwrap_or_default();
                if !changed.iter().any(|c| same_path(c, &dir)) {
                    changed.push(dir);
                }
                if test.is_none() {
                    notify_moved(real, &target);
                }
                restored.push(target);
            }
            Err(why) => problems.push(format!("\u{201c}{shown}\u{201d}: {why}")),
        }
    }
    let note = match (restored.as_slice(), problems.first()) {
        (_, Some(first)) => {
            let more = problems.len() - 1;
            Some(if more > 0 { format!("Couldn't restore {first} (and {} more)", format_count(more)) } else { format!("Couldn't restore {first}") })
        }
        ([one], None) if renamed > 0 => Some(format!("Restored as \u{201c}{}\u{201d} (the name was taken)", file_name(one))),
        ([one], None) => Some(format!("Restored \u{201c}{}\u{201d} to {}", file_name(one), parent(one).unwrap_or_default())),
        ([], None) => None,
        (many, None) if renamed > 0 => Some(format!("Restored {} items ({} with a number added: the name was taken)", format_count(many.len()), format_count(renamed))),
        (many, None) => Some(format!("Restored {} items", format_count(many.len()))),
    };
    Outcome { ok: problems.is_empty(), changed, note, ..Default::default() }
}

/// Moves one `$R…` item back to `original`, or next to it with a number when the name is taken.
/// Returns where it went.
fn restore_one(real: &str, original: &str) -> Result<String, String> {
    let dir = parent(original).unwrap_or_default();
    let name = original_name(original);
    // The folder it came from may be gone (deleted later): make it again, like Explorer.
    if !exists(&dir) {
        std::fs::create_dir_all(&dir).map_err(|e| format!("its folder can't be made again ({e})"))?;
    }
    let is_dir = attributes(real).ok_or("it isn't in the Recycle Bin any more")? & ATTR_DIRECTORY != 0;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => name.split_at(i),
        _ => (name.as_str(), ""),
    };
    // A name that is free now can be taken a moment later: then try the next number. The move
    // itself never replaces anything.
    for _ in 0..20 {
        let free = unique_name(stem, ext, |n| exists(&join(&dir, n)));
        let target = join(&dir, &free);
        match move_item(real, &target, is_dir) {
            Ok(()) => return Ok(target),
            Err(80 | 183) => continue, // ERROR_FILE_EXISTS, ERROR_ALREADY_EXISTS
            Err(code) => return Err(fileops::explain(windows::core::HRESULT::from_win32(code))),
        }
    }
    Err("no free name was found for it".to_string())
}

/// Moves `from` to `to` without ever replacing anything there. Another drive (a mounted folder)
/// works for files; a folder can only move on its own drive. Err: the Windows error number.
fn move_item(from: &str, to: &str, is_dir: bool) -> Result<(), u32> {
    let (f, t) = (wide_path(from), wide_path(to));
    let attempt = |flags: MOVE_FILE_FLAGS| {
        // SAFETY: both paths are NUL-terminated and outlive the call. Without
        // MOVEFILE_REPLACE_EXISTING an item already at `to` makes this fail instead.
        unsafe { MoveFileExW(PCWSTR(f.as_ptr()), PCWSTR(t.as_ptr()), flags) }.map_err(|e| e.code().0 as u32 & 0xFFFF)
    };
    match attempt(MOVE_FILE_FLAGS(0)) {
        Err(17) if !is_dir => attempt(MOVEFILE_COPY_ALLOWED), // ERROR_NOT_SAME_DEVICE
        other => other,
    }
}

/// Deletes `items` for good - each `$R…` item, then its `$I…` file - with plain file-system calls,
/// so exactly these and nothing else go.
fn delete(items: &[BinItem], test: Option<&str>) -> Outcome {
    let mut gone: Vec<String> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    for item in items {
        let name = original_name(&item.original);
        let Some(real) = real_of(item, test) else {
            problems.push(format!("\u{201c}{name}\u{201d}: Windows didn't say where it's kept"));
            continue;
        };
        if !still_listed(item, test) {
            problems.push(format!("\u{201c}{name}\u{201d}: {NOT_LISTED}"));
            continue;
        }
        match delete_for_good(real) {
            Ok(()) => {
                if let Some(info) = info_of(item, test) {
                    let _ = std::fs::remove_file(info);
                }
                gone.push(name);
            }
            Err(e) => problems.push(format!("\u{201c}{name}\u{201d}: {}", io_problem(&e))),
        }
    }
    let note = match (gone.as_slice(), problems.first()) {
        (_, Some(first)) => {
            let more = problems.len() - 1;
            Some(if more > 0 { format!("Couldn't delete {first} (and {} more)", format_count(more)) } else { format!("Couldn't delete {first}") })
        }
        ([], None) => None,
        ([one], None) => Some(format!("\u{201c}{one}\u{201d} deleted permanently")),
        (many, None) => Some(format!("{} items deleted permanently", format_count(many.len()))),
    };
    Outcome { ok: problems.is_empty(), changed: vec![LOCATION.to_string()], note, ..Default::default() }
}

/// Deletes a file, or a folder with everything in it, for good. Links inside a folder (junctions,
/// symbolic links) are removed themselves, never followed; read-only files don't stop it.
fn delete_for_good(path: &str) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        // A link itself (to a folder it's removed like a folder, to a file like a file).
        return std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path));
    }
    if meta.is_dir() {
        return std::fs::remove_dir_all(path).or_else(|_| {
            clear_read_only(std::path::Path::new(path));
            std::fs::remove_dir_all(path)
        });
    }
    if meta.permissions().readonly() {
        writable(path);
    }
    std::fs::remove_file(path)
}

/// Takes the read-only attribute off one item (Windows' own attribute call: nothing else changes).
fn writable(path: &str) {
    if let Some(a) = attributes(path) {
        let w = wide_path(path);
        // SAFETY: `w` is NUL-terminated and outlives the call.
        unsafe {
            let _ = SetFileAttributesW(PCWSTR(w.as_ptr()), FILE_FLAGS_AND_ATTRIBUTES(a & !FILE_ATTRIBUTE_READONLY.0));
        }
    }
}

/// Takes "read-only" off everything inside a folder (not following links), so it can be deleted.
fn clear_read_only(dir: &std::path::Path) {
    let Ok(list) = std::fs::read_dir(dir) else { return };
    for item in list.flatten() {
        let Ok(meta) = std::fs::symlink_metadata(item.path()) else { continue };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            clear_read_only(&item.path());
        }
        if meta.permissions().readonly() {
            writable(&item.path().to_string_lossy());
        }
    }
}

/// A file-system error in plain words.
fn io_problem(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => fileops::explain(windows::core::HRESULT::from_win32(code as u32)),
        None => e.to_string(),
    }
}

/// Only one "Empty Recycle Bin" at a time (a second click while Windows asks would ask twice).
static EMPTYING: AtomicBool = AtomicBool::new(false);

/// Empties the whole bin: Windows' own function, which asks first, shows its progress and updates
/// the desktop icon. The test bin is emptied directly (Glint asked).
fn empty(owner: HWND, test: Option<&str>) -> Outcome {
    if EMPTYING.swap(true, Ordering::SeqCst) {
        return note("The Recycle Bin is being emptied already.");
    }
    let out = match test {
        Some(dir) => {
            let items: Vec<BinItem> = read_pairs(dir).map(|p| p.into_iter().map(|(_, i)| i).collect()).unwrap_or_default();
            let mut out = delete(&items, test);
            if out.ok {
                out.note = Some("The Recycle Bin is empty".to_string());
            }
            out
        }
        None => {
            // SAFETY: a plain call; `owner` is our window (or null). Windows shows its question and
            // progress window and waits here until they're done.
            let result = unsafe { SHEmptyRecycleBinW(Some(owner), PCWSTR::null(), 0) };
            let left = count();
            log::line(&format!("empty the Recycle Bin: {:?}, {:?} items left", result.as_ref().map_err(|e| e.code()), left));
            match left {
                Some(0) => Outcome { ok: true, ..note("The Recycle Bin is empty") },
                // Windows' own question was answered with No, or it stopped.
                _ => Outcome::default(),
            }
        }
    };
    EMPTYING.store(false, Ordering::SeqCst);
    out
}

/// An outcome that only says something.
fn note(text: &str) -> Outcome {
    Outcome { note: Some(text.to_string()), ..Default::default() }
}

/// How many items the bin holds (None when Windows doesn't say).
fn count() -> Option<i64> {
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: a valid struct with its size set; null means every drive.
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.ok().map(|_| info.i64NumItems)
}

/// Tells Explorer windows that an item moved out of the bin (so a window showing its folder
/// shows it at once).
fn notify_moved(from: &str, to: &str) {
    let is_dir = attributes(to).is_some_and(|a| a & ATTR_DIRECTORY != 0);
    let (f, t) = (wide(from), wide(to));
    let (gone, made): (SHCNE_ID, SHCNE_ID) = if is_dir { (SHCNE_RMDIR, SHCNE_MKDIR) } else { (SHCNE_DELETE, SHCNE_CREATE) };
    // SAFETY: NUL-terminated paths that outlive the calls; FLUSHNOWAIT doesn't wait for anyone.
    unsafe {
        SHChangeNotify(gone, SHCNF_PATHW | SHCNF_FLUSHNOWAIT, Some(f.as_ptr() as *const _), None);
        SHChangeNotify(made, SHCNF_PATHW | SHCNF_FLUSHNOWAIT, Some(t.as_ptr() as *const _), None);
    }
}

/// Lets the desktop's Recycle Bin icon show full or empty again (Windows' SHUpdateRecycleBinIcon,
/// looked up by name: not every import library has it).
fn update_bin_icon() {
    // SAFETY: shell32 is loaded (Glint uses it all the time); the function takes and returns nothing.
    unsafe {
        let Ok(shell32) = GetModuleHandleW(w!("shell32.dll")) else { return };
        if let Some(f) = GetProcAddress(shell32, windows::core::s!("SHUpdateRecycleBinIcon")) {
            let f: unsafe extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

// ---------------------------------------------------------------- noticing changes

/// The message Windows posts to the watch's window when the bin changed.
const WM_BIN_CHANGED: u32 = WM_APP + 30;
/// Changes are told once things have been quiet this long (deleting 1,000 files is one report).
const QUIET_MS: u32 = 300;
const QUIET_TIMER: usize = 1;

/// While a tab shows the bin: Windows tells a thread of Glint's when the bin changes (something
/// deleted, restored or emptied anywhere - in Explorer too), and `on_change` is called (on that
/// thread) once things have been quiet for a moment. The thread sleeps in GetMessage in between.
/// Dropping the watch ends it.
pub struct Watch {
    shared: Arc<WatchShared>,
}

/// What the watch and its thread share: the thread's id (0 until its message queue is there) and
/// whether it should end.
#[derive(Default)]
struct WatchShared {
    thread: AtomicU32,
    stop: AtomicBool,
}

impl Watch {
    /// None when no thread could be had (the bin is then read again only on F5 and after Glint's
    /// own changes). Doesn't wait for the thread.
    pub fn start(on_change: impl Fn() + Send + 'static) -> Option<Watch> {
        if test_dir().is_some() {
            return None; // the pretend bin: Glint's own changes re-read it anyway
        }
        let shared = Arc::new(WatchShared::default());
        let theirs = shared.clone();
        let started = crate::shell::background("Recycle Bin watch", move || {
            let _com = crate::shell::ComGuard::init();
            let mut msg = MSG::default();
            // SAFETY: makes this thread's message queue (so WM_QUIT can be posted to it), then
            // tells its id; both plain calls about this thread.
            unsafe {
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                theirs.thread.store(GetCurrentThreadId(), Ordering::SeqCst);
            }
            // Dropped before the queue was there: nobody could tell it to end, so it ends here.
            if !theirs.stop.load(Ordering::SeqCst) {
                watch_thread(on_change);
            }
        });
        started.then_some(Watch { shared })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let thread = self.shared.thread.load(Ordering::SeqCst);
        if thread != 0 {
            // SAFETY: posting to a thread with a message queue; if it has ended already this just fails.
            unsafe {
                let _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

unsafe extern "system" fn watch_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the default handling for a window of ours.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn watch_thread(on_change: impl Fn()) {
    // SAFETY: (the whole body) a message-only window made, registered for the bin's changes, and
    // destroyed on this thread; the bin's id list lives until the registration is gone.
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(watch_wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("GlintRecycleBinWatch"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let Ok(hwnd) =
            CreateWindowExW(WINDOW_EX_STYLE(0), w!("GlintRecycleBinWatch"), w!(""), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None)
        else {
            log::line("Recycle Bin watch: no window");
            return;
        };
        let Ok(pidl) = SHGetKnownFolderIDList(&FOLDERID_RecycleBinFolder, 0, None).map(Pidl) else {
            let _ = DestroyWindow(hwnd);
            return;
        };
        let entry = SHChangeNotifyEntry { pidl: pidl.0, fRecursive: true.into() };
        let mut id = SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_InterruptLevel | SHCNRF_NewDelivery, SHCNE_ALLEVENTS.0 as i32, WM_BIN_CHANGED, 1, &entry);
        if id == 0 {
            id = SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_NewDelivery, SHCNE_ALLEVENTS.0 as i32, WM_BIN_CHANGED, 1, &entry);
        }
        if id == 0 {
            log::line("Recycle Bin watch: Windows wouldn't tell about changes");
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd == hwnd && msg.message == WM_BIN_CHANGED {
                // New delivery: the details sit in shared memory that must be locked and let go.
                let lock = SHChangeNotification_Lock(HANDLE(msg.wParam.0 as *mut _), msg.lParam.0 as u32, None, None);
                if !lock.is_invalid() {
                    let _ = SHChangeNotification_Unlock(lock);
                }
                SetTimer(Some(hwnd), QUIET_TIMER, QUIET_MS, None);
                continue;
            }
            if msg.hwnd == hwnd && msg.message == WM_TIMER && msg.wParam.0 == QUIET_TIMER {
                let _ = KillTimer(Some(hwnd), QUIET_TIMER);
                on_change();
                continue;
            }
            DispatchMessageW(&msg);
        }
        if id != 0 {
            let _ = SHChangeNotifyDeregister(id);
        }
        let _ = DestroyWindow(hwnd);
        drop(pidl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder;

    #[test]
    fn typed_names_of_the_bin() {
        assert!(is_bin(LOCATION) && is_bin(&LOCATION.to_lowercase()));
        assert!(!is_bin("C:\\Recycle Bin"));
        for typed in ["recycle bin", " Recycle Bin ", "shell:RecycleBinFolder", "::{645ff040-5081-101b-9f08-00aa002f954e}", "shell:::{645FF040-5081-101B-9F08-00AA002F954E}"] {
            assert!(means_bin(typed), "{typed}");
        }
        assert!(!means_bin("C:\\$Recycle.Bin") && !means_bin("Recycle"));
        assert_eq!(folder::normalize("Recycle Bin"), LOCATION);
        assert_eq!(folder::resolve("shell:RecycleBinFolder", "C:\\Users"), LOCATION);
        assert_eq!(folder::parent(LOCATION).as_deref(), Some(""), "up from the bin is This PC");
    }

    #[test]
    fn info_files_both_versions() {
        let v2 = info_bytes("C:\\Users\\me\\Desktop\\notes.txt", 1234, 0x01DA_0000_0000_0000);
        assert_eq!(parse_info(&v2), Some(Info { original: "C:\\Users\\me\\Desktop\\notes.txt".into(), size: 1234, deleted: 0x01DA_0000_0000_0000 }));
        // Version 1: the path in a fixed 520-byte field after the date.
        let mut v1 = Vec::new();
        v1.extend(1u64.to_le_bytes());
        v1.extend(7u64.to_le_bytes());
        v1.extend(9u64.to_le_bytes());
        let mut path: Vec<u8> = "D:\\Ü\\a.bin".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        path.resize(520, 0);
        v1.extend(path);
        assert_eq!(parse_info(&v1), Some(Info { original: "D:\\Ü\\a.bin".into(), size: 7, deleted: 9 }));
        // Too short, or no path: nothing (never a panic).
        assert_eq!(parse_info(&v2[..20]), None);
        assert_eq!(parse_info(&info_bytes("", 1, 1)), None);
        let mut lying = info_bytes("C:\\x", 1, 1);
        lying[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_info(&lying).map(|i| i.original), Some("C:\\x".into()), "a length past the end is cut at the end");
    }

    #[test]
    fn only_bin_items_can_be_touched() {
        let r = "C:\\$Recycle.Bin\\S-1-5-21-1\\$RAB12CD.txt";
        assert_eq!(info_path_for(r).as_deref(), Some("C:\\$Recycle.Bin\\S-1-5-21-1\\$IAB12CD.txt"));
        assert!(in_storage(r, "$r", None));
        assert!(in_storage("D:\\$RECYCLE.BIN\\S-1\\$rX", "$r", None), "any case");
        assert!(in_storage("C:\\$Recycle.Bin\\S-1\\$IAB12CD.txt", "$i", None));
        assert!(!in_storage("C:\\$Recycle.Bin\\S-1\\$IAB12CD.txt", "$r", None), "the info file isn't the item");
        assert!(!in_storage("C:\\Users\\me\\$Rnotes.txt", "$r", None), "not in a bin");
        assert!(!in_storage("C:\\$Recycle.Bin\\S-1\\notes.txt", "$r", None), "not a $R item");
        assert!(!in_storage("C:\\$Recycle.Bin\\S-1\\deep\\$RX", "$r", None), "not directly in the user's bin");
        assert!(!in_storage("C:\\$Recycle.Bin\\S-1\\$R", "$r", None));
        assert!(!in_storage("$RX", "$r", None), "not a full path");
        assert!(in_storage("Z:\\tmp\\bin\\$RX", "$r", Some("Z:\\tmp\\bin")));
        assert!(!in_storage("Z:\\tmp\\other\\$RX", "$r", Some("Z:\\tmp\\bin")));
        let item = BinItem { real: Some("C:\\Users\\me\\a.txt".into()), ..Default::default() };
        assert_eq!(real_of(&item, None), None);
    }

    /// A pretend bin in a fresh temporary folder, with the pairs for `items` (name, original).
    fn pretend_bin(tag: &str, items: &[(&str, &str, bool)]) -> (String, Vec<BinItem>) {
        let root = std::env::temp_dir().join(format!("glint-bin-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bin_s = bin.to_string_lossy().to_string();
        let mut out = Vec::new();
        for (i, (_, original, is_dir)) in items.iter().enumerate() {
            let real = join(&bin_s, &format!("$R{i:06}"));
            if *is_dir {
                std::fs::create_dir_all(join(&real, "inner")).unwrap();
                std::fs::write(join(&real, "inner\\f.txt"), "x").unwrap();
            } else {
                std::fs::write(&real, format!("content {i}")).unwrap();
            }
            let info = join(&bin_s, &format!("$I{i:06}"));
            std::fs::write(&info, info_bytes(original, 9, 1)).unwrap();
            out.push(BinItem { original: original.to_string(), location: parent(original).unwrap(), real: Some(real), info: Some(info), deleted: 1 });
        }
        (bin_s, out)
    }

    #[test]
    fn restoring_never_replaces_anything() {
        let root = std::env::temp_dir().join(format!("glint-bin-test-restore-{}", std::process::id()));
        let home = root.join("home");
        let home_s = home.to_string_lossy().to_string();
        let taken = join(&home_s, "taken.txt");
        let gone_dir = join(&home_s, "gone\\deeper");
        let (bin, items) = pretend_bin(
            "restore",
            &[("a", &taken, false), ("b", &join(&gone_dir, "back.txt"), false), ("c", &join(&home_s, "Folder.v1"), true)],
        );
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(&taken, "the one that is there now").unwrap();
        let out = restore(&items, Some(&bin));
        assert!(out.ok, "{:?}", out.note);
        assert_eq!(std::fs::read_to_string(&taken).unwrap(), "the one that is there now", "never replaced");
        assert_eq!(std::fs::read_to_string(join(&home_s, "taken (2).txt")).unwrap(), "content 0", "a number added");
        assert_eq!(std::fs::read_to_string(join(&gone_dir, "back.txt")).unwrap(), "content 1", "its folder made again");
        assert!(exists(&join(&home_s, "Folder.v1\\inner\\f.txt")), "a folder keeps its whole name");
        assert!(read_pairs(&bin).unwrap().is_empty(), "the pairs are gone from the bin");
        assert!(out.changed.iter().any(|c| same_path(c, &home_s)));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(parent(&bin).unwrap());
    }

    #[test]
    fn an_item_that_changed_since_it_was_listed_is_left_alone() {
        let (bin, items) = pretend_bin("stale", &[("a", "C:\\x\\old.txt", false)]);
        let (real, info) = (items[0].real.clone().unwrap(), items[0].info.clone().unwrap());
        // Restored elsewhere meanwhile, and its $R name given to something deleted later.
        std::fs::write(&info, info_bytes("C:\\y\\other.txt", 9, 2)).unwrap();
        assert!(!delete(&items, Some(&bin)).ok);
        assert!(!restore(&items, Some(&bin)).ok);
        assert!(exists(&real) && exists(&info), "neither deleted nor restored");
        // From the same place, but deleted at another time: another item too.
        std::fs::write(&info, info_bytes("C:\\x\\old.txt", 9, 5)).unwrap();
        assert!(!delete(&items, Some(&bin)).ok);
        assert!(exists(&real));
        // Its $I file gone (restored or emptied elsewhere): nothing is done either.
        std::fs::remove_file(&info).unwrap();
        assert!(!delete(&items, Some(&bin)).ok);
        assert!(exists(&real));
        // The listing as it is: deleted.
        std::fs::write(&info, info_bytes("C:\\x\\old.txt", 9, 1)).unwrap();
        assert!(delete(&items, Some(&bin)).ok);
        assert!(!exists(&real) && !exists(&info));
        let _ = std::fs::remove_dir_all(parent(&bin).unwrap());
    }

    #[test]
    fn deleting_for_good_never_follows_links() {
        let (bin, items) = pretend_bin("links", &[("a", "C:\\x\\Folder", true)]);
        let real = items[0].real.clone().unwrap();
        // A read-only file inside doesn't stop it.
        let locked = join(&real, "inner\\locked.txt");
        std::fs::write(&locked, "x").unwrap();
        let mut p = std::fs::metadata(&locked).unwrap().permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&locked, p).unwrap();
        // A link inside to a folder outside the bin: the link goes, what it points to stays.
        let outside = std::env::temp_dir().join(format!("glint-bin-test-target-{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious.txt"), "keep").unwrap();
        let linked = std::os::windows::fs::symlink_dir(&outside, join(&real, "link")).is_ok();
        let out = delete(&items, Some(&bin));
        assert!(out.ok, "{:?}", out.note);
        assert!(!exists(&real), "the whole item is gone");
        assert!(read_pairs(&bin).unwrap().is_empty());
        if linked {
            assert!(outside.join("precious.txt").exists(), "never followed into the link");
        }
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(parent(&bin).unwrap());
    }

    #[test]
    fn reading_and_deleting_a_pretend_bin() {
        let (bin, items) = pretend_bin("delete", &[("a", "C:\\x\\one.txt", false), ("b", "C:\\x\\Dir", true), ("c", "C:\\y\\one.txt", false)]);
        let mut pairs = read_pairs(&bin).unwrap();
        pairs.sort_by(|a, b| a.0.target.cmp(&b.0.target));
        let names: Vec<&str> = pairs.iter().map(|(e, _)| e.name.as_str()).collect();
        assert_eq!(names, vec!["one.txt", "Dir", "one.txt"], "two items of the same name");
        assert_ne!(pairs[0].0.key(), pairs[2].0.key(), "told apart by where they're kept");
        assert_eq!(pairs[1].0.kind, EntryKind::Folder);
        assert_eq!(pairs[2].1.location, "C:\\y");
        // Delete one of the two "one.txt": exactly that one goes.
        let out = delete(&items[2..], Some(&bin));
        assert!(out.ok, "{:?}", out.note);
        let left: Vec<String> = read_pairs(&bin).unwrap().into_iter().map(|(_, i)| i.original).collect();
        assert_eq!(left.len(), 2);
        assert!(left.contains(&"C:\\x\\one.txt".to_string()) && left.contains(&"C:\\x\\Dir".to_string()));
        // An item that claims to live outside the bin is never touched.
        let outside = std::env::temp_dir().join(format!("glint-bin-test-outside-{}.txt", std::process::id()));
        std::fs::write(&outside, "keep me").unwrap();
        let rogue = BinItem { original: "C:\\x\\r.txt".into(), real: Some(outside.to_string_lossy().to_string()), ..Default::default() };
        let out = delete(std::slice::from_ref(&rogue), Some(&bin));
        assert!(!out.ok);
        assert!(outside.exists(), "a path outside the bin is refused");
        assert!(!restore(&[rogue], Some(&bin)).ok);
        assert!(outside.exists());
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(parent(&bin).unwrap());
    }
}
