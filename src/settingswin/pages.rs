//! What the Settings window shows: its five pages (General, Appearance, Files, Keyboard shortcuts, About).
//! Every row has a short title, a note only where the title can't say enough, a control, and the
//! setting it changes - by its name in settings.json ("ShowHidden"), so a row works with whatever
//! Rust type holds it.
//!
//! **For whoever connects a feature** (the integrator, or the next version):
//! - [`LIVE`] lists what works in this version. A row whose `key` isn't in it stays hidden until
//!   it works. Add the key when the feature is in.
//! - [`after_change`] is what happens right away when a setting changes in the window (besides
//!   being saved, which always happens). Settings that are read when they're used (New tabs open,
//!   Ask before deleting, the terminal...) need nothing there.
//! - [`outside`] and [`set_outside`] are the switches whose value isn't in settings.json but in
//!   Windows (Glint as the default file explorer, Win+E): keys starting with "@".

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::app::AppState;
use crate::folder;
use crate::life;
use crate::settings::{DateFormat, Settings, SizeFormat, Terminal, COLUMN_NAMES};
use crate::update;
use crate::ui::Command;

/// The pages, in the order of the tabs (or the list on the left).
pub const PAGES: [&str; 5] = ["General", "Appearance", "Files", "Keyboard shortcuts", "About"];
/// Where SETTINGS > About Glint goes: the About page, at the row about updates.
pub const ABOUT: usize = 4;
pub const ABOUT_ROW: &str = "Updates";

/// The (i) next to "System info" in About.
pub const SETUP_INFO: &str = "Copies your Glint and Windows versions and the settings that change how Glint works. Paste it into a bug report: then whoever looks at the bug can set up Glint the same way and see it happen. No file, folder, user or PC names are included.";

/// Why "Alternative Win+E handling" is there, behind its (i).
pub const TEMP_WIN_E_FIX: &str = "Windows 11 sometimes takes Win+E back for File Explorer, like after a restart. This keeps a small part of Glint running in the background so Win+E keeps opening Glint, even when Glint is closed.";

/// How to close Glint fully, behind the (i) of "Keep running in the background".
pub const STAY_RUNNING: &str = "When on, closing the window only hides Glint, so it opens again instantly. To close it fully, right-click the Settings button or the tray icon and pick Exit Glint. With the tray icon hidden, open Glint first, then use the Settings button.";

/// When "Install updates automatically" installs, and that Glint doesn't restart, behind its (i).
pub const UPDATE_AUTOMATICALLY: &str = "Installs a new version as soon as an automatic check finds one, without asking. Glint doesn't restart: the new version opens the next time Glint starts, and the status bar says so.";

/// What "Row size" changes, behind its (i).
pub const ROW_SIZE: &str = "Only changes spacing.";

/// Where the columns picked in Settings show, under "Default columns".
pub const DEFAULT_COLUMNS: &str = "The same columns show in every folder.";

/// The file size the "Size format" choices show as their example.
const SIZE_EXAMPLE: u64 = 3_247_000;
/// From Windows' first second (1601) to 1970's, to turn the time now into a file's kind of time.
const SECONDS_1601_TO_1970: u64 = 11_644_473_600;

/// What works in this version: settings by their name in settings.json, and "@" names for the
/// rest (buttons, things Windows keeps). Rows about anything else are hidden.
pub const LIVE: &[&str] = &[
    "Logo",
    // General
    "Startup",
    "StartupFolder",
    "NewTab", "HomeOpens", "HomeColumns", "HomeTiles",
    "SettingsOpens",
    "SettingsPages",
    "SettingsStyle",
    "SettingsShow",
    "SettingsSkins",
    "SettingsSearch",
    "SettingsDots",
    "SettingsReset",
    "SettingsLastPage",
    "RememberSplit",
    "@Folders",
    "@WinE",
    "WinEAfterQuit",
    "StayRunning",
    "TrayIcon",
    "StartWithWindows",
    "ShellItemsInMenu",
    "Terminal",
    // Appearance
    "Skin",
    "Theme",
    "ZipIcon",
    "@SkinEditor",
    "SkinsWindow",
    "Size",
    "ShowSidebar",
    "ShowDetails",
    "ShowStatusBar",
    "StatusCount", "StatusSelected", "StatusFree", "StatusTotal", "StatusOpened", "StatusStarted", "StatusMemory", "DetailsWidth",
    "View",
    // Files and folders
    "ShowHidden",
    "ShowSystem",
    "HideKnownExtensions",
    "Columns",
    "DateFormat",
    "SizeFormat",
    "FoldersFirst",
    "CustomSort",
    "CustomViews",
    "SortWindow",
    "ConfirmDelete",
    // Mouse and keyboard
    "SingleClick",
    "DoubleClickUp",
    "CtrlWheelView",
    "DiskBarClick",
    "Backspace",
    "@Search",
    "@Pins",
    "@DiskUsage",
    "@Preview",
    // Glint itself
    "@About",
    "@Update",
    "CheckForUpdates",
    "UpdateAutomatically",
    "@ReportBug",
    "@BugInfo",
    "@SettingsFolder",
    "@Portable",
    "@Cache",
    "ErrorLog",
    "@LogsFolder",
    // The shortcuts that work
    "@Keys",
];

/// Every setting the command box (Ctrl+U) can find: its title and its page, in the pages' order.
/// The shortcut list (Keys' last group) and rows without a title are left out.
pub fn findable(settings: &Settings) -> Vec<(String, usize)> {
    let values = match serde_json::to_value(settings) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let facts = plain_facts(&values);
    let mut out: Vec<(String, usize)> = Vec::new();
    for i in 0..PAGES.len() {
        let mut heading = "";
        for item in page(i, &facts) {
            let rows = match item {
                Item::Heading(h) => {
                    heading = h;
                    continue;
                }
                Item::Card(rows) => rows,
            };
            for row in rows {
                // Round buttons with no question of their own go by their group's title.
                let title = if row.title.is_empty() && matches!(row.ctl, Ctl::Choice(..)) { heading } else { row.title.trim() };
                if title.is_empty() || !live(row.key) || matches!(row.ctl, Ctl::Text(_) | Ctl::Shortcut(..)) || out.iter().any(|(t, _)| t == title) {
                    continue;
                }
                out.push((title.to_string(), i));
            }
        }
    }
    out
}

/// The pages' facts without anything looked up (the command box, `title_of`).
pub fn plain_facts(values: &Map<String, Value>) -> Facts<'_> {
    Facts {
        values,
        terminals: None,
        config: String::new(),
        logs: String::new(),
        cache: String::new(),
        cache_size: None,
        cache_said: None,
        clear_asked: false,
        portable_said: None,
        bug_said: None,
        info_open: None,
        portable_locked: false,
        skins: &[],
        update_said: String::new(),
        update_busy: false,
    }
}

/// What Settings calls the setting `key` (its name in settings.json): its row's title - or its
/// group's, for round buttons without a question of their own and for the columns. None for a
/// setting no row shows.
pub fn title_of(key: &str) -> Option<String> {
    let Ok(Value::Object(values)) = serde_json::to_value(Settings::default()) else { return None };
    let facts = plain_facts(&values);
    for i in 0..PAGES.len() {
        let mut heading = "";
        for item in page(i, &facts) {
            let rows = match item {
                Item::Heading(h) => {
                    heading = h;
                    continue;
                }
                Item::Card(rows) => rows,
            };
            for row in rows {
                let bound = match row.ctl {
                    Ctl::Toggle(k) | Ctl::ToggleNot(k) | Ctl::ButtonToggle(_, _, k) | Ctl::Choice(k, _) => Some(k),
                    _ => None,
                };
                if row.key != key && bound != Some(key) {
                    continue;
                }
                let title = if row.title.is_empty() || matches!(row.ctl, Ctl::ToggleIn(..)) { heading } else { row.title.trim() };
                if !title.is_empty() {
                    return Some(title.to_string());
                }
            }
        }
    }
    None
}

/// Whether the row about `key` works in this version.
pub fn live(key: &str) -> bool {
    LIVE.contains(&key)
}

/// What happens right away when the setting `key` got a new value in `app.settings` (from the
/// Settings window; `before` is the settings as they were). A command that comes back is the
/// window's to run (light or dark repaints the whole window).
pub fn after_change(key: &str, before: &Settings, app: &mut AppState) -> Option<Command> {
    match key {
        // Light or dark picks the skin's light or dark version (AppState::set_dark keeps "Skin" in
        // step with what's on screen).
        "Theme" => Some(Command::SetTheme(app.settings.theme)),
        // A skin picked: "Light/dark mode" follows it (app/skins.rs); the windows follow the look.
        "Skin" => {
            let id = app.settings.skin.clone();
            app.pick_skin(&id);
            None
        }
        // Every title bar follows (the style rides on the theme).
        "Logo" => {
            app.apply_skin();
            None
        }
        // Windows is told (registry.rs). Until zip or 7z files open in Glint there's nothing to see.
        "ZipIcon" => {
            match life::apply_zip_icon(app.settings.zip_icon) {
                Ok(true) => {}
                Ok(false) => app.say("The icon shows once zip and 7z files open in Glint."),
                Err(e) => app.say(e),
            }
            None
        }
        "ShowHidden" => {
            let on = app.settings.show_hidden;
            app.set_show_hidden(on);
            None
        }
        "ShowSystem" => {
            let on = app.settings.show_system;
            app.set_show_system(on);
            None
        }
        // The Win+E catcher starts or ends, and its Run value comes or goes (catcher.rs).
        "WinEAfterQuit" => {
            crate::catcher::sync(crate::catcher::wanted(app.settings.win_e_after_quit));
            None
        }
        // Every tab is sorted again (by the rules, or by its column).
        "CustomSort" => {
            let on = app.settings.custom_sort;
            app.set_custom_sort(on, None);
            None
        }
        "CustomViews" => {
            let on = app.settings.custom_views;
            app.set_custom_views(on);
            None
        }
        "ErrorLog" => {
            let on = app.settings.error_log;
            app.set_error_log(on);
            None
        }
        // How Glint runs (life.rs).
        "StayRunning" => {
            let on = app.settings.stay_running;
            life::apply_stay_running(&mut app.settings, on);
            let keys = app.keys(crate::keymap::Action::ExitGlint);
            app.say(if on { crate::keymap::say_with(&keys, "Closing now hides Glint ({keys} exits).", "Closing now hides Glint.") } else { "Closing now exits Glint.".to_string() });
            None
        }
        "TrayIcon" => {
            let on = app.settings.tray_icon;
            life::apply_tray_icon(&mut app.settings, on);
            None
        }
        "StartWithWindows" => {
            // The setting only says what the registry says: when Windows says no, it goes back.
            let on = app.settings.start_with_windows;
            app.settings.start_with_windows = !on;
            match life::apply_start_with_windows(&mut app.settings, on) {
                Ok(()) if on => app.say("Glint now starts with Windows."),
                Ok(()) => app.say("Glint no longer starts with Windows."),
                Err(e) => app.say(e),
            }
            None
        }
        // The file list's (list.rs reads the rest - dates, sizes, extensions, clicks, Backspace - when
        // it uses them).
        "Size" => {
            // Changed from the size before, so every tab keeps the same rows on screen.
            let size = app.settings.size;
            app.settings.size = before.size;
            app.set_list_size(size);
            None
        }
        "ShowSidebar" => Some(Command::ShowSidebar(app.settings.show_sidebar)),
        "ShowDetails" => Some(Command::ShowDetails(app.settings.show_details)),
        "ShowStatusBar" => Some(Command::ShowStatusBar(app.settings.show_status_bar)),
        // Every tab shows its files this way now (the VIEW button changes one tab).
        "View" => {
            let view = app.settings.view;
            app.set_view_everywhere(view);
            None
        }
        "FoldersFirst" => {
            let on = app.settings.folders_first;
            app.set_folders_first(on);
            None
        }
        "Columns" => {
            // A tab sorted by a column that went away is sorted by name again.
            for name in COLUMN_NAMES {
                let on = app.settings.shows_column(name);
                app.set_column(name, on);
            }
            None
        }
        _ => None,
    }
}

/// What the registry said about the "@" switches, and when: looked up again after a second at the
/// most (they're drawn every frame; another Glint or a command line may have changed them meanwhile).
static OUTSIDE: Mutex<Option<(Instant, bool, bool)>> = Mutex::new(None);

/// The value of a switch kept by Windows rather than settings.json ("@Folders": folders and drives
/// open in Glint; "@WinE": Win+E opens Glint). None: not one of them.
pub fn outside(key: &str) -> Option<bool> {
    if key == "@UpdateReady" {
        // CHECK FOR UPDATES can be pressed: this is the main Glint, and no check or install is
        // under way (update.rs).
        return Some(update::about(life::is_main(), true).1);
    }
    if key != "@Folders" && key != "@WinE" {
        return None;
    }
    let mut known = OUTSIDE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (folders, win_e) = match *known {
        Some((at, f, w)) if at.elapsed() < Duration::from_secs(1) => (f, w),
        _ => {
            let (f, w) = (life::default_explorer_on(), life::win_e_on());
            *known = Some((Instant::now(), f, w));
            (f, w)
        }
    };
    Some(if key == "@Folders" { folders } else { win_e })
}

/// Changes such a switch (only ever when the person flips it). Returns why it couldn't be done.
pub fn set_outside(key: &str, on: bool) -> Result<(), String> {
    let result = match key {
        "@Folders" => life::apply_default_explorer(on),
        "@WinE" => life::apply_win_e(on),
        _ => Err(format!("{key} isn't in this version yet.")),
    };
    // Read back from the registry the next time it's drawn.
    *OUTSIDE.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    result
}

// ---------------------------------------------------------------- the rows

/// A value a drop-down can choose, as it's written in settings.json.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    S(&'static str),
    B(bool),
    /// A number (a width, how many in a row), saved as settings.json writes an f32.
    N(f32),
}

impl Val {
    pub fn json(self) -> Value {
        match self {
            Val::S(s) => Value::String(s.to_string()),
            Val::B(b) => Value::Bool(b),
            Val::N(n) => Value::from(n as f64),
        }
    }
}

/// Something a button does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Open the config folder in a Glint tab.
    OpenSettingsFolder,
    /// Open the logs folder in a Glint tab.
    OpenLogsFolder,
    ClearCache,
    Update,
    SkinEditor,
    SortEditor,
    /// GitHub's issues page, to report a bug.
    ReportBug,
    /// Copy the bug info (bugreport.rs) to the clipboard.
    CopyBugInfo,
    /// Glint's Discord (not there yet: the row is hidden).
    Discord,
}

/// The control on the right of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Ctl {
    /// An on/off switch for a true/false setting.
    Toggle(&'static str),
    /// The same, the other way round: on when the setting is false ("Show file extensions" is
    /// HideKnownExtensions).
    ToggleNot(&'static str),
    /// On when the list setting `.0` holds `.1` (a column in Columns).
    ToggleIn(&'static str, &'static str),
    /// A drop-down list: the setting and its choices with their names.
    Choice(&'static str, Vec<(Val, String)>),
    Button(String, Act),
    /// A button and a switch side by side (EDIT… and the sort rules).
    ButtonToggle(String, Act, &'static str),
    /// Text in a second column (the shortcuts that can't be changed).
    Text(&'static str),
    /// A shortcut that can be changed: its name in settings.json's "Shortcuts" (keymap.rs), the keys
    /// as they show on its button, and whether it differs from its default (then RESET shows too).
    Shortcut(&'static str, String, bool),
    /// The text box for the start folder, with BROWSE.
    Folder,
    /// Nothing: only words (Glint's version on About).
    None,
}

/// One row of a card.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub title: String,
    pub note: Option<String>,
    pub ctl: Ctl,
    /// What the row is about: a setting's name in settings.json or an "@" name. Rows not in
    /// [`LIVE`] are greyed with "Coming later".
    pub key: &'static str,
    /// Pulled in a little: it belongs to the row above it (the tray icon under background mode).
    pub indent: bool,
    /// Usable only while this switch is on - or, written "!Name", while it's off.
    pub needs: Option<&'static str>,
    /// Shown only while this setting has this value (the start folder, for "A specific folder").
    pub only_if: Option<(&'static str, Val)>,
    /// Not an everyday setting: hidden behind "More" while Settings shows the basics.
    pub more: bool,
    /// An (i) after the title: clicking it shows (or hides) why the setting is there, as its note.
    pub info: bool,
    /// A choice shown as round buttons, one per line, also when choices are ticks (a short choice,
    /// as File Explorer shows them).
    pub radios: bool,
    /// A choice that is always a drop-down (too many choices, or long ones, to spell out).
    pub long: bool,
}

impl Row {
    fn new(key: &'static str, title: &str, note: Option<&str>, ctl: Ctl) -> Row {
        Row { title: title.to_string(), note: note.map(str::to_string), ctl, key, indent: false, needs: None, only_if: None, more: false, radios: false, long: false, info: false }
    }
    fn info(mut self) -> Row {
        self.info = true;
        self
    }
    fn more(mut self) -> Row {
        self.more = true;
        self
    }
    fn radios(mut self) -> Row {
        self.radios = true;
        self
    }
    fn long(mut self) -> Row {
        self.long = true;
        self
    }
    fn indent(mut self) -> Row {
        self.indent = true;
        self
    }
    fn needs(mut self, key: &'static str) -> Row {
        self.needs = Some(key);
        self
    }
    fn only_if(mut self, key: &'static str, v: Val) -> Row {
        self.only_if = Some((key, v));
        self
    }
    /// Whether the row works in this version.
    pub fn live(&self) -> bool {
        live(self.key)
    }
}

/// A page is headings and groups of rows.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Heading(&'static str),
    Card(Vec<Row>),
}

/// What the pages need to know that changes while Glint runs.
pub struct Facts<'a> {
    /// The settings as they are written in settings.json.
    pub values: &'a Map<String, Value>,
    /// Which terminals are installed (in `Terminal::ALL`'s order), once looked up.
    pub terminals: Option<[bool; 6]>,
    pub config: String,
    pub logs: String,
    pub cache: String,
    /// How much the cache holds, once measured.
    pub cache_size: Option<u64>,
    /// What pressing CLEAR said last.
    pub cache_said: Option<String>,
    /// CLEAR was pressed once and waits for a second press (it says PRESS AGAIN).
    pub clear_asked: bool,
    /// What switching Portable said last.
    pub portable_said: Option<String>,
    /// What COPY next to System info said last.
    pub bug_said: Option<String>,
    /// The row whose (i) is open: its words show as its note.
    pub info_open: Option<&'static str>,
    /// Portable can't be switched (the folders were pointed elsewhere for testing).
    pub portable_locked: bool,
    /// Every skin, built-in ones first: id, name and its one line about itself (skin.rs).
    pub skins: &'a [(String, String, String)],
    /// What About says about updates (update::about), and whether a check is under way.
    pub update_said: String,
    pub update_busy: bool,
}

/// A skin's id as the `&'static str` a drop-down's choice holds. Skin ids aren't fixed words (a
/// person's own are file names), so each is kept once for the rest of the run - a handful of short
/// strings.
fn interned(id: &str) -> &'static str {
    static KEPT: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(s) = kept.iter().find(|s| **s == id) {
        return s;
    }
    let s: &'static str = Box::leak(id.to_string().into_boxed_str());
    kept.push(s);
    s
}

fn toggle(key: &'static str, title: &str, note: Option<&str>) -> Row {
    Row::new(key, title, note, Ctl::Toggle(key))
}

fn choice(key: &'static str, title: &str, note: Option<&str>, choices: &[(Val, &str)]) -> Row {
    Row::new(key, title, note, Ctl::Choice(key, choices.iter().map(|(v, l)| (*v, l.to_string())).collect()))
}

/// A shortcut and what it does; `key` says whether it works yet.
fn keys(key: &'static str, keys: &'static str, what: &str) -> Row {
    Row::new(key, what, None, Ctl::Text(keys))
}

/// The rows of page `i` (see [`PAGES`]).
pub fn page(i: usize, f: &Facts) -> Vec<Item> {
    let items = match i {
        0 => general(f),
        1 => look(f),
        2 => files(),
        3 => keys_page(f),
        _ => about(f),
    };
    // Rows about what isn't in this version yet stay hidden until it works (see LIVE).
    items
        .into_iter()
        .map(|item| match item {
            Item::Card(rows) => Item::Card(rows.into_iter().filter(Row::live).collect()),
            heading => heading,
        })
        .collect()
}

fn general(f: &Facts) -> Vec<Item> {
    use Val::{B, S};
    let terminals: Vec<(Val, String)> = Terminal::ALL
        .iter()
        .enumerate()
        .map(|(i, (t, name))| {
            let missing = *t != Terminal::Auto && f.terminals.is_some_and(|found| !found[i]);
            let value = serde_json::to_value(t).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            (S(terminal_key(&value)), if missing { format!("{name} (not found)") } else { name.to_string() })
        })
        .collect();
    // As File Explorer's Folder Options: a group's title asks, its round buttons and ticks answer.
    vec![
        Item::Heading("Startup"),
        Item::Card(vec![
            choice("Startup", "On startup, open", None, &[(S("restore"), "Previous tabs"), (S("home"), "User folder"), (S("thispc"), "This PC"), (S("folder"), "A specific folder")]).long(),
            Row::new("StartupFolder", "", None, Ctl::Folder).only_if("Startup", S("folder")),
            toggle("RememberSplit", "Restore split view on startup", None).more(),
        ]),
        Item::Heading("Home page"),
        Item::Card(vec![
            choice("HomeOpens", "Home button opens", None, &[(S("page"), "Home page"), (S("folder"), "User folder")]).long().more(),
            choice("HomeColumns", "Maximum columns on Home", None, &[(Val::N(2.0), "Up to 2"), (Val::N(3.0), "Up to 3"), (Val::N(4.0), "Up to 4"), (Val::N(5.0), "Up to 5"), (Val::N(6.0), "Up to 6")]).only_if("HomeOpens", S("page")).more(),
            choice("HomeTiles", "Home icon size", None, &[(S("small"), "Small"), (S("normal"), "Medium"), (S("large"), "Large")]).only_if("HomeOpens", S("page")).more(),
        ]),
        Item::Heading("New tabs"),
        Item::Card(vec![choice("NewTab", "", None, &[(S("same"), "Current folder"), (S("home"), "Home"), (S("thispc"), "This PC")]).radios()]),
        Item::Heading("Opening items"),
        Item::Card(vec![
            choice("SingleClick", "", None, &[(B(true), "Single-click to open"), (B(false), "Double-click to open")]).radios(),
            toggle("DoubleClickUp", "Double-click empty space to go up", None),
            toggle("CtrlWheelView", "Ctrl+scroll wheel changes view", None).more(),
            toggle("DiskBarClick", "Click a drive's usage bar to open Disk space", None).more(),
        ]),
        Item::Heading("Background"),
        Item::Card(vec![
            toggle("StayRunning", "Keep running in the background", f.info_open.filter(|k| *k == "StayRunning").map(|_| STAY_RUNNING)).info(),
            toggle("TrayIcon", "Show system tray icon", None).indent().needs("StayRunning"),
            toggle("StartWithWindows", "Start with Windows", None),
        ]),
        Item::Heading("Default file explorer"),
        Item::Card(vec![
            toggle("@Folders", "Open folders and drives in Glint", None),
            toggle("@WinE", "Win+E opens Glint", None),
            toggle("WinEAfterQuit", "Alternative Win+E handling", f.info_open.filter(|k| *k == "WinEAfterQuit").map(|_| TEMP_WIN_E_FIX)).indent().needs("@WinE").info(),
        ]),
        Item::Heading("Right-click menu"),
        Item::Card(vec![
            toggle("ShellItemsInMenu", "Show menu commands from other apps", None).more(),
            Row::new("Terminal", "Open terminals in", None, Ctl::Choice("Terminal", terminals)).long().more(),
        ]),
    ]
}

/// The terminal's value in settings.json as a `&'static str` (they're fixed words).
fn terminal_key(value: &str) -> &'static str {
    ["auto", "wt", "powershell", "pwsh", "cmd", "gitbash"].into_iter().find(|k| *k == value).unwrap_or("auto")
}

/// The skin choices: the built-in skins, then the person's own. A skin whose file is gone still
/// shows by its id, so the list never looks empty.
pub fn skin_choices(f: &Facts) -> Vec<(Val, String)> {
    let current = f.values.get("Skin").and_then(Value::as_str).unwrap_or("");
    let mut skins: Vec<(Val, String)> = f.skins.iter().map(|(id, name, _)| (Val::S(interned(id)), name.clone())).collect();
    if !current.is_empty() && !f.skins.iter().any(|(id, _, _)| id == current) {
        let name = current.strip_prefix(crate::skin::FILE_PREFIX).unwrap_or(current);
        skins.push((Val::S(interned(current)), format!("{name} (file missing)")));
    }
    skins
}

fn look(f: &Facts) -> Vec<Item> {
    use Val::S;
    vec![
        Item::Heading("Skin"),
        Item::Card(vec![
            Row::new("Skin", "", None, Ctl::Choice("Skin", skin_choices(f))).long(),
            choice("Theme", "Light/dark mode", None, &[(S("system"), "System"), (S("light"), "Light"), (S("dark"), "Dark")]).radios(),
            choice("ZipIcon", "ZIP icon style", None, &[(S("label"), "Red label"), (S("folder"), "Folder"), (S("page"), "Page"), (S("glint"), "Glint icon")]).long().more(),
            Row::new("@SkinEditor", "Skins", None, Ctl::Button("EDIT SKINS\u{2026}".into(), Act::SkinEditor)),
            choice("SkinsWindow", "Skin editor layout", None, &[(S("compact"), "Compact"), (S("slim"), "Slim"), (S("wide"), "Wide")]).long(),
        ]),
        Item::Heading("Layout"),
        Item::Card(vec![
            choice(
                "View",
                "File view",
                None,
                &[(S("details"), "Details"), (S("list"), "List"), (S("medium"), "Medium icons"), (S("large"), "Large icons"), (S("extralarge"), "Extra large icons"), (S("tree"), "Tree")],
            )
            .long(),
            choice("Size", "Row size", f.info_open.filter(|k| *k == "Size").map(|_| ROW_SIZE), &[(S("small"), "Small"), (S("medium"), "Medium"), (S("large"), "Large")]).radios().info(),
            toggle("ShowSidebar", "Sidebar", None),
            toggle("ShowDetails", "Details panel", None),
            choice(
                "DetailsWidth",
                "Details panel width",
                None,
                &[(Val::N(0.0), "Default (280 px)"), (Val::N(240.0), "240 px"), (Val::N(300.0), "300 px"), (Val::N(340.0), "340 px"), (Val::N(400.0), "400 px"), (Val::N(480.0), "480 px")],
            )
            .long()
            .more(),
            toggle("ShowStatusBar", "Status bar", None),
        ]),
        Item::Heading("Status bar"),
        Item::Card(vec![
            toggle("StatusCount", "Item count", None).needs("ShowStatusBar").more(),
            toggle("StatusSelected", "Selected items\u{2019} size", None).needs("ShowStatusBar").more(),
            toggle("StatusFree", "Available disk space", None).needs("ShowStatusBar").more(),
            toggle("StatusTotal", "Total size", None).needs("ShowStatusBar").more(),
            toggle("StatusOpened", "Folder opening time", None).needs("ShowStatusBar").more(),
            toggle("StatusStarted", "Startup time", None).needs("ShowStatusBar").more(),
            toggle("StatusMemory", "Memory usage", None).needs("ShowStatusBar").more(),
        ]),
        // The window's own choices, together and last, so they never get in the way.
        Item::Heading("Settings window"),
        Item::Card(vec![
            choice("SettingsOpens", "Display settings as", None, &[(S("floating"), "Floating"), (S("side"), "Next to Glint"), (S("panel"), "In the details panel")]).long(),
            choice("SettingsPages", "Page navigation", None, &[(S("tabs"), "Tabs on top"), (S("list"), "List on the left")]).long(),
            choice("SettingsStyle", "Option style", None, &[(S("ticks"), "Checkboxes and radio buttons"), (S("sentences"), "Sentences")]).long(),
            choice("SettingsShow", "Settings to show", None, &[(S("all"), "All settings"), (S("basic"), "Basic first")]).long(),
            choice("SettingsSkins", "Skin picker style", None, &[(S("list"), "Drop-down"), (S("tiles"), "Tiles")]).long(),
            toggle("SettingsSearch", "Show settings search", None),
            toggle("SettingsDots", "Mark settings that differ from defaults", None),
            toggle("SettingsReset", "Show \u{201c}Reset this page\u{201d} button", None),
            toggle("SettingsLastPage", "Remember last settings page", None),
        ]),
    ]
}

fn files() -> Vec<Item> {
    use Val::S;
    let column = |name: &'static str, title: &str| Row::new("Columns", title, None, Ctl::ToggleIn("Columns", name)).more();
    // Each choice with an example, in the person's own date and number style (as the column
    // titles' menu shows them).
    let now = (update::now_seconds() + SECONDS_1601_TO_1970) * 10_000_000;
    let earlier = now.saturating_sub(2 * 3600 * 10_000_000);
    let dates = [
        (S("default"), format!("System ({})", folder::format_date(now))),
        (S("relative"), format!("Relative ({})", folder::format_date_as(earlier, DateFormat::Relative))),
        (S("iso"), format!("Year first ({})", folder::format_date_as(now, DateFormat::Iso))),
    ];
    let sizes = [
        (S("auto"), format!("Automatic ({})", folder::format_size_as(SIZE_EXAMPLE, SizeFormat::Auto))),
        (S("kb"), format!("Always in KB ({})", folder::format_size_as(SIZE_EXAMPLE, SizeFormat::Kb))),
    ];
    vec![
        Item::Heading("Show"),
        Item::Card(vec![
            toggle("ShowHidden", "Hidden files", None),
            toggle("ShowSystem", "Protected system files", None).indent().needs("ShowHidden"),
            Row::new("HideKnownExtensions", "File extensions", None, Ctl::ToggleNot("HideKnownExtensions")),
        ]),
        Item::Heading("Default columns"),
        Item::Card(vec![
            // (Only words: where these columns show. It comes and goes with the columns under it.)
            Row::new("Columns", "", Some(DEFAULT_COLUMNS), Ctl::None).more(),
            column("modified", "Date modified"),
            column("created", "Date created"),
            column("type", "Type"),
            column("size", "Size"),
            column("dimensions", "Image dimensions"),
            column("length", "Duration"),
        ]),
        Item::Heading("Dates and sizes"),
        Item::Card(vec![
            Row::new("DateFormat", "Date format", None, Ctl::Choice("DateFormat", dates.to_vec())).more(),
            Row::new("SizeFormat", "Size format", None, Ctl::Choice("SizeFormat", sizes.to_vec())).more(),
        ]),
        Item::Heading("Folder rules"),
        Item::Card(vec![
            toggle("FoldersFirst", "Folders first", None),
            Row::new("CustomSort", "Sort rules", None, Ctl::ButtonToggle("EDIT\u{2026}".into(), Act::SortEditor, "CustomSort")),
            Row::new("CustomViews", "View rules", None, Ctl::ButtonToggle("EDIT\u{2026}".into(), Act::SortEditor, "CustomViews")).more(),
            choice("SortWindow", "Rules editor", None, &[(S("compact"), "Compact"), (S("slim"), "Slim"), (S("wide"), "Wide")]).long(),
        ]),
        Item::Heading("Ask before deleting"),
        Item::Card(vec![choice("ConfirmDelete", "", None, &[(S("permanent"), "Only when deleting permanently"), (S("always"), "Always"), (S("never"), "Never")]).radios()]),
    ]
}

fn keys_page(f: &Facts) -> Vec<Item> {
    use crate::keymap::{Group, KeyMap, ALL};
    use Val::S;
    // The shortcuts as they are now (settings.json's "Shortcuts" holds the changed ones).
    let saved = f.values.get("Shortcuts").and_then(Value::as_object).map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()).unwrap_or_default();
    let map = KeyMap::of(&saved);
    let mut items = vec![
        Item::Heading("Backspace"),
        Item::Card(vec![choice("Backspace", "", None, &[(S("up"), "Goes up a folder"), (S("back"), "Goes back")]).radios()]),
    ];
    // Every one in one box, Glint's own first.
    let rows = Group::ALL
        .iter()
        .flat_map(|group| ALL.iter().filter(move |s| s.group == *group))
        .map(|s| {
            let text = map.text(s.action);
            // (No keys: removed with a right-click, or a hand-edited settings.json gave them to another.)
            let shown = if text.is_empty() { "Not set".to_string() } else { text };
            Row::new(s.live, s.label, None, Ctl::Shortcut(s.name, shown, map.changed(s.action)))
        })
        .collect();
    items.push(Item::Heading("Shortcuts"));
    items.push(Item::Card(rows));
    items.push(Item::Heading("Can't be changed"));
    items.push(Item::Card(vec![
        keys("@Keys", "Shift+right-click", "Full right-click menu"),
        keys("@Keys", "Ctrl+Shift+1 to 6", "Change view"),
        keys("@Keys", "Ctrl+1 to 9", "Go to tab 1 to 9"),
    ]));
    items
}

/// About: Glint's version and updates, help, and Glint's own files (Portable last: it moves them).
fn about(f: &Facts) -> Vec<Item> {
    let cache_label = match f.cache_size {
        _ if f.clear_asked => "PRESS AGAIN".to_string(),
        Some(bytes) => format!("CLEAR CACHE ({})", folder::format_size(bytes)),
        None => "CLEAR CACHE".to_string(),
    };
    let mut cache_note = f.cache.clone();
    if let Some(said) = &f.cache_said {
        cache_note = format!("{cache_note}\n{said}");
    }
    // The explanation shows while its (i) is open; what COPY said shows under it either way.
    let bug_note: Option<String> = match (f.info_open.filter(|k| *k == "@BugInfo").map(|_| SETUP_INFO), &f.bug_said) {
        (Some(why), Some(said)) => Some(format!("{why}\n{said}")),
        (Some(why), None) => Some(why.to_string()),
        (None, Some(said)) => Some(said.clone()),
        (None, None) => None,
    };
    let mut portable_note = "Keep Glint's files next to Glint.exe, in \u{201c}Glint data\u{201d}.".to_string();
    if f.portable_locked {
        portable_note.push_str("\nPortable mode is locked while testing.");
    }
    if let Some(said) = &f.portable_said {
        portable_note = format!("{portable_note}\n{said}");
    }
    vec![
        Item::Heading("Glint"),
        Item::Card(vec![
            Row::new("@About", &format!("Glint {}", crate::log::VERSION), Some("A light file explorer for Windows."), Ctl::None),
            Row::new("@Update", ABOUT_ROW, Some(&f.update_said), Ctl::Button(if f.update_busy { "CHECKING\u{2026}".into() } else { "CHECK FOR UPDATES".into() }, Act::Update)).needs("@UpdateReady"),
            toggle("CheckForUpdates", "Check for updates automatically", None).indent(),
            toggle("UpdateAutomatically", "Install updates automatically", f.info_open.filter(|k| *k == "UpdateAutomatically").map(|_| UPDATE_AUTOMATICALLY)).indent().needs("CheckForUpdates").info(),
        ]),
        Item::Heading("Help"),
        Item::Card(vec![
            Row::new("@ReportBug", "Report an issue on GitHub", None, Ctl::Button("REPORT".into(), Act::ReportBug)),
            Row::new("@BugInfo", "System info", bug_note.as_deref(), Ctl::Button("COPY".into(), Act::CopyBugInfo)).info(),
            Row::new("@Discord", "Discord", None, Ctl::Button("OPEN".into(), Act::Discord)),
        ]),
        Item::Heading("Glint's files"),
        Item::Card(vec![
            Row::new("@SettingsFolder", "Settings folder", Some(&f.config), Ctl::Button("OPEN".into(), Act::OpenSettingsFolder)),
            Row::new("@LogsFolder", "Log folder", Some(&f.logs), Ctl::Button("OPEN".into(), Act::OpenLogsFolder)),
            toggle("ErrorLog", "Write errors to a file", None),
            Row::new("@Cache", "Disk cache", Some(&cache_note), Ctl::Button(cache_label, Act::ClearCache)),
            Row::new("@Portable", "Portable mode", Some(&portable_note), Ctl::Toggle("@Portable")).more(),
        ]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn facts(values: &Map<String, Value>) -> Facts<'_> {
        Facts {
            values,
            terminals: Some([true, false, true, true, true, false]),
            config: "C:\\c".into(),
            logs: "C:\\l".into(),
            cache: "C:\\k".into(),
            cache_size: Some(0),
            cache_said: None,
            clear_asked: false,
            portable_said: None,
            bug_said: None,
            info_open: None,
            portable_locked: false,
            skins: &[],
            update_said: "You have the newest Glint.".into(),
            update_busy: false,
        }
    }

    fn rows(items: &[Item]) -> impl Iterator<Item = &Row> {
        items.iter().flat_map(|i| match i {
            Item::Card(rows) => rows.as_slice(),
            Item::Heading(_) => &[],
        })
    }

    /// Every row is bound to a real setting (or an "@" name), every choice is a value that setting
    /// can hold, and there are four pages.
    #[test]
    fn every_row_is_bound_to_a_real_setting() {
        let Value::Object(values) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        assert_eq!(PAGES.len(), 5);
        for p in 0..PAGES.len() {
            let items = page(p, &facts(&values));
            assert!(rows(&items).count() > 0, "page {p}");
            for row in rows(&items) {
                let bound = match &row.ctl {
                    Ctl::Toggle(k) | Ctl::ToggleNot(k) | Ctl::ToggleIn(k, _) | Ctl::ButtonToggle(_, _, k) | Ctl::Choice(k, _) => Some(*k),
                    _ => None,
                };
                for k in bound.into_iter().chain(Some(row.key)).chain(row.needs.map(|n| n.trim_start_matches('!'))).chain(row.only_if.map(|o| o.0)) {
                    assert!(k.starts_with('@') || values.contains_key(k), "{k} (row \"{}\") isn't a setting", row.title);
                }
                if let Ctl::Choice(k, choices) = &row.ctl {
                    for (v, label) in choices {
                        let mut trial = values.clone();
                        trial.insert(k.to_string(), v.json());
                        assert!(serde_json::from_value::<Settings>(Value::Object(trial)).is_ok(), "{label} isn't a value of {k}");
                    }
                }
                if let Ctl::ToggleIn(_, item) = row.ctl {
                    assert!(crate::settings::COLUMN_NAMES.contains(&item));
                }
            }
        }
        for key in LIVE {
            assert!(key.starts_with('@') || values.contains_key(*key), "{key} in LIVE isn't a setting");
        }
    }

    /// About Glint finds its row; the Settings window's own choices are on the Appearance page and work;
    /// round buttons are only for a few short choices.
    #[test]
    fn about_and_the_window_choices_are_there() {
        let Value::Object(values) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        assert!(rows(&page(ABOUT, &facts(&values))).any(|r| r.title == ABOUT_ROW));
        let look = page(1, &facts(&values));
        for key in ["SettingsOpens", "SettingsPages", "SettingsStyle", "SettingsShow", "SettingsSkins", "SettingsSearch", "SettingsDots", "SettingsReset", "SettingsLastPage"] {
            let row = rows(&look).find(|r| r.key == key).unwrap_or_else(|| panic!("{key}"));
            assert!(row.live() && !row.more, "{key} works and always shows");
        }
        for p in 0..PAGES.len() {
            for row in rows(&page(p, &facts(&values))) {
                if let Ctl::Choice(k, choices) = &row.ctl {
                    assert!(!(row.radios && row.long), "{k}");
                    assert!(!row.radios || choices.len() <= 4, "{k}: too many to spell out");
                }
            }
        }
    }

    #[test]
    fn missing_terminals_say_so() {
        let values = Map::new();
        let items = page(0, &facts(&values));
        let terminal = rows(&items).find(|r| r.key == "Terminal").unwrap();
        let Ctl::Choice(_, choices) = &terminal.ctl else { panic!() };
        let labels: Vec<&str> = choices.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels[0], "Automatic");
        assert_eq!(labels[1], "Windows Terminal (not found)");
        assert_eq!(labels[2], "Windows PowerShell");
        assert_eq!(choices[5].0, Val::S("gitbash"));
    }

    #[test]
    fn what_works_and_what_comes_later() {
        assert!(live("ShowHidden") && live("ConfirmDelete") && live("Theme") && live("ErrorLog"));
        assert!(live("StayRunning") && live("Columns") && live("SingleClick") && live("@Folders") && live("@Pins"));
        assert!(live("@Update") && live("CheckForUpdates"), "0.3.0: updates");
        assert!(live("ShowDetails") && live("@Preview"), "the details panel and the preview");
        assert!(live("View") && live("ShowStatusBar") && live("ShowSidebar"), "views and the status bar switch");
        assert!(live("CustomSort"), "the sort rules");
        assert!(live("Skin") && live("@SkinEditor"));
    }

    /// The skin drop-down lists the built-in skins, then the person's own (and a chosen one whose
    /// file is gone, by its name); "Light/dark mode" offers Same as Windows.
    #[test]
    fn the_skins_are_listed() {
        let lib = crate::skin::Library::new();
        let mut skins: Vec<(String, String, String)> = lib.entries().iter().map(|e| (e.id.clone(), e.name.clone(), e.description.clone())).collect();
        skins.push(("file:Moss".into(), "Moss".into(), String::new()));
        let mut values = Map::new();
        values.insert("Skin".into(), Value::String("file:Gone".into()));
        let f = Facts { skins: &skins, ..facts(&values) };
        let items = page(1, &f);
        let skin = rows(&items).find(|r| r.key == "Skin").unwrap();
        let Ctl::Choice(_, choices) = &skin.ctl else { panic!() };
        let labels: Vec<&str> = choices.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["AMD Radeon", "AMD Radeon (Light)", "NVIDIA", "NVIDIA (Light)", "Chrome", "Gunmetal", "Moss", "Gone (file missing)"]);
        assert_eq!(choices[0].0, Val::S("amd-radeon"));
        assert_eq!((&choices[4].0, &choices[5].0), (&Val::S("chrome"), &Val::S("gunmetal")), "a pair, side by side");
        assert_eq!(choices[6].0, Val::S("file:Moss"));
        let theme = rows(&items).find(|r| r.key == "Theme").unwrap();
        let Ctl::Choice(_, modes) = &theme.ctl else { panic!() };
        assert_eq!(modes[0], (Val::S("system"), "System".to_string()));
        let editor = rows(&items).find(|r| r.key == "@SkinEditor").unwrap();
        assert_eq!(editor.ctl, Ctl::Button("EDIT SKINS\u{2026}".into(), Act::SkinEditor));
    }

    /// The Keyboard shortcuts page: Backspace first, then every shortcut that can be changed in one
    /// box (Glint's own first), with its keys as they are now; the fixed ones last, as plain text.
    #[test]
    fn keyboard_shortcuts_page() {
        assert_eq!(PAGES[3], "Keyboard shortcuts");
        let values = |shortcuts: Value| {
            let Value::Object(mut v) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
            v.insert("Shortcuts".into(), shortcuts);
            v
        };
        let plain = values(serde_json::json!({}));
        let items = page(3, &facts(&plain));
        let headings: Vec<&str> = items.iter().filter_map(|i| if let Item::Heading(h) = i { Some(*h) } else { None }).collect();
        assert_eq!(headings, ["Backspace", "Shortcuts", "Can't be changed"]);
        let order: Vec<&str> = rows(&items).filter(|r| matches!(r.ctl, Ctl::Shortcut(..))).map(|r| r.title.as_str()).collect();
        assert_eq!(order[..4], ["Command box", "Settings", "Exit Glint", "New tab"], "Glint's own first");
        assert_eq!(order.last(), Some(&"Light/dark mode"));
        assert!(rows(&items).any(|r| r.ctl == Ctl::Choice("Backspace", vec![(Val::S("up"), "Goes up a folder".into()), (Val::S("back"), "Goes back".into())])));
        let shortcut = |items: &[Item], title: &str| rows(items).find(|r| r.title == title).map(|r| r.ctl.clone());
        // The rows that held two hold one each now.
        for (title, keys, name) in [("New tab", "Ctrl+T", "NewTab"), ("Close tab", "Ctrl+W", "CloseTab"), ("Undo", "Ctrl+Z", "Undo"), ("Redo", "Ctrl+Y", "Redo"), ("Split view", "Ctrl+\\", "SplitView"), ("Switch pane", "F6", "SwitchPane"), ("Light/dark mode", "Ctrl+Shift+L", "LightDarkMode")] {
            assert_eq!(shortcut(&items, title), Some(Ctl::Shortcut(name, keys.into(), false)), "{title}");
        }
        for r in rows(&items) {
            assert!(!r.title.contains(" / ") && !r.title.contains(" or F"), "{}", r.title);
            if let Ctl::Shortcut(_, keys, _) = &r.ctl {
                assert!(!keys.contains(" / "), "{keys}");
            }
        }
        assert_eq!(rows(&items).filter(|r| matches!(r.ctl, Ctl::Shortcut(..))).count(), crate::keymap::ALL.len());
        assert!(!rows(&items).any(|r| r.title == "Light or dark"));
        // The fixed ones: listed, not buttons. Preview isn't one: it has a button like the others.
        for (keys, what) in [("Shift+right-click", "Full right-click menu"), ("Ctrl+Shift+1 to 6", "Change view"), ("Ctrl+1 to 9", "Go to tab 1 to 9")] {
            assert!(rows(&items).any(|r| r.title == what && r.ctl == Ctl::Text(keys)), "{keys}");
        }
        assert_eq!(shortcut(&items, "Preview"), Some(Ctl::Shortcut("Preview", "Space".into(), false)));
        assert_eq!(rows(&items).filter(|r| r.title == "Preview").count(), 1);
        // Changed: its new keys, marked changed; one a hand-edited file left without keys says so.
        let changed = values(serde_json::json!({"CloseTab": "Ctrl+T", "Rename": "Alt+Shift+R", "Redo": 5}));
        let items = page(3, &facts(&changed));
        assert_eq!(shortcut(&items, "Close tab"), Some(Ctl::Shortcut("CloseTab", "Ctrl+T".into(), true)));
        assert_eq!(shortcut(&items, "Rename"), Some(Ctl::Shortcut("Rename", "Alt+Shift+R".into(), true)));
        assert_eq!(shortcut(&items, "New tab"), Some(Ctl::Shortcut("NewTab", "Not set".into(), true)));
        assert_eq!(shortcut(&items, "Redo"), Some(Ctl::Shortcut("Redo", "Ctrl+Y".into(), false)));
        // Removed with a right-click: "Not set", and RESET beside it.
        let removed = values(serde_json::json!({"Preview": "", "ExitGlint": ""}));
        let items = page(3, &facts(&removed));
        assert_eq!(shortcut(&items, "Preview"), Some(Ctl::Shortcut("Preview", "Not set".into(), true)));
        assert_eq!(shortcut(&items, "Exit Glint"), Some(Ctl::Shortcut("ExitGlint", "Not set".into(), true)));
        assert_eq!(shortcut(&items, "Settings"), Some(Ctl::Shortcut("Settings", "Ctrl+,".into(), false)));
        // Not settings for the command box to find (it has the actions themselves).
        assert!(!findable(&Settings::default()).iter().any(|(t, _)| t == "New tab" || t == "Rename"));
    }

    /// Rows about what doesn't work yet (Discord) are hidden; the shortcuts list Ctrl+Q and Disk space;
    /// a setting goes by its name in Settings.
    #[test]
    fn hidden_rows_shortcuts_and_names() {
        let Value::Object(values) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        for p in 0..PAGES.len() {
            assert!(rows(&page(p, &facts(&values))).all(Row::live), "page {p}");
        }
        let keys = page(3, &facts(&values));
        assert!(rows(&keys).any(|r| r.key == "@DiskUsage" && r.title == "Disk space" && r.ctl == Ctl::Shortcut("DiskSpace", "Ctrl+Shift+U".into(), false)));
        assert!(rows(&keys).any(|r| r.title == "Exit Glint" && r.ctl == Ctl::Shortcut("ExitGlint", "Ctrl+Q".into(), false)));
        assert!(!rows(&page(ABOUT, &facts(&values))).any(|r| r.key == "@Discord"));
        assert_eq!(title_of("StayRunning").as_deref(), Some("Keep running in the background"));
        assert_eq!(title_of("NewTab").as_deref(), Some("New tabs"), "round buttons go by their group");
        assert_eq!(title_of("Columns").as_deref(), Some("Default columns"));
        assert_eq!(title_of("HideKnownExtensions").as_deref(), Some("File extensions"));
        assert_eq!(title_of("NoSuchSetting"), None);
    }

    /// Date format and Size format show an example next to each choice, in the person's own
    /// style; the columns say where they apply; the rules about folders go by one name.
    #[test]
    fn examples_columns_and_folder_rules() {
        let Value::Object(values) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        let items = page(2, &facts(&values));
        let labels = |key: &str| -> Vec<String> {
            let row = rows(&items).find(|r| r.key == key).unwrap_or_else(|| panic!("{key}"));
            let Ctl::Choice(_, choices) = &row.ctl else { panic!("{key}") };
            choices.iter().map(|(_, l)| l.clone()).collect()
        };
        assert_eq!(title_of("DateFormat").as_deref(), Some("Date format"));
        let dates = labels("DateFormat");
        assert_eq!(dates.len(), 3);
        for (label, start) in dates.iter().zip(["System (", "Relative (", "Year first (20"]) {
            assert!(label.starts_with(start) && label.ends_with(')') && label.len() > start.len() + 4, "{label}");
        }
        // Year first is the same everywhere: "Year first (2026-10-06 14:32)".
        assert_eq!(dates[2].len(), "Year first (2026-10-06 14:32)".len(), "{}", dates[2]);
        assert_eq!(title_of("SizeFormat").as_deref(), Some("Size format"));
        let sizes = labels("SizeFormat");
        assert_eq!(sizes.len(), 2);
        assert!(sizes[0].starts_with("Automatic (3") && sizes[0].ends_with(" MB)"), "{}", sizes[0]);
        assert!(sizes[1].starts_with("Always in KB (3") && sizes[1].ends_with(" KB)"), "{}", sizes[1]);
        // The columns: a line of words first (no control, hidden with the columns), then the six.
        let at = items.iter().position(|i| *i == Item::Heading("Default columns")).expect("the heading");
        let Item::Card(columns) = &items[at + 1] else { panic!() };
        assert_eq!((columns[0].title.as_str(), columns[0].note.as_deref(), &columns[0].ctl, columns[0].more), ("", Some(DEFAULT_COLUMNS), &Ctl::None, true));
        let titles: Vec<&str> = columns[1..].iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Date modified", "Date created", "Type", "Size", "Image dimensions", "Duration"]);
        assert!(columns[1..].iter().all(|r| matches!(r.ctl, Ctl::ToggleIn("Columns", _)) && r.more));
        assert!(!findable(&Settings::default()).iter().any(|(t, _)| t.is_empty()), "a line of words isn't a setting to find");
        // Folder rules: one name for the sort and view rules and their editor.
        assert!(items.contains(&Item::Heading("Folder rules")));
        assert_eq!(title_of("FoldersFirst").as_deref(), Some("Folders first"));
        assert_eq!(title_of("CustomSort").as_deref(), Some("Sort rules"));
        assert_eq!(title_of("CustomViews").as_deref(), Some("View rules"));
        assert_eq!(title_of("SortWindow").as_deref(), Some("Rules editor"));
    }

    /// An (i) shows its words only while it's open, on the row it belongs to.
    #[test]
    fn the_i_rows_say_their_words_when_open() {
        let Value::Object(values) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        for (p, key, words) in [(0, "StayRunning", STAY_RUNNING), (0, "WinEAfterQuit", TEMP_WIN_E_FIX), (1, "Size", ROW_SIZE), (ABOUT, "UpdateAutomatically", UPDATE_AUTOMATICALLY)] {
            let closed = page(p, &facts(&values));
            let row = rows(&closed).find(|r| r.key == key).unwrap_or_else(|| panic!("{key}"));
            assert!(row.info && row.note.is_none(), "{key}");
            let open = page(p, &Facts { info_open: Some(key), ..facts(&values) });
            let row = rows(&open).find(|r| r.key == key).unwrap();
            assert_eq!(row.note.as_deref(), Some(words), "{key}");
            assert!(rows(&open).filter(|r| r.key != key).all(|r| r.note.as_deref() != Some(words)));
            // (Whole sentences, and never the word the app doesn't use for closing.)
            assert!(words.ends_with('.') && !words.to_lowercase().contains("quit"), "{words}");
        }
    }

    /// About: the buttons and rows by their names; the cache button says what it clears and keeps
    /// the size's unit.
    #[test]
    fn about_names_its_rows_and_buttons() {
        let Value::Object(values) = serde_json::to_value(Settings::default()).unwrap() else { panic!() };
        let items = page(ABOUT, &facts(&values));
        let row = |key: &str| rows(&items).find(|r| r.key == key).unwrap_or_else(|| panic!("{key}")).clone();
        assert_eq!(row("@Update").ctl, Ctl::Button("CHECK FOR UPDATES".into(), Act::Update));
        assert_eq!(row("UpdateAutomatically").title, "Install updates automatically");
        assert_eq!(row("@ReportBug").title, "Report an issue on GitHub");
        assert_eq!(row("@BugInfo").title, "System info");
        assert_eq!(row("ErrorLog").title, "Write errors to a file");
        assert_eq!((row("@Cache").title.as_str(), row("@Cache").ctl), ("Disk cache", Ctl::Button("CLEAR CACHE (0 B)".into(), Act::ClearCache)));
        assert_eq!(row("@Portable").title, "Portable mode");
        let asked = page(ABOUT, &Facts { clear_asked: true, ..facts(&values) });
        assert!(rows(&asked).any(|r| r.ctl == Ctl::Button("PRESS AGAIN".into(), Act::ClearCache)));
        let unmeasured = page(ABOUT, &Facts { cache_size: None, ..facts(&values) });
        assert!(rows(&unmeasured).any(|r| r.ctl == Ctl::Button("CLEAR CACHE".into(), Act::ClearCache)));
        let big = page(ABOUT, &Facts { cache_size: Some(5 * 1024 * 1024 * 1024), ..facts(&values) });
        let Some(Ctl::Button(label, _)) = rows(&big).find(|r| r.key == "@Cache").map(|r| r.ctl.clone()) else { panic!() };
        assert!(label.starts_with("CLEAR CACHE (5") && label.ends_with(" GB)"), "{label}");
        assert_eq!(crate::kit::sentence_case(&label), label.replace("CLEAR CACHE", "Clear cache"), "the unit keeps its capitals on a button in sentence case");
    }

    /// A row size picked in the window keeps the same rows on screen, as the column titles' menu
    /// does (the window has already put the new size in the settings when it asks).
    #[test]
    fn a_new_row_size_keeps_the_rows_on_screen() {
        let mut app = AppState::for_tests(Settings::default());
        let small = app.row_height();
        app.tab_mut().scroll = 10.0 * small;
        let before = app.settings.clone();
        app.settings.size = crate::settings::ListSize::Large;
        after_change("Size", &before, &mut app);
        assert_eq!(app.settings.size, crate::settings::ListSize::Large);
        assert!(app.row_height() > small);
        assert!((app.tab().scroll - 10.0 * app.row_height()).abs() < 0.01, "still ten rows down");
    }
}
