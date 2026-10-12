//! What the Settings window shows: its five pages.
//! Every row has a short title, a note only where the title can't say enough, a control, and the
//! setting it changes - by its name in settings.json ("ShowHidden"), so a row works with whatever
//! Rust type holds it.
//!
//! **For whoever connects a feature** (the integrator, or the next version):
//! - [`LIVE`] lists what works in this version. A row whose `key` isn't in it is greyed, with
//!   "Coming later", so the pages already look the way they will. Add the key when
//!   the feature is in.
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
use crate::settings::{Settings, Terminal, COLUMN_NAMES};
use crate::update;
use crate::ui::Command;

/// The pages, in the order of the list on the left.
pub const PAGES: [&str; 5] = ["General", "Appearance", "Files and folders", "Mouse and keyboard", "About"];
/// The About page (SETTINGS > About Glint opens it).
pub const ABOUT: usize = 4;

/// What works in this version: settings by their name in settings.json, and "@" names for the
/// rest (buttons, things Windows keeps). Rows about anything else are greyed.
pub const LIVE: &[&str] = &[
    "Logo",
    // General
    "Startup",
    "StartupFolder",
    "NewTab", "HomeOpens",
    "@Folders",
    "@WinE",
    "StayRunning",
    "TrayIcon",
    "StartWithWindows",
    "ShellItemsInMenu",
    "Terminal",
    // Appearance
    "Skin",
    "Theme",
    "@SkinEditor",
    "Size",
    "ShowSidebar",
    "ShowDetails",
    "ShowStatusBar",
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
    "ConfirmDelete",
    // Mouse and keyboard
    "SingleClick",
    "DoubleClickUp",
    "Backspace",
    "@Search",
    "@Pins",
    "@Preview",
    // About
    "@About",
    "@Update",
    "CheckForUpdates",
    "@SettingsFolder",
    "@Portable",
    "@Cache",
    "ErrorLog",
    "@LogsFolder",
    // The shortcuts that work
    "@Keys",
];

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
        // A skin picked: "Light or dark" follows it (app/skins.rs); the windows follow the look.
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
        // Every tab is sorted again (by the rules, or by its column).
        "CustomSort" => {
            let on = app.settings.custom_sort;
            app.set_custom_sort(on, None);
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
            app.say(if on { "Closing now hides Glint (Ctrl+Q quits)" } else { "Closing now quits Glint" });
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
                Ok(()) if on => app.say("Glint now starts with Windows"),
                Ok(()) => app.say("Glint no longer starts with Windows"),
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
/// most (they're drawn every frame; a .bat file may have changed them meanwhile).
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
}

impl Val {
    pub fn json(self) -> Value {
        match self {
            Val::S(s) => Value::String(s.to_string()),
            Val::B(b) => Value::Bool(b),
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
}

/// The control on the right of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Ctl {
    /// Nothing: the row only explains something.
    None,
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
    /// A button and a switch side by side (EDIT… and the custom sort order).
    ButtonToggle(String, Act, &'static str),
    /// Text in a second column (the shortcuts).
    Text(&'static str),
    /// The text box for the start folder, with BROWSE.
    Folder,
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
}

impl Row {
    fn new(key: &'static str, title: &str, note: Option<&str>, ctl: Ctl) -> Row {
        Row { title: title.to_string(), note: note.map(str::to_string), ctl, key, indent: false, needs: None, only_if: None }
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

/// A page is headings and cards of rows.
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
    /// What switching Portable said last.
    pub portable_said: Option<String>,
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
fn keys(key: &'static str, keys: &str, what: &'static str) -> Row {
    Row::new(key, keys, None, Ctl::Text(what))
}

/// The rows of page `i` (see [`PAGES`]).
pub fn page(i: usize, f: &Facts) -> Vec<Item> {
    match i {
        0 => general(f),
        1 => appearance(f),
        2 => files(f),
        3 => mouse(),
        _ => about(f),
    }
}

fn general(f: &Facts) -> Vec<Item> {
    use Val::S;
    let terminals: Vec<(Val, String)> = Terminal::ALL
        .iter()
        .enumerate()
        .map(|(i, (t, name))| {
            let missing = *t != Terminal::Auto && f.terminals.is_some_and(|found| !found[i]);
            let value = serde_json::to_value(t).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
            (S(terminal_key(&value)), if missing { format!("{name} (not found)") } else { name.to_string() })
        })
        .collect();
    vec![
        Item::Heading("Startup"),
        Item::Card(vec![
            choice("Startup", "Open on startup", None, &[(S("restore"), "Tabs from last time"), (S("home"), "Home folder"), (S("thispc"), "This PC"), (S("folder"), "A specific folder")]),
            Row::new("StartupFolder", "", None, Ctl::Folder).only_if("Startup", S("folder")),
            choice("NewTab", "New tabs open", None, &[(S("same"), "Current folder"), (S("home"), "Home folder"), (S("thispc"), "This PC")]),
            choice("HomeOpens", "Home opens", None, &[(S("page"), "Home, like File Explorer"), (S("folder"), "Your user folder")]),
        ]),
        Item::Heading("Default file explorer"),
        Item::Card(vec![toggle("@Folders", "Open folders and drives in Glint", None), toggle("@WinE", "Open with Win+E", None)]),
        Item::Heading("Background"),
        Item::Card(vec![
            toggle("StayRunning", "Keep running in the background", Some("Closing hides Glint; Ctrl+Q quits")),
            toggle("TrayIcon", "Show tray icon", None).indent().needs("StayRunning"),
            toggle("StartWithWindows", "Start with Windows", None),
        ]),
        Item::Heading("Right-click menu"),
        Item::Card(vec![toggle("ShellItemsInMenu", "Show other apps' items", None), Row::new("Terminal", "Open in terminal uses", None, Ctl::Choice("Terminal", terminals))]),
    ]
}

/// The terminal's value in settings.json as a `&'static str` (they're fixed words).
fn terminal_key(value: &str) -> &'static str {
    ["auto", "wt", "powershell", "pwsh", "cmd", "gitbash"].into_iter().find(|k| *k == value).unwrap_or("auto")
}

fn appearance(f: &Facts) -> Vec<Item> {
    use Val::S;
    // Every skin: the built-in ones, then the person's own. A skin whose file is gone still shows
    // by its id, so the drop-down never looks empty.
    let current = f.values.get("Skin").and_then(Value::as_str).unwrap_or("");
    let mut skins: Vec<(Val, String)> = f.skins.iter().map(|(id, name, _)| (S(interned(id)), name.clone())).collect();
    if !current.is_empty() && !f.skins.iter().any(|(id, _, _)| id == current) {
        let name = current.strip_prefix(crate::skin::FILE_PREFIX).unwrap_or(current);
        skins.push((S(interned(current)), format!("{name} (file missing)")));
    }
    vec![
        Item::Heading("Skin"),
        Item::Card(vec![
            Row::new("Skin", "Skin", None, Ctl::Choice("Skin", skins)),
            choice("Theme", "Light or dark", None, &[(S("system"), "Same as Windows"), (S("light"), "Light"), (S("dark"), "Dark")]),
            choice("Logo", "Logo", None, &[(S("box"), "Box"), (S("chrome"), "Chrome"), (S("none"), "None")]),
            Row::new("@SkinEditor", "Customise skins", None, Ctl::Button("CUSTOMISE\u{2026}".into(), Act::SkinEditor)),
        ]),
        Item::Heading("Layout"),
        Item::Card(vec![
            choice(
                "View",
                "View",
                Some("Every tab, but folders with a view of their own keep it (VIEW \u{203a} Views for folders)"),
                &[(S("details"), "Details"), (S("list"), "List"), (S("medium"), "Medium icons"), (S("large"), "Large icons"), (S("extralarge"), "Extra large icons"), (S("tree"), "Tree")],
            ),
            choice("Size", "Row size", None, &[(S("small"), "Small"), (S("medium"), "Medium"), (S("large"), "Large")]),
            toggle("ShowSidebar", "Show sidebar", None),
            toggle("ShowDetails", "Show details panel", None),
            toggle("ShowStatusBar", "Show status bar", None),
        ]),
    ]
}

fn files(f: &Facts) -> Vec<Item> {
    use Val::S;
    let custom = f.values.get("CustomSort").and_then(Value::as_bool).unwrap_or(false);
    // While the custom sort order is on, it decides in the folders it has rules for (in the others,
    // this still does): say so.
    let folders_note = custom.then_some("Where the custom sort order has rules, it decides");
    let column = |name: &'static str, title: &str| Row::new("Columns", title, None, Ctl::ToggleIn("Columns", name));
    vec![
        Item::Heading("Show"),
        Item::Card(vec![
            toggle("ShowHidden", "Hidden files", None),
            toggle("ShowSystem", "Protected system files", Some("Best left off")).indent().needs("ShowHidden"),
            Row::new("HideKnownExtensions", "File extensions", None, Ctl::ToggleNot("HideKnownExtensions")),
        ]),
        Item::Heading("Columns"),
        Item::Card(vec![column("modified", "Date modified"), column("created", "Date created"), column("type", "Type"), column("size", "Size")]),
        Item::Heading("Dates and sizes"),
        Item::Card(vec![
            choice("DateFormat", "Dates", None, &[(S("default"), "Windows default"), (S("relative"), "Relative (Today, Yesterday…)"), (S("iso"), "Year first (2026-10-06 14:32)")]),
            choice("SizeFormat", "Sizes", None, &[(S("auto"), "Automatic (KB, MB, GB)"), (S("kb"), "Always KB")]),
        ]),
        Item::Heading("Sorting"),
        Item::Card(vec![
            toggle("FoldersFirst", "Folders first", folders_note),
            Row::new("CustomSort", "Custom sort order", None, Ctl::ButtonToggle("EDIT…".into(), Act::SortEditor, "CustomSort")),
        ]),
        Item::Heading("Deleting"),
        Item::Card(vec![choice("ConfirmDelete", "Ask before deleting", None, &[(S("permanent"), "Only when deleting permanently"), (S("always"), "Always"), (S("never"), "Never")])]),
    ]
}

fn mouse() -> Vec<Item> {
    use Val::{B, S};
    vec![
        Item::Heading("Mouse"),
        Item::Card(vec![
            choice("SingleClick", "Open items with", None, &[(B(false), "Double-click"), (B(true), "Single click")]),
            toggle("DoubleClickUp", "Double-click empty space to go up", None),
        ]),
        Item::Heading("Keyboard"),
        Item::Card(vec![choice("Backspace", "Backspace", None, &[(S("up"), "Goes up a folder"), (S("back"), "Goes back")])]),
        Item::Heading("Shortcuts"),
        Item::Card(vec![
            keys("@Keys", "Ctrl+T / Ctrl+W", "New tab / close tab"),
            keys("@Keys", "Ctrl+Shift+T", "Reopen closed tab"),
            keys("@Keys", "Ctrl+L or F4", "Type a path"),
            keys("@Keys", "Ctrl+F", "Filter"),
            keys("@Keys", "Ctrl+Shift+1 to 6", "Change view"),
            keys("@Keys", "Ctrl+B", "Show or hide sidebar"),
            keys("@Search", "Ctrl+Shift+F", "Search subfolders"),
            keys("@Keys", "Ctrl+Shift+C", "Copy path"),
            keys("@Pins", "Ctrl+D", "Pin folder to sidebar"),
            keys("@Keys", "Ctrl+`", "Open terminal here"),
            keys("@DiskUsage", "Ctrl+Shift+U", "Disk usage"),
            keys("ShowDetails", "Alt+P", "Show or hide details panel"),
            keys("@Preview", "Space", "Preview"),
            keys("@Keys", "Shift+right-click", "Full Windows menu"),
            keys("@Keys", "Ctrl+H", "Show or hide hidden files"),
            keys("@Keys", "Ctrl+Shift+L", "Light or dark"),
            keys("@Keys", "Ctrl+,", "Settings"),
        ]),
    ]
}

fn about(f: &Facts) -> Vec<Item> {
    let cache_label = match f.cache_size {
        Some(bytes) => format!("CLEAR ({})", folder::format_size(bytes)),
        None => "CLEAR".to_string(),
    };
    let mut cache_note = f.cache.clone();
    if let Some(said) = &f.cache_said {
        cache_note = format!("{cache_note}\n{said}");
    }
    let mut portable_note = "Keep Glint's files next to glint.exe, in \u{201c}Glint data\u{201d}".to_string();
    if f.portable_locked {
        portable_note.push_str("\nLocked while GLINT_SETTINGS_DIR is set (for testing)");
    }
    if let Some(said) = &f.portable_said {
        portable_note = format!("{portable_note}\n{said}");
    }
    vec![
        Item::Card(vec![
            Row::new("@About", &format!("Glint {}", crate::log::VERSION), Some("A lightweight file explorer for Windows. This is a beta, so some things may not work yet."), Ctl::None),
            Row::new("@Update", "Updates", Some(&f.update_said), Ctl::Button(if f.update_busy { "CHECKING…".into() } else { "CHECK FOR UPDATES".into() }, Act::Update)).needs("@UpdateReady"),
            toggle("CheckForUpdates", "Check for updates automatically", None).indent(),
        ]),
        Item::Heading("Settings and cache"),
        Item::Card(vec![
            Row::new("@SettingsFolder", "Settings folder", Some(&f.config), Ctl::Button("OPEN FOLDER".into(), Act::OpenSettingsFolder)),
            Row::new("@Portable", "Portable", Some(&portable_note), Ctl::Toggle("@Portable")),
            Row::new("@Cache", "Cache", Some(&cache_note), Ctl::Button(cache_label, Act::ClearCache)),
        ]),
        Item::Heading("Error log"),
        Item::Card(vec![
            toggle("ErrorLog", "Save errors to a log file", None),
            Row::new("@LogsFolder", "Log folder", Some(&f.logs), Ctl::Button("OPEN FOLDER".into(), Act::OpenLogsFolder)),
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
            portable_said: None,
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
    /// can hold, and there are five pages.
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
        assert!(live("CustomSort"), "the custom sort order");
        assert!(live("Skin") && live("@SkinEditor"));
    }

    /// The skin drop-down lists the built-in skins, then the person's own (and a chosen one whose
    /// file is gone, by its name); "Light or dark" offers Same as Windows.
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
        assert_eq!(modes[0], (Val::S("system"), "Same as Windows".to_string()));
        let editor = rows(&items).find(|r| r.key == "@SkinEditor").unwrap();
        assert_eq!(editor.ctl, Ctl::Button("CUSTOMISE\u{2026}".into(), Act::SkinEditor));
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
