//! Remembering things between runs: the folders Glint keeps its files in, and settings.json.
//!
//! **Folders.** All of Glint's files are in one folder, `%APPDATA%\Glint`:
//! - `config` for settings.json and the `skins` folder (skin.rs);
//! - `cache` for things Glint can always make again;
//! - `logs` for error.log.
//!
//! **Portable mode:** when a folder called `Glint data` sits next to glint.exe, that is Glint's
//! folder instead, so the whole folder can go on a USB stick. Switching Portable in Settings moves
//! the files from one place to the other when Glint next starts (`settle`), as nothing has them
//! open then. (`GLINT_SETTINGS_DIR` points them somewhere else, for testing.)
//!
//! **settings.json** uses plain names and values ("Startup": "restore", "Theme": "dark",
//! "ShowHidden"...), so the file is easy to read and edit by hand.
//! - Reading never stops Glint. A file that can't be understood is copied to settings.json.bad and
//!   Glint starts with the defaults - or, when only some values are wrong (a typo in a hand edit),
//!   with every value that still makes sense.
//! - Writing is atomic: the new file is written next to the old one and then renamed over it, so a
//!   crash or a full disk never leaves half a file.
//! - Writing happens on a background thread (the window asks for it a moment after something
//!   changed, and once more when it closes), so a slow or redirected AppData folder never freezes
//!   the window.
//! - Two Glint windows at once: each one writes its own tabs and window position (the last one to
//!   close wins), but a choice changed in one (Show hidden files, say) is not
//!   undone by the other: when writing, a choice this window didn't change is taken from the file,
//!   and lists both change (pinned folders, tab sets...) are merged item by item.
//!   Values this version of Glint doesn't know (from a newer version) are kept as they are.
//! - An extra window (Ctrl+N, life.rs) writes only choices: the tabs and the window's place in the
//!   file stay the main window's (`set_extra_window`).

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};

use crate::fileops::same_path;
use crate::folder::SortKey;
use crate::log;
use crate::shell;
use crate::skin::decode;

/// The settings file's name, in the config folder.
pub const FILE_NAME: &str = "settings.json";
/// How many tabs a settings file may bring back: far more than anyone opens (a hand-edited file
/// with tens of thousands would only hurt).
const MAX_TABS: usize = 1000;
/// Column widths outside this range (DIPs) are ignored.
const COLUMN_MIN: f32 = 24.0;
const COLUMN_MAX: f32 = 2000.0;

// ---------------------------------------------------------------- folders

/// Where Glint keeps its files. The folders are only made when something is written into them.
#[derive(Clone, Debug)]
pub struct Folders {
    /// settings.json and the skins folder (skin.rs).
    pub config: PathBuf,
    /// Things Glint can always make again; safe to delete.
    pub cache: PathBuf,
    /// error.log.
    pub logs: PathBuf,
    /// True when the folders sit next to glint.exe (a `config` folder was there at start).
    pub portable: bool,
}

/// The folders, worked out once: the main Glint `settle`s them first, at start.
pub fn folders() -> &'static Folders {
    FOLDERS.get_or_init(find_folders)
}

static FOLDERS: OnceLock<Folders> = OnceLock::new();

/// The folder next to glint.exe that makes Glint portable, and holds all its files then.
pub const PORTABLE_FOLDER: &str = "Glint data";
/// A note in the config folder: Glint's files move to the other place at the next start (Settings
/// > About > Portable). Its first line says where to: "portable" or "installed".
const MOVE_NOTE: &str = "moving.txt";
/// What is in Glint's folder, moved together.
const PARTS: [&str; 3] = ["config", "cache", "logs"];

fn find_folders() -> Folders {
    if let Some(dir) = std::env::var_os("GLINT_SETTINGS_DIR").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        return Folders { cache: dir.join("cache"), logs: dir.join("logs"), config: dir, portable: false };
    }
    find_in(exe_dir().as_deref(), &installed_dirs().0)
}

/// Whose files this glint.exe uses, to tell Glints with different files apart (life.rs: a glint.exe
/// hands its folder only to a running Glint with the same files). None for the installed Glint's
/// (%APPDATA%\Glint); else GLINT_SETTINGS_DIR, or glint.exe's folder when it's portable (also
/// before `settle` has moved an older Glint's files into `Glint data`). Worked out afresh, so it
/// doesn't fix the folders before `settle`.
///
/// Two answers: whose the files are now, and whose they will be once `settle` has made a move
/// asked for in Settings (Portable, MOVE_NOTE). They differ only while such a move waits: the
/// Glint that asked for it still runs under the first, and the next main Glint moves them and so
/// runs under the second (life.rs picks).
pub fn whose_files() -> (Option<PathBuf>, Option<PathBuf>) {
    if let Some(dir) = std::env::var_os("GLINT_SETTINGS_DIR").filter(|d| !d.is_empty()) {
        return (Some(PathBuf::from(dir.clone())), Some(PathBuf::from(dir)));
    }
    let Some(exe_dir) = exe_dir() else { return (None, None) };
    let (now, after) = portable_now_and_after(&exe_dir, &installed_dirs().0);
    (now.then(|| exe_dir.clone()), after.then_some(exe_dir))
}

/// Whether Glint's files are portable now, and once `settle` has made a move asked for in Settings
/// (the same test `settle_in` makes before it moves them).
fn portable_now_and_after(exe_dir: &Path, home: &Path) -> (bool, bool) {
    let found = find_in(Some(exe_dir), home);
    let mine = if found.portable { exe_dir.join(PORTABLE_FOLDER) } else { home.to_path_buf() };
    let after = match move_wanted(&found.config) {
        Some(to_portable) if found.config == mine.join("config") => to_portable,
        _ => found.portable,
    };
    (found.portable, after)
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Glint's one folder, with config, cache and logs in it: `Glint data` next to glint.exe when that
/// makes Glint portable, else `home` (%APPDATA%\Glint). An older Glint's portable files straight
/// next to glint.exe (until `settle` moves them into `Glint data`) count as portable too.
fn find_in(exe_dir: Option<&Path>, home: &Path) -> Folders {
    let at = |dir: &Path, portable| Folders { config: dir.join("config"), cache: dir.join("cache"), logs: dir.join("logs"), portable };
    if let Some(exe_dir) = exe_dir {
        let portable = exe_dir.join(PORTABLE_FOLDER);
        if makes_portable(&portable) {
            return at(&portable, true);
        }
        if older_portable(exe_dir) {
            return at(exe_dir, true);
        }
    }
    at(home, false)
}

/// Whether `Glint data` next to glint.exe makes Glint portable: it has Glint's config folder, or it
/// is empty (made by hand for that). What's left after Glint's files moved away doesn't.
fn makes_portable(dir: &Path) -> bool {
    dir.join("config").is_dir() || std::fs::read_dir(dir).is_ok_and(|mut e| e.next().is_none())
}

/// An older Glint's portable files: a `config` folder straight next to glint.exe that is Glint's.
fn older_portable(exe_dir: &Path) -> bool {
    let config = exe_dir.join("config");
    config.is_dir() && glint_config(&config)
}

/// Keys every settings.json Glint writes has, and other programs' hardly would.
const GLINT_KEYS: [&str; 6] = ["\"ActiveTab\"", "\"Startup\"", "\"ShowHidden\"", "\"ConfirmDelete\"", "\"HiddenPlaces\"", "\"StayRunning\""];

/// Whether a `config` folder next to glint.exe is Glint's: it is empty (made to make Glint
/// portable), or holds Glint's own settings.json. A folder of that name that belongs to another
/// program (glint.exe put in a folder with other tools) must not make Glint portable: Glint would
/// rewrite that program's settings.json as its own, and `settle` would move the folder away.
fn glint_config(dir: &Path) -> bool {
    match std::fs::read(dir.join(FILE_NAME)) {
        Ok(bytes) => {
            let text = decode(&bytes);
            // (By its words, not by reading it: a Glint settings.json with a typo in it is still Glint's.)
            text.trim().trim_start_matches('\u{feff}').is_empty() || GLINT_KEYS.iter().any(|k| text.contains(k))
        }
        // No settings.json: only an empty folder is Glint's (another program's has its own files in it).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::read_dir(dir).is_ok_and(|mut left| left.next().is_none()),
        // There but unreadable right now: whose it is can't be told, so the folder is left alone.
        Err(_) => false,
    }
}

/// "%APPDATA%\Glint" (Glint's folder when it isn't portable) and "%LOCALAPPDATA%\Glint" (where
/// older versions kept the cache and logs).
pub(crate) fn installed_dirs() -> (PathBuf, PathBuf) {
    // The known-folder lookup doesn't check that the folder exists (that could wait on a server),
    // and the environment is the fallback for odd starts.
    let known = |id, var: &str| {
        shell::known_folder(id).map(PathBuf::from).or_else(|| std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from))
    };
    let roaming = known(&FOLDERID_RoamingAppData, "APPDATA").unwrap_or_else(std::env::temp_dir).join("Glint");
    let local = known(&FOLDERID_LocalAppData, "LOCALAPPDATA").unwrap_or_else(std::env::temp_dir).join("Glint");
    (roaming, local)
}

/// The settings file's full path.
pub fn file_path() -> PathBuf {
    config_now().join(FILE_NAME)
}

// ---------------------------------------------------------------- putting the files in place

/// Puts Glint's files where they belong, before anything reads them. Only the main Glint does this
/// (an extra window or a second start leaves the files alone while another Glint uses them), and
/// only before `folders` is first asked. Returns what went wrong, for the error log.
/// - An older Glint's portable files (config, cache, logs straight next to glint.exe) go into
///   `Glint data`.
/// - An older Glint's logs in %LOCALAPPDATA%\Glint go into %APPDATA%\Glint; its cache goes.
/// - A move asked for in Settings (Portable, MOVE_NOTE) happens now, while nothing has the files
///   open.
/// - settings.json.old, which older versions left when Portable was switched, goes.
pub fn settle() -> Vec<String> {
    if test_folders() || FOLDERS.get().is_some() {
        return Vec::new();
    }
    let (home, local) = installed_dirs();
    match exe_dir() {
        Some(exe_dir) => settle_in(&exe_dir, &home, &local),
        None => Vec::new(),
    }
}

fn settle_in(exe_dir: &Path, home: &Path, local: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let portable = exe_dir.join(PORTABLE_FOLDER);
    if !portable.join("config").exists() && older_portable(exe_dir) {
        // (Only Glint's own files: a cache or logs folder there may be shared with another program.)
        if let Err(e) = move_folder(&exe_dir.join("config"), &portable.join("config")) {
            problems.push(format!("Glint's settings next to glint.exe couldn't be moved into {} ({e}).", portable.display()));
        }
        for log in ["error.log", "error.old.log"] {
            let _ = move_file(&exe_dir.join("logs").join(log), &portable.join("logs").join(log));
        }
        crate::update::remove_downloads(&exe_dir.join("cache"));
        let _ = std::fs::remove_dir(exe_dir.join("logs"));
        let _ = std::fs::remove_dir(exe_dir.join("cache"));
    }
    if local != home && local.is_dir() {
        let _ = move_folder(&local.join("logs"), &home.join("logs"));
        let _ = std::fs::remove_dir_all(local.join("cache"));
        let _ = std::fs::remove_dir(local);
    }
    let found = find_in(Some(exe_dir), home);
    let mine = |portable_folder: bool| if portable_folder { portable.clone() } else { home.to_path_buf() };
    if let Some(to_portable) = move_wanted(&found.config) {
        let _ = std::fs::remove_file(found.config.join(MOVE_NOTE));
        if to_portable != found.portable && found.config == mine(found.portable).join("config") {
            let (from, to) = (mine(found.portable), mine(to_portable));
            for part in PARTS {
                if let Err(e) = move_folder(&from.join(part), &to.join(part)) {
                    problems.push(format!("Glint's {part} folder couldn't be moved from {} to {} ({e}).", from.display(), to.display()));
                }
            }
            // A link, or a file in use, can't move and keeps the old config folder in `Glint data`.
            // Once settings.json has gone, that folder would make Glint portable again, with none of
            // its settings: it's renamed, and its things are kept.
            let config = from.join("config");
            if !to_portable && config.is_dir() && !config.join(FILE_NAME).exists() {
                let _ = std::fs::rename(&config, from.join("config.old"));
            }
            // What's left of the old place goes when nothing else is in it.
            let _ = std::fs::remove_dir(&from);
        }
    }
    let found = find_in(Some(exe_dir), home);
    let _ = std::fs::remove_file(found.config.join(format!("{FILE_NAME}.old")));
    problems
}

/// Where a move asked for in Settings goes (MOVE_NOTE in `config`): true for next to glint.exe.
fn move_wanted(config: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(config.join(MOVE_NOTE)).ok()?;
    match text.lines().next().map(str::trim) {
        Some("portable") => Some(true),
        Some("installed") => Some(false),
        _ => None,
    }
}

/// Moves the folder `from` to `to` and returns once `from` is gone (or was never there): a rename
/// when `to` isn't there yet and both are on the same drive; else file by file (copied and then
/// deleted across drives), a file in `to` with the same name replaced. Links and junctions aren't
/// followed: they stay behind, and so does their folder.
fn move_folder(from: &Path, to: &Path) -> std::io::Result<()> {
    move_level(from, to, 0)
}

fn move_level(from: &Path, to: &Path, depth: u32) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if depth > 8 {
        return Ok(()); // nothing Glint writes is this deep
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !to.exists() && std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for entry in entries {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if is_link(&meta) {
            continue;
        }
        let target = to.join(entry.file_name());
        if meta.is_dir() {
            move_level(&entry.path(), &target, depth + 1)?;
        } else {
            move_file(&entry.path(), &target)?;
        }
    }
    let _ = std::fs::remove_dir(from);
    Ok(())
}

/// Moves one file, replacing one of the same name: renamed, or copied (checked whole) and deleted
/// when it goes to another drive. A missing file moves nothing.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let len = match std::fs::metadata(from) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    // (What std::fs::copy says it copied isn't reliable everywhere - under Wine it's 0 - so the copy
    // itself is measured.)
    if std::fs::metadata(to)?.len() != len {
        let _ = std::fs::remove_file(to);
        return Err(std::io::Error::other(format!("{} wasn't copied whole", from.display())));
    }
    std::fs::remove_file(from)
}

// ---------------------------------------------------------------- portable, switched while running

/// Where Portable was switched to in the Settings window this run (the move happens at the next
/// start). None: as Glint started.
static PORTABLE_WANTED: Mutex<Option<bool>> = Mutex::new(None);

/// The config folder settings.json and the skins are in (until the next start, also after Portable
/// was switched: the files move then).
pub fn config_now() -> PathBuf {
    folders().config.clone()
}

/// Whether Portable is on: as Glint started, or as it was switched since (see `set_portable`).
pub fn portable_now() -> bool {
    PORTABLE_WANTED.lock().unwrap_or_else(PoisonError::into_inner).unwrap_or(folders().portable)
}

/// Whether the folders were pointed elsewhere for testing (GLINT_SETTINGS_DIR): then Portable can't
/// be switched.
pub fn test_folders() -> bool {
    std::env::var_os("GLINT_SETTINGS_DIR").is_some_and(|d| !d.is_empty())
}

/// Portable on (Glint's files next to glint.exe, in `Glint data`) or off (in %APPDATA%\Glint), from
/// Settings > About. Glint has its files open while it runs, so they move at the next start
/// (`settle`): this leaves a note saying so in the config folder, or takes it away again when
/// Portable is switched back before that. Touches the disk: call it on a background thread.
pub fn set_portable(on: bool) -> Result<String, String> {
    if test_folders() {
        return Err("Portable can't be switched while GLINT_SETTINGS_DIR is set (it's for testing).".to_string());
    }
    let exe_dir = exe_dir().ok_or("Glint couldn't find out where glint.exe is.")?;
    let f = folders();
    let note = f.config.join(MOVE_NOTE);
    if on == f.portable {
        let _ = std::fs::remove_file(&note);
        *PORTABLE_WANTED.lock().unwrap_or_else(PoisonError::into_inner) = None;
        return Ok(format!("The settings stay in {}.", f.config.display()));
    }
    let to = if on { exe_dir.join(PORTABLE_FOLDER) } else { installed_dirs().0 };
    if on && !to.exists() {
        // Can Glint write next to glint.exe? A test file says: Windows deletes it as it's closed,
        // so nothing can keep it there. (Not a test `Glint data` folder: if a sync tool held that
        // for a moment and it stayed, the next start would be portable without the settings. The
        // move makes the folder.)
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        let test = exe_dir.join(format!("glint-{}.tmp", std::process::id()));
        if let Err(e) = std::fs::OpenOptions::new().write(true).create_new(true).share_mode(0).custom_flags(FILE_FLAG_DELETE_ON_CLOSE).open(test) {
            return Err(format!("Glint can't write to {} ({e}). Move Glint to a folder you can write to.", exe_dir.display()));
        }
    }
    let what = if on { "portable" } else { "installed" };
    let text = format!("{what}\r\nGlint moves its files to {} when it next starts.\r\n", to.display());
    if let Err(e) = std::fs::create_dir_all(&f.config).and_then(|_| std::fs::write(&note, text)) {
        return Err(format!("{} couldn't be written ({e}).", note.display()));
    }
    *PORTABLE_WANTED.lock().unwrap_or_else(PoisonError::into_inner) = Some(on);
    Ok(format!("Glint's files move to {} when Glint next starts.", to.display()))
}

/// Whether a file system entry is a link or junction (never followed, moved or deleted here).
fn is_link(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

// ---------------------------------------------------------------- the cache

/// The plain files directly in the cache folder, with their sizes. Subfolders and links are left
/// alone (not looked into, not touched): Glint keeps nothing in subfolders of its cache, and a link
/// or junction there could lead anywhere - under Wine even a link to a folder looks like a folder.
fn cache_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|entry| {
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            (meta.is_file() && !is_link(&meta)).then(|| (entry.path(), meta.len()))
        })
        .collect()
}

/// How many bytes the files in the cache folder take (see `cache_files`). Touches the disk: call it
/// on a background thread.
pub fn cache_size() -> u64 {
    cache_files(&folders().cache).iter().map(|(_, len)| len).sum()
}

/// Empties the cache folder (Settings > About > Cache: things Glint can always make again): the
/// plain files directly in it are deleted (see `cache_files`); a file in use is skipped. Touches the
/// disk: call it on a background thread. Returns the bytes left.
pub fn clear_cache() -> u64 {
    clear_files(&folders().cache);
    cache_size()
}

/// Deletes the plain files directly in `dir` (see `cache_files`).
fn clear_files(dir: &Path) {
    for (file, _) in cache_files(dir) {
        let _ = std::fs::remove_file(file);
    }
}

// ---------------------------------------------------------------- the settings

/// What Glint opens when it starts ("When Glint starts, open").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Startup {
    /// The tabs that were open last time.
    Restore,
    /// The home folder (C:\Users\name): the default.
    #[default]
    Home,
    ThisPc,
    /// `Settings::startup_folder`.
    Folder,
}

/// Where Ctrl+T and "+" open a new tab ("New tabs open"). Middle-clicking a folder always opens
/// that folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NewTab {
    /// The folder the current tab is in.
    #[default]
    Same,
    Home,
    ThisPc,
}

/// What "Home" opens ("Home opens"): the sidebar's Home, and "Home folder" when Glint starts or a
/// tab is opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomePage {
    /// Glint's Home page, like File Explorer's: your folders, then the files you opened lately
    /// (recent.rs).
    #[default]
    Page,
    /// Your user folder (C:\Users\name).
    Folder,
}

/// Where the Settings window opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsOpens {
    /// A small window of its own on top of Glint, where it was left (the middle at first). It
    /// stays open while you use Glint, so every change shows behind it. (Before 1.0.23 there were
    /// Docked, Small, Tab and Last position: they all open floating now.)
    #[default]
    #[serde(alias = "docked", alias = "small", alias = "tab", alias = "free")]
    Floating,
    /// Glued to the main window's side, as tall as it, moving with it.
    Side,
    /// Over the main window's right edge, where the details panel goes.
    Panel,
}

/// How the Settings window shows its pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsPages {
    /// Tabs along the top.
    #[default]
    Tabs,
    /// A list down the left (tabs again when the window is too narrow for it).
    List,
}

/// What the Settings window's choices look like.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsStyle {
    /// Like File Explorer's Folder Options: group boxes, ticks, round buttons for short choices.
    #[default]
    Ticks,
    /// Every choice spelled out as a sentence with round buttons under it, no boxes.
    Sentences,
}

/// How much each Settings page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsShow {
    #[default]
    All,
    /// The everyday settings, then "More" for the rest.
    Basic,
}

/// How the Settings window lists the skins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsSkins {
    #[default]
    List,
    /// Little tiles in each skin's colours.
    Tiles,
}

/// Light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Whatever Windows uses for apps (Settings > Personalisation > Colours).
    System,
    Light,
    /// Glint's own look, and the default.
    #[default]
    Dark,
}

/// When deleting asks first ("Ask before deleting").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmDelete {
    /// Only when the files would be gone for good (Shift+Delete, or a drive without a Recycle Bin).
    #[default]
    Permanent,
    Always,
    Never,
}

/// Which terminal "Open in terminal" (Ctrl+`) starts ("Terminal").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Terminal {
    /// Windows Terminal if it's installed, otherwise Windows PowerShell.
    #[default]
    Auto,
    /// Windows Terminal.
    Wt,
    /// Windows PowerShell (the one that comes with Windows).
    PowerShell,
    /// PowerShell 7.
    Pwsh,
    /// Command Prompt.
    Cmd,
    /// Git Bash (Git for Windows).
    GitBash,
}

impl Terminal {
    /// Every choice with its name, in menu order.
    pub const ALL: [(Terminal, &'static str); 6] = [
        (Terminal::Auto, "Automatic"),
        (Terminal::Wt, "Windows Terminal"),
        (Terminal::PowerShell, "Windows PowerShell"),
        (Terminal::Pwsh, "PowerShell 7"),
        (Terminal::Cmd, "Command Prompt"),
        (Terminal::GitBash, "Git Bash"),
    ];

    pub fn name(self) -> &'static str {
        Terminal::ALL.iter().find(|(t, _)| *t == self).map_or("", |(_, n)| n)
    }
}

/// How big the file list's rows and text are ("Size"): small | medium | large.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListSize {
    /// The normal look.
    #[default]
    Small,
    /// More room between rows.
    Medium,
    /// Bigger text and big (32-pixel) icons.
    Large,
}

/// How dates look in the list ("DateFormat"): default | relative | iso.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DateFormat {
    /// Windows' short date and time ("08/10/2026 14:32", as the person set Windows up).
    #[default]
    Default,
    /// "Just now", "5 min ago", "Today, 3:02 PM", "Yesterday, ...", "Monday, ...", then the date.
    Relative,
    /// "2026-10-09 15:02".
    Iso,
}

/// How file sizes look in the list ("SizeFormat"): auto | kb.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SizeFormat {
    /// "512 B", "13 KB", "3.1 MB", "1.2 GB".
    #[default]
    Auto,
    /// Always in KB, like File Explorer ("3,172 KB").
    Kb,
}

/// What Backspace does in the file list ("Backspace"): up | back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backspace {
    /// Up one folder (Alt+Up does that too).
    #[default]
    Up,
    /// Back, like a web browser (Alt+Left does that too).
    Back,
}

/// How the file list shows the items of a folder ("View", and each saved tab's own "View").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewMode {
    /// Rows with columns (name, date, type, size): the list as it always was.
    #[default]
    Details,
    /// Names in columns, flowing from left to right (Explorer's List).
    List,
    /// Tiles with 48-pixel pictures and the name under them.
    Medium,
    /// Tiles with 96-pixel pictures.
    Large,
    /// Tiles with 256-pixel pictures.
    #[serde(rename = "extralarge")]
    ExtraLarge,
    /// Details, with folders opening in place under a ▸ arrow.
    Tree,
}

impl ViewMode {
    /// Every view with its name, in the order of Explorer's Ctrl+Shift+1 to 6 (the VIEW menu's order).
    pub const ALL: [(ViewMode, &'static str); 6] = [
        (ViewMode::ExtraLarge, "Extra large icons"),
        (ViewMode::Large, "Large icons"),
        (ViewMode::Medium, "Medium icons"),
        (ViewMode::Tree, "Tree"),
        (ViewMode::List, "List"),
        (ViewMode::Details, "Details"),
    ];

    pub fn name(self) -> &'static str {
        ViewMode::ALL.iter().find(|(v, _)| *v == self).map_or("Details", |(_, n)| n)
    }

    /// The view Ctrl+Shift+`n` (1 to 6) picks, as in Explorer.
    pub fn for_key(n: u32) -> Option<ViewMode> {
        ViewMode::ALL.get((n as usize).checked_sub(1)?).map(|(v, _)| *v)
    }

    /// The view as settings.json writes it ("details", "extralarge"...).
    pub fn key(self) -> &'static str {
        match self {
            ViewMode::Details => "details",
            ViewMode::List => "list",
            ViewMode::Medium => "medium",
            ViewMode::Large => "large",
            ViewMode::ExtraLarge => "extralarge",
            ViewMode::Tree => "tree",
        }
    }

    /// The other way round; anything unknown is None.
    pub fn from_key(key: &str) -> Option<ViewMode> {
        ViewMode::ALL.iter().map(|(v, _)| *v).find(|v| v.key().eq_ignore_ascii_case(key.trim()))
    }
}

/// The extra columns next to Name, by their names in settings.json ("Columns"), in the list's order:
/// "dimensions" is Picture size, "length" how long a video or song plays (both read by media.rs).
pub const COLUMN_NAMES: [&str; 6] = ["modified", "created", "type", "size", "dimensions", "length"];

/// What the title bars show at the left: the skin's word in its slanted box (GLINT unless the skin
/// says otherwise), the chrome wordmark (logo.rs) in small letters ("glint") or in capitals
/// ("GLiNT"), or nothing (the tabs start at the left edge).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogoStyle {
    #[default]
    Box,
    Chrome,
    /// The chrome wordmark in capitals, GLiNT (a little wider).
    Caps,
    None,
}

/// The icon zip and 7z files show while they open in Glint ("ZipIcon", registry.rs): label | folder |
/// page | glint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZipIcon {
    /// A white page with a red label saying ZIP (or 7Z).
    #[default]
    Label,
    /// A chrome folder with a zipper.
    Folder,
    /// A chrome page with a zipper.
    Page,
    /// Glint's own icon, the chrome star (what Windows shows for them without one of the others).
    Glint,
}

/// The sidebar group Glint Minimal's icon rail shows: quick | pinned | pc.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RailGroup {
    #[default]
    Quick,
    Pinned,
    Pc,
}

/// Where updates come from unless settings.json says otherwise ("UpdateRepo"): Glint's repository
/// on GitHub, as owner/name.
pub const UPDATE_REPO: &str = "npr-tami/glint";

/// The default skin: AMD Radeon, Glint's own dark look.
pub const DEFAULT_SKIN: &str = crate::skin::DEFAULT;

/// One open tab, as saved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", from = "SavedTabForm")]
pub struct SavedTab {
    /// The folder ("" = This PC).
    pub path: String,
    /// See [`Settings::sort_column`].
    pub sort_column: u32,
    pub sort_descending: bool,
    /// How the tab shows its items ("View"); None: as the "View" setting says (tabs saved before
    /// there were views, or a value this Glint doesn't know).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewMode>,
    /// A pinned tab ("Pin tab"): it sits at the left, doesn't close by accident, and comes back
    /// when Glint starts whatever "When Glint starts, open" says.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// Tabs saved together under a name ("Save tabs as a set…"), opened again with one click.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TabSet {
    pub name: String,
    pub tabs: Vec<SavedTab>,
}

/// How many tab sets are kept.
pub const MAX_TAB_SETS: usize = 30;

/// A saved tab is written as {"Path": ..., "SortColumn": ..., "SortDescending": ..., "View": ...};
/// a plain "C:\\folder" (an older file, or a hand edit) is read too.
#[derive(Deserialize)]
#[serde(untagged)]
enum SavedTabForm {
    Path(String),
    Full {
        #[serde(rename = "Path")]
        path: String,
        #[serde(rename = "SortColumn", default)]
        sort_column: u32,
        #[serde(rename = "SortDescending", default)]
        sort_descending: bool,
        // Read as text, so a view a newer Glint wrote (or a typo) loses only itself, not the tab.
        #[serde(rename = "View", default)]
        view: Option<String>,
        #[serde(rename = "Pinned", default)]
        pinned: bool,
    },
}

impl From<SavedTabForm> for SavedTab {
    fn from(form: SavedTabForm) -> SavedTab {
        match form {
            SavedTabForm::Path(path) => SavedTab { path, sort_column: 0, sort_descending: false, view: None, pinned: false },
            SavedTabForm::Full { path, sort_column, sort_descending, view, pinned } => {
                SavedTab { path, sort_column, sort_descending, view: view.as_deref().and_then(ViewMode::from_key), pinned }
            }
        }
    }
}

/// Everything Glint remembers. Field names in the file are the same with capitals ("ShowHidden").
///
/// Every field is written to the file (none is `#[serde(skip)]`): the Settings window changes a
/// value by its name in the file and reads the whole struct back (settingswin.rs), which keeps
/// every field only because they all go through the file's form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    // ----- choices (the Settings window)
    /// "When Glint starts, open": restore | home | thispc | folder.
    pub startup: Startup,
    /// The folder for `Startup::Folder`.
    pub startup_folder: Option<String>,
    /// Light or dark: system | light | dark.
    pub theme: ThemeMode,
    /// "New tabs open": same | home | thispc.
    pub new_tab: NewTab,
    /// "Home opens": page | folder.
    #[serde(rename = "HomeOpens")]
    pub home: HomePage,
    /// "Settings opens": floating | side | panel.
    pub settings_opens: SettingsOpens,
    /// The Settings window's own look ("Settings window" on the Appearance page): its pages as tabs or a
    /// list, its choices as ticks or sentences, everything or the basics, skins as a list or tiles,
    /// and the search box, the dots on changed settings, Reset page and reopening on the last page.
    pub settings_pages: SettingsPages,
    pub settings_style: SettingsStyle,
    pub settings_show: SettingsShow,
    pub settings_skins: SettingsSkins,
    pub settings_search: bool,
    pub settings_dots: bool,
    pub settings_reset: bool,
    pub settings_last_page: bool,
    /// The page Settings showed last (for `settings_last_page`).
    pub settings_page: usize,
    /// Show hidden files and folders (greyed). Ctrl+H.
    pub show_hidden: bool,
    /// Also show protected operating-system files (hidden + system, like desktop.ini), when hidden
    /// files are shown. Explorer's "Hide protected operating system files", the other way round.
    pub show_system: bool,
    /// "Ask before deleting": permanent | always | never.
    pub confirm_delete: ConfirmDelete,
    /// Other apps' items (Open with Code, Send to...) in Glint's right-click menus, not only under
    /// "Show more options".
    pub shell_items_in_menu: bool,
    /// The terminal "Open in terminal" starts: auto | wt | powershell | pwsh | cmd | gitbash.
    pub terminal: Terminal,
    /// Write unexpected errors and crashes to logs\error.log.
    pub error_log: bool,
    /// The look: a skin id (skin.rs) - "amd-radeon", "amd-radeon-light", "nvidia", "nvidia-light",
    /// "chrome", "gunmetal", or "file:<name>" for one of the person's own in the skins folder. The light/dark choice
    /// ("Theme") switches it to its light or dark version (`AppState::set_dark`). Empty means
    /// AMD Radeon.
    pub skin: String,
    /// Show the details panel on the right of the list (Alt+P; comes with the details panel).
    pub show_details: bool,
    /// Show the status bar.
    pub show_status_bar: bool,
    /// Sort by the rules the person wrote (`sort_recipe`) instead of the columns (comes with
    /// custom sort orders).
    pub custom_sort: bool,
    /// Folders the rules give a view of their own (-large_icons…) are shown that way (VIEW >
    /// Custom views). Off: every folder shows its tab's view.
    pub custom_views: bool,
    /// Split view comes back after a restart, with the same folder on the other side.
    pub remember_split: bool,
    /// Ctrl + the scroll wheel over the files changes the view, as in File Explorer.
    pub ctrl_wheel_view: bool,
    /// The custom sort order's rules; None: the starter rules.
    pub sort_recipe: Option<String>,
    /// The folders pinned to the sidebar (Ctrl+D, right-click > Pin to sidebar), in their order.
    pub pinned: Vec<String>,
    /// Names given to pinned folders (right-click > Rename): pinned path -> the name shown. Only the
    /// sidebar shows it; the folder itself keeps its name.
    pub pin_names: BTreeMap<String, String>,
    /// Quick access places removed from the sidebar (right-click > Remove from Quick access), by
    /// id: home, desktop, documents, downloads, pictures, music, videos, recycle.
    pub hidden_places: Vec<String>,
    /// Files and folders marked with right-click > Add to Favorites (full paths, newest last):
    /// Home's Favorites list (home.rs).
    pub favorites: Vec<String>,
    /// Groups of tabs saved under a name (a tab's right-click > Save tabs as a set…), to open
    /// again with one click.
    pub tab_sets: Vec<TabSet>,
    /// Folders show how big they are in the Size column, worked out in the background
    /// (foldersize.rs).
    pub folder_sizes: bool,

    // ----- the file list (list.rs)
    /// Row and text size: small | medium | large.
    pub size: ListSize,
    /// The sidebar is shown (Ctrl+B hides it).
    pub show_sidebar: bool,
    /// Leave known extensions out of names ("report" for report.pdf), like Explorer's "Hide
    /// extensions for known file types". The Settings window shows it the other way round, as
    /// "Show file name extensions".
    pub hide_known_extensions: bool,
    /// The columns next to Name: any of "modified", "created", "type", "size", "dimensions"
    /// (Picture size), "length".
    pub columns: Vec<String>,
    /// Folders above files (otherwise they're sorted together).
    pub folders_first: bool,
    /// How dates look: default | relative | iso.
    pub date_format: DateFormat,
    /// How sizes look: auto | kb.
    pub size_format: SizeFormat,
    /// One click opens an item (otherwise a double-click does).
    pub single_click: bool,
    /// What Backspace does: up | back.
    pub backspace: Backspace,
    /// Double-clicking the list's empty space goes up a folder.
    pub double_click_up: bool,
    /// How tabs show their items when they have no view of their own (new windows, folders from
    /// outside, a fresh start): details | list | medium | large | extralarge | tree. Each tab keeps
    /// its own (the VIEW button); changing this in the Settings window switches every tab.
    pub view: ViewMode,

    // ----- how Glint runs (life.rs)
    /// "Keep Glint running in the background": closing the main window hides Glint instead of
    /// quitting, so it opens instantly next time (life.rs).
    pub stay_running: bool,
    /// "Show the tray icon" while Glint runs hidden in the background (tray.rs).
    pub tray_icon: bool,
    /// "Start with Windows": start hidden in the background at sign-in. The Run value in the
    /// registry is what really does it (registry.rs); change both with `life::apply_start_with_windows`.
    pub start_with_windows: bool,

    // ----- updates (update.rs)
    /// "Check for updates automatically": about 30 seconds after Glint starts, and then every 12
    /// hours while it's on screen (never while it's hidden in the background), Glint looks for a new
    /// version and says so when there is one. The update window's "Turn off automatic updates"
    /// turns it off.
    pub check_for_updates: bool,
    /// Where new versions come from: a GitHub repository, as owner/name. Its newest release is
    /// tagged with the version ("v0.8.0") and has a ready-built Glint.exe attached.
    pub update_repo: String,
    /// "Don't remind me about this update": the version not to ask about again. A newer one
    /// asks again.
    pub skipped_update: Option<String>,
    /// "Remind me again later": automatic checks don't ask again before this time (seconds since
    /// 1970; 0 = no waiting).
    pub remind_update_after: u64,

    // ----- remembered state
    /// The open tabs, left to right.
    pub tabs: Vec<SavedTab>,
    pub active_tab: usize,
    /// The folder split view's other side showed; None when split view was off.
    pub split_folder: Option<String>,
    /// How much of the files' width split view's left side takes (0.2 to 0.8).
    pub split_ratio: f32,
    /// The window's position and size when not maximised: left, top, width, height in screen
    /// pixels.
    pub bounds: Option<[i32; 4]>,
    /// The screen's DPI when `bounds` was saved, so the window keeps its size when the scaling
    /// changes. 0 = unknown.
    pub bounds_dpi: u32,
    pub maximized: bool,
    /// Column widths in DIPs (1/96 inch), by column: "modified", "type", "size". The name column
    /// takes the rest.
    pub column_width_map: BTreeMap<String, f32>,
    /// The view picked with VIEW in each folder, by its path in lower case, so the folder shows
    /// that way again (as in Explorer). Folders the view rules give a view aren't in it.
    pub folder_views: BTreeMap<String, ViewMode>,
    /// How new tabs are sorted when Glint starts without its old tabs: 0 Name, 1 Date modified,
    /// 2 Type, 3 Size. Each saved tab has its own.
    pub sort_column: u32,
    pub sort_descending: bool,
    /// The sidebar's width in DIPs; 0 = the standard width.
    pub sidebar_width: i32,
    /// Column widths by position (Name, Date, Type, Size), from older files. Only read, never
    /// written (ColumnWidthMap took its place).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_widths: Option<Vec<f32>>,
    /// The last one-time settings upgrade step done.
    pub upgraded: i32,
    /// The sort order editor's guide is shown (GUIDE; hidden at first). (Older versions wrote
    /// "SortGuideHidden", shown at first: it's left out, so everyone starts with it hidden.)
    pub sort_guide_shown: bool,
    /// The sort order editor is on its own (DETACH), not docked beside the main window.
    pub sort_detached: bool,
    /// Where the sort order editor was last on its own: left, top, width, height in screen pixels.
    pub sort_bounds: Option<[i32; 4]>,
    /// The skin editor's guide is hidden.
    pub theme_guide_hidden: bool,
    /// The group Glint Minimal's icon rail shows: quick | pinned | pc.
    pub rail_group: RailGroup,
    /// "Logo": box | chrome | caps | none.
    pub logo: LogoStyle,
    /// "ZipIcon": what zip and 7z files that open in Glint look like in Windows.
    pub zip_icon: ZipIcon,
    /// Where the floating Settings window was left (screen pixels: x, y, width, height). (Before
    /// 1.0.23: SettingsSize and SettingsBounds, for the old ways it opened; they're not read now.)
    pub settings_place: Option<[i32; 4]>,
    /// The details panel's width in DIPs (dragged at its left edge); 0 = the standard width (side.rs).
    pub details_width: f32,
    /// The Space preview's width in DIPs; 0 = half of the room next to the sidebar.
    pub preview_width: f32,
    /// The Space preview shows a picture's or video's size and other facts under it (its "i" button).
    pub preview_info: bool,
    /// How loud the Space preview plays videos (0 to 1), as last set there.
    pub preview_volume: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            startup: Startup::Home,
            startup_folder: None,
            theme: ThemeMode::Dark,
            new_tab: NewTab::Same,
            home: HomePage::Page,
            settings_opens: SettingsOpens::Floating,
            settings_pages: SettingsPages::Tabs,
            settings_style: SettingsStyle::Ticks,
            settings_show: SettingsShow::All,
            settings_skins: SettingsSkins::List,
            settings_search: true,
            settings_dots: true,
            settings_reset: true,
            settings_last_page: false,
            settings_page: 0,
            show_hidden: false,
            show_system: false,
            confirm_delete: ConfirmDelete::Permanent,
            shell_items_in_menu: true,
            terminal: Terminal::Auto,
            error_log: true,
            skin: DEFAULT_SKIN.to_string(),
            show_details: false,
            show_status_bar: true,
            custom_sort: false,
            custom_views: true,
            remember_split: false,
            ctrl_wheel_view: false,
            split_folder: None,
            split_ratio: 0.5,
            sort_recipe: None,
            size: ListSize::Small,
            show_sidebar: true,
            hide_known_extensions: false,
            columns: ["modified", "type", "size"].map(String::from).to_vec(),
            folders_first: true,
            date_format: DateFormat::Default,
            size_format: SizeFormat::Auto,
            single_click: false,
            backspace: Backspace::Up,
            double_click_up: false,
            view: ViewMode::Details,
            pinned: Vec::new(),
            pin_names: BTreeMap::new(),
            hidden_places: Vec::new(),
            favorites: Vec::new(),
            tab_sets: Vec::new(),
            folder_sizes: true,
            stay_running: true,
            tray_icon: true,
            start_with_windows: false,
            check_for_updates: true,
            update_repo: UPDATE_REPO.to_string(),
            skipped_update: None,
            remind_update_after: 0,
            tabs: Vec::new(),
            active_tab: 0,
            bounds: None,
            bounds_dpi: 0,
            maximized: false,
            column_width_map: BTreeMap::new(),
            folder_views: BTreeMap::new(),
            sort_column: 0,
            sort_descending: false,
            sidebar_width: 0,
            column_widths: None,
            upgraded: 0,
            sort_guide_shown: false,
            sort_detached: false,
            sort_bounds: None,
            theme_guide_hidden: false,
            rail_group: RailGroup::Quick,
            logo: LogoStyle::Box,
            zip_icon: ZipIcon::Label,
            settings_place: None,
            details_width: 0.0,
            preview_width: 0.0,
            preview_info: true,
            preview_volume: 0.5,
        }
    }
}

/// The choices (as opposed to remembered state such as tabs). When two Glint windows are open, a
/// choice one of them didn't change is taken from the file when it writes (see the module comment).
/// The pins and the hidden Quick access places count as choices: a folder pinned in one window
/// isn't unpinned by the other.
const CHOICES: &[&str] = &[
    "Startup",
    "StartupFolder",
    "HomeOpens",
    "SettingsOpens",
    "SettingsPages",
    "SettingsStyle",
    "SettingsShow",
    "SettingsSkins",
    "SettingsSearch",
    "SettingsDots",
    "SettingsReset",
    "SettingsLastPage",
    "Theme",
    "NewTab",
    "ShowHidden",
    "ShowSystem",
    "ConfirmDelete",
    "ShellItemsInMenu",
    "Terminal",
    "ErrorLog",
    "Skin",
    "ShowDetails",
    "ShowStatusBar",
    "CustomSort",
    "CustomViews",
    "RememberSplit",
    "CtrlWheelView",
    "SortRecipe",
    // The file list's (list.rs).
    "Size",
    "ShowSidebar",
    "HideKnownExtensions",
    "Columns",
    "FoldersFirst",
    "DateFormat",
    "SizeFormat",
    "SingleClick",
    "Backspace",
    "DoubleClickUp",
    "View",
    "FolderViews",
    // The sidebar's (sidebar.rs, app/places.rs).
    "Pinned",
    "PinNames",
    "HiddenPlaces",
    // Home's Favorites (home.rs).
    "Favorites",
    // Saved groups of tabs (app/forms.rs), and folder sizes (foldersize.rs).
    "TabSets",
    "FolderSizes",
    // How Glint runs (life.rs).
    "StayRunning",
    "TrayIcon",
    "StartWithWindows",
    // The title bars (logo.rs).
    "Logo",
    // Zip and 7z files' icon in Windows (registry.rs).
    "ZipIcon",
    // Updates (update.rs).
    "CheckForUpdates",
    "UpdateRepo",
    "SkippedUpdate",
    "RemindUpdateAfter",
    // The Space preview (preview.rs).
    "PreviewInfo",
    "PreviewVolume",
];

/// How many pinned folders, and how many favorites, a settings file may bring back: far more than
/// anyone adds (nothing stops adding, so what's past this would be lost at the next start).
const MAX_PINS: usize = 5000;

/// Set in an extra window (Ctrl+N, life.rs): it writes only choices; the remembered state (tabs,
/// window place, column widths...) in the file stays the main window's.
static EXTRA_WINDOW: AtomicBool = AtomicBool::new(false);

/// This process is an extra window (see `EXTRA_WINDOW`).
pub fn set_extra_window() {
    EXTRA_WINDOW.store(true, Ordering::SeqCst);
}

/// Whether this process is an extra window.
pub fn is_extra_window() -> bool {
    EXTRA_WINDOW.load(Ordering::SeqCst)
}

impl Settings {
    /// Where Home goes: the Home page (like File Explorer's), or the user folder when "Home opens"
    /// says so.
    pub fn home_place(&self) -> String {
        match self.home {
            HomePage::Folder => crate::shell::home(),
            HomePage::Page => crate::recent::LOCATION.to_string(),
        }
    }

    /// Whether deleting should ask first. `permanent`: the files would be gone for good (Shift+Delete,
    /// or no Recycle Bin on that drive) rather than go to the Recycle Bin.
    pub fn ask_before_deleting(&self, permanent: bool) -> bool {
        match self.confirm_delete {
            ConfirmDelete::Always => true,
            ConfirmDelete::Never => false,
            ConfirmDelete::Permanent => permanent,
        }
    }

    /// The remembered state (tabs, the window's place, column widths, sort) back to the defaults,
    /// leaving the choices. An extra window's snapshot is made this way, so moving around in it
    /// never counts as a change worth saving.
    pub fn forget_window_state(&mut self) {
        let d = Settings::default();
        self.tabs = d.tabs;
        self.active_tab = d.active_tab;
        self.split_folder = d.split_folder;
        self.split_ratio = d.split_ratio;
        self.bounds = d.bounds;
        self.bounds_dpi = d.bounds_dpi;
        self.maximized = d.maximized;
        self.column_width_map = d.column_width_map;
        self.sort_column = d.sort_column;
        self.sort_descending = d.sort_descending;
    }

    /// The settings as the text of settings.json.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Whether the list shows this column next to Name ("modified", "created", "type", "size").
    pub fn shows_column(&self, name: &str) -> bool {
        self.columns.iter().any(|c| c.eq_ignore_ascii_case(name))
    }

    /// Shows or hides a column; the list keeps them in their usual order whatever order they were
    /// switched on in.
    pub fn set_column(&mut self, name: &str, on: bool) {
        self.columns.retain(|c| !c.eq_ignore_ascii_case(name));
        if on {
            self.columns.push(name.to_string());
        }
        self.columns.sort_by_key(|c| COLUMN_NAMES.iter().position(|n| n == c));
    }

    /// Puts values that make no sense back in range (hand edits, a file from another computer).
    fn tidy(&mut self) {
        self.tabs.truncate(MAX_TABS);
        for t in &mut self.tabs {
            t.path = t.path.trim().to_string();
            if !known_sort_number(t.sort_column) {
                t.sort_column = 0;
            }
        }
        self.active_tab = self.active_tab.min(self.tabs.len().saturating_sub(1));
        self.split_ratio = if self.split_ratio.is_finite() { self.split_ratio.clamp(0.2, 0.8) } else { 0.5 };
        if !known_sort_number(self.sort_column) {
            self.sort_column = 0;
        }
        if self.bounds.is_some_and(|[_, _, w, h]| w <= 0 || h <= 0) {
            self.bounds = None;
        }
        // Columns by their old names, each once, in the list's order; unknown ones are left out.
        let mut columns: Vec<String> = Vec::new();
        for c in &self.columns {
            let c = c.trim().to_lowercase();
            if COLUMN_NAMES.contains(&c.as_str()) && !columns.contains(&c) {
                columns.push(c);
            }
        }
        columns.sort_by_key(|c| COLUMN_NAMES.iter().position(|n| n == c));
        self.columns = columns;
        self.column_width_map.retain(|key, w| {
            matches!(key.as_str(), "modified" | "created" | "type" | "size" | "dimensions" | "length" | "folder" | "free" | "location")
                && w.is_finite()
                && (COLUMN_MIN..=COLUMN_MAX).contains(w)
        });
        if self.startup_folder.as_deref().is_some_and(|f| f.trim().is_empty()) {
            self.startup_folder = None;
        }
        // Pins: no blanks, no folder twice (Windows paths ignore case, as same_path compares them;
        // a set, so thousands don't slow the start), a sane number (the newest, last, kept).
        let key = |p: &str| p.trim_end_matches('\\').to_lowercase();
        let mut seen = HashSet::new();
        let mut pinned: Vec<String> = Vec::new();
        for p in self.pinned.drain(..).map(|p| p.trim().to_string()) {
            if !p.is_empty() && seen.insert(key(&p)) {
                pinned.push(p);
            }
        }
        pinned.drain(..pinned.len().saturating_sub(MAX_PINS));
        self.pinned = pinned;
        // Favorites the same way.
        let mut seen = HashSet::new();
        let mut favorites: Vec<String> = Vec::new();
        for p in self.favorites.drain(..).map(|p| p.trim().to_string()) {
            if !p.is_empty() && seen.insert(key(&p)) {
                favorites.push(p);
            }
        }
        favorites.drain(..favorites.len().saturating_sub(MAX_PINS));
        self.favorites = favorites;
        // Tab sets: each with a name and tabs, each name once (the newest kept), not too many.
        let mut sets: Vec<TabSet> = Vec::new();
        for mut set in self.tab_sets.drain(..).rev() {
            set.name = set.name.trim().to_string();
            if !set.name.is_empty() && !set.tabs.is_empty() && !sets.iter().any(|s| s.name.eq_ignore_ascii_case(&set.name)) {
                sets.push(set);
            }
        }
        sets.reverse();
        let extra = sets.len().saturating_sub(MAX_TAB_SETS);
        sets.drain(..extra);
        self.tab_sets = sets;
        // A name only for a folder that is pinned, and only a name that says something.
        let pinned = &self.pinned;
        self.pin_names.retain(|path, name| !name.trim().is_empty() && pinned.iter().any(|p| same_path(p, path)));
        let mut seen: Vec<String> = Vec::new();
        self.hidden_places.retain(|id| {
            let new = !id.trim().is_empty() && !seen.contains(id);
            if new {
                seen.push(id.clone());
            }
            new
        });
        // No skin is the default one.
        self.skin = crate::skin::id_or_default(&self.skin);
        // Where updates come from: a GitHub repository as owner/name, nothing else (update.rs).
        self.update_repo = self.update_repo.trim().to_string();
        if !crate::update::valid_repo(&self.update_repo) {
            self.update_repo = UPDATE_REPO.to_string();
        }
        if self.skipped_update.as_deref().is_some_and(|v| v.trim().is_empty()) {
            self.skipped_update = None;
        }
        if self.sidebar_width < 0 {
            self.sidebar_width = 0;
        }
        if self.settings_place.is_some_and(|[_, _, w, h]| !((200..=20000).contains(&w) && (150..=20000).contains(&h))) {
            self.settings_place = None;
        }
        if self.sort_bounds.is_some_and(|[_, _, w, h]| !((200..=20000).contains(&w) && (150..=20000).contains(&h))) {
            self.sort_bounds = None;
        }
        // The side pane's widths: 0 (standard) or a sane width.
        for w in [&mut self.details_width, &mut self.preview_width] {
            if !w.is_finite() || *w < 0.0 || *w > 20000.0 {
                *w = 0.0;
            }
        }
        if !self.preview_volume.is_finite() {
            self.preview_volume = 0.5;
        }
        self.preview_volume = self.preview_volume.clamp(0.0, 1.0);
    }
}

/// A sort column as the number saved in settings.json (0 Name, 1 Date
/// modified, 2 Type, 3 Size, 4 Folder (search results), 5 Date created, 6 Picture size, 7 Length).
pub fn sort_number(key: SortKey) -> u32 {
    match key {
        SortKey::Name => 0,
        SortKey::Date => 1,
        SortKey::Type => 2,
        SortKey::Size => 3,
        SortKey::Folder => 4,
        SortKey::Created => 5,
        SortKey::Dimensions => 6,
        SortKey::Length => 7,
    }
}

/// The other way round; anything unknown is Name. Folder (4) only means something while search
/// results are shown, so a tab that comes back is sorted by name instead.
pub fn sort_key(n: u32) -> SortKey {
    match n {
        1 => SortKey::Date,
        2 => SortKey::Type,
        3 => SortKey::Size,
        5 => SortKey::Created,
        6 => SortKey::Dimensions,
        7 => SortKey::Length,
        _ => SortKey::Name,
    }
}

/// Whether a saved sort column number is one Glint knows.
fn known_sort_number(n: u32) -> bool {
    matches!(n, 0..=7)
}

/// Whether a light/dark choice means dark right now ("Follow Windows" asks Windows; dark when
/// Windows doesn't say).
pub fn dark_for(mode: ThemeMode) -> bool {
    match mode {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => shell::windows_apps_dark().unwrap_or(true),
    }
}

/// The tabs to open at start and which one is active, from "When Glint starts, open" and the folder
/// given on the command line (if any). A folder from outside opens on its own,
/// except with "Tabs from last time", where last time's tabs come back too (like a browser), so
/// they aren't lost; the folder is then the active tab (an already open tab for it is reused).
pub fn initial_tabs(s: &Settings, opened: Option<&str>, home: &str) -> (Vec<SavedTab>, usize) {
    let plain = |path: &str| SavedTab { path: path.to_string(), sort_column: s.sort_column, sort_descending: s.sort_descending, view: None, pinned: false };
    let restore = s.startup == Startup::Restore;
    // Pinned tabs come back whatever the start says, first in the row as they were.
    let mut tabs: Vec<SavedTab> = if restore { Vec::new() } else { s.tabs.iter().filter(|t| t.pinned).cloned().collect() };
    let pinned = tabs.len();
    tabs.extend(match s.startup {
        _ if opened.is_some() && !restore => Vec::new(),
        Startup::Restore => s.tabs.clone(),
        Startup::Home => vec![plain(home)],
        Startup::ThisPc => vec![plain("")],
        Startup::Folder => vec![plain(s.startup_folder.as_deref().unwrap_or(home))],
    });
    let mut active = if restore {
        s.active_tab.min(tabs.len().saturating_sub(1))
    } else {
        pinned.min(tabs.len().saturating_sub(1))
    };
    if let Some(dir) = opened {
        match tabs.iter().position(|t| same_path(&t.path, dir)) {
            Some(i) => active = i,
            None => {
                tabs.push(plain(dir));
                active = tabs.len() - 1;
            }
        }
    }
    if tabs.is_empty() {
        tabs.push(plain(home));
        active = 0;
    }
    (tabs, active)
}

// ---------------------------------------------------------------- reading

/// What reading settings.json found.
#[derive(Debug, PartialEq)]
enum Parsed {
    /// Everything was understood.
    Good(Settings),
    /// Some values made no sense and were left at their defaults (the names are listed).
    Partly(Settings, Vec<String>),
    /// Nothing could be understood (the reason is given).
    Broken(String),
}

/// Understands the text of a settings file: strictly first, then value by value.
fn parse(text: &str) -> Parsed {
    let text = text.trim_start_matches('\u{feff}');
    match serde_json::from_str::<Settings>(text) {
        Ok(mut s) => {
            s.tidy();
            Parsed::Good(s)
        }
        Err(strict) => {
            // Something in it is off: keep every value that still makes sense on its own.
            let Ok(Value::Object(file)) = serde_json::from_str::<Value>(text) else { return Parsed::Broken(strict.to_string()) };
            let Ok(Value::Object(mut merged)) = serde_json::to_value(Settings::default()) else { return Parsed::Broken(strict.to_string()) };
            let mut dropped = Vec::new();
            for (key, value) in file {
                let mut trial = merged.clone();
                trial.insert(key.clone(), value);
                if serde_json::from_value::<Settings>(Value::Object(trial.clone())).is_ok() {
                    merged = trial;
                } else {
                    dropped.push(key);
                }
            }
            match serde_json::from_value::<Settings>(Value::Object(merged)) {
                Ok(mut s) => {
                    s.tidy();
                    Parsed::Partly(s, dropped)
                }
                Err(_) => Parsed::Broken(strict.to_string()),
            }
        }
    }
}

/// Saving is switched off for this run when the file is there but couldn't be read: writing would
/// replace the person's settings with the defaults.
static SAVING_OFF: AtomicBool = AtomicBool::new(false);
/// The file was partly or wholly unusable: a clean one should be written soon, not only once
/// something changes.
static REWRITE: AtomicBool = AtomicBool::new(false);

/// Whether settings are saved this run (see `SAVING_OFF`).
pub fn saving_allowed() -> bool {
    !SAVING_OFF.load(Ordering::SeqCst)
}

/// Whether the file read at start needs writing again (it was repaired).
pub fn needs_rewrite() -> bool {
    REWRITE.load(Ordering::SeqCst)
}

/// Reads settings.json (on the UI thread, at start: it's a small local file). Never fails: with no
/// file, or one that can't be used, the defaults come back (and the problem goes to the error log).
pub fn load() -> Settings {
    let path = file_path();
    let mut tries = 0;
    let bytes = loop {
        match std::fs::read(&path) {
            Ok(b) => break b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                remember_synced(&Settings::default(), false);
                return Settings::default();
            }
            // Another program may have it open for a moment (a virus scanner, a sync tool).
            Err(_) if tries < 3 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(e) => {
                SAVING_OFF.store(true, Ordering::SeqCst);
                log::error(&format!("Couldn't read the settings ({}): {e}. Glint started with the defaults and won't save settings this time, so the file stays as it is.", path.display()));
                return Settings::default();
            }
        }
    };
    let keep_bad_copy = |why: &str| {
        REWRITE.store(true, Ordering::SeqCst);
        let bad = path.with_file_name(format!("{FILE_NAME}.bad"));
        let kept = std::fs::copy(&path, &bad).is_ok();
        log::error(&format!("{why} {}", if kept { format!("The file was kept as {}.", bad.display()) } else { String::new() }));
    };
    match parse(&decode(&bytes)) {
        Parsed::Good(s) => {
            remember_synced(&s, true);
            s
        }
        Parsed::Partly(s, dropped) => {
            keep_bad_copy(&format!("Some settings in {} made no sense and are back to their defaults: {}.", path.display(), dropped.join(", ")));
            remember_synced(&s, true);
            s
        }
        Parsed::Broken(reason) => {
            keep_bad_copy(&format!("The settings in {} couldn't be read ({reason}), so Glint started with the defaults.", path.display()));
            remember_synced(&Settings::default(), false);
            Settings::default()
        }
    }
}

// ---------------------------------------------------------------- writing

/// What the writer knows, shared by every save (one at a time).
struct Writer {
    /// The number of the newest save on disk; an older save that comes late is skipped.
    written: u64,
    /// The choices as this window last read or wrote them (see the module comment); None when the
    /// file was missing or unusable at start.
    synced: Option<Map<String, Value>>,
    /// The last problem logged, so a failing save (read-only drive) is logged once, not every time.
    last_problem: Option<String>,
}

static WRITER: Mutex<Writer> = Mutex::new(Writer { written: 0, synced: None, last_problem: None });
static NEXT_SAVE: AtomicU64 = AtomicU64::new(1);
/// `Writer::written`, for `needs_saving` (which mustn't wait for a save under way).
static WRITTEN: AtomicU64 = AtomicU64::new(0);

fn writer() -> std::sync::MutexGuard<'static, Writer> {
    // panic = abort, so the lock can't be left poisoned - but never panic over it either way.
    WRITER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The choices of `s` as JSON values.
fn choices_of(s: &Settings) -> Map<String, Value> {
    let Ok(Value::Object(all)) = serde_json::to_value(s) else { return Map::new() };
    all.into_iter().filter(|(k, _)| CHOICES.contains(&k.as_str())).collect()
}

fn remember_synced(s: &Settings, from_file: bool) {
    writer().synced = from_file.then(|| choices_of(s));
}

/// What to write: `ours`, except that choices this window didn't change since it last read or wrote
/// the file (`synced`) are taken from the file as it is now (`disk`) - each only if it makes sense -
/// and values only the file has (a newer Glint's) are kept; they come back as the second part.
/// `state_from_disk` (an extra window): everything that isn't a choice is taken from the file too.
fn merge(ours: &Settings, synced: Option<&Map<String, Value>>, disk: Option<Map<String, Value>>, state_from_disk: bool) -> (Settings, Map<String, Value>) {
    let mut out = ours.clone();
    let mut extra = Map::new();
    let (Some(disk), Ok(Value::Object(mut fields))) = (disk, serde_json::to_value(ours)) else { return (out, extra) };
    let mine = choices_of(ours);
    let mut combined = false;
    for (key, value) in disk {
        let choice = CHOICES.contains(&key.as_str());
        if !fields.contains_key(&key) {
            extra.insert(key, value);
            continue;
        }
        let base = synced.and_then(|s| s.get(&key));
        let take = if (choice && synced.is_some_and(|s| s.get(&key) == mine.get(&key))) || (!choice && state_from_disk) {
            Some(value)
        } else if let (true, Some(base), Some(m)) = (choice && LIST_CHOICES.contains(&key.as_str()), base, mine.get(&key)) {
            // Changed here and maybe in the other window too: both windows' changes are kept.
            let v = combine(m, base, &value).filter(|v| v != m);
            combined |= v.is_some();
            v
        } else {
            None
        };
        if let Some(value) = take {
            let mut trial = fields.clone();
            trial.insert(key, value);
            if let Ok(s) = serde_json::from_value::<Settings>(Value::Object(trial.clone())) {
                (fields, out) = (trial, s);
            }
        }
    }
    if combined {
        out.tidy(); // (a folder pinned in both windows: once)
    }
    (out, extra)
}

/// Choices that are lists or maps both windows add to and take from (pins, tab sets...): merged item
/// by item, not taken whole.
const LIST_CHOICES: &[&str] = &["Pinned", "PinNames", "HiddenPlaces", "Favorites", "TabSets", "FolderViews"];

/// `ours` with the changes the file got since `base` made to it too (what the other window added or
/// removed); an item both changed keeps ours.
fn combine(ours: &Value, base: &Value, disk: &Value) -> Option<Value> {
    match (ours, base, disk) {
        (Value::Array(o), Value::Array(b), Value::Array(d)) => {
            let mut out: Vec<Value> = o.iter().filter(|v| !b.contains(v) || d.contains(v)).cloned().collect();
            out.extend(d.iter().filter(|v| !b.contains(v) && !o.contains(v)).cloned());
            Some(Value::Array(out))
        }
        (Value::Object(o), Value::Object(b), Value::Object(d)) => {
            let mut out = o.clone();
            for key in b.keys().chain(d.keys()) {
                if d.get(key) != b.get(key) && o.get(key) == b.get(key) {
                    match d.get(key) {
                        Some(v) => out.insert(key.clone(), v.clone()),
                        None => out.remove(key),
                    };
                }
            }
            Some(Value::Object(out))
        }
        _ => None,
    }
}

/// The settings with values from a newer Glint after them, as they're written.
#[derive(Serialize)]
struct WithExtras<'a> {
    #[serde(flatten)]
    settings: &'a Settings,
    #[serde(flatten)]
    extra: &'a Map<String, Value>,
}

/// Writes `text` to `path` atomically: a temporary file next to it, flushed to the disk, then
/// renamed over the old one.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // One temporary name per process: two Glint windows saving at the same moment must not write
    // into the same temporary file (one of them would rename the other's half-written text).
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    // A virus scanner or sync tool may hold the old file for a moment: try a few times.
    let mut tries = 0;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(_) if tries < 4 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// settings.json as it is on disk now, to merge a save with (None: there is no file yet, or it
/// can't be made sense of, and is written afresh). A file that is there but can't be read (another
/// program has it open for a moment: a virus scanner, a sync tool) is tried a few times, as `load`
/// does; if it still can't be read, that's an error.
fn read_disk(path: &Path) -> std::io::Result<Option<Map<String, Value>>> {
    let mut tries = 0;
    let bytes = loop {
        match std::fs::read(path) {
            Ok(b) => break b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) if tries < 3 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(60));
            }
            Err(e) => return Err(e),
        }
    };
    Ok(match serde_json::from_str::<Value>(decode(&bytes).trim_start_matches('\u{feff}')) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    })
}

/// One save, in order: skipped when a newer one already reached the disk. Returns false if the
/// file couldn't be written.
fn write_in_order(s: &Settings, number: u64) -> bool {
    let mut w = writer();
    if w.written > number {
        return true;
    }
    let path = file_path();
    let disk = match read_disk(&path) {
        Ok(disk) => disk,
        Err(e) => {
            // Written without what's in the file, the other windows' choices would be lost - and an
            // extra window would wipe the main window's tabs. Tried again a little later.
            let problem = format!("Couldn't read the settings in {} to save them ({e}). Glint tries again shortly.", path.display());
            if w.last_problem.as_deref() != Some(problem.as_str()) {
                log::error(&problem);
                w.last_problem = Some(problem);
            }
            return false;
        }
    };
    let (merged, extra) = merge(s, w.synced.as_ref(), disk, is_extra_window());
    let text = serde_json::to_string_pretty(&WithExtras { settings: &merged, extra: &extra }).unwrap_or_else(|_| s.to_json());
    match write_atomically(&path, &text) {
        Ok(()) => {
            w.written = number;
            WRITTEN.store(number, Ordering::SeqCst);
            w.synced = Some(choices_of(s));
            if w.last_problem.take().is_some() {
                log::error(&format!("The settings could be saved again ({}).", path.display()));
            }
            true
        }
        Err(e) => {
            let problem = format!("Couldn't save the settings to {}: {e}", path.display());
            if w.last_problem.as_deref() != Some(problem.as_str()) {
                log::error(&problem);
                w.last_problem = Some(problem);
            }
            false
        }
    }
}

/// Saves on a background thread (the window calls this a moment after something changed).
/// Returns false if the save couldn't even start (saving is off, or no thread could be had).
/// The window counts the settings as saved once this starts, so a file that couldn't be written (a
/// sync tool holding it longer than `write_atomically` waits) is tried again a little later, until
/// a newer save takes over - and once more when the window closes (`needs_saving`).
pub fn save_in_background(s: Settings) -> bool {
    if SAVING_OFF.load(Ordering::SeqCst) {
        return false;
    }
    let number = NEXT_SAVE.fetch_add(1, Ordering::SeqCst);
    shell::background("save settings", move || {
        let mut waits = [2, 10, 60].into_iter();
        while !write_in_order(&s, number) {
            let Some(secs) = waits.next() else { return };
            std::thread::sleep(Duration::from_secs(secs));
            if NEXT_SAVE.load(Ordering::SeqCst) > number + 1 {
                return; // a newer save has all of this
            }
        }
    })
}

/// Whether `s` has to be written when the window closes: it isn't what was last handed over to be
/// saved (`last_saved`), or a save handed over hasn't reached the disk (it failed and waits to be
/// tried again, which the process ending would stop - or it's still under way).
pub fn needs_saving(s: &Settings, last_saved: &Settings) -> bool {
    s != last_saved || WRITTEN.load(Ordering::SeqCst) + 1 < NEXT_SAVE.load(Ordering::SeqCst)
}

/// Saves and waits until it's on disk, but no longer than `limit` (closing, Windows shutting down):
/// a hung network drive mustn't keep a closed window's process alive for ever.
pub fn save_and_wait(s: Settings, limit: Duration) {
    if SAVING_OFF.load(Ordering::SeqCst) {
        return;
    }
    let number = NEXT_SAVE.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = channel();
    let s2 = s.clone();
    let started = shell::background("save settings", move || {
        let _ = tx.send(write_in_order(&s2, number));
    });
    if started {
        let _ = rx.recv_timeout(limit);
    } else {
        write_in_order(&s, number); // no thread to be had: write it here
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let s = Settings::default();
        // Glint starts in the home folder.
        assert_eq!(s.startup, Startup::Home);
        assert_eq!(s.new_tab, NewTab::Same);
        assert!(!s.show_hidden && !s.show_system);
        assert_eq!(s.confirm_delete, ConfirmDelete::Permanent);
        assert!(s.error_log);
        assert_eq!(s.theme, ThemeMode::Dark);
        assert!(s.ask_before_deleting(true) && !s.ask_before_deleting(false));
        assert!(s.shell_items_in_menu);
        assert_eq!(s.terminal, Terminal::Auto);
        assert!(s.stay_running && s.tray_icon && !s.start_with_windows);
    }

    #[test]
    fn round_trip_with_the_old_names() {
        let mut s = Settings {
            show_hidden: true,
            startup: Startup::ThisPc,
            new_tab: NewTab::Home,
            theme: ThemeMode::System,
            shell_items_in_menu: false,
            terminal: Terminal::GitBash,
            ..Default::default()
        };
        s.tabs = vec![SavedTab { path: "C:\\Users".into(), sort_column: 1, sort_descending: true, view: None, pinned: false }, SavedTab { path: "".into(), sort_column: 0, sort_descending: false, view: None, pinned: false }];
        s.active_tab = 1;
        s.bounds = Some([10, 20, 1000, 700]);
        s.column_width_map.insert("size".into(), 120.0);
        let text = s.to_json();
        for name in ["\"ShowHidden\": true", "\"Startup\": \"thispc\"", "\"NewTab\": \"home\"", "\"Theme\": \"system\"", "\"ConfirmDelete\": \"permanent\"", "\"ErrorLog\": true", "\"ActiveTab\": 1", "\"ShellItemsInMenu\": false", "\"Terminal\": \"gitbash\""] {
            assert!(text.contains(name), "{name} missing in {text}");
        }
        assert_eq!(parse(&text), Parsed::Good(s));
    }

    #[test]
    fn missing_values_get_defaults_and_unknown_ones_are_ignored() {
        match parse("{\"ShowHidden\": true, \"SomethingNew\": [1, 2]}") {
            Parsed::Good(s) => {
                assert!(s.show_hidden);
                assert!(s.error_log);
                assert_eq!(s.startup, Startup::Home);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn where_updates_come_from_is_always_owner_and_name() {
        let Parsed::Good(s) = parse("{\"UpdateRepo\": \"  \", \"SkippedUpdate\": \"\"}") else { panic!() };
        assert_eq!((s.update_repo.as_str(), s.skipped_update), (UPDATE_REPO, None));
        for bad in ["https://github.com/npr-tami/glint.git", "D:\\\\mirror\\\\glint.git", "glint", "a/b/c", "../x", "a b/c"] {
            let Parsed::Good(s) = parse(&format!("{{\"UpdateRepo\": \"{bad}\"}}")) else { panic!("{bad}") };
            assert_eq!(s.update_repo, UPDATE_REPO, "{bad}");
        }
        let Parsed::Good(s) = parse("{\"UpdateRepo\": \" someone/glint-fork \", \"SkippedUpdate\": \"0.3.1\"}") else { panic!() };
        assert_eq!((s.update_repo.as_str(), s.skipped_update.as_deref()), ("someone/glint-fork", Some("0.3.1")));
        // A file from an older Glint, with the branch it updated from, still reads as a whole.
        let Parsed::Good(s) = parse("{\"UpdateRepo\": \"https://github.com/x/y.git\", \"UpdateBranch\": \"main\", \"ShowHidden\": true}") else { panic!() };
        assert_eq!((s.update_repo.as_str(), s.show_hidden), (UPDATE_REPO, true));
        assert!(!s.to_json().contains("UpdateBranch"));
    }

    #[test]
    fn old_style_tabs_are_read() {
        let Parsed::Good(s) = parse("{\"Tabs\": [\"C:\\\\Users\", {\"Path\": \"D:\\\\\", \"SortColumn\": 3, \"SortDescending\": true}], \"ActiveTab\": 7}") else { panic!() };
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].path, "C:\\Users");
        assert_eq!((s.tabs[1].sort_column, s.tabs[1].sort_descending), (3, true));
        assert_eq!(s.active_tab, 1, "out of range is brought back");
    }

    #[test]
    fn a_wrong_value_only_loses_itself() {
        let text = "\u{feff}{\"ShowHidden\": true, \"Theme\": \"purple\", \"Startup\": null, \"Tabs\": [\"C:\\\\x\"], \"ColumnWidthMap\": {\"size\": 5, \"type\": 200}}";
        match parse(text) {
            Parsed::Partly(s, dropped) => {
                assert!(s.show_hidden);
                assert_eq!(s.theme, ThemeMode::Dark);
                assert_eq!(s.startup, Startup::Home);
                assert_eq!(s.tabs.len(), 1);
                assert_eq!(s.column_width_map.get("type"), Some(&200.0));
                assert_eq!(s.column_width_map.get("size"), None, "too narrow");
                dropped.iter().for_each(|d| assert!(d == "Theme" || d == "Startup", "{d}"));
                assert_eq!(dropped.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn broken_files_are_broken() {
        assert!(matches!(parse("{\"ShowHidden\": tr"), Parsed::Broken(_)));
        assert!(matches!(parse("[1, 2]"), Parsed::Broken(_)));
        assert!(matches!(parse(""), Parsed::Broken(_)));
    }

    #[test]
    fn utf16_files_are_read() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend("{\"ShowHidden\": true}".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert!(matches!(parse(&decode(&bytes)), Parsed::Good(s) if s.show_hidden));
    }

    #[test]
    fn start_tabs() {
        let home = "C:\\Users\\me";
        let mut s = Settings { sort_column: 2, ..Default::default() };
        // Nothing saved yet: home.
        let (tabs, active) = initial_tabs(&s, None, home);
        assert_eq!((tabs.len(), tabs[0].path.as_str(), tabs[0].sort_column, active), (1, home, 2, 0));
        // By default (Home) the saved tabs stay saved, but Glint starts in the home folder.
        s.tabs = ["C:\\a", "C:\\b"].iter().map(|p| SavedTab { path: p.to_string(), sort_column: 1, sort_descending: false, view: None, pinned: false }).collect();
        s.active_tab = 1;
        let (tabs, active) = initial_tabs(&s, None, home);
        assert_eq!((tabs.len(), tabs[0].path.as_str(), active), (1, home, 0));
        // Tabs from last time, with the active one.
        s.startup = Startup::Restore;
        let (tabs, active) = initial_tabs(&s, None, home);
        assert_eq!((tabs.len(), active), (2, 1));
        // A folder from the command line joins them, or reuses its tab.
        let (tabs, active) = initial_tabs(&s, Some("D:\\x"), home);
        assert_eq!((tabs.len(), tabs[2].path.as_str(), active), (3, "D:\\x", 2));
        let (tabs, active) = initial_tabs(&s, Some("c:\\A\\"), home);
        assert_eq!((tabs.len(), active), (2, 0));
        // Other start choices: the command-line folder alone, or the chosen place.
        s.startup = Startup::ThisPc;
        let (tabs, _) = initial_tabs(&s, Some("D:\\x"), home);
        assert_eq!(tabs.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(), vec!["D:\\x"]);
        let (tabs, _) = initial_tabs(&s, None, home);
        assert_eq!(tabs[0].path, "");
        s.startup = Startup::Folder;
        assert_eq!(initial_tabs(&s, None, home).0[0].path, home, "no folder chosen: home");
        s.startup_folder = Some("E:\\Work".into());
        assert_eq!(initial_tabs(&s, None, home).0[0].path, "E:\\Work");
        // Pinned tabs come back first whatever the start says; the fresh tab is the one shown.
        s.tabs[1].pinned = true;
        let (tabs, active) = initial_tabs(&s, None, home);
        assert_eq!(tabs.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(), vec!["C:\\b", "E:\\Work"]);
        assert_eq!(active, 1);
        assert!(tabs[0].pinned);
        // And a pinned tab is saved as one.
        let text = serde_json::to_string(&tabs[0]).unwrap();
        assert!(text.contains("\"Pinned\":true"));
        assert!(serde_json::from_str::<SavedTab>(&text).unwrap().pinned);
    }

    #[test]
    fn two_windows_dont_undo_each_others_choices() {
        // This window read the file with hidden files off and dark, then changed only the theme.
        let synced = choices_of(&Settings::default());
        let ours = Settings { theme: ThemeMode::Light, tabs: vec![SavedTab { path: "C:\\ours".into(), sort_column: 0, sort_descending: false, view: None, pinned: false }], ..Default::default() };
        // Meanwhile the other window turned hidden files on, and a newer Glint added a value.
        let mut disk = choices_of(&Settings { show_hidden: true, ..Default::default() });
        disk.insert("Tabs".into(), serde_json::json!(["C:\\theirs"]));
        disk.insert("FutureThing".into(), serde_json::json!(42));
        let (out, extra) = merge(&ours, Some(&synced), Some(disk), false);
        assert_eq!(out.theme, ThemeMode::Light, "our change wins");
        assert!(out.show_hidden, "their change is kept");
        assert_eq!(out.tabs[0].path, "C:\\ours", "our tabs win");
        assert_eq!(extra.len(), 1);
        // Written in the struct's order, the newer Glint's value at the end.
        let text = serde_json::to_string_pretty(&WithExtras { settings: &out, extra: &extra }).unwrap();
        assert!(text.find("\"Startup\"").unwrap() < text.find("\"Tabs\"").unwrap(), "{text}");
        assert!(text.trim_end().trim_end_matches('}').trim_end().ends_with("\"FutureThing\": 42"), "{text}");
        // Without a known starting point, ours is written as it is.
        let (out, _) = merge(&ours, None, Some(choices_of(&Settings { show_hidden: true, ..Default::default() })), false);
        assert!(!out.show_hidden);
    }

    #[test]
    fn a_bad_value_in_the_file_is_not_copied() {
        let synced = choices_of(&Settings::default());
        let mut disk = Map::new();
        disk.insert("Theme".into(), serde_json::json!("purple"));
        disk.insert("ShowHidden".into(), serde_json::json!(true));
        let (out, _) = merge(&Settings::default(), Some(&synced), Some(disk), false);
        assert_eq!(out.theme, ThemeMode::Dark);
        assert!(out.show_hidden, "the good value next to it still counts");
    }

    /// Every setting is in the file under its name, with its default (dark AMD Radeon by default).
    #[test]
    fn every_setting_has_its_name_and_default() {
        let Value::Object(file) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        let old = [
            ("Pinned", serde_json::json!([])),
            ("PinNames", serde_json::json!({})),
            ("HiddenPlaces", serde_json::json!([])),
            ("Tabs", serde_json::json!([])),
            ("ActiveTab", serde_json::json!(0)),
            ("Maximized", serde_json::json!(false)),
            ("SidebarWidth", serde_json::json!(0)),
            ("ColumnWidthMap", serde_json::json!({})),
            ("SortColumn", serde_json::json!(0)),
            ("SortDescending", serde_json::json!(false)),
            ("Startup", serde_json::json!("home")),
            ("StartupFolder", Value::Null),
            ("Theme", serde_json::json!("dark")),
            ("Size", serde_json::json!("small")),
            ("ShowSidebar", serde_json::json!(true)),
            ("ShowDetails", serde_json::json!(false)),
            ("ShowHidden", serde_json::json!(false)),
            ("ShowSystem", serde_json::json!(false)),
            ("ConfirmDelete", serde_json::json!("permanent")),
            ("Terminal", serde_json::json!("auto")),
            ("HideKnownExtensions", serde_json::json!(false)),
            ("Columns", serde_json::json!(["modified", "type", "size"])),
            ("FoldersFirst", serde_json::json!(true)),
            ("DateFormat", serde_json::json!("default")),
            ("SizeFormat", serde_json::json!("auto")),
            ("SingleClick", serde_json::json!(false)),
            ("Backspace", serde_json::json!("up")),
            ("DoubleClickUp", serde_json::json!(false)),
            ("NewTab", serde_json::json!("same")),
            ("ShellItemsInMenu", serde_json::json!(true)),
            ("ShowStatusBar", serde_json::json!(true)),
            ("Skin", serde_json::json!("amd-radeon")),
            ("Upgraded", serde_json::json!(0)),
            ("CustomSort", serde_json::json!(false)),
            ("CustomViews", serde_json::json!(true)),
            ("SortRecipe", Value::Null),
            ("SortGuideShown", serde_json::json!(false)),
            ("ThemeGuideHidden", serde_json::json!(false)),
            ("StayRunning", serde_json::json!(true)),
            ("TrayIcon", serde_json::json!(true)),
            ("StartWithWindows", serde_json::json!(false)),
            ("RailGroup", serde_json::json!("quick")),
            ("Logo", serde_json::json!("box")),
            // New in 0.3.0 (update.rs).
            ("CheckForUpdates", serde_json::json!(true)),
            ("UpdateRepo", serde_json::json!(UPDATE_REPO)),
            ("SkippedUpdate", Value::Null),
            ("RemindUpdateAfter", serde_json::json!(0)),
        ];
        for (name, value) in old {
            assert_eq!(file.get(name), Some(&value), "{name}");
        }
        // Bounds and the older ColumnWidths are read, and written only when there is something.
        let Parsed::Good(s) = parse("{\"ColumnWidths\": [200, 120, 100, 80], \"Bounds\": [1, 2, 300, 400]}") else { panic!() };
        assert_eq!(s.column_widths.as_deref(), Some(&[200.0, 120.0, 100.0, 80.0][..]));
        assert!(!Settings::default().to_json().contains("ColumnWidths"));
    }

    /// The four old ways Settings opened all open it floating now; the new ones are read back.
    #[test]
    fn the_old_ways_settings_opened_open_it_floating() {
        for old in ["docked", "small", "tab", "free"] {
            let Parsed::Good(s) = parse(&format!("{{\"SettingsOpens\": \"{old}\"}}")) else { panic!("{old}") };
            assert_eq!(s.settings_opens, SettingsOpens::Floating, "{old}");
        }
        let s = Settings { settings_opens: SettingsOpens::Panel, settings_pages: SettingsPages::List, settings_style: SettingsStyle::Sentences, settings_skins: SettingsSkins::Tiles, ..Default::default() };
        let text = serde_json::to_string(&s).unwrap();
        assert!(text.contains("\"SettingsOpens\":\"panel\"") && text.contains("\"SettingsStyle\":\"sentences\""));
        let Parsed::Good(back) = parse(&text) else { panic!() };
        assert_eq!((back.settings_opens, back.settings_pages, back.settings_style, back.settings_skins), (SettingsOpens::Panel, SettingsPages::List, SettingsStyle::Sentences, SettingsSkins::Tiles));
        // By default: floating, tabs, ticks, everything, a drop-down; search, dots and RESET PAGE on.
        let d = Settings::default();
        assert_eq!((d.settings_opens, d.settings_pages, d.settings_style, d.settings_show, d.settings_skins), (SettingsOpens::Floating, SettingsPages::Tabs, SettingsStyle::Ticks, SettingsShow::All, SettingsSkins::List));
        assert!(d.settings_search && d.settings_dots && d.settings_reset && !d.settings_last_page);
    }

    #[test]
    fn the_old_values_are_read() {
        let text = "{\"Size\": \"large\", \"DateFormat\": \"iso\", \"SizeFormat\": \"kb\", \"Backspace\": \"back\", \"RailGroup\": \"pc\", \
                    \"Columns\": [\"Size\", \"created\", \"nonsense\", \"size\"], \"Pinned\": [\"C:\\\\Work\"], \"PinNames\": {\"C:\\\\Work\": \"Work\"}, \
                    \"StayRunning\": false, \"Skin\": \" amd-radeon-light \", \"SettingsPlace\": [10, 20, 440, 520]}";
        let Parsed::Good(s) = parse(text) else { panic!() };
        assert_eq!((s.size, s.date_format, s.size_format, s.backspace, s.rail_group), (ListSize::Large, DateFormat::Iso, SizeFormat::Kb, Backspace::Back, RailGroup::Pc));
        assert_eq!(s.columns, vec!["created", "size"], "known columns only, once each, in their order");
        assert_eq!(s.pinned, vec!["C:\\Work"]);
        assert_eq!(s.pin_names.get("C:\\Work").map(String::as_str), Some("Work"));
        assert!(!s.stay_running);
        assert_eq!(s.skin, "amd-radeon-light", "spaces around a skin id go");
        assert_eq!(s.settings_place, Some([10, 20, 440, 520]));
        // A silly size is forgotten; a wrong value only loses itself.
        let Parsed::Good(s) = parse("{\"SettingsPlace\": [0, 0, 5, 5]}") else { panic!() };
        assert_eq!(s.settings_place, None);
        assert!(matches!(parse("{\"Size\": \"huge\", \"ShowHidden\": true}"), Parsed::Partly(s, d) if s.show_hidden && s.size == ListSize::Small && d == ["Size"]));
    }

    /// "View" and each tab's own view: written by name, read back, and a view this Glint doesn't
    /// know loses only itself (the tab stays).
    #[test]
    fn views_are_remembered() {
        assert_eq!(serde_json::to_value(Settings::default()).unwrap().get("View"), Some(&serde_json::json!("details")));
        let text = "{\"View\": \"extralarge\", \"Tabs\": [{\"Path\": \"C:\\\\a\", \"View\": \"tree\"}, {\"Path\": \"C:\\\\b\", \"View\": \"wobbly\"}, \"C:\\\\c\"]}";
        let Parsed::Good(s) = parse(text) else { panic!() };
        assert_eq!(s.view, ViewMode::ExtraLarge);
        assert_eq!(s.tabs.iter().map(|t| t.view).collect::<Vec<_>>(), vec![Some(ViewMode::Tree), None, None]);
        assert_eq!(s.tabs[1].path, "C:\\b");
        // Written back with the tab's view, and without one when it has none.
        let json = s.to_json();
        assert!(json.contains("\"View\": \"tree\"") && json.contains("\"View\": \"extralarge\""));
        assert_eq!(json.matches("\"View\"").count(), 2);
        // Every view by its key, and Explorer's Ctrl+Shift+1 to 6.
        for (v, _) in ViewMode::ALL {
            assert_eq!(ViewMode::from_key(v.key()), Some(v));
            assert_eq!(serde_json::to_value(v).unwrap(), serde_json::json!(v.key()));
        }
        assert_eq!(ViewMode::for_key(1), Some(ViewMode::ExtraLarge));
        assert_eq!(ViewMode::for_key(6), Some(ViewMode::Details));
        assert_eq!(ViewMode::for_key(0), None);
        assert_eq!(ViewMode::for_key(7), None);
        assert!(matches!(parse("{\"View\": \"huge\"}"), Parsed::Partly(s, d) if s.view == ViewMode::Details && d == ["View"]));
    }

    #[test]
    fn moving_a_folder() {
        let base = std::env::temp_dir().join(format!("glint-settings-move-test-{}", std::process::id()));
        let (from, to) = (base.join("from"), base.join("to"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(from.join("skins")).unwrap();
        std::fs::write(from.join(FILE_NAME), "{\"ShowHidden\": true}").unwrap();
        std::fs::write(from.join("skins").join("mine.skin"), "x").unwrap();
        std::fs::create_dir_all(to.join("skins")).unwrap();
        std::fs::write(to.join(FILE_NAME), "older").unwrap();
        std::fs::write(to.join("skins").join("other.skin"), "y").unwrap();
        // Into a folder that's there: file by file, the moved ones replacing theirs.
        move_folder(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(std::fs::read_to_string(to.join(FILE_NAME)).unwrap(), "{\"ShowHidden\": true}");
        assert_eq!(std::fs::read_to_string(to.join("skins").join("mine.skin")).unwrap(), "x");
        assert!(to.join("skins").join("other.skin").exists(), "what was there and isn't replaced stays");
        assert_eq!(std::fs::read_dir(&to).unwrap().count(), 2, "nothing extra is left (no .old)");
        // Where nothing is yet: one rename. A folder that isn't there moves nothing.
        move_folder(&to, &base.join("renamed")).unwrap();
        assert!(!to.exists() && base.join("renamed").join("skins").join("other.skin").exists());
        move_folder(&base.join("missing"), &base.join("made")).unwrap();
        assert!(!base.join("made").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn whose_files_tells_where_a_waiting_move_goes() {
        let base = std::env::temp_dir().join(format!("glint-whose-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (exe, home, local) = (base.join("Glint tools (x86)"), base.join("Roaming").join("Glint"), base.join("Local").join("Glint"));
        let glint_settings = serde_json::to_string_pretty(&Settings::default()).unwrap();
        std::fs::create_dir_all(&exe).unwrap();
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::write(home.join("config").join(FILE_NAME), &glint_settings).unwrap();
        assert_eq!(portable_now_and_after(&exe, &home), (false, false));
        // Portable on, asked for: still installed now, portable once the next start has settled.
        std::fs::write(home.join("config").join(MOVE_NOTE), "portable\r\n").unwrap();
        assert_eq!(portable_now_and_after(&exe, &home), (false, true));
        assert!(settle_in(&exe, &home, &local).is_empty());
        // What every later glint.exe works out is what the settled Glint worked out before it moved.
        assert_eq!(portable_now_and_after(&exe, &home), (true, true));
        // Switched on and straight back off (the note says where they are): nothing changes.
        let config = exe.join(PORTABLE_FOLDER).join("config");
        std::fs::write(config.join(MOVE_NOTE), "portable\r\n").unwrap();
        assert_eq!(portable_now_and_after(&exe, &home), (true, true));
        // Off again.
        std::fs::write(config.join(MOVE_NOTE), "installed\r\n").unwrap();
        assert_eq!(portable_now_and_after(&exe, &home), (true, false));
        assert!(settle_in(&exe, &home, &local).is_empty());
        assert_eq!(portable_now_and_after(&exe, &home), (false, false));
        // A note with nonsense in it is no move.
        std::fs::write(home.join("config").join(MOVE_NOTE), "sideways\r\n").unwrap();
        assert_eq!(portable_now_and_after(&exe, &home), (false, false));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn portable_moves_the_files_at_the_next_start() {
        let base = std::env::temp_dir().join(format!("glint-portable-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (exe, home, local) = (base.join("Glint"), base.join("Roaming").join("Glint"), base.join("Local").join("Glint"));
        let glint_settings = serde_json::to_string_pretty(&Settings::default()).unwrap();
        std::fs::create_dir_all(&exe).unwrap();
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::write(home.join("config").join(FILE_NAME), &glint_settings).unwrap();
        std::fs::write(home.join("config").join(format!("{FILE_NAME}.old")), "left by an older Glint").unwrap();
        // An older Glint kept its logs and cache in %LOCALAPPDATA%: the logs come along, the cache goes.
        std::fs::create_dir_all(local.join("logs")).unwrap();
        std::fs::create_dir_all(local.join("cache")).unwrap();
        std::fs::write(local.join("logs").join("error.log"), "an error").unwrap();
        std::fs::write(local.join("cache").join("Glint-0.8.0.exe"), "x").unwrap();
        assert!(settle_in(&exe, &home, &local).is_empty());
        let f = find_in(Some(&exe), &home);
        assert!(!f.portable && f.config == home.join("config"));
        assert!(home.join("logs").join("error.log").exists() && !local.exists());
        assert!(!home.join("config").join(format!("{FILE_NAME}.old")).exists());
        // Portable on, from Settings: the note, then the move at the next start.
        std::fs::write(f.config.join(MOVE_NOTE), "portable\r\nGlint moves its files...\r\n").unwrap();
        assert!(settle_in(&exe, &home, &local).is_empty());
        let f = find_in(Some(&exe), &home);
        assert!(f.portable && f.config == exe.join(PORTABLE_FOLDER).join("config"));
        assert_eq!(std::fs::read_to_string(f.config.join(FILE_NAME)).unwrap(), glint_settings);
        assert!(f.logs.join("error.log").exists());
        assert!(!f.config.join(MOVE_NOTE).exists() && !home.exists(), "nothing is left behind");
        // And off again.
        std::fs::write(f.config.join(MOVE_NOTE), "installed\r\n").unwrap();
        assert!(settle_in(&exe, &home, &local).is_empty());
        let f = find_in(Some(&exe), &home);
        assert!(!f.portable && f.config == home.join("config") && f.config.join(FILE_NAME).exists());
        assert!(!exe.join(PORTABLE_FOLDER).exists());
        // A note for where the files are already changes nothing.
        std::fs::write(f.config.join(MOVE_NOTE), "installed\r\n").unwrap();
        assert!(settle_in(&exe, &home, &local).is_empty());
        assert!(find_in(Some(&exe), &home).config.join(FILE_NAME).exists() && !f.config.join(MOVE_NOTE).exists());
        // An older Glint's portable files straight next to glint.exe go into "Glint data" (only
        // Glint's own: its error logs and downloads).
        std::fs::create_dir_all(exe.join("config")).unwrap();
        std::fs::create_dir_all(exe.join("logs")).unwrap();
        std::fs::create_dir_all(exe.join("cache")).unwrap();
        std::fs::write(exe.join("config").join(FILE_NAME), &glint_settings).unwrap();
        std::fs::write(exe.join("logs").join("error.log"), "an error").unwrap();
        std::fs::write(exe.join("cache").join("Glint-0.9.0.exe"), "x").unwrap();
        std::fs::write(exe.join("cache").join("theirs.txt"), "x").unwrap();
        assert!(find_in(Some(&exe), &home).portable, "still portable before it moves");
        assert!(settle_in(&exe, &home, &local).is_empty());
        let f = find_in(Some(&exe), &home);
        assert!(f.portable && f.config == exe.join(PORTABLE_FOLDER).join("config") && f.config.join(FILE_NAME).exists());
        assert!(f.logs.join("error.log").exists() && !exe.join("config").exists() && !exe.join("logs").exists());
        assert!(!exe.join("cache").join("Glint-0.9.0.exe").exists() && exe.join("cache").join("theirs.txt").exists());
        // An empty "Glint data" made by hand makes Glint portable; one with only leftovers doesn't.
        let _ = std::fs::remove_dir_all(exe.join(PORTABLE_FOLDER));
        std::fs::create_dir_all(exe.join(PORTABLE_FOLDER)).unwrap();
        assert!(find_in(Some(&exe), &home).portable);
        std::fs::write(exe.join(PORTABLE_FOLDER).join("notes.txt"), "x").unwrap();
        assert!(!find_in(Some(&exe), &home).portable);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn another_programs_config_folder_doesnt_make_glint_portable() {
        let dir = std::env::temp_dir().join(format!("glint-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(glint_config(&dir), "an empty config folder: made to make Glint portable");
        std::fs::write(dir.join("app.ini"), "[window]").unwrap();
        assert!(!glint_config(&dir), "another program's files, and no settings.json");
        std::fs::write(dir.join(FILE_NAME), "{\"theme\": \"blue\", \"servers\": [1, 2]}").unwrap();
        assert!(!glint_config(&dir), "another program's settings");
        std::fs::write(dir.join(FILE_NAME), serde_json::to_string_pretty(&Settings::default()).unwrap()).unwrap();
        assert!(glint_config(&dir), "Glint's own");
        std::fs::write(dir.join(FILE_NAME), "{\"Startup\": \"home\", oops").unwrap();
        assert!(glint_config(&dir), "Glint's own, with a typo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clearing_the_cache_leaves_subfolders_alone() {
        let dir = std::env::temp_dir().join(format!("glint-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.bin"), [0u8; 100]).unwrap();
        std::fs::write(dir.join("sub").join("keep.txt"), "x").unwrap();
        assert_eq!(cache_files(&dir).iter().map(|(_, n)| n).sum::<u64>(), 100, "only the files right in it count");
        clear_files(&dir);
        assert!(!dir.join("a.bin").exists());
        assert!(dir.join("sub").join("keep.txt").exists(), "nothing in a subfolder is touched");
        assert!(cache_files(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pins_and_hidden_places_use_the_old_names() {
        let mut s = Settings { pinned: vec!["C:\\Work".into(), "D:\\".into()], hidden_places: vec!["music".into()], ..Default::default() };
        s.pin_names.insert("C:\\Work".into(), "Job".into());
        let text = s.to_json();
        for name in ["\"Pinned\": [", "\"PinNames\": {", "\"HiddenPlaces\": [", "\"Job\""] {
            assert!(text.contains(name), "{name} missing in {text}");
        }
        assert_eq!(parse(&text), Parsed::Good(s));
        // A hand-edited file reads as it is; doubles, blanks and names of unpinned folders go.
        let old = r#"{"Pinned": ["C:\\A", " ", "c:\\a\\", "C:\\B"], "PinNames": {"C:\\A": "Alpha", "C:\\Gone": "x", "C:\\B": " "}, "HiddenPlaces": ["recycle", ""]}"#;
        let Parsed::Good(s) = parse(old) else { panic!("{old}") };
        assert_eq!(s.pinned, vec!["C:\\A", "C:\\B"]);
        assert_eq!(s.pin_names.len(), 1);
        assert_eq!(s.pin_names.get("C:\\A").map(String::as_str), Some("Alpha"));
        assert_eq!(s.hidden_places, vec!["recycle"]);
        // More favorites than a file brings back: the newest (last) stay.
        let many: Vec<String> = (0..MAX_PINS + 2).map(|i| format!("C:\\f{i}")).collect();
        let Parsed::Good(s) = parse(&Settings { favorites: many.clone(), ..Default::default() }.to_json()) else { panic!() };
        assert_eq!(s.favorites, many[2..]);
        // Pins are choices: a pin made in another window isn't undone by this one.
        let synced = choices_of(&Settings::default());
        let disk = choices_of(&Settings { pinned: vec!["E:\\Theirs".into()], ..Default::default() });
        let (out, _) = merge(&Settings::default(), Some(&synced), Some(disk), false);
        assert_eq!(out.pinned, vec!["E:\\Theirs"]);
        // Both windows pinned, unpinned and named folders since: neither's changes are lost.
        let base = Settings { pinned: vec!["C:\\A".into(), "C:\\B".into()], ..Default::default() };
        let synced = choices_of(&base);
        let mut ours = Settings { pinned: vec!["C:\\A".into(), "C:\\Ours".into(), "C:\\Both".into()], ..Default::default() };
        ours.pin_names.insert("C:\\A".into(), "Alpha".into());
        let mut theirs = Settings { pinned: vec!["C:\\B".into(), "C:\\A".into(), "c:\\both".into(), "C:\\Theirs".into()], ..Default::default() };
        theirs.pin_names.insert("C:\\B".into(), "Beta".into());
        let (out, _) = merge(&ours, Some(&synced), Some(choices_of(&theirs)), false);
        assert_eq!(out.pinned, vec!["C:\\A", "C:\\Ours", "C:\\Both", "C:\\Theirs"]);
        assert_eq!(out.pin_names.len(), 1, "B was unpinned here, so its name goes");
        assert_eq!(out.pin_names.get("C:\\A").map(String::as_str), Some("Alpha"));
    }

    /// A tab set saved in each window: both are kept.
    #[test]
    fn tab_sets_saved_in_two_windows_are_both_kept() {
        let set = |name: &str| TabSet { name: name.into(), tabs: vec![SavedTab { path: format!("C:\\{name}"), sort_column: 0, sort_descending: false, view: None, pinned: false }] };
        let synced = choices_of(&Settings::default());
        let ours = Settings { tab_sets: vec![set("Home")], ..Default::default() };
        let disk = choices_of(&Settings { tab_sets: vec![set("Work")], ..Default::default() });
        let (out, _) = merge(&ours, Some(&synced), Some(disk), false);
        assert_eq!(out.tab_sets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Home", "Work"]);
    }

    #[test]
    fn running_choices_use_the_old_names() {
        let s = Settings { stay_running: false, tray_icon: false, start_with_windows: true, ..Default::default() };
        let text = s.to_json();
        for name in ["\"StayRunning\": false", "\"TrayIcon\": false", "\"StartWithWindows\": true"] {
            assert!(text.contains(name), "{name} missing in {text}");
        }
        assert_eq!(parse(&text), Parsed::Good(s));
        // A file from before they existed: the defaults.
        let Parsed::Good(old) = parse("{\"ShowHidden\": true}") else { panic!() };
        assert!(old.stay_running && old.tray_icon && !old.start_with_windows);
    }

    #[test]
    fn an_extra_window_writes_only_choices() {
        // The extra window read the file, then turned hidden files on; its own tabs don't count.
        let synced = choices_of(&Settings::default());
        let mut ours = Settings { show_hidden: true, tabs: vec![SavedTab { path: "C:\\extra".into(), sort_column: 0, sort_descending: false, view: None, pinned: false }], ..Default::default() };
        ours.forget_window_state();
        assert!(ours.tabs.is_empty() && ours.show_hidden);
        // Meanwhile the main window saved its tabs and place.
        let mut disk = choices_of(&Settings::default());
        disk.insert("Tabs".into(), serde_json::json!(["C:\\main"]));
        disk.insert("ActiveTab".into(), serde_json::json!(0));
        disk.insert("Bounds".into(), serde_json::json!([10, 20, 900, 600]));
        disk.insert("Maximized".into(), serde_json::json!(true));
        let (out, _) = merge(&ours, Some(&synced), Some(disk.clone()), true);
        assert!(out.show_hidden, "the extra window's choice is written");
        assert_eq!(out.tabs[0].path, "C:\\main", "the main window's tabs stay");
        assert_eq!((out.bounds, out.maximized), (Some([10, 20, 900, 600]), true));
        // A main window writes its own state, as before.
        let (out, _) = merge(&ours, Some(&synced), Some(disk), false);
        assert!(out.tabs.is_empty());
    }

    #[test]
    fn sort_numbers() {
        for key in [SortKey::Name, SortKey::Date, SortKey::Type, SortKey::Size, SortKey::Created, SortKey::Dimensions, SortKey::Length] {
            assert_eq!(sort_key(sort_number(key)), key);
        }
        assert_eq!(sort_key(9), SortKey::Name);
        // Sorted by the search results' Folder column: comes back sorted by name.
        assert_eq!(sort_key(sort_number(SortKey::Folder)), SortKey::Name);
    }

    #[test]
    fn list_options_use_the_old_names_and_defaults() {
        let s = Settings::default();
        assert_eq!(s.columns, vec!["modified", "type", "size"]);
        assert!(s.folders_first && s.show_sidebar && !s.hide_known_extensions && !s.single_click && !s.double_click_up);
        assert_eq!((s.size, s.date_format, s.size_format, s.backspace), (ListSize::Small, DateFormat::Default, SizeFormat::Auto, Backspace::Up));
        let text = s.to_json();
        for name in [
            "\"Size\": \"small\"",
            "\"ShowSidebar\": true",
            "\"HideKnownExtensions\": false",
            "\"FoldersFirst\": true",
            "\"DateFormat\": \"default\"",
            "\"SizeFormat\": \"auto\"",
            "\"SingleClick\": false",
            "\"Backspace\": \"up\"",
            "\"DoubleClickUp\": false",
        ] {
            assert!(text.contains(name), "{name} missing in {text}");
        }
        // Older settings read as they are (hand-edited columns tidied up).
        let old = "{\"Size\": \"large\", \"Columns\": [\"Size\", \"created\", \"name\", \"created\", \"bogus\"], \"DateFormat\": \"relative\", \
                   \"SizeFormat\": \"kb\", \"SingleClick\": true, \"Backspace\": \"back\", \"DoubleClickUp\": true, \
                   \"HideKnownExtensions\": true, \"FoldersFirst\": false, \"ShowSidebar\": false, \"SortColumn\": 5}";
        let Parsed::Good(s) = parse(old) else { panic!("{:?}", parse(old)) };
        assert_eq!(s.columns, vec!["created", "size"], "known ones, once each, in the list's order");
        assert_eq!((s.size, s.date_format, s.size_format, s.backspace), (ListSize::Large, DateFormat::Relative, SizeFormat::Kb, Backspace::Back));
        assert!(s.single_click && s.double_click_up && s.hide_known_extensions && !s.folders_first && !s.show_sidebar);
        assert_eq!(sort_key(s.sort_column), SortKey::Created);
        // A value Glint doesn't know only loses itself.
        let Parsed::Partly(s, dropped) = parse("{\"DateFormat\": \"fancy\", \"SingleClick\": true}") else { panic!() };
        assert_eq!((s.date_format, s.single_click, dropped), (DateFormat::Default, true, vec!["DateFormat".to_string()]));
    }

    #[test]
    fn columns_switch_on_and_off_in_their_order() {
        let mut s = Settings::default();
        s.set_column("created", true);
        assert_eq!(s.columns, vec!["modified", "created", "type", "size"]);
        s.set_column("modified", false);
        s.set_column("type", true);
        assert_eq!(s.columns, vec!["created", "type", "size"]);
        assert!(!s.shows_column("modified") && s.shows_column("CREATED"));
        // Picture size and Length (off unless asked for) go after Size, and are read back.
        assert!(!Settings::default().shows_column("dimensions") && !Settings::default().shows_column("length"));
        s.set_column("length", true);
        s.set_column("dimensions", true);
        assert_eq!(s.columns, vec!["created", "type", "size", "dimensions", "length"]);
        s.column_width_map.insert("length".into(), 90.0);
        let Parsed::Good(back) = parse(&s.to_json()) else { panic!() };
        assert_eq!((back.columns, back.column_width_map.get("length")), (s.columns.clone(), Some(&90.0)));
    }

    fn saved(path: &str, pinned: bool) -> SavedTab {
        SavedTab { path: path.into(), sort_column: 0, sort_descending: false, view: None, pinned }
    }

    #[test]
    fn pinned_tabs_come_back_first_whatever_the_start_says() {
        let s = Settings { startup: Startup::Home, tabs: vec![saved("C:\\a", false), saved("C:\\p", true), saved("C:\\q", true)], ..Default::default() };
        let (tabs, active) = initial_tabs(&s, None, "C:\\home");
        assert_eq!(tabs.iter().map(|t| t.path.as_str()).collect::<Vec<_>>(), ["C:\\p", "C:\\q", "C:\\home"]);
        assert_eq!(active, 2, "the home tab, not a pinned one");
        // A folder from outside that a pinned tab shows is that tab; any other opens after them.
        let (tabs, active) = initial_tabs(&s, Some("c:\\q\\"), "C:\\home");
        assert_eq!((tabs.len(), active), (2, 1));
        let (tabs, active) = initial_tabs(&s, Some("C:\\new"), "C:\\home");
        assert_eq!((tabs.len(), active), (3, 2));
        // Restoring keeps the saved order and the active tab; a wrong number is held in.
        let s = Settings { startup: Startup::Restore, tabs: vec![saved("C:\\p", true), saved("C:\\a", false)], active_tab: 40, ..Default::default() };
        assert_eq!(initial_tabs(&s, None, "C:\\home").1, 1);
    }

    #[test]
    fn a_pinned_tab_and_the_wheel_setting_survive_saving_and_reading() {
        let mut s = Settings { ctrl_wheel_view: true, tabs: vec![saved("Z:\\gone \u{1F600}", true), saved("C:\\a", false)], ..Default::default() };
        s.split_folder = Some("Z:\\gone \u{1F600}".into());
        let Parsed::Good(back) = parse(&s.to_json()) else { panic!("not understood") };
        assert!(back.ctrl_wheel_view);
        assert_eq!(back.tabs, s.tabs);
        assert_eq!(back.split_folder, s.split_folder);
        // Off is the default, and an old file without the names reads fine.
        assert!(!Settings::default().ctrl_wheel_view);
        let Parsed::Good(old) = parse(r#"{"Tabs": ["C:\\a"]}"#) else { panic!("not understood") };
        assert!(!old.tabs[0].pinned && !old.ctrl_wheel_view);
    }

    #[test]
    fn tab_sets_read_from_a_hand_edited_file_are_tidied() {
        let set = |name: &str, n: usize| TabSet { name: name.into(), tabs: (0..n).map(|i| saved(&format!("C:\\{i}"), false)).collect() };
        let mut s = Settings { tab_sets: vec![set("A", 1), set(" ", 1), set("empty", 0), set("a ", 2), set("B", 1)], ..Default::default() };
        s.tidy();
        // No empty names, no empty sets, each name once (the later one wins, in capitals or not).
        assert_eq!(s.tab_sets.iter().map(|t| (t.name.as_str(), t.tabs.len())).collect::<Vec<_>>(), [("a", 2), ("B", 1)]);
        let mut many = Settings { tab_sets: (0..MAX_TAB_SETS + 10).map(|n| set(&format!("s{n}"), 1)).collect(), ..Default::default() };
        many.tidy();
        assert_eq!(many.tab_sets.len(), MAX_TAB_SETS);
        assert_eq!(many.tab_sets.last().map(|t| t.name.as_str()), Some("s39"));
    }

    #[test]
    fn a_settings_file_that_cant_be_read_is_not_written_over() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("glint-read-disk-test-{} x y", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        // No file yet: nothing to merge with.
        assert!(read_disk(&path).unwrap().is_none());
        // A file: what's in it.
        std::fs::write(&path, "\u{feff}{\"Tabs\": [\"C:\\\\Work\"], \"ShowHidden\": true}").unwrap();
        let disk = read_disk(&path).unwrap().unwrap();
        assert_eq!(disk.get("ShowHidden"), Some(&Value::Bool(true)));
        // Held by another program with no sharing: an error, never "no file" (which would let a
        // save write over it without the other window's tabs).
        {
            let _held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
            assert!(read_disk(&path).is_err());
        }
        // Let go of within the tries: read.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(&path).unwrap();
        let let_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(70));
            drop(held);
        });
        assert!(read_disk(&path).unwrap().is_some());
        let_go.join().unwrap();
        // Broken: written afresh (as before), so a broken file gets mended.
        std::fs::write(&path, "{ not json").unwrap();
        assert!(read_disk(&path).unwrap().is_none());
        std::fs::write(&path, "").unwrap();
        assert!(read_disk(&path).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
