//! Remembering things between runs: the folders Glint keeps its files in, and settings.json.
//!
//! **Folders.** Normally:
//! - `%APPDATA%\Glint\config` for settings.json and the `skins` folder (skin.rs);
//! - `%LOCALAPPDATA%\Glint\cache` for things Glint can always make again;
//! - `%LOCALAPPDATA%\Glint\logs` for error.log.
//!
//! **Portable mode:** when a folder called `config` sits next to glint.exe (empty, or with Glint's
//! own settings.json - not another program's), Glint uses `config\`, `cache\` and `logs\` next to
//! glint.exe instead, so the whole folder can go on a USB stick. (`GLINT_SETTINGS_DIR` points all
//! three somewhere else, for testing.)
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
//!   undone by the other: when writing, a choice this window didn't change is taken from the file.
//!   Values this version of Glint doesn't know (from a newer version) are kept as they are.
//! - An extra window (Ctrl+N, life.rs) writes only choices: the tabs and the window's place in the
//!   file stay the main window's (`set_extra_window`).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_RoamingAppData};

use crate::folder::SortKey;
use crate::log;
use crate::shell;

/// The settings file's name, in the config folder.
pub const FILE_NAME: &str = "settings.json";
/// How many tabs a settings file may bring back (a hand-edited file with thousands would only hurt).
const MAX_TABS: usize = 100;
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
    #[allow(dead_code)] // nothing is cached on disk yet (thumbnails, later)
    pub cache: PathBuf,
    /// error.log.
    pub logs: PathBuf,
    /// True when the folders sit next to glint.exe (a `config` folder was there at start).
    pub portable: bool,
}

/// The folders, worked out once (main.rs asks first, at start).
pub fn folders() -> &'static Folders {
    static FOLDERS: OnceLock<Folders> = OnceLock::new();
    FOLDERS.get_or_init(find_folders)
}

fn find_folders() -> Folders {
    if let Some(dir) = std::env::var_os("GLINT_SETTINGS_DIR").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        return Folders { cache: dir.join("cache"), logs: dir.join("logs"), config: dir, portable: false };
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    if let Some(dir) = exe_dir.filter(|d| d.join("config").is_dir() && glint_config(&d.join("config"))) {
        return Folders { config: dir.join("config"), cache: dir.join("cache"), logs: dir.join("logs"), portable: true };
    }
    let (roaming, local) = installed_dirs();
    Folders { config: roaming.join("config"), cache: local.join("cache"), logs: local.join("logs"), portable: false }
}

/// Keys every settings.json Glint writes has, and other programs' hardly would.
const GLINT_KEYS: [&str; 6] = ["\"ActiveTab\"", "\"Startup\"", "\"ShowHidden\"", "\"ConfirmDelete\"", "\"HiddenPlaces\"", "\"StayRunning\""];

/// Whether a `config` folder next to glint.exe is Glint's: it holds no settings.json yet (made empty
/// to make Glint portable), or Glint's own. A folder of that name that belongs to another program
/// (glint.exe put in a folder with other tools) must not make Glint portable: Glint would rewrite that
/// program's settings.json as its own, and "Clear cache" would empty the `cache` folder beside it.
fn glint_config(dir: &Path) -> bool {
    match std::fs::read(dir.join(FILE_NAME)) {
        Ok(bytes) => {
            let text = decode(&bytes);
            // (By its words, not by reading it: a Glint settings.json with a typo in it is still Glint's.)
            text.trim().trim_start_matches('\u{feff}').is_empty() || GLINT_KEYS.iter().any(|k| text.contains(k))
        }
        // Not there; or there but unreadable right now, which load() handles (no saving then).
        Err(_) => true,
    }
}

/// "%APPDATA%\Glint" and "%LOCALAPPDATA%\Glint": where Glint keeps its files when it
/// isn't portable.
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

/// The settings file's full path (in the config folder in use now, see `config_now`).
pub fn file_path() -> PathBuf {
    config_now().join(FILE_NAME)
}

// ---------------------------------------------------------------- portable, switched while running

/// The config folder in use since Portable was switched in the Settings window, and whether it is
/// the portable one. None: the one Glint started with (`folders()`).
static CONFIG_NOW: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);

/// The config folder settings.json is written to now: the one Glint started with, or the other one
/// once Portable was switched (Settings > About), which takes effect at once.
pub fn config_now() -> PathBuf {
    match &*CONFIG_NOW.lock().unwrap_or_else(PoisonError::into_inner) {
        Some((dir, _)) => dir.clone(),
        None => folders().config.clone(),
    }
}

/// Whether settings are kept next to glint.exe now (see `config_now`).
pub fn portable_now() -> bool {
    match &*CONFIG_NOW.lock().unwrap_or_else(PoisonError::into_inner) {
        Some((_, portable)) => *portable,
        None => folders().portable,
    }
}

/// Whether the folders were pointed elsewhere for testing (GLINT_SETTINGS_DIR): then Portable can't
/// be switched.
pub fn test_folders() -> bool {
    std::env::var_os("GLINT_SETTINGS_DIR").is_some_and(|d| !d.is_empty())
}

/// Moves the settings next to glint.exe (`on`) or back to %APPDATA% (Settings > About > Portable).
/// Touches the disk: call it on a background thread.
/// Returns what to tell the person, or why it couldn't be done.
///
/// Nothing is ever deleted:
/// - Turning it on copies the config folder into a new `config` folder next to glint.exe, and
///   settings are written there from now on.
/// - Turning it off copies that folder back to %APPDATA% (an older settings.json there is kept as
///   settings.json.old) and then renames the folder next to glint.exe to `config.off`, because a
///   folder called `config` there is what makes Glint portable at the next start.
///
/// Settings aren't saved while this runs, so no save lands in the folder being left behind. The
/// cache and error.log move at the next start.
pub fn set_portable(on: bool) -> Result<String, String> {
    if test_folders() {
        return Err("Portable can't be switched while GLINT_SETTINGS_DIR is set (it's for testing).".to_string());
    }
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).ok_or("Glint couldn't find out where glint.exe is.")?;
    let portable = exe_dir.join("config");
    let installed = installed_dirs().0.join("config");
    if same_path(&portable.to_string_lossy(), &installed.to_string_lossy()) {
        // glint.exe sits in Glint's own %APPDATA% folder: both are the same folder.
        return Err(format!("glint.exe is already in the settings folder ({}).", exe_dir.display()));
    }
    // Holding the writer: a save waits until the folder has moved (and then goes to the new one).
    let _no_saving = writer();
    if portable_now() == on {
        return Ok(format!("The settings are already in {}.", config_now().display()));
    }
    if on {
        if portable.is_dir() && !glint_config(&portable) {
            return Err(format!("The config folder next to glint.exe ({}) belongs to another program. Put Glint in a folder of its own first.", portable.display()));
        }
        let made = !portable.exists();
        if let Err(e) = std::fs::create_dir_all(&portable) {
            return Err(format!("Glint can't write to {} ({e}). Move Glint to a folder you can write to.", exe_dir.display()));
        }
        let mut copied = Vec::new();
        if let Err(e) = copy_folder(&installed, &portable, &mut copied) {
            // Take back only what this just made: the copies, then the folders if that left them empty.
            for file in copied.iter().rev() {
                let _ = std::fs::remove_file(file);
            }
            if made {
                remove_empty_folders(&portable);
            }
            return Err(format!("Couldn't copy the settings next to glint.exe ({}): {e}", exe_dir.display()));
        }
        *CONFIG_NOW.lock().unwrap_or_else(PoisonError::into_inner) = Some((portable.clone(), true));
        Ok(format!("Settings moved to {}. Restart Glint to finish.", portable.display()))
    } else {
        let mut copied = Vec::new();
        if let Err(e) = std::fs::create_dir_all(&installed).and_then(|_| copy_folder(&portable, &installed, &mut copied)) {
            return Err(format!("Couldn't copy the settings to {}: {e}", installed.display()));
        }
        let off = free_name(&exe_dir, "config.off");
        if let Err(e) = std::fs::rename(&portable, &off) {
            return Err(format!(
                "The settings were copied to {}, but the config folder next to glint.exe couldn't be renamed ({e}). Close any file in it and try again.",
                installed.display()
            ));
        }
        *CONFIG_NOW.lock().unwrap_or_else(PoisonError::into_inner) = Some((installed.clone(), false));
        Ok(format!(
            "Settings moved to {}. The old folder is now {} (safe to delete). Restart Glint to finish.",
            installed.display(),
            off.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())
        ))
    }
}

/// Whether a file system entry is a link or junction (never followed, copied or deleted here).
fn is_link(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_type().is_symlink() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

/// Copies the files of `from` into `to`, subfolders too (links and junctions are left out), and
/// lists each copy made. A settings.json already in `to` is kept as settings.json.old first. A
/// missing `from` copies nothing. Each copy is checked to be as long as its original.
fn copy_folder(from: &Path, to: &Path, copied: &mut Vec<PathBuf>) -> std::io::Result<()> {
    copy_level(from, to, copied, 0)
}

fn copy_level(from: &Path, to: &Path, copied: &mut Vec<PathBuf>, depth: u32) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if depth > 8 {
        return Ok(()); // nothing Glint writes is this deep
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
            copy_level(&entry.path(), &target, copied, depth + 1)?;
            continue;
        }
        if depth == 0 && entry.file_name().eq_ignore_ascii_case(FILE_NAME) && target.exists() {
            std::fs::copy(&target, target.with_file_name(format!("{FILE_NAME}.old")))?;
        }
        std::fs::copy(entry.path(), &target)?;
        copied.push(target.clone());
        // (What std::fs::copy says it copied isn't reliable everywhere - under Wine it's 0 - so the
        // copy itself is measured.)
        if std::fs::metadata(&target)?.len() != meta.len() {
            return Err(std::io::Error::other(format!("{} wasn't copied whole", entry.path().display())));
        }
    }
    Ok(())
}

/// Removes `dir` and the folders in it, as far as they are empty (a folder with anything in it
/// stays: `remove_dir` only removes empty folders).
fn remove_empty_folders(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if std::fs::symlink_metadata(entry.path()).is_ok_and(|m| m.is_dir() && !is_link(&m)) {
                remove_empty_folders(&entry.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// `name` in `dir`, or "name 2", "name 3"... when it's taken.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let candidate = if n == 1 { dir.join(name) } else { dir.join(format!("{name} {n}")) };
        if !candidate.exists() || n > 999 {
            return candidate;
        }
        n += 1;
    }
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

/// The extra columns next to Name, by their names in settings.json ("Columns"), in the list's order.
pub const COLUMN_NAMES: [&str; 4] = ["modified", "created", "type", "size"];

/// What the title bars show at the left: the skin's word in its slanted box (GLINT unless the skin
/// says otherwise), the chrome wordmark (logo.rs), or nothing (the tabs start at the left edge).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogoStyle {
    #[default]
    Box,
    Chrome,
    None,
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
}

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
    },
}

impl From<SavedTabForm> for SavedTab {
    fn from(form: SavedTabForm) -> SavedTab {
        match form {
            SavedTabForm::Path(path) => SavedTab { path, sort_column: 0, sort_descending: false, view: None },
            SavedTabForm::Full { path, sort_column, sort_descending, view } => {
                SavedTab { path, sort_column, sort_descending, view: view.as_deref().and_then(ViewMode::from_key) }
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

    // ----- the file list (list.rs)
    /// Row and text size: small | medium | large.
    pub size: ListSize,
    /// The sidebar is shown (Ctrl+B hides it).
    pub show_sidebar: bool,
    /// Leave known extensions out of names ("report" for report.pdf), like Explorer's "Hide
    /// extensions for known file types". The Settings window shows it the other way round, as
    /// "Show file name extensions".
    pub hide_known_extensions: bool,
    /// The columns next to Name: any of "modified", "created", "type", "size".
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
    /// The sort order editor's guide is hidden.
    pub sort_guide_hidden: bool,
    /// The skin editor's guide is hidden.
    pub theme_guide_hidden: bool,
    /// The group Glint Minimal's icon rail shows: quick | pinned | pc.
    pub rail_group: RailGroup,
    /// "Logo": box | chrome | none.
    pub logo: LogoStyle,
    /// The Settings window's size in DIPs (width, height), when it was last resized.
    pub settings_size: Option<[f32; 2]>,
    /// The details panel's width in DIPs (dragged at its left edge); 0 = the standard width (side.rs).
    pub details_width: f32,
    /// The Space preview's width in DIPs; 0 = half of the room next to the sidebar.
    pub preview_width: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            startup: Startup::Home,
            startup_folder: None,
            theme: ThemeMode::Dark,
            new_tab: NewTab::Same,
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
            sort_column: 0,
            sort_descending: false,
            sidebar_width: 0,
            column_widths: None,
            upgraded: 0,
            sort_guide_hidden: false,
            theme_guide_hidden: false,
            rail_group: RailGroup::Quick,
            logo: LogoStyle::Box,
            settings_size: None,
            details_width: 0.0,
            preview_width: 0.0,
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
    // The sidebar's (sidebar.rs, app/places.rs).
    "Pinned",
    "PinNames",
    "HiddenPlaces",
    // How Glint runs (life.rs).
    "StayRunning",
    "TrayIcon",
    "StartWithWindows",
    // The title bars (logo.rs).
    "Logo",
    // Updates (update.rs).
    "CheckForUpdates",
    "UpdateRepo",
    "SkippedUpdate",
    "RemindUpdateAfter",
];

/// How many pinned folders a settings file may bring back.
const MAX_PINS: usize = 200;

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
            matches!(key.as_str(), "modified" | "created" | "type" | "size" | "folder" | "free" | "location") && w.is_finite() && (COLUMN_MIN..=COLUMN_MAX).contains(w)
        });
        if self.startup_folder.as_deref().is_some_and(|f| f.trim().is_empty()) {
            self.startup_folder = None;
        }
        // Pins: no blanks, no folder twice (Windows paths ignore case), a sane number.
        let mut pinned: Vec<String> = Vec::new();
        for p in self.pinned.drain(..).map(|p| p.trim().to_string()) {
            if !p.is_empty() && !pinned.iter().any(|q| same_path(q, &p)) {
                pinned.push(p);
            }
        }
        pinned.truncate(MAX_PINS);
        self.pinned = pinned;
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
        if self.settings_size.is_some_and(|[w, h]| !(w.is_finite() && h.is_finite() && w >= 200.0 && h >= 150.0 && w <= 20000.0 && h <= 20000.0)) {
            self.settings_size = None;
        }
        // The side pane's widths: 0 (standard) or a sane width.
        for w in [&mut self.details_width, &mut self.preview_width] {
            if !w.is_finite() || *w < 0.0 || *w > 20000.0 {
                *w = 0.0;
            }
        }
    }
}

/// A sort column as the number saved in settings.json (0 Name, 1 Date
/// modified, 2 Type, 3 Size, 4 Folder (search results), 5 Date created).
pub fn sort_number(key: SortKey) -> u32 {
    match key {
        SortKey::Name => 0,
        SortKey::Date => 1,
        SortKey::Type => 2,
        SortKey::Size => 3,
        SortKey::Folder => 4,
        SortKey::Created => 5,
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
        _ => SortKey::Name,
    }
}

/// Whether a saved sort column number is one Glint knows.
fn known_sort_number(n: u32) -> bool {
    matches!(n, 0..=5)
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

/// Whether two folder paths are the same folder (Windows paths ignore case).
fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').to_lowercase() == b.trim_end_matches('\\').to_lowercase()
}

/// The tabs to open at start and which one is active, from "When Glint starts, open" and the folder
/// given on the command line (if any). A folder from outside opens on its own,
/// except with "Tabs from last time", where last time's tabs come back too (like a browser), so
/// they aren't lost; the folder is then the active tab (an already open tab for it is reused).
pub fn initial_tabs(s: &Settings, opened: Option<&str>, home: &str) -> (Vec<SavedTab>, usize) {
    let plain = |path: &str| SavedTab { path: path.to_string(), sort_column: s.sort_column, sort_descending: s.sort_descending, view: None };
    let restore = s.startup == Startup::Restore;
    let mut tabs = match s.startup {
        _ if opened.is_some() && !restore => Vec::new(),
        Startup::Restore => s.tabs.clone(),
        Startup::Home => vec![plain(home)],
        Startup::ThisPc => vec![plain("")],
        Startup::Folder => vec![plain(s.startup_folder.as_deref().unwrap_or(home))],
    };
    let mut active = if restore { s.active_tab.min(tabs.len().saturating_sub(1)) } else { 0 };
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

/// The text of a file: UTF-8, or UTF-16 when it starts with that marker (Notepad's "Unicode").
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
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
    for (key, value) in disk {
        let choice = CHOICES.contains(&key.as_str());
        if !fields.contains_key(&key) {
            extra.insert(key, value);
        } else if (choice && synced.is_some_and(|s| s.get(&key) == mine.get(&key))) || (!choice && state_from_disk) {
            let mut trial = fields.clone();
            trial.insert(key, value);
            if let Ok(s) = serde_json::from_value::<Settings>(Value::Object(trial.clone())) {
                (fields, out) = (trial, s);
            }
        }
    }
    (out, extra)
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

/// One save, in order: skipped when a newer one already reached the disk. Returns false if the
/// file couldn't be written.
fn write_in_order(s: &Settings, number: u64) -> bool {
    let mut w = writer();
    if w.written > number {
        return true;
    }
    let path = file_path();
    let disk = std::fs::read(&path).ok().and_then(|b| match serde_json::from_str::<Value>(decode(&b).trim_start_matches('\u{feff}')) {
        Ok(Value::Object(m)) => Some(m),
        _ => None,
    });
    let (merged, extra) = merge(s, w.synced.as_ref(), disk, is_extra_window());
    let text = serde_json::to_string_pretty(&WithExtras { settings: &merged, extra: &extra }).unwrap_or_else(|_| s.to_json());
    match write_atomically(&path, &text) {
        Ok(()) => {
            w.written = number;
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
pub fn save_in_background(s: Settings) -> bool {
    if SAVING_OFF.load(Ordering::SeqCst) {
        return false;
    }
    let number = NEXT_SAVE.fetch_add(1, Ordering::SeqCst);
    shell::background("save settings", move || {
        write_in_order(&s, number);
    })
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
        s.tabs = vec![SavedTab { path: "C:\\Users".into(), sort_column: 1, sort_descending: true, view: None }, SavedTab { path: "".into(), sort_column: 0, sort_descending: false, view: None }];
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
        s.tabs = ["C:\\a", "C:\\b"].iter().map(|p| SavedTab { path: p.to_string(), sort_column: 1, sort_descending: false, view: None }).collect();
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
    }

    #[test]
    fn two_windows_dont_undo_each_others_choices() {
        // This window read the file with hidden files off and dark, then changed only the theme.
        let synced = choices_of(&Settings::default());
        let ours = Settings { theme: ThemeMode::Light, tabs: vec![SavedTab { path: "C:\\ours".into(), sort_column: 0, sort_descending: false, view: None }], ..Default::default() };
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
            ("SortRecipe", Value::Null),
            ("SortGuideHidden", serde_json::json!(false)),
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

    #[test]
    fn the_old_values_are_read() {
        let text = "{\"Size\": \"large\", \"DateFormat\": \"iso\", \"SizeFormat\": \"kb\", \"Backspace\": \"back\", \"RailGroup\": \"pc\", \
                    \"Columns\": [\"Size\", \"created\", \"nonsense\", \"size\"], \"Pinned\": [\"C:\\\\Work\"], \"PinNames\": {\"C:\\\\Work\": \"Work\"}, \
                    \"StayRunning\": false, \"Skin\": \" amd-radeon-light \", \"SettingsSize\": [900, 700]}";
        let Parsed::Good(s) = parse(text) else { panic!() };
        assert_eq!((s.size, s.date_format, s.size_format, s.backspace, s.rail_group), (ListSize::Large, DateFormat::Iso, SizeFormat::Kb, Backspace::Back, RailGroup::Pc));
        assert_eq!(s.columns, vec!["created", "size"], "known columns only, once each, in their order");
        assert_eq!(s.pinned, vec!["C:\\Work"]);
        assert_eq!(s.pin_names.get("C:\\Work").map(String::as_str), Some("Work"));
        assert!(!s.stay_running);
        assert_eq!(s.skin, "amd-radeon-light", "spaces around a skin id go");
        assert_eq!(s.settings_size, Some([900.0, 700.0]));
        // A silly size is forgotten; a wrong value only loses itself.
        let Parsed::Good(s) = parse("{\"SettingsSize\": [5, 5]}") else { panic!() };
        assert_eq!(s.settings_size, None);
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
    fn copying_a_settings_folder() {
        let base = std::env::temp_dir().join(format!("glint-settings-copy-test-{}", std::process::id()));
        let (from, to) = (base.join("from"), base.join("to"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(from.join("themes")).unwrap();
        std::fs::write(from.join(FILE_NAME), "{\"ShowHidden\": true}").unwrap();
        std::fs::write(from.join("themes").join("mine.css"), "x").unwrap();
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join(FILE_NAME), "older").unwrap();
        let mut copied = Vec::new();
        copy_folder(&from, &to, &mut copied).unwrap();
        assert_eq!(copied.len(), 2);
        assert_eq!(std::fs::read_to_string(to.join(FILE_NAME)).unwrap(), "{\"ShowHidden\": true}");
        assert_eq!(std::fs::read_to_string(to.join(format!("{FILE_NAME}.old"))).unwrap(), "older", "the older file is kept");
        assert_eq!(std::fs::read_to_string(to.join("themes").join("mine.css")).unwrap(), "x");
        // A folder that isn't there copies nothing; emptying only removes empty folders.
        copy_folder(&base.join("missing"), &to, &mut copied).unwrap();
        remove_empty_folders(&to);
        assert!(to.join(FILE_NAME).exists(), "a folder with files in it stays");
        assert_eq!(free_name(&base, "from"), base.join("from 2"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn another_programs_config_folder_doesnt_make_glint_portable() {
        let dir = std::env::temp_dir().join(format!("glint-config-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(glint_config(&dir), "an empty config folder: made to make Glint portable");
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
        // Pins are choices: a pin made in another window isn't undone by this one.
        let synced = choices_of(&Settings::default());
        let disk = choices_of(&Settings { pinned: vec!["E:\\Theirs".into()], ..Default::default() });
        let (out, _) = merge(&Settings::default(), Some(&synced), Some(disk), false);
        assert_eq!(out.pinned, vec!["E:\\Theirs"]);
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
        let mut ours = Settings { show_hidden: true, tabs: vec![SavedTab { path: "C:\\extra".into(), sort_column: 0, sort_descending: false, view: None }], ..Default::default() };
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
        for key in [SortKey::Name, SortKey::Date, SortKey::Type, SortKey::Size, SortKey::Created] {
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
    }
}
