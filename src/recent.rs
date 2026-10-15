//! Home's items, like File Explorer's: your folders (Desktop, Downloads, Documents...) and the
//! pinned folders, then the files you opened lately, in any program, newest first, then the
//! favorites (settings.json "Favorites"). home.rs lays them out.
//!
//! **Where the files come from.** Windows keeps that list itself: each file opened gets a shortcut in
//! `%APPDATA%\Microsoft\Windows\Recent`, written again each time it's opened. [`read`] follows
//! those shortcuts to the files (folders and files that are gone are left out; files on another
//! computer aren't looked at, as that could take long) and takes each shortcut's time as when the
//! file was last opened. Tabs and the sidebar use [`LOCATION`] (the name Windows gives Explorer's
//! Home) as its path.
//!
//! The entries are the files themselves with their full path as `target` (like search results), so
//! opening, copying, deleting and the right-click menus act on the real file. Their date is when
//! they were opened ("Date accessed"), and the list keeps that order until a column is clicked.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use windows::core::{Interface, PCWSTR};
use windows::Win32::Storage::FileSystem::{FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, GetFileAttributesW, FIND_FIRST_EX_FLAGS, INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, FOLDERID_Recent, SLGP_RAWPATH};

use crate::folder::{self, entry_from, join, wide_path, Entry, EntryKind, Folder};
use crate::fileops::same_path;
use crate::shell::{self, ComGuard};

/// Home's path in Glint (what tabs, the sidebar and settings.json use): Windows' name for
/// Explorer's Home.
pub const LOCATION: &str = "::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}";
/// What it's called on screen.
pub const NAME: &str = "Home";
/// At most this many files (Explorer's list stops near here too).
const MAX_ITEMS: usize = 500;

/// Whether `path` is Home (Glint's own path for it, in any case).
pub fn is_recent(path: &str) -> bool {
    path.eq_ignore_ascii_case(LOCATION)
}

/// Whether something typed into the path box means Home: its path, or just "Home" (what the path
/// box shows there).
pub fn means_home(typed: &str) -> bool {
    let t = typed.trim();
    is_recent(t) || t.eq_ignore_ascii_case(NAME)
}

/// Where each part of Home's entries starts: the standard folders come first (from 0), then the
/// pinned folders, the recent files and the favorites (to the end).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Parts {
    pub pinned: usize,
    pub recent: usize,
    pub favorites: usize,
}

/// The pinned folders and the favorites, handed over by the window before Home is read (the
/// reading thread has no settings).
static EXTRAS: Mutex<(Vec<String>, Vec<String>)> = Mutex::new((Vec::new(), Vec::new()));

/// Sets the pinned folders and the favorites the next read of Home lists.
pub fn set_extras(pinned: &[String], favorites: &[String]) {
    if let Ok(mut x) = EXTRAS.lock() {
        *x = (pinned.to_vec(), favorites.to_vec());
    }
}

/// The standard folders in the order File Explorer's Quick access shows them.
const ORDER: [&str; 6] = ["Desktop", "Downloads", "Documents", "Pictures", "Music", "Videos"];

/// Reads the list (on the folder-reading thread).
pub fn read() -> Folder {
    let mut f = Folder { path: LOCATION.to_string(), ..Default::default() };
    let Some(dir) = std::env::var("GLINT_TEST_RECENT").ok().or_else(|| shell::known_folder(&FOLDERID_Recent)) else {
        f.error = Some("Windows didn't say where its list of recent files is.".into());
        return f;
    };
    let (pinned, favorites) = EXTRAS.lock().map(|x| x.clone()).unwrap_or_default();
    // The folders first, in Quick access's order (not Home itself: that's this page).
    let mut types = HashMap::new();
    if std::env::var("GLINT_TEST_RECENT").is_err() {
        let mut known: Vec<_> = shell::known_folders().into_iter().filter(|k| ORDER.contains(&k.name)).collect();
        known.sort_by_key(|k| ORDER.iter().position(|n| *n == k.name));
        for k in known {
            // (One moved to another computer, such as Documents on a server, isn't asked for either:
            // it could take long.)
            let e = if is_network(&k.path) { Some(unchecked_entry(&k.path, Some(true))) } else { item_entry(&k.path, &mut types, Some(true)) };
            if let Some(mut e) = e {
                e.modified = 0; // (no date, as in Explorer's Quick access)
                f.entries.push(e);
            }
        }
    }
    let mut parts = Parts { pinned: f.entries.len(), ..Default::default() };
    // Then the pinned folders (not a standard one twice). A network folder isn't asked for (it
    // could take long): it shows as it's written.
    for p in &pinned {
        if f.entries.iter().any(|e| e.target.as_deref().is_some_and(|t| same_path(t, p))) {
            continue;
        }
        if let Some(e) = if is_network(p) { Some(unchecked_entry(p, Some(true))) } else { item_entry(p, &mut types, Some(true)) } {
            f.entries.push(e);
        }
    }
    parts.recent = f.entries.len();
    let listing = folder::read(&dir);
    // The shortcuts, newest first.
    let mut links: Vec<&Entry> = listing.entries.iter().filter(|e| e.kind == EntryKind::File && e.ext.eq_ignore_ascii_case("lnk")).collect();
    links.sort_by_key(|e| std::cmp::Reverse(e.modified));
    let _com = ComGuard::init();
    let mut seen = std::collections::HashSet::new();
    for link in links {
        if seen.len() >= MAX_ITEMS {
            break;
        }
        let Some((path, attributes)) = target_of(&join(&dir, &link.name)) else { continue };
        if !seen.insert(path.to_lowercase()) {
            continue;
        }
        // A file on another computer isn't looked at either: the shortcut says if it was a folder.
        let e = if is_network(&path) {
            (attributes & 0x10 == 0).then(|| unchecked_entry(&path, Some(false))) // FILE_ATTRIBUTE_DIRECTORY
        } else {
            item_entry(&path, &mut types, Some(false))
        };
        if let Some(mut e) = e {
            e.modified = link.modified; // when it was opened
            f.entries.push(e);
        }
    }
    // Last the favorites (files and folders), as marked; ones that are gone are left out.
    parts.favorites = f.entries.len();
    for p in &favorites {
        if let Some(e) = if is_network(p) { Some(unchecked_entry(p, None)) } else { item_entry(p, &mut types, None) } {
            f.entries.push(e);
        }
    }
    f.home = Some(Box::new(parts));
    f
}

/// Whether entry `index` of Home's list `f` is a favorite on another computer: whether it's a file
/// or a folder was only guessed from its name (see `read`), so opening it lets the folder read
/// find out (app.rs, `open_entry`).
pub fn guessed(f: &Folder, index: usize) -> bool {
    f.home.as_ref().is_some_and(|p| index >= p.favorites) && f.entries.get(index).and_then(|e| e.target.as_deref()).is_some_and(is_network)
}

/// A path on another computer (\\server\share..., or a network drive such as Z:\): not looked at
/// while Home is read.
fn is_network(path: &str) -> bool {
    path.starts_with("\\\\") || (path.get(1..3) == Some(":\\") && crate::peek::on_network(path))
}

/// A drive's root, such as D:\.
fn is_drive_root(path: &str) -> bool {
    path.len() == 3 && path.ends_with(":\\") && path.as_bytes()[0].is_ascii_alphabetic()
}

/// An entry for a path without asking the disk: a folder with `Some(true)`, a file with
/// `Some(false)`, else a file when its name has an extension.
fn unchecked_entry(path: &str, folder: Option<bool>) -> Entry {
    let name = folder::file_name(path).to_string();
    let ext = if folder == Some(true) { String::new() } else { name.rsplit_once('.').map(|(_, x)| x.to_lowercase()).unwrap_or_default() };
    let kind = if folder.unwrap_or(ext.is_empty()) { EntryKind::Folder } else { EntryKind::File };
    Entry {
        lower: name.to_lowercase(),
        name,
        kind,
        size: 0,
        modified: 0,
        created: 0,
        attributes: 0,
        ext,
        type_name: Arc::from(if kind == EntryKind::Folder { "File folder" } else { "File" }),
        target: Some(path.to_string()),
        free: 0,
    }
}

/// The file a shortcut points at, as written in it, and its attributes as the shortcut recorded
/// them (0 if it didn't).
fn target_of(lnk: &str) -> Option<(String, u32)> {
    // SAFETY: COM calls in this thread's apartment (ComGuard in `read`); the buffers outlive them.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let w = wide_path(lnk);
        file.Load(PCWSTR(w.as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        let mut data = WIN32_FIND_DATAW::default();
        link.GetPath(&mut buf, &mut data, SLGP_RAWPATH.0 as u32).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let path = String::from_utf16_lossy(&buf[..len]);
        let path = expand(&path);
        (!path.is_empty()).then_some((path, data.dwFileAttributes))
    }
}

/// %USERPROFILE% and the like, as Windows writes them in some shortcuts.
fn expand(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else { break };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

/// The entry for a file or folder that's still there (only a folder with `Some(true)`, only a file
/// with `Some(false)`), None otherwise.
fn item_entry(path: &str, types: &mut HashMap<String, Arc<str>>, folder: Option<bool>) -> Option<Entry> {
    let w = wide_path(path);
    // FindFirstFile can't open a drive's root (D:\), so a drive is checked by its attributes.
    if is_drive_root(path) {
        // SAFETY: `w` is NUL-terminated and outlives the call.
        let attrs = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
        return (folder != Some(false) && attrs != INVALID_FILE_ATTRIBUTES).then(|| unchecked_entry(path, Some(true)));
    }
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `w` is NUL-terminated and outlives the call; `data` is a valid WIN32_FIND_DATAW.
    let handle = unsafe { FindFirstFileExW(PCWSTR(w.as_ptr()), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_FLAGS(0)) }.ok()?;
    // SAFETY: the handle FindFirstFileExW just gave.
    let _ = unsafe { FindClose(handle) };
    let mut e = entry_from(&data, types)?;
    if folder.is_some_and(|f| (e.kind == EntryKind::Folder) != f) {
        return None;
    }
    e.target = Some(path.to_string());
    Some(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_names_are_filled_in() {
        std::env::set_var("GLINT_RECENT_TEST_DIR", "C:\\Users\\me");
        assert_eq!(expand("%GLINT_RECENT_TEST_DIR%\\Downloads\\a.txt"), "C:\\Users\\me\\Downloads\\a.txt");
        assert_eq!(expand("C:\\100% sure\\a.txt"), "C:\\100% sure\\a.txt");
        assert!(is_recent(&LOCATION.to_lowercase()));
    }

    #[test]
    fn a_drive_root_is_told_apart() {
        assert!(is_drive_root("D:\\"));
        assert!(!is_drive_root("D:\\Work"));
        assert!(!is_drive_root("\\\\s\\"));
    }

    /// Windows' shortcuts are followed to the files, newest first; folders and files that are gone
    /// are left out, and so is a second shortcut to the same file.
    #[test]
    fn lists_the_files_the_shortcuts_point_at() {
        let root = std::env::temp_dir().join(format!("glint-recent-{}", std::process::id()));
        let (files, links) = (root.join("files"), root.join("Recent"));
        std::fs::create_dir_all(&files).unwrap();
        std::fs::create_dir_all(&links).unwrap();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        std::fs::write(files.join("old.txt"), "a").unwrap();
        std::fs::write(files.join("new.txt"), "b").unwrap();
        std::fs::write(files.join("gone.txt"), "c").unwrap();
        let _com = ComGuard::init();
        for (target, lnk) in [("old.txt", "old.txt.lnk"), ("gone.txt", "gone.txt.lnk"), ("", "files.lnk"), ("new.txt", "new.txt.lnk"), ("new.txt", "again.lnk")] {
            crate::fileops::make_shortcut(&s(&files.join(target)).trim_end_matches('\\').to_string(), &s(&links.join(lnk))).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        std::fs::remove_file(files.join("gone.txt")).unwrap();
        std::env::set_var("GLINT_TEST_RECENT", s(&links));
        let f = read();
        std::env::remove_var("GLINT_TEST_RECENT");
        let names: Vec<&str> = f.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["new.txt", "old.txt"]);
        assert!(f.entries[0].target.as_deref().is_some_and(|t| t.ends_with("files\\new.txt")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
