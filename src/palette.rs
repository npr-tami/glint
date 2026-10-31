//! The command box (Ctrl+U): a small box at the top of the files where you type what you want to
//! do ("split", "dark", "sort size") and pick it from a short list, each with its shortcut.
//!
//! The list of things it can do is made when the box opens (`actions`), so names like "Hide
//! hidden files" say what happens now. Typing narrows it (`rows_for`): words in any order, and
//! first letters work too ("sv" finds Split view). Text that looks like a path offers "Go to" first;
//! a pattern with `*` or `?`, or a colour tag (`tag:red`), offers "Select" first (`*.pdf` selects
//! every PDF here, and the other files stay in view); other text ends with "Search for … here",
//! which puts it in the filter. "?" or "help" shows the guide instead: every command with what it
//! does in a few plain words, by group, A to Z in each (`guide`); it scrolls, and a row still runs.
//!
//! While the box is open the window hands it the mouse and the keys (window.rs):
//! - Up / Down pick a row, Enter or a click runs it;
//! - Esc, Ctrl+U or a click outside closes it.

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetFocus, VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_UP,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{GetParent, SendMessageW, WM_CHAR, WM_KEYDOWN};

use crate::app::{AppState, SortKey, Tab};
use crate::folder::NameFilter;
use crate::kit;
use crate::retroctl::Hold;
use crate::render::{Align, Renderer, TextStyle};
use crate::settings::ViewMode;
use crate::textbox::{TextBox, TextEvent, TextEventKind};
use crate::theme::{Color, Theme};
use crate::ui::{Command, Mods, Point, Rect};

/// The box's text box id (textbox.rs: 700-799 are the command box's).
const BOX_ID: u16 = 700;
/// The most rows shown (but for the guide, which scrolls).
const MAX_ROWS: usize = 8;
const WIDTH: f32 = 560.0;
/// The guide (help or ?) is this wide, room allowing.
const GUIDE_WIDTH: f32 = 780.0;
/// A group's title in the guide.
const HEADING_H: f32 = 30.0;
const PAD: f32 = 8.0;
const FIELD_H: f32 = 34.0;
const ROW_H: f32 = 30.0;

/// What a row does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// An app command, run as if its shortcut was pressed.
    Run(Command),
    /// Use this skin (its id).
    Skin(String),
    /// Select every item on screen (Ctrl+A in the list).
    SelectAll,
    /// Folder sizes on or off (the column titles' menu).
    FolderSizes(bool),
    /// Rename the selected items all at once (needs two or more).
    RenameMany,
    /// Put this text in the filter, with the cursor there.
    Search(String),
    /// Select the items here whose names match this pattern (`*.pdf`), and only those.
    SelectPattern(String),
    /// Put this in the box and keep it open (the start of a pattern).
    Fill(&'static str),
}

/// One row: its name, its shortcut ("" for none) and what it does.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub keys: &'static str,
    pub action: Action,
    /// What it does in a few plain words, for the guide (help or ?); "" keeps it out of the guide.
    pub about: &'static str,
    /// Where the guide lists it.
    pub group: Group,
}

impl Item {
    fn new(name: &str, keys: &'static str, action: Action) -> Item {
        Item { name: name.to_string(), keys, action, about: "", group: Group::Glint }
    }

    /// Where the guide lists it, and what it says it does.
    fn guide(self, group: Group, about: &'static str) -> Item {
        Item { group, about, ..self }
    }
}

/// The guide's groups (help or ?), in the order it shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Typing,
    Tabs,
    Places,
    Files,
    Select,
    View,
    Sort,
    Glint,
}

impl Group {
    /// The title the guide shows above the group.
    pub fn title(self) -> &'static str {
        match self {
            Group::Typing => "Things to type",
            Group::Tabs => "Tabs and windows",
            Group::Places => "Going places",
            Group::Files => "Files and folders",
            Group::Select => "Finding and selecting",
            Group::View => "How it looks",
            Group::Sort => "Sorting",
            Group::Glint => "Glint",
        }
    }
}

/// What the window should do after the box handled something.
#[derive(Clone, Debug, PartialEq)]
pub enum Choice {
    Nothing,
    Redraw,
    Close,
    /// Close the box and do this.
    Run(Action),
    /// The box lost the keyboard: it closes unless the window itself went to the background.
    LostFocus,
}

/// Everything the box can do right now, in the order shown before anything is typed, each with
/// what it does for the guide.
pub fn actions(app: &AppState) -> Vec<Item> {
    use Action::Run;
    use Group as G;
    let s = &app.settings;
    let tab = app.tab();
    let here = tab.path.clone();
    let on_off = |on: bool, name: &str| format!("Turn {} {name}", if on { "off" } else { "on" });
    let show_hide = |shown: bool, name: &str| format!("{} {name}", if shown { "Hide" } else { "Show" });
    let mut v = vec![
        Item::new("New tab", "Ctrl+T", Run(Command::NewTab)).guide(G::Tabs, "Opens a new tab"),
        Item::new("Close tab", "Ctrl+W", Run(Command::CloseTab(app.active))).guide(G::Tabs, "Closes this tab"),
        Item::new("Reopen closed tab", "Ctrl+Shift+T", Run(Command::ReopenClosedTab)).guide(G::Tabs, "Brings back the last tab you closed"),
        Item::new("New window", "Ctrl+N", Run(Command::NewWindow)).guide(G::Tabs, "Opens a second Glint window"),
        Item::new("New folder", "Ctrl+Shift+N", Run(Command::NewFolder)).guide(G::Files, "Makes a new folder here"),
        Item::new("New text file", "", Run(Command::NewTextFile)).guide(G::Files, "Makes a new text file here"),
        Item::new("Refresh", "F5", Run(Command::Refresh)).guide(G::View, "Loads this folder again"),
        Item::new("Go up", "Alt+Up", Run(Command::Up)).guide(G::Places, "Goes to the folder this one is in"),
        Item::new("Back", "Alt+Left", Run(Command::Back)).guide(G::Places, "Goes to the folder you were in before"),
        Item::new("Forward", "Alt+Right", Run(Command::Forward)).guide(G::Places, "Goes forward again after Back"),
        Item::new("Undo", "Ctrl+Z", Run(Command::Undo)).guide(G::Files, "Takes back your last move, copy, rename or delete"),
        Item::new("Redo", "Ctrl+Y", Run(Command::Redo)).guide(G::Files, "Does it again after Undo"),
        Item::new("Select all", "Ctrl+A", Action::SelectAll).guide(G::Select, "Selects everything here"),
        // (The guide shows *.pdf under its things to type instead.)
        Item::new("Select by pattern\u{2026}", "", Action::Fill("*.")),
        Item::new("Copy path", "Ctrl+Shift+C", Run(Command::CopyPath)).guide(G::Files, "Copies where the selected items are, as text"),
        Item::new("Type a path", "Ctrl+L", Run(Command::EditPath)).guide(G::Places, "Puts the cursor in the address bar"),
        Item::new("Filter", "Ctrl+F", Run(Command::FocusFilter)).guide(G::Select, "Shows only the items with what you type in their name"),
        Item::new(&show_hide(s.show_hidden, "hidden files"), "Ctrl+H", Run(Command::ToggleHidden)).guide(G::View, "The files Windows keeps out of sight"),
        Item::new(&show_hide(s.show_sidebar, "sidebar"), "Ctrl+B", Run(Command::ToggleSidebar)).guide(G::View, "The list of places on the left"),
        Item::new(&show_hide(s.show_status_bar, "status bar"), "", Run(Command::ShowStatusBar(!s.show_status_bar))).guide(G::View, "The line at the bottom that counts the items"),
        Item::new(&show_hide(s.show_details, "details panel"), "Alt+P", Run(Command::ToggleDetails)).guide(G::View, "The panel on the right with a file's details"),
        match app.split {
            Some(_) => Item::new("Close split view", "Ctrl+\\", Run(Command::ToggleSplit)).guide(G::Tabs, "Goes back to one folder"),
            None => Item::new("Split view", "Ctrl+\\", Run(Command::ToggleSplit)).guide(G::Tabs, "Shows two folders side by side"),
        },
    ];
    if app.split.is_some() {
        v.push(Item::new("Switch side", "F6", Run(Command::OtherPane)).guide(G::Tabs, "Moves to the other side of split view"));
    }
    // Tab sets, as in a tab's right-click menu: save these tabs, open or remove a saved set.
    v.push(Item::new("Save tabs as a set\u{2026}", "", Run(Command::SaveTabSet)).guide(G::Tabs, "Keeps these tabs, to open them all again later"));
    for (n, set) in s.tab_sets.iter().enumerate() {
        v.push(Item::new(&format!("Open tab set: {}", set.name), "", Run(Command::OpenTabSet(n))));
    }
    for (n, set) in s.tab_sets.iter().enumerate() {
        v.push(Item::new(&format!("Remove tab set: {}", set.name), "", Run(Command::DeleteTabSet(n))));
    }
    // The views, with their Ctrl+Shift+1 to 6.
    const VIEW_KEYS: [&str; 6] = ["Ctrl+Shift+1", "Ctrl+Shift+2", "Ctrl+Shift+3", "Ctrl+Shift+4", "Ctrl+Shift+5", "Ctrl+Shift+6"];
    for (i, (mode, name)) in ViewMode::ALL.iter().enumerate() {
        let about = match mode {
            ViewMode::ExtraLarge => "Very large pictures of each item",
            ViewMode::Large => "Large pictures of each item",
            ViewMode::Medium => "Medium pictures of each item",
            ViewMode::Tree => "Folders open right inside the list",
            ViewMode::List => "Small icons and names, side by side",
            ViewMode::Details => "One item per line, with columns",
        };
        v.push(Item::new(&format!("View: {name}"), VIEW_KEYS[i], Run(Command::SetView(*mode))).guide(G::View, about));
    }
    // Newest and biggest first, as a click on the column title does.
    let keys = [
        (SortKey::Name, "name", "A to Z"),
        (SortKey::Date, "date modified", "Last changed first"),
        (SortKey::Created, "date created", "Newest first"),
        (SortKey::Type, "type", "The same kinds of files together"),
        (SortKey::Size, "size", "Biggest first"),
        (SortKey::Dimensions, "picture size", "Biggest pictures and videos first"),
        (SortKey::Length, "length", "Longest videos and songs first"),
    ];
    for (key, name, about) in keys {
        let desc = matches!(key, SortKey::Date | SortKey::Created | SortKey::Size | SortKey::Dimensions | SortKey::Length);
        v.push(Item::new(&format!("Sort by {name}"), "", Run(Command::SetSort(key, desc))).guide(G::Sort, about));
    }
    // (As SORT > Custom order: on while a column decides here turns it back on.)
    let custom_on = s.custom_sort && !tab.column_now;
    let about = if custom_on { "Sorts by the columns again" } else { "Sorts with your own rules" };
    v.push(Item::new(&on_off(custom_on, "custom order"), "", Run(Command::SetCustomSort(!custom_on))).guide(G::Sort, about));
    v.push(Item::new("Edit custom order", "", Run(Command::OpenSortEditor)).guide(G::Sort, "Opens the editor for your own sort rules"));
    let about = if s.custom_views { "Every folder uses the same view" } else { "Folders keep the view you gave them" };
    v.push(Item::new(&on_off(s.custom_views, "custom views"), "", Run(Command::SetCustomViews(!s.custom_views))).guide(G::View, about));
    let about = if s.folder_sizes { "Stops adding up how big folders are" } else { "Shows how big each folder is" };
    v.push(Item::new(&on_off(s.folder_sizes, "folder sizes"), "", Action::FolderSizes(!s.folder_sizes)).guide(G::View, about));
    v.push(Item::new("Find duplicates", "", Run(Command::FindDuplicates)).guide(G::Files, "Finds files that are exact copies of each other"));
    v.push(match app.subfolders {
        true => Item::new("Stop searching subfolders", "Ctrl+Shift+F", Run(Command::ToggleSubfolders)).guide(G::Select, "The filter looks only in this folder"),
        false => Item::new("Search subfolders", "Ctrl+Shift+F", Run(Command::ToggleSubfolders)).guide(G::Select, "The filter also looks inside the folders here"),
    });
    v.push(Item::new("Rename many", "F2", Action::RenameMany).guide(G::Files, "Renames all the selected files at once"));
    v.push(Item::new("Properties", "Alt+Enter", Run(Command::Properties)).guide(G::Files, "Opens Windows' Properties window"));
    if !here.is_empty() {
        v.push(Item::new("Open terminal here", "Ctrl+`", Run(Command::OpenTerminal(here.clone()))).guide(G::Places, "Opens a command window in this folder"));
    }
    v.push(Item::new("Show in File Explorer", "", Run(Command::ShowInExplorer)).guide(G::Places, "Opens this folder in File Explorer"));
    if crate::app::can_pin(&here) {
        v.push(match app.is_pinned(&here) {
            true => Item::new("Unpin folder from sidebar", "Ctrl+D", Run(Command::TogglePin(here))).guide(G::Places, "Takes this folder off the sidebar"),
            false => Item::new("Pin folder to sidebar", "Ctrl+D", Run(Command::TogglePin(here))).guide(G::Places, "Adds this folder to the sidebar"),
        });
    }
    v.push(Item::new("Light or dark", "Ctrl+Shift+L", Run(Command::ToggleTheme)).guide(G::View, "Switches between the light and dark look"));
    v.push(Item::new("Settings", "Ctrl+,", Run(Command::OpenSettings(None))).guide(G::Glint, "Opens Glint's settings"));
    v.push(Item::new("Quit Glint", "Ctrl+Q", Run(Command::Quit)).guide(G::Glint, "Closes Glint completely"));
    v.push(Item::new("Help", "", Action::Fill("?")));
    // Every setting, by its title: Settings opens at it. (The guide has one line for them all.)
    for (title, page) in crate::settingswin::findable(&app.settings) {
        let name = format!("Setting: {title}");
        v.push(Item::new(&name, crate::settingswin::PAGES[page], Run(Command::OpenSetting(page, title))));
    }
    // (The guide has one line for them all.)
    for e in app.skins.entries() {
        v.push(Item::new(&format!("Skin: {}", e.name), "", Action::Skin(e.id.clone())));
    }
    v
}

/// Whether typed text asks for the guide: "?" or "help".
pub fn is_help(text: &str) -> bool {
    let t = text.trim();
    t == "?" || t.eq_ignore_ascii_case("help")
}

/// The guide (help or ?): a few things to type, then every command that says what it does, by
/// group, A to Z in each.
pub fn guide(all: &[Item]) -> Vec<Item> {
    let typed = [
        ("C:\\Users", "Type a folder's path to go there", "C:\\"),
        ("*.pdf", "Selects every PDF here", "*."),
        ("tag:red", "Selects everything with a red tag", "tag:"),
        ("holiday", "Any other word searches for it here", ""),
    ];
    let mut v: Vec<Item> = typed.iter().map(|&(name, about, start)| Item::new(name, "", Action::Fill(start)).guide(Group::Typing, about)).collect();
    v.extend(all.iter().filter(|it| !it.about.is_empty()).cloned());
    if all.iter().any(|it| matches!(it.action, Action::Skin(_))) {
        v.push(Item::new("Skins", "", Action::Fill("skin ")).guide(Group::Glint, "Type skin to pick a different look"));
    }
    if all.iter().any(|it| matches!(it.action, Action::Run(Command::OpenTabSet(_)))) {
        v.push(Item::new("Tab sets", "", Action::Fill("tab set ")).guide(Group::Tabs, "Type tab set to open one you saved"));
    }
    if all.iter().any(|it| matches!(it.action, Action::Run(Command::OpenSetting(..)))) {
        v.push(Item::new("Settings by name", "", Action::Fill("setting ")).guide(Group::Glint, "Type setting and a word to find a setting"));
    }
    v.sort_by_cached_key(|it| (it.group, it.name.to_lowercase()));
    v
}

/// Whether typed text is a place to go to: a drive ("C:\Users", "d:"), a network path
/// ("\\server\share"), the home folder ("~", "~\Downloads"), a %VARIABLE% or a "shell:" place.
pub fn looks_like_path(text: &str) -> bool {
    let t = text.trim().trim_matches('"');
    let b = t.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || b[2] == b'\\' || b[2] == b'/');
    let home = t == "~" || t.starts_with("~\\") || t.starts_with("~/");
    let var = t.starts_with('%') && t[1..].contains('%');
    let shell = t.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("shell:")) && t.len() > 6;
    drive || t.starts_with("\\\\") || home || var || shell
}

/// How well `name` matches what was typed: lower is better, None is no match. Every typed word
/// must be found in the name (in any order; the start of a word counts most, and a whole word as
/// much as the start of the name: "set" finds "Save tabs as a set" as well as "Settings"), or the
/// typed letters must be the first letters of the name's words ("sv" for Split view).
pub fn score(name: &str, query: &str) -> Option<u32> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let n = name.to_lowercase();
    if n.starts_with(&q) {
        let whole = n[q.len()..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        return Some(if whole { 0 } else { 1 });
    }
    let words: Vec<&str> = n.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut by_words = Some(1);
    for part in q.split_whitespace() {
        let s = if words.first().is_some_and(|w| w.starts_with(part)) || words.contains(&part) {
            0
        } else if words.iter().any(|w| w.starts_with(part)) {
            1
        } else if n.contains(part) {
            3
        } else if part.len() > 3 && part.strip_suffix('s').is_some_and(|one| words.contains(&one)) {
            // One typed in the plural ("tab sets" for "Save tabs as a set").
            1
        } else {
            by_words = None;
            break;
        };
        by_words = by_words.map(|t| t + s);
    }
    let initials: String = words.iter().filter_map(|w| w.chars().next()).collect();
    let by_letters = (q.chars().count() >= 2 && !q.contains(' ') && initials.starts_with(&q)).then_some(if initials == q { 1 } else { 2 });
    match (by_words, by_letters) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Whether typed text is a pattern for "Select": a `*` or `?` in it, or a colour tag (`tag:red`,
/// tags.rs), and not a path.
pub fn is_pattern(text: &str) -> bool {
    let t = text.trim();
    let tag = t.split_whitespace().any(|w| w.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("tag:")));
    (t.contains(['*', '?']) || tag) && !looks_like_path(t)
}

/// The rows to show for `text`: the best matches first (ties keep the list's order), a "Go to"
/// row first for a path, a "Select" row first for a pattern, and a "Search for" row last for
/// anything but a path. At most MAX_ROWS.
pub fn rows_for(all: &[Item], text: &str) -> Vec<Item> {
    let t = text.trim();
    if t.is_empty() {
        return all.iter().take(MAX_ROWS).cloned().collect();
    }
    let mut found: Vec<(u32, usize)> = all.iter().enumerate().filter_map(|(i, it)| score(&it.name, t).map(|s| (s, i))).collect();
    found.sort();
    if looks_like_path(t) {
        let go = Item::new(&format!("Go to {t}"), "", Action::Run(Command::Navigate(t.to_string())));
        let best = found.into_iter().take(MAX_ROWS - 1).map(|(_, i)| all[i].clone());
        return std::iter::once(go).chain(best).collect();
    }
    let search = Item::new(&format!("Search for \u{201c}{t}\u{201d} here"), "", Action::Search(t.to_string()));
    if is_pattern(t) {
        let select = Item::new(&format!("Select {t}"), "", Action::SelectPattern(t.to_string()));
        let best = found.into_iter().take(MAX_ROWS - 2).map(|(_, i)| all[i].clone());
        return std::iter::once(select).chain(best).chain(std::iter::once(search)).collect();
    }
    let best = found.into_iter().take(MAX_ROWS - 1).map(|(_, i)| all[i].clone());
    best.chain(std::iter::once(search)).collect()
}

/// Selects the items on screen whose names match `pattern` (`*.pdf`, `IMG_2024*`, `*holiday*`;
/// capitals don't matter) or that have its colour tag (`tag:red`), and only those, as the filter
/// box would keep them, but without hiding anything. The cursor goes to the first, which is
/// scrolled to. How many were selected.
pub fn select_matching(tab: &mut Tab, pattern: &str) -> usize {
    let filter = NameFilter::new(pattern);
    tab.clear_selection();
    let mut first = None;
    let mut n = 0;
    for &i in &tab.view {
        let i = i as usize;
        let e = &tab.folder.entries[i];
        if filter.keeps(&e.lower, || e.path_in(&tab.folder.path)) {
            tab.selected[i] = true;
            first.get_or_insert(i);
            n += 1;
        }
    }
    if first.is_some() {
        (tab.focus, tab.anchor) = (first, first);
        tab.reveal_focus = true;
    }
    n
}

/// The command box itself.
#[derive(Default)]
pub struct CommandBox {
    open: bool,
    /// Opened and not drawn yet: the next paint puts the cursor in the box.
    fresh: bool,
    edit: Option<TextBox>,
    all: Vec<Item>,
    rows: Vec<Item>,
    pick: usize,
    hover: Option<usize>,
    panel: Rect,
    row_rects: Vec<Rect>,
    /// The rows are the guide (help or ?): wider, with each group's title, and it scrolls.
    guide: bool,
    scroll: kit::Scroller,
    /// Scroll the picked row into view on the next paint.
    reveal: bool,
    /// Where the rows show (the guide's others are scrolled out of sight).
    list: Rect,
    /// How many rows Page Up and Page Down move through the guide.
    page: usize,
}

impl CommandBox {
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the box, empty, with what can be done right now.
    pub fn open(&mut self, window: HWND, app: &AppState) {
        let edit = self.edit.get_or_insert_with(|| {
            let b = TextBox::new(window, BOX_ID);
            b.set_hint("Type a command, a path, or ? for help");
            // SAFETY: a second subclass on our own EDIT (it runs before textbox.rs's); a plain function.
            unsafe {
                let _ = SetWindowSubclass(b.hwnd(), Some(keys_proc), 2, 0);
            }
            b
        });
        edit.set_text("");
        self.all = actions(app);
        self.show("");
        self.open = true;
        self.fresh = true;
    }

    /// The rows for `text`: the guide for help or ?, otherwise what matches it.
    fn show(&mut self, text: &str) {
        self.guide = is_help(text);
        self.rows = if self.guide { guide(&self.all) } else { rows_for(&self.all, text) };
        (self.pick, self.hover) = (0, None);
        self.scroll.jump_to(0.0);
    }

    /// Closes the box; the keyboard goes back to the window.
    pub fn close(&mut self, window: HWND) {
        if !self.open {
            return;
        }
        self.open = false;
        self.fresh = false;
        self.all.clear();
        self.rows.clear();
        self.row_rects.clear();
        if let Some(b) = self.edit.as_mut() {
            b.hide();
        }
        // SAFETY: handing the keyboard to our own window.
        unsafe {
            let _ = SetFocus(Some(window));
        }
    }

    /// Whether `p` is on the box (the pointer there is a plain arrow).
    pub fn contains(&self, p: Point) -> bool {
        self.open && self.panel.contains(p)
    }

    /// Whether row `i` is the first of its group (the guide shows the group's title above it).
    fn starts_group(&self, i: usize) -> bool {
        i == 0 || self.rows.get(i - 1).map(|p| p.group) != self.rows.get(i).map(|it| it.group)
    }

    /// Draws the box at the top middle of `area` (the files) and puts its text box in place. The
    /// guide is wider, as tall as the files allow, and scrolls.
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, scale: f32, area: Rect) {
        if !self.open {
            return;
        }
        let guide = self.guide;
        let w = (if guide { GUIDE_WIDTH } else { WIDTH }).min(area.w - 32.0).max(240.0);
        let x = area.x + ((area.w - w) / 2.0).max(0.0);
        let y = area.y + 16.0;
        // Each row's top, from the top of the list (in the guide, a group's title goes above it).
        let mut tops = Vec::with_capacity(self.rows.len());
        let mut at = 0.0;
        for i in 0..self.rows.len() {
            if guide && self.starts_group(i) {
                at += HEADING_H;
            }
            tops.push(at);
            at += ROW_H;
        }
        let room = area.bottom() - 16.0 - y - (PAD + FIELD_H + 6.0 + PAD);
        let list_h = if guide { at.min(room.max(3.0 * ROW_H)) } else { at };
        let h = PAD + FIELD_H + 6.0 + list_h + PAD;
        self.panel = Rect::new(x, y, w, h);

        // A soft shadow, then the panel in the skin's style (as the form box).
        if kit::glass(theme) {
            let radius = theme.corner(h).max(6.0);
            r.fill_round_rect(Rect::new(x - 1.0, y + 2.0, w + 2.0, h + 3.0), radius + 1.0, Color::rgba(0, 0, 0, 0.22));
            r.fill_paint(self.panel, radius, &theme.toolbar_paint);
            r.stroke_round_rect(self.panel.inset(0.5, 0.5), radius - 0.5, 1.0, theme.line);
        } else {
            r.fill_rect(Rect::new(x + 2.0, y + 3.0, w, h), Color::rgba(0, 0, 0, 0.25));
            r.fill_rect(self.panel.inset(-1.0, -1.0), theme.line);
            r.fill_rect(self.panel, theme.toolbar);
            r.fill_rect(Rect::new(x, y, w, 2.0), theme.accent);
        }

        let field = Rect::new(x + PAD, y + PAD, w - 2.0 * PAD, FIELD_H);
        r.fill_rect(field, theme.input);
        r.stroke_rect(field.inset(0.5, 0.5), 1.0, theme.input_border);
        if let Some(b) = self.edit.as_mut() {
            b.place(field.inset(8.0, 4.0), 0.0, scale);
            b.show();
            if std::mem::take(&mut self.fresh) {
                b.focus();
            }
        }

        let list = Rect::new(x + PAD, field.bottom() + 6.0, w - 2.0 * PAD, list_h);
        self.list = list;
        self.scroll.set_extent(at, list_h);
        self.page = ((list_h / ROW_H) as usize).saturating_sub(1).max(1);
        if std::mem::take(&mut self.reveal) {
            if let Some(&top) = tops.get(self.pick) {
                // (The first row of a group with its title.)
                let from = if guide && self.starts_group(self.pick) { top - HEADING_H } else { top };
                if from < self.scroll.offset {
                    self.scroll.jump_to(from);
                } else if top + ROW_H > self.scroll.offset + list_h {
                    self.scroll.jump_to(top + ROW_H - list_h);
                }
            }
        }
        let scrolls = self.scroll.max() > 0.0;
        // (Room for the scrollbar.)
        let row_w = list.w - if scrolls { 12.0 } else { 0.0 };
        let dy = list.y - self.scroll.offset;
        let keys_style = TextStyle::new(theme.muted).align(Align::Right);
        let about_style = TextStyle::new(theme.muted);
        let head_style = TextStyle::new(theme.muted).semibold();
        // The guide: the names in a column of their own, what each does after it.
        let name_w = if guide {
            let plain = TextStyle::new(theme.text);
            self.rows.iter().map(|it| r.measure(&it.name, &plain)).fold(0.0, f32::max).min(row_w * 0.4).ceil()
        } else {
            0.0
        };
        r.push_clip(list);
        self.row_rects.clear();
        for i in 0..self.rows.len() {
            let row = Rect::new(list.x, dy + tops[i], row_w, ROW_H);
            self.row_rects.push(row);
            let item = &self.rows[i];
            if guide && self.starts_group(i) {
                r.text(item.group.title(), Rect::new(row.x + 10.0, row.y - HEADING_H + 6.0, row.w - 20.0, HEADING_H - 6.0), &head_style);
            }
            if row.bottom() <= list.y || row.y >= list.bottom() {
                continue;
            }
            let picked = i == self.pick;
            if picked {
                r.fill_rect(row, theme.accent_tint);
                r.fill_rect(Rect::new(row.x, row.y, 3.0, row.h), theme.select_edge);
            } else if self.hover == Some(i) {
                r.fill_rect(row, theme.hover);
            }
            let keys_w = if item.keys.is_empty() { 0.0 } else { r.measure(item.keys, &keys_style) + 16.0 };
            if !item.keys.is_empty() {
                r.text(item.keys, Rect::new(row.x, row.y, row.w - 10.0, row.h), &keys_style);
            }
            let name_style = if picked { TextStyle::new(theme.selection_text) } else { TextStyle::new(theme.text) };
            if guide {
                r.text(&item.name, Rect::new(row.x + 10.0, row.y, name_w + 1.0, row.h), &name_style);
                let about_x = row.x + 10.0 + name_w + 24.0;
                r.text(item.about, Rect::new(about_x, row.y, (row.right() - 10.0 - keys_w - about_x).max(0.0), row.h), &about_style);
            } else {
                r.text(&item.name, Rect::new(row.x + 10.0, row.y, (row.w - 20.0 - keys_w).max(0.0), row.h), &name_style);
            }
        }
        r.pop_clip();
        if scrolls {
            kit::scrollbar(r, theme, kit::Bar::of(theme).track(list), &self.scroll, false, Hold::None);
        }
    }

    fn row_at(&self, p: Point) -> Option<usize> {
        if !self.list.contains(p) {
            return None;
        }
        self.row_rects.iter().position(|r| r.contains(p))
    }

    fn run(&mut self, i: usize) -> Choice {
        match self.rows.get(i).map(|it| it.action.clone()) {
            None => Choice::Nothing,
            // The box stays open with the text started, the cursor after it.
            Some(Action::Fill(start)) => {
                if let Some(b) = &self.edit {
                    b.set_text(start);
                    let end = start.encode_utf16().count();
                    b.select_range(end, end);
                    b.focus();
                }
                self.show(start);
                Choice::Redraw
            }
            Some(a) => Choice::Run(a),
        }
    }

    /// A key, from the text box (Up, Down, Page Up, Page Down, Ctrl+U) or the window itself.
    pub fn key(&mut self, vk: u16, m: Mods) -> Choice {
        let n = self.rows.len();
        let to = match vk {
            v if v == VK_RETURN.0 => return self.run(self.pick),
            v if v == VK_ESCAPE.0 => return Choice::Close,
            v if v == u16::from(b'U') && m.ctrl => return Choice::Close,
            _ if n == 0 => return Choice::Nothing,
            v if v == VK_UP.0 => (self.pick + n - 1) % n,
            v if v == VK_DOWN.0 => (self.pick + 1) % n,
            // The guide: a page at a time.
            v if v == VK_PRIOR.0 && self.guide => self.pick.saturating_sub(self.page.max(1)),
            v if v == VK_NEXT.0 && self.guide => (self.pick + self.page.max(1)).min(n - 1),
            v if v == VK_PRIOR.0 || v == VK_HOME.0 => 0,
            v if v == VK_NEXT.0 || v == VK_END.0 => n - 1,
            _ => return Choice::Nothing,
        };
        self.pick = to;
        self.reveal = true;
        Choice::Redraw
    }

    pub fn mouse_move(&mut self, p: Point) -> Choice {
        let hover = self.row_at(p);
        if hover != self.hover {
            self.hover = hover;
            return Choice::Redraw;
        }
        Choice::Nothing
    }

    /// The mouse wheel over the box: the guide scrolls, three rows a notch.
    pub fn wheel(&mut self, p: Point, notches: f32) -> Choice {
        if !self.guide || !self.panel.contains(p) {
            return Choice::Nothing;
        }
        let before = self.scroll.offset;
        self.scroll.jump_to(before - notches * 3.0 * ROW_H);
        if self.scroll.offset == before {
            return Choice::Nothing;
        }
        // (The row under the pointer moved away.)
        self.hover = None;
        Choice::Redraw
    }

    /// A button went down: a row runs (left button), a click outside closes the box.
    pub fn mouse_down(&mut self, p: Point, left: bool) -> Choice {
        if !self.panel.contains(p) {
            return Choice::Close;
        }
        match self.row_at(p) {
            Some(i) if left => self.run(i),
            _ => {
                // On the panel: the keyboard stays in the box.
                if let Some(b) = &self.edit {
                    b.focus();
                }
                Choice::Nothing
            }
        }
    }

    /// News from the text box. None: not the box's.
    pub fn text_event(&mut self, ev: &TextEvent) -> Option<Choice> {
        if ev.id != BOX_ID {
            return None;
        }
        if !self.open {
            return Some(Choice::Nothing);
        }
        Some(match ev.kind {
            TextEventKind::Changed => {
                let text = self.edit.as_ref().map(TextBox::text).unwrap_or_default();
                self.show(&text);
                Choice::Redraw
            }
            TextEventKind::Enter => self.run(self.pick),
            TextEventKind::Escape => Choice::Close,
            TextEventKind::LostFocus => Choice::LostFocus,
            TextEventKind::Down | TextEventKind::Tab => Choice::Nothing,
        })
    }
}

/// On the box's EDIT: the keys that move through the rows (and Ctrl+U) go to the window, which
/// hands them to the box (`CommandBox::key`).
unsafe extern "system" fn keys_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _uid: usize, _data: usize) -> LRESULT {
    if msg == WM_KEYDOWN {
        let vk = wparam.0 as u16;
        // SAFETY: plain key-state query.
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if [VK_UP.0, VK_DOWN.0, VK_PRIOR.0, VK_NEXT.0].contains(&vk) || (ctrl && vk == u16::from(b'U')) {
            // Sent, not posted, so the window sees Ctrl as it is now.
            // SAFETY: a message to our own parent window, on this same thread.
            unsafe {
                if let Ok(parent) = GetParent(hwnd) {
                    SendMessageW(parent, WM_KEYDOWN, Some(wparam), Some(lparam));
                }
            }
            return LRESULT(0);
        }
    }
    // Ctrl+U's character isn't typed.
    if msg == WM_CHAR && wparam.0 == 0x15 {
        return LRESULT(0);
    }
    // SAFETY: everything else goes on to textbox.rs's handling and the EDIT's own.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(names: &[&str]) -> Vec<Item> {
        names.iter().map(|n| Item::new(n, "", Action::SelectAll)).collect()
    }

    fn names(rows: &[Item]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    const LIST: [&str; 8] = ["New tab", "Close tab", "New text file", "Split view", "Show hidden files", "View: Details", "Sort by size", "Settings"];

    #[test]
    fn typed_words_match_in_any_order_and_any_case() {
        assert!(score("Show hidden files", "HIDDEN").is_some());
        assert!(score("Show hidden files", "files hidden").is_some());
        assert!(score("Split view", "view split").is_some());
        assert!(score("Split view", "split tree").is_none(), "every word must be found");
        assert!(score("Split view", "xyz").is_none());
        assert_eq!(score("Anything", "  "), Some(0));
    }

    #[test]
    fn a_pattern_offers_select_first_and_search_last() {
        let all = items(&LIST);
        let rows = rows_for(&all, "*.pdf");
        assert_eq!(rows[0].name, "Select *.pdf");
        assert_eq!(rows[0].action, Action::SelectPattern("*.pdf".into()));
        assert_eq!(rows.last().map(|r| r.action.clone()), Some(Action::Search("*.pdf".into())));
        assert!(rows.len() <= MAX_ROWS);
        assert!(is_pattern("IMG_2024*") && is_pattern("report-??.txt") && is_pattern("tag:red") && is_pattern("invoice Tag:green"));
        assert!(!is_pattern("holiday"));
        assert!(!is_pattern("C:\\Users\\*.pdf"), "a path is for Go to");
    }

    #[test]
    fn first_letters_find_a_name() {
        assert!(score("Split view", "sv").is_some());
        assert!(score("New text file", "ntf").is_some());
        assert!(score("New text file", "nt").is_some(), "the start of the first letters counts");
        assert!(score("Split view", "vs").is_none());
        let rows = rows_for(&items(&LIST), "sv");
        assert_eq!(rows[0].name, "Split view");
    }

    #[test]
    fn the_best_match_comes_first() {
        let all = items(&LIST);
        // The name's start beats a later word.
        assert_eq!(rows_for(&all, "s")[0].name, "Split view");
        assert_eq!(rows_for(&all, "sort")[0].name, "Sort by size");
        // "tab": a word's start; both tabs, in the list's order.
        assert_eq!(&names(&rows_for(&all, "tab"))[..2], ["New tab", "Close tab"]);
        // A whole word at the start beats one found inside another word.
        assert_eq!(rows_for(&all, "view")[0].name, "View: Details");
        // "nt": "New tab" starts with it? No - but its letters are exactly N T.
        assert_eq!(rows_for(&all, "nt")[0].name, "New tab");
    }

    #[test]
    fn text_ends_with_a_search_row_and_a_path_starts_with_go_to() {
        let all = items(&LIST);
        let rows = rows_for(&all, "split");
        assert_eq!(rows.last().map(|r| r.action.clone()), Some(Action::Search("split".into())));
        assert_eq!(rows.last().unwrap().name, "Search for \u{201c}split\u{201d} here");
        let rows = rows_for(&all, "nothing like it");
        assert_eq!(rows.len(), 1, "only the search row");
        let rows = rows_for(&all, "C:\\Users");
        assert_eq!(rows[0].action, Action::Run(Command::Navigate("C:\\Users".into())));
        assert_eq!(rows[0].name, "Go to C:\\Users");
        assert!(!rows.iter().any(|r| matches!(r.action, Action::Search(_))));
    }

    #[test]
    fn at_most_eight_rows() {
        let many: Vec<String> = (0..30).map(|i| format!("Thing {i}")).collect();
        let all = items(&many.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(rows_for(&all, "").len(), MAX_ROWS);
        let rows = rows_for(&all, "thing");
        assert_eq!(rows.len(), MAX_ROWS);
        assert!(matches!(rows[MAX_ROWS - 1].action, Action::Search(_)), "the search row is always there");
    }

    #[test]
    fn paths_are_told_from_words() {
        for p in ["C:\\Users", "c:", "d:/games", "\\\\server\\share", "~", "~\\Downloads", "%USERPROFILE%", "%appdata%\\x", "shell:startup", "SHELL:Downloads", " \"C:\\a b\" "] {
            assert!(looks_like_path(p), "{p}");
        }
        for t in ["split", "c", "cd", "c:x", "~x", "%", "50%", "shell:", "sort by size", "a:b"] {
            assert!(!looks_like_path(t), "{t}");
        }
    }

    #[test]
    fn help_or_a_question_mark_shows_every_command_with_what_it_does() {
        assert!(is_help("?") && is_help(" help ") && is_help("HELP"));
        assert!(!is_help("??") && !is_help("helping") && !is_help("*.pdf"));
        let app = AppState::for_tests(crate::settings::Settings::default());
        let all = actions(&app);
        let g = guide(&all);
        // Each command that says what it does is there once; only the pattern starter, Help and
        // the skins say nothing (the guide has a line for each kind instead).
        for it in all.iter().filter(|it| !it.about.is_empty()) {
            assert_eq!(g.iter().filter(|x| x.name == it.name).count(), 1, "{}", it.name);
        }
        let quiet: Vec<&str> = all.iter().filter(|it| it.about.is_empty() && !matches!(it.action, Action::Skin(_) | Action::Run(Command::OpenSetting(..)))).map(|it| it.name.as_str()).collect();
        assert_eq!(quiet, ["Select by pattern\u{2026}", "Help"]);
        assert!(g.iter().any(|it| it.name == "*.pdf" && it.action == Action::Fill("*.")));
        assert!(g.iter().any(|it| it.name == "Skins"));
        assert!(g.iter().any(|it| it.name == "Settings by name"));
        // By group, things to type first, A to Z in each; a short line each.
        assert_eq!(g[0].group, Group::Typing);
        assert!(g.windows(2).all(|w| (w[0].group, w[0].name.to_lowercase()) <= (w[1].group, w[1].name.to_lowercase())));
        assert!(g.iter().all(|it| !it.about.is_empty() && it.about.chars().count() <= 60), "short lines");
    }

    #[test]
    fn the_guide_scrolls_and_its_rows_still_run() {
        let app = AppState::for_tests(crate::settings::Settings::default());
        let mut b = CommandBox { all: actions(&app), ..Default::default() };
        b.show("help");
        assert!(b.guide && b.rows.len() > MAX_ROWS, "every command, not eight");
        // A command runs as from its own row.
        let i = b.rows.iter().position(|it| it.name == "New tab").expect("listed");
        assert_eq!(b.run(i), Choice::Run(Action::Run(Command::NewTab)));
        // Page Down goes a page at a time and stops at the end.
        b.page = 5;
        b.key(VK_NEXT.0, Mods::default());
        assert_eq!(b.pick, 5);
        for _ in 0..40 {
            b.key(VK_NEXT.0, Mods::default());
        }
        assert_eq!(b.pick, b.rows.len() - 1);
        // The wheel scrolls it (over the box only).
        (b.panel, b.list) = (Rect::new(0.0, 0.0, 780.0, 400.0), Rect::new(8.0, 48.0, 764.0, 344.0));
        b.scroll.set_extent(2000.0, 344.0);
        assert_eq!(b.wheel(Point::new(900.0, 10.0), -1.0), Choice::Nothing);
        assert_eq!(b.wheel(Point::new(100.0, 100.0), -1.0), Choice::Redraw);
        assert_eq!(b.scroll.offset, 3.0 * ROW_H);
        // A thing to type starts it in the box, and the guide makes way for what it finds.
        let i = b.rows.iter().position(|it| it.name == "*.pdf").expect("listed");
        assert_eq!(b.run(i), Choice::Redraw);
        assert!(!b.guide);
        assert_eq!(b.rows[0].action, Action::SelectPattern("*.".into()));
    }

    #[test]
    fn settings_are_found_by_name_and_open_at_their_row() {
        let app = AppState::for_tests(crate::settings::Settings::default());
        let all = actions(&app);
        let rows = rows_for(&all, "hidden");
        let found = rows.iter().find(|r| r.name == "Setting: Hidden files").expect("found by a word");
        assert_eq!(found.keys, "Files");
        assert_eq!(found.action, Action::Run(Command::OpenSetting(2, "Hidden files".into())));
        // Any setting on any page, once each; the shortcut list isn't one.
        let settings: Vec<&Item> = all.iter().filter(|i| matches!(i.action, Action::Run(Command::OpenSetting(..)))).collect();
        for t in ["Setting: Skin", "Setting: Zip icon", "Setting: Open Glint to", "Setting: Check for updates automatically"] {
            assert_eq!(settings.iter().filter(|i| i.name == t).count(), 1, "{t}");
        }
        assert!(!settings.iter().any(|i| i.name.contains("Ctrl+")), "no shortcuts");
        assert!(rows_for(&all, "setting zip").iter().any(|r| r.name == "Setting: Zip icon"));
    }

    /// Saving the tabs as a set, and each saved set (to open or remove), are found by "tab set",
    /// "set" or the set's name, as in a tab's right-click menu.
    #[test]
    fn tab_sets_are_found() {
        use crate::settings::{Settings, TabSet};
        let set = |name: &str| TabSet { name: name.into(), tabs: Vec::new() };
        let none = AppState::for_tests(Settings::default());
        let all = actions(&none);
        for typed in ["tab set", "set", "Tab Sets", "save tabs", "sets", "  TAB   set "] {
            let rows = rows_for(&all, typed);
            assert!(rows.iter().any(|r| r.action == Action::Run(Command::SaveTabSet)), "{typed}: {:?}", names(&rows));
        }
        assert!(!all.iter().any(|r| matches!(r.action, Action::Run(Command::OpenTabSet(_)))), "no sets, nothing to open");
        assert!(!guide(&all).iter().any(|r| r.name == "Tab sets"));
        // Two sets, one with an odd name.
        let app = AppState::for_tests(Settings { tab_sets: vec![set("Work"), set("Photos & 2024 \u{2014} trip")], ..Default::default() });
        let all = actions(&app);
        let rows = rows_for(&all, "tab set");
        for want in [Command::SaveTabSet, Command::OpenTabSet(0), Command::OpenTabSet(1), Command::DeleteTabSet(0), Command::DeleteTabSet(1)] {
            assert!(rows.iter().any(|r| r.action == Action::Run(want.clone())), "{want:?}: {:?}", names(&rows));
        }
        let rows = rows_for(&all, "set");
        assert_eq!(rows[0].action, Action::Run(Command::SaveTabSet), "{:?}", names(&rows));
        assert_eq!(rows[1].action, Action::Run(Command::OpenTabSet(0)), "{:?}", names(&rows));
        assert_eq!(rows_for(&all, "work")[0].action, Action::Run(Command::OpenTabSet(0)));
        assert_eq!(rows_for(&all, "open trip")[0].action, Action::Run(Command::OpenTabSet(1)));
        assert_eq!(rows_for(&all, "remove work")[0].action, Action::Run(Command::DeleteTabSet(0)));
        // Settings still come first for what starts their name.
        assert_eq!(rows_for(&all, "settings")[0].name, "Settings");
        assert_eq!(rows_for(&all, "setting zip")[0].name, "Setting: Zip icon");
        // The guide tells how to find them.
        assert!(guide(&all).iter().any(|r| r.name == "Tab sets" && r.group == Group::Tabs && r.action == Action::Fill("tab set ")));
        assert!(guide(&all).iter().any(|r| r.action == Action::Run(Command::SaveTabSet)));
    }

    #[test]
    fn every_action_has_a_name_and_skins_are_listed() {
        let app = AppState::for_tests(crate::settings::Settings::default());
        let all = actions(&app);
        assert!(all.iter().all(|i| !i.name.is_empty()));
        assert!(all.iter().any(|i| i.name == "Split view" && i.keys == "Ctrl+\\"));
        assert!(all.iter().any(|i| i.name == "Show hidden files" && i.keys == "Ctrl+H"));
        assert!(all.iter().any(|i| i.name == "View: Details" && i.keys == "Ctrl+Shift+6"));
        assert!(all.iter().any(|i| i.name == "View: Extra large icons" && i.keys == "Ctrl+Shift+1"));
        assert!(!all.iter().any(|i| i.name == "Switch side"), "only in split view");
        let skins = all.iter().filter(|i| matches!(i.action, Action::Skin(_))).count();
        assert_eq!(skins, app.skins.entries().len());
        assert!(skins > 0);
    }

    /// A tab at C:\x showing these names (a name ending in "\" is a folder, one starting with "." hidden).
    fn tab_with(names: &[&str]) -> Tab {
        use crate::folder::{Entry, EntryKind, Folder};
        let entries = names
            .iter()
            .map(|n| {
                let name = n.trim_end_matches('\\');
                Entry {
                    name: name.into(),
                    lower: name.to_lowercase(),
                    kind: if n.ends_with('\\') { EntryKind::Folder } else { EntryKind::File },
                    size: 1,
                    modified: 0,
                    created: 0,
                    attributes: if name.starts_with('.') { 2 } else { 0 },
                    ext: String::new(),
                    type_name: std::sync::Arc::from("File"),
                    target: None,
                    free: 0,
                }
            })
            .collect::<Vec<_>>();
        let mut t = Tab::new(1, "C:\\x".into());
        t.selected = vec![false; entries.len()];
        t.folder = Folder { path: "C:\\x".into(), entries, ..Default::default() };
        t.rebuild_view();
        t
    }

    fn picked(t: &Tab) -> Vec<String> {
        let mut v: Vec<String> = t.selected_entries().iter().map(|&i| t.folder.entries[i].name.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn empty_or_blank_input_lists_the_first_commands_and_offers_no_search() {
        let all = items(&LIST);
        for blank in ["", " ", "     ", "\t", " \t "] {
            let rows = rows_for(&all, blank);
            assert_eq!(names(&rows), names(&all), "{blank:?}: everything, in order");
            assert!(!rows.iter().any(|r| matches!(r.action, Action::Search(_))), "{blank:?}");
            assert!(!is_help(blank) && !is_pattern(blank) && !looks_like_path(blank));
        }
        // With no commands at all there is still a search row for text, and nothing for blanks.
        assert!(rows_for(&[], "   ").is_empty());
        assert_eq!(rows_for(&[], "x").len(), 1);
    }

    #[test]
    fn odd_text_never_breaks_the_rows() {
        let all = items(&LIST);
        let long = "a".repeat(20_000);
        let long_words = "split ".repeat(3_000);
        let odd = [
            "[", "]", "[abc]", "[a-z]*", "(", "\\", "/", "\"", "'", "%", "%%", "~~", "\u{0}", "caf\u{e9}", "CAF\u{c9}", "\u{130}stanbul", "\u{1f600}", "\u{1f600}*", "\u{fffd}", "a\tb", "tag:",
            "TAG:Red", "?", "??", "*", "**", "*?*", &long, &long_words,
        ];
        for text in odd {
            let rows = rows_for(&all, text);
            assert!(!rows.is_empty() && rows.len() <= MAX_ROWS, "{:?}: {} rows", text.chars().take(10).collect::<String>(), rows.len());
            // Whatever is typed can always be searched for, or gone to, or selected.
            assert!(rows.iter().any(|r| matches!(r.action, Action::Search(_) | Action::Run(Command::Navigate(_)) | Action::SelectPattern(_))), "{text:?}");
            if is_pattern(text) {
                assert!(matches!(rows[0].action, Action::SelectPattern(_)) && matches!(rows.last().unwrap().action, Action::Search(_)));
            }
            // And scoring any name against it is just as safe.
            let _ = score("Sort by size", text);
            let _ = score(text, "sv");
        }
    }

    #[test]
    fn a_name_that_means_nothing_gets_only_the_search_row() {
        let all = items(&LIST);
        for text in ["qzx", "xyzzy plugh", "splitt", "\u{1f600}", "caf\u{e9}"] {
            let rows = rows_for(&all, text);
            assert_eq!(rows.len(), 1, "{text:?}");
            assert_eq!(rows[0].action, Action::Search(text.to_string()));
        }
        // Spaces around what is typed don't change it (the row says it trimmed).
        assert_eq!(rows_for(&all, "  qzx  ")[0].action, Action::Search("qzx".into()));
        // Capitals and accents: capitals don't matter, accents do.
        assert_eq!(rows_for(&all, "SPLIT VIEW")[0].name, "Split view");
        assert!(score("Caf\u{e9} menu", "CAF\u{c9}").is_some());
        assert!(score("Cafe menu", "caf\u{e9}").is_none());
    }

    #[test]
    fn help_is_help_only_when_it_is_all_that_was_typed() {
        for t in ["?", " ? ", "help", "Help", "HELP", "\thelp\t"] {
            assert!(is_help(t), "{t:?}");
        }
        for t in ["", "??", "? ?", "help?", "help me", "helpful", "he lp", "\u{ff1f}", "*", "tag:help"] {
            assert!(!is_help(t), "{t:?}");
        }
        // "??" and "help?" are patterns, not the guide.
        assert!(is_pattern("??") && is_pattern("help?"));
        // Typing "?" shows the guide, and it goes away again when more is typed.
        let app = AppState::for_tests(crate::settings::Settings::default());
        let mut b = CommandBox { all: actions(&app), ..Default::default() };
        b.show("?");
        assert!(b.guide);
        b.show("?x");
        assert!(!b.guide && b.rows.len() <= MAX_ROWS);
        b.show("  HELP ");
        assert!(b.guide);
        b.show("");
        assert!(!b.guide && b.rows.len() == MAX_ROWS && b.pick == 0);
    }

    #[test]
    fn the_guide_has_every_group_once_and_no_blank_lines() {
        let app = AppState::for_tests(crate::settings::Settings::default());
        let g = guide(&actions(&app));
        for group in [Group::Typing, Group::Tabs, Group::Places, Group::Files, Group::Select, Group::View, Group::Sort, Group::Glint] {
            assert!(g.iter().any(|it| it.group == group), "{}", group.title());
            assert!(!group.title().is_empty());
        }
        // No row repeats, and every row has a name and a line saying what it does.
        let mut seen = std::collections::HashSet::new();
        for it in &g {
            assert!(!it.name.trim().is_empty() && !it.about.trim().is_empty());
            assert!(seen.insert((it.group, it.name.clone())), "{} twice", it.name);
        }
        // A guide made from nothing is just the things to type.
        assert_eq!(guide(&[]).len(), 4);
        // Keys and clicks on an empty list don't panic.
        let mut b = CommandBox::default();
        assert_eq!(b.key(VK_DOWN.0, Mods::default()), Choice::Nothing);
        assert_eq!(b.key(VK_RETURN.0, Mods::default()), Choice::Nothing);
        assert_eq!(b.run(0), Choice::Nothing);
    }

    #[test]
    fn up_and_down_go_round_the_rows() {
        let mut b = CommandBox { all: items(&LIST), ..Default::default() };
        b.show("");
        let n = b.rows.len();
        b.key(VK_UP.0, Mods::default());
        assert_eq!(b.pick, n - 1, "up from the first goes to the last");
        b.key(VK_DOWN.0, Mods::default());
        assert_eq!(b.pick, 0);
        // Typing again starts at the first row.
        b.key(VK_DOWN.0, Mods::default());
        b.show("tab");
        assert_eq!(b.pick, 0);
        // The search row hands its text on; a row that isn't there does nothing.
        let last = b.rows.len() - 1;
        assert_eq!(b.run(last), Choice::Run(Action::Search("tab".into())));
        assert_eq!(b.run(99), Choice::Nothing);
    }

    #[test]
    fn a_pattern_with_no_match_selects_nothing_and_clears_the_old_selection() {
        let mut t = tab_with(&["a.jpg", "b.JPG", "c.png"]);
        assert_eq!(select_matching(&mut t, "*.jpg"), 2);
        assert_eq!(picked(&t), ["a.jpg", "b.JPG"]);
        assert_eq!(select_matching(&mut t, "*.gif"), 0);
        assert!(picked(&t).is_empty(), "the earlier selection is gone");
        // Twice in a row: the same.
        assert_eq!(select_matching(&mut t, "*.gif"), 0);
        assert_eq!(select_matching(&mut t, "*.png"), 1);
        assert_eq!(select_matching(&mut t, "*.png"), 1, "the second time selects the same, not more");
        assert_eq!(picked(&t), ["c.png"]);
        // An empty folder.
        let mut empty = tab_with(&[]);
        assert_eq!(select_matching(&mut empty, "*"), 0);
        assert!(empty.focus.is_none());
    }

    #[test]
    fn a_pattern_that_matches_everything_selects_every_visible_item() {
        let mut t = tab_with(&["a.txt", "B", "sub\\", ".hidden", "no dot"]);
        // Hidden items aren't on screen, so they aren't selected.
        assert_eq!(t.view.len(), 4);
        assert_eq!(select_matching(&mut t, "*"), 4);
        assert_eq!(picked(&t), ["B", "a.txt", "no dot", "sub"]);
        assert!(!t.selected[3], "the hidden one stays out");
        // Folders are matched by name like files.
        assert_eq!(select_matching(&mut t, "su?"), 1);
        assert_eq!(picked(&t), ["sub"]);
        // `?` is exactly one character.
        assert_eq!(select_matching(&mut t, "?"), 1);
        assert_eq!(picked(&t), ["B"]);
        assert_eq!(select_matching(&mut t, "??"), 0);
    }

    #[test]
    fn brackets_and_other_odd_characters_in_a_pattern_are_plain_letters() {
        let mut t = tab_with(&["[draft] plan.txt", "d plan.txt", "a+b (1).txt", "100%.txt", "#1 song.mp3", "emoji \u{1f600}.png", "\u{e9}t\u{e9}.txt", "\u{c9}T\u{c9}2.TXT"]);
        assert_eq!(select_matching(&mut t, "[draft]*"), 1, "[ ] aren't a character set");
        assert_eq!(picked(&t), ["[draft] plan.txt"]);
        assert_eq!(select_matching(&mut t, "[d]*"), 0);
        assert_eq!(select_matching(&mut t, "*(1)*"), 1);
        assert_eq!(select_matching(&mut t, "a+b*"), 1);
        assert_eq!(select_matching(&mut t, "100%*"), 1);
        assert_eq!(select_matching(&mut t, "#?*"), 1);
        assert_eq!(select_matching(&mut t, "*\u{1f600}*"), 1);
        // One emoji is one `?`, however many bytes it takes.
        assert_eq!(select_matching(&mut t, "emoji ?.png"), 1);
        // Accents and capitals: e-acute and its capital are the same letter in another case.
        assert_eq!(select_matching(&mut t, "\u{c9}T\u{c9}*"), 2);
        assert_eq!(select_matching(&mut t, "*.TxT"), 6);
        // Spaces around the pattern are not part of it; spaces inside are.
        assert_eq!(select_matching(&mut t, "   d plan*  "), 1);
    }

    #[test]
    fn a_very_long_pattern_or_name_is_still_quick() {
        let long_name = format!("{}.txt", "a".repeat(5_000));
        let mut t = tab_with(&[long_name.as_str(), "b.txt"]);
        let start = std::time::Instant::now();
        assert_eq!(select_matching(&mut t, &format!("*{}*", "a*".repeat(500))), 1);
        assert_eq!(select_matching(&mut t, &"*".repeat(100_000)), 2);
        assert_eq!(select_matching(&mut t, &format!("{}b", "a*".repeat(2_000))), 0);
        assert_eq!(select_matching(&mut t, &"?".repeat(10_000)), 0);
        assert!(start.elapsed() < std::time::Duration::from_secs(3), "{:?}", start.elapsed());
        // A path-sized pattern never reaches select_matching: it is a "Go to".
        assert!(!is_pattern(&format!("C:\\{}\\*.txt", "x".repeat(300))));
    }

    #[test]
    fn a_tag_pattern_selects_the_tagged_items_and_a_name_narrows_it() {
        let dir = std::env::temp_dir().join(format!("glint-palette-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut tags = crate::tags::Tags::at(dir.join("tags.txt"));
        let mut t = tab_with(&["a.pdf", "b.pdf", "c.txt", "d.txt"]);
        // Nothing is tagged yet.
        assert_eq!(select_matching(&mut t, "tag:red"), 0);
        assert_eq!(select_matching(&mut t, "tag:"), 0);
        tags.set(&["C:\\x\\a.pdf".to_string(), "C:\\x\\C.TXT".to_string()], Some(crate::tags::Tag::Red));
        tags.set(&["C:\\x\\b.pdf".to_string()], Some(crate::tags::Tag::Blue));
        assert_eq!(select_matching(&mut t, "tag:red"), 2);
        assert_eq!(picked(&t), ["a.pdf", "c.txt"], "the tag's path is compared without minding capitals");
        assert_eq!(select_matching(&mut t, "TAG:Red *.pdf"), 1);
        assert_eq!(select_matching(&mut t, "tag:"), 3);
        assert_eq!(select_matching(&mut t, "tag:red tag:blue"), 3);
        assert_eq!(select_matching(&mut t, "tag:green"), 0);
        assert_eq!(select_matching(&mut t, "tag:nonsense"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
