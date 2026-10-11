//! The app's state: tabs, what each tab shows, the drives, light/dark - and the commands that change it.
//!
//! Folders are read on background threads. Each request gets a generation number; the result is
//! sent back through a channel and the window is woken with WM_APP_LOADED. A result whose
//! generation no longer matches what its tab is waiting for (the person already went elsewhere)
//! is dropped. The UI thread never waits for the disk.
//!
//! The active tab's folder is watched (watch.rs). When something in it changes, the folder is read
//! again quietly in the background and merged in (`reload`, `merge_listing`): the selection, the
//! cursor and the rows on screen stay on the same items. A tab that wasn't watched while in the
//! background is read again the same way when it's shown.

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE, WM_APP};

pub use crate::folder::SortKey;
use crate::fileops::{self, Outcome};
use crate::folder::{self, Drive, Entry, Folder, NameFilter};
use crate::icons::Icons;
use crate::log;
use crate::recycle;
use crate::search::{self, Search};
use crate::settings::{self, ListSize, NewTab, SavedTab, Settings, ViewMode};
use crate::shell::{self, ComGuard};
use crate::shellmenu;
use crate::skin;
use crate::sortrecipe::{self, Recipe};
use crate::theme::{size, Theme};
use crate::ui::Command;
use crate::watch::{self, Armed, Watcher};

mod bin;
mod dropping;
mod files;
mod places;
pub use files::{Question, RenameRequest, THIS_PC_NOTE};
pub use places::can_pin;
// Background mode and folders from outside (life.rs): hiding, coming back, opening a folder sent by
// another glint.exe.
mod life;
// The skin in use, light or dark, and the skins folder's news (skin.rs).
mod skins;
// The tree view: folders opening in place (app/tree.rs).
mod tree;
pub use tree::Tree;

/// Posted to the window when a background job (folder, drives) has a result waiting.
pub const WM_APP_LOADED: u32 = WM_APP + 1;

/// How long a status-bar note ("Path copied") stays before the item counts come back.
pub const NOTE_SECONDS: f64 = 5.0;

/// While a folder takes a moment to read (a network drive, a disc spinning up), the list keeps
/// showing the folder you came from for this long - nearly every folder is in well before, so
/// nothing flashes - and then says "Loading…" instead of files that aren't in the folder the path
/// bar now names (and that a click would otherwise open).
pub const LOADING_DELAY: Duration = Duration::from_millis(250);

/// A folder read that is under way.
#[derive(Clone, Debug)]
pub struct Pending {
    pub generation: u64,
    pub started: Instant,
}

/// One tab: a folder, its history, and how it's shown.
pub struct Tab {
    pub id: u64,
    /// The folder this tab is at ("" = This PC). Changes as soon as you navigate, before the
    /// listing arrives.
    pub path: String,
    pub back: Vec<String>,
    pub forward: Vec<String>,
    /// The last listing that arrived (may still be the previous folder while a new one loads).
    pub folder: Folder,
    /// What the list shows, in order: indexes into `folder.entries` after filtering and sorting.
    pub view: Vec<u32>,
    /// Entry index -> position in `view` (u32::MAX if filtered out).
    pos: Vec<u32>,
    /// Per entry: is it selected?
    pub selected: Vec<bool>,
    /// The entry with the keyboard cursor.
    pub focus: Option<usize>,
    /// Where a Shift-selection starts.
    pub anchor: Option<usize>,
    pub sort: SortKey,
    pub descending: bool,
    /// What the filter box holds: only names it matches are shown (folder.rs `NameFilter`: several
    /// words, or a pattern like `*.pdf`).
    pub filter: String,
    /// Show hidden items / protected operating-system files too, and keep folders above files (the
    /// settings of the same names; every tab has a copy so `rebuild_view` needs nothing else).
    pub show_hidden: bool,
    pub show_system: bool,
    pub folders_first: bool,
    /// The custom sort order, while it's on ("CustomSort"): it sorts instead of the column, and
    /// decides where folders go (folders_first is left alone), in folders it has rules for (see
    /// `custom_here`).
    pub custom: Option<Arc<Recipe>>,
    /// The same rules, read for their views (-large_icons…), whether the custom sort order is on or
    /// off: a folder they give a view is shown that way (`own_view` elsewhere).
    pub views: Option<Arc<Recipe>>,
    /// Searching the folder's subfolders for the filter text (search.rs): while this is set,
    /// `folder` holds the results (each with its full path as `target`) and the folder's own
    /// listing waits in the search.
    pub search: Option<Search>,
    /// The folder on its way was asked for by `AppState::navigate`: if it turns out to be a file,
    /// the file opens and the tab stays where it was (a file path typed in the path bar). Holds
    /// the Forward list going there emptied, to put back then.
    opens_file: Option<Vec<String>>,
    /// How many entries show when no filter is typed (the listing minus hidden items), for "3 of
    /// 12 items".
    pub visible_total: usize,
    /// The list's scroll offset in DIPs. The list view owns the animation; it lives here so each
    /// tab keeps its place.
    pub scroll: f32,
    /// Ask the list to scroll the focused row into view on its next paint.
    pub reveal_focus: bool,
    pub loading: Option<Pending>,
    /// How long the last folder took from "go" to "on screen", in ms.
    pub load_ms: Option<f64>,
    /// Name to select when the next listing arrives (going up selects the folder you came from).
    select_after_load: Option<String>,
    /// A quiet re-read of the folder is under way (its generation; see `AppState::reload`).
    reloading: Option<u64>,
    /// The folder changed while it was being read: read it once more when that read is done.
    stale: bool,
    /// Items renamed in the folder since it was last merged in (old, new, lower case), so the
    /// selection follows them.
    renamed: Vec<(String, String)>,
    /// Items to select when the next listing arrives, after a paste, rename or delete (app/files.rs).
    pending_select: Option<files::PendingSelect>,
    /// Open the name box on an item: a new folder, or a rename to try again. The list does it.
    pub rename_request: Option<RenameRequest>,
    /// Waiting for the item Windows' "New ›" makes, to open its name box (app/files.rs).
    appear: Option<files::Appear>,
    /// How the list shows this tab now: details, list, icons or tree (the VIEW button, or the view
    /// rules for this folder).
    pub view_mode: ViewMode,
    /// The tab's own view, for folders the view rules say nothing about (the VIEW button picked in
    /// one of those, Settings' View). Remembered with the tab.
    pub own_view: ViewMode,
    /// The folders open in the tree view, whose items follow the folder's own in `folder.entries`
    /// (app/tree.rs).
    pub tree: Tree,
    /// How the list scrolls this tab, from its last frame: the size of one step (a row, a row of
    /// tiles, a column of the List view) and how many items it holds. A re-read keeps the item at
    /// the top in place with it (`merge_listing`). None until the list has drawn the tab.
    pub lines: Option<(f32, usize)>,
}

impl Tab {
    /// The custom sort order, when it decides this folder's order: it's on, this is a real folder
    /// (This PC's drives keep their own order), and some of its rules are for this folder. In a
    /// folder none are for (only "in …" rules, for other folders), the column sorts as usual.
    pub fn custom_here(&self) -> Option<&Arc<Recipe>> {
        self.custom.as_ref().filter(|r| !self.folder.path.is_empty() && r.applies(&self.folder.path))
    }

    fn new(id: u64, path: String) -> Tab {
        Tab {
            id,
            path,
            back: Vec::new(),
            forward: Vec::new(),
            folder: Folder::default(),
            view: Vec::new(),
            pos: Vec::new(),
            selected: Vec::new(),
            focus: None,
            anchor: None,
            sort: SortKey::Name,
            descending: false,
            filter: String::new(),
            show_hidden: false,
            show_system: false,
            folders_first: true,
            custom: None,
            views: None,
            search: None,
            opens_file: None,
            visible_total: 0,
            scroll: 0.0,
            reveal_focus: false,
            loading: None,
            load_ms: None,
            select_after_load: None,
            reloading: None,
            stale: false,
            renamed: Vec::new(),
            pending_select: None,
            rename_request: None,
            appear: None,
            view_mode: ViewMode::Details,
            own_view: ViewMode::Details,
            tree: Tree::default(),
            lines: None,
        }
    }

    /// The entry shown at row `row` of the list.
    pub fn entry_at(&self, row: usize) -> Option<&Entry> {
        self.view.get(row).and_then(|&i| self.folder.entries.get(i as usize))
    }

    /// The entry index shown at row `row`.
    pub fn index_at(&self, row: usize) -> Option<usize> {
        self.view.get(row).map(|&i| i as usize)
    }

    /// The row an entry is shown at (None if filtered out).
    pub fn row_of(&self, entry: usize) -> Option<usize> {
        self.pos.get(entry).copied().filter(|&p| p != u32::MAX).map(|p| p as usize)
    }

    /// The keyboard cursor's row.
    pub fn focus_row(&self) -> Option<usize> {
        self.focus.and_then(|e| self.row_of(e))
    }

    /// True while the list should say "Loading…" rather than show (and let you click) the folder
    /// you came from: another folder has been on its way for longer than LOADING_DELAY.
    pub fn waiting_long(&self) -> bool {
        self.waiting_long_in().is_some_and(|left| left.is_zero())
    }

    /// How long until `waiting_long` turns true (zero once it is); None when no other folder is
    /// on its way. The window sets a timer with this, so the change shows without polling.
    pub fn waiting_long_in(&self) -> Option<Duration> {
        let p = self.loading.as_ref().filter(|_| self.folder.path != self.path)?;
        Some(LOADING_DELAY.saturating_sub(p.started.elapsed()))
    }

    pub fn is_selected(&self, entry: usize) -> bool {
        self.selected.get(entry).copied().unwrap_or(false)
    }

    /// Whether the list shows search results (from the folder and its subfolders) rather than the
    /// folder itself.
    pub fn is_searching(&self) -> bool {
        self.search.is_some()
    }

    /// What tells an item apart from every other one in the list, in lower case (`Entry::key`): its
    /// name - or, where items can share a name, where it really is: a search result's full path
    /// (results come from many folders), a Recycle Bin item's place in the bin, a drive's root.
    /// Right-click menus find their items again by it (menu.rs, app/files.rs).
    pub fn item_key(&self, entry: usize) -> String {
        self.folder.entries.get(entry).map(Entry::key).unwrap_or_default()
    }

    /// Re-applies the hidden-files choice, filter and sort (keeps the selection).
    pub fn rebuild_view(&mut self) {
        if self.sort == SortKey::Folder && self.search.is_none() {
            // Sorted by the search results' Folder column (a tab copied or reopened from a search):
            // without results there's no such column, so by name.
            (self.sort, self.descending) = (SortKey::Name, false);
        }
        let filter = NameFilter::new(&self.filter);
        let entries = &self.folder.entries;
        self.view.clear();
        self.visible_total = 0;
        for (i, e) in entries.iter().enumerate() {
            if !e.is_visible(self.show_hidden, self.show_system) {
                continue;
            }
            self.visible_total += 1;
            if filter.matches(&e.lower) {
                self.view.push(i as u32);
            }
        }
        match self.custom_here().cloned() {
            Some(recipe) => recipe.sort(entries, &mut self.view, &self.folder.path, &sortrecipe::Clock::now()),
            None => folder::sort_view(entries, &mut self.view, self.sort, self.descending, self.folders_first),
        }
        // The tree view: each open folder's items right under it, sorted among themselves.
        if self.tree.is_open() {
            tree::nest(&self.folder, &self.tree, &mut self.view);
        }
        self.pos = vec![u32::MAX; entries.len()];
        for (row, &i) in self.view.iter().enumerate() {
            self.pos[i as usize] = row as u32;
        }
        // Hidden by the filter (or as a hidden file) means not selected (otherwise "3 selected" would
        // count things you can't see).
        for (i, sel) in self.selected.iter_mut().enumerate() {
            if self.pos[i] == u32::MAX {
                *sel = false;
            }
        }
    }

    // ----- selection (rows are positions in the list, entries are indexes into folder.entries)

    pub fn clear_selection(&mut self) {
        self.selected.iter_mut().for_each(|s| *s = false);
    }

    /// Select just this row and put the cursor there.
    pub fn select_only_row(&mut self, row: usize) {
        self.clear_selection();
        if let Some(e) = self.index_at(row) {
            self.selected[e] = true;
            self.focus = Some(e);
            self.anchor = Some(e);
        }
    }

    /// Ctrl-click: flip one row.
    pub fn toggle_row(&mut self, row: usize) {
        if let Some(e) = self.index_at(row) {
            self.selected[e] = !self.selected[e];
            self.focus = Some(e);
            self.anchor = Some(e);
        }
    }

    /// Shift-click / Shift+arrows: select from the anchor to `row` (adding to the selection if `add`).
    pub fn select_range_to(&mut self, row: usize, add: bool) {
        let anchor_row = self.anchor.and_then(|a| self.row_of(a)).unwrap_or(row);
        if !add {
            self.clear_selection();
        }
        let (lo, hi) = (anchor_row.min(row), anchor_row.max(row));
        for r in lo..=hi.min(self.view.len().saturating_sub(1)) {
            let e = self.view[r] as usize;
            self.selected[e] = true;
        }
        self.focus = self.index_at(row);
        if self.anchor.is_none() {
            self.anchor = self.focus;
        }
    }

    pub fn select_all(&mut self) {
        for &i in &self.view {
            self.selected[i as usize] = true;
        }
    }

    /// Indexes of the selected entries, in list order.
    pub fn selected_entries(&self) -> Vec<usize> {
        self.view.iter().map(|&i| i as usize).filter(|&i| self.selected[i]).collect()
    }

    /// (number selected, total bytes of the selected files).
    pub fn selection_summary(&self) -> (usize, u64) {
        let mut n = 0;
        let mut bytes = 0;
        for (i, &s) in self.selected.iter().enumerate() {
            if s {
                n += 1;
                if !self.folder.entries[i].is_container() {
                    bytes += self.folder.entries[i].size;
                }
            }
        }
        (n, bytes)
    }

    fn set_sort(&mut self, key: SortKey, descending: bool) {
        self.sort = key;
        self.descending = descending;
        self.rebuild_view();
        self.reveal_focus = true;
    }
}

/// A closed tab, kept for Ctrl+Shift+T.
struct ClosedTab {
    /// Where it was in the tab row.
    index: usize,
    path: String,
    back: Vec<String>,
    forward: Vec<String>,
    sort: SortKey,
    descending: bool,
    view: ViewMode,
}

/// How many closed tabs Ctrl+Shift+T can bring back.
const CLOSED_TABS_KEPT: usize = 10;

/// Results coming back from background threads.
enum Loaded {
    Folder { tab_id: u64, generation: u64, folder: Folder, finished: Instant },
    /// A quiet re-read of the folder a tab already shows (see `AppState::reload`).
    Reload { tab_id: u64, generation: u64, folder: Folder },
    /// The drive list; the number tells reads apart, so one that finishes late can't undo a newer one.
    Drives(u64, Vec<Drive>),
    /// A file job (copy, delete, rename...) finished; see app/files.rs.
    FileOp { after: files::AfterJob, outcome: Outcome },
    /// More results of a tab's subfolder search (search.rs).
    Search { tab_id: u64, generation: u64, batch: search::Batch },
    /// Windows said the Recycle Bin changed (app/bin.rs).
    BinChanged,
    /// A folder open in a tab's tree view was read (app/tree.rs).
    Branch { tab_id: u64, key: String, generation: u64, folder: Folder },
}

/// Everything the window shows. Lives on the UI thread.
pub struct AppState {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub dark: bool,
    pub theme: Theme,
    /// The choices from settings.json and the SETTINGS menu (Show hidden files, New tabs open,
    /// Ask before deleting...). Change them here: the window notices and saves them a moment later
    /// (see `snapshot` and window.rs). Its remembered state (tabs, window position) is only
    /// up to date in `snapshot`.
    pub settings: Settings,
    /// Recently closed tabs, the last one at the end.
    closed: Vec<ClosedTab>,
    /// All drives (for This PC, the sidebar and drive names in paths). Filled shortly after start.
    pub drives: Vec<Drive>,
    pub icons: Icons,
    /// Watches for the folders open in the active tab's tree view (app/tree.rs).
    branch_watch: Vec<tree::BranchWatch>,
    /// Process start to first frame on screen, in ms.
    pub startup_ms: Option<f64>,
    /// A short note for the status bar ("Path copied") and when it was said. It shows for
    /// NOTE_SECONDS, or until the next navigation.
    note: Option<(String, Instant)>,
    /// A question waiting for an answer before a file job runs (ask.rs shows it).
    pub question: Option<Question>,
    next_question: u64,
    /// The items Glint cut to the clipboard (drawn faded while they're there).
    cut: Option<files::CutMark>,
    /// The "Subfolders" switch next to the filter (Ctrl+Shift+F): the filter searches the
    /// subfolders too (search.rs). Not remembered.
    pub subfolders: bool,
    hwnd: Arc<AtomicIsize>,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    next_generation: u64,
    next_tab_id: u64,
    /// Watches the active tab's folder (watch.rs).
    watcher: Watcher,
    /// The window is hidden in the background (life.rs): nothing is watched until it's back.
    hidden: bool,
    /// Tabs whose subfolder search was stopped when the window went to the background (it could
    /// walk a whole drive while nobody looks); they search again when it comes back (app/life.rs).
    searches_stopped: Vec<u64>,
    /// Drive-list reads asked for, and the newest one shown (see `Loaded::Drives`).
    drives_asked: u64,
    drives_shown: u64,
    /// Windows telling about changes to the Recycle Bin while the active tab (this id) shows it
    /// (None inside when that couldn't be started). See app/bin.rs.
    bin_watch: Option<(u64, Option<recycle::Watch>)>,
    /// Every skin there is: the built-in ones and the person's own (skin.rs, app/skins.rs). The one
    /// in use is `settings.skin`, drawn with `theme`.
    pub skins: skin::Library,
    /// Goes up whenever `theme` changes, so the windows can follow (their text boxes' colours,
    /// Windows' menus, the font).
    pub theme_generation: u64,
}

impl AppState {
    /// Creates the state with these tabs (settings::initial_tabs) and starts reading the active
    /// one straight away (before the window even exists, so the first frame usually already has the
    /// files). The other tabs are read the first time they are shown, so ten restored tabs cost no
    /// more at start than one - and a tab on a sleeping network drive doesn't hold anything up.
    pub fn new(mut loaded: Settings, tabs: &[SavedTab], active: usize, dark: bool) -> AppState {
        let (tx, rx) = channel();
        loaded.tabs.clear(); // the live tabs are the truth from now on (see `snapshot`)
        // The skin, in its light or dark version as asked (one of the person's own is read now).
        let mut skins = skin::Library::new();
        skins.read_now(&loaded.skin);
        loaded.skin = skins.twin(&loaded.skin, dark);
        let mut theme = skins.theme(&loaded.skin, dark);
        theme.logo_style = loaded.logo;
        let hwnd = Arc::new(AtomicIsize::new(0));
        let mut app = AppState {
            tabs: Vec::new(),
            active: 0,
            dark: theme.dark,
            theme,
            skins,
            theme_generation: 1,
            settings: loaded,
            closed: Vec::new(),
            drives: Vec::new(),
            icons: Icons::new(),
            branch_watch: Vec::new(),
            startup_ms: None,
            note: None,
            watcher: Watcher::new(hwnd.clone()),
            hidden: false,
            searches_stopped: Vec::new(),
            question: None,
            next_question: 0,
            cut: None,
            subfolders: false,
            hwnd,
            tx,
            rx,
            next_generation: 1,
            next_tab_id: 1,
            drives_asked: 0,
            drives_shown: 0,
            bin_watch: None,
        };
        app.refresh_drives();
        for saved in tabs {
            let mut tab = app.make_tab(saved.path.clone());
            tab.sort = settings::sort_key(saved.sort_column);
            tab.descending = saved.sort_descending;
            if let Some(v) = saved.view {
                tab.set_own_view(v);
            }
            app.tabs.push(tab);
        }
        if app.tabs.is_empty() {
            let tab = app.make_tab(shell::home());
            app.tabs.push(tab);
        }
        app.active = active.min(app.tabs.len() - 1);
        app.start_load(app.active);
        app
    }

    /// For tests: the state with one tab at C:\ and these settings, reading nothing in the background.
    #[cfg(test)]
    pub(crate) fn for_tests(settings: Settings) -> AppState {
        let (tx, rx) = channel();
        let hwnd = Arc::new(AtomicIsize::new(0));
        AppState {
            tabs: vec![Tab::new(1, "C:\\".into())],
            active: 0,
            dark: true,
            theme: Theme::dark(),
            skins: skin::Library::new(),
            theme_generation: 1,
            settings,
            closed: Vec::new(),
            drives: Vec::new(),
            icons: Icons::new(),
            branch_watch: Vec::new(),
            startup_ms: None,
            note: None,
            watcher: Watcher::new(hwnd.clone()),
            question: None,
            next_question: 0,
            cut: None,
            hwnd,
            tx,
            rx,
            next_generation: 1,
            next_tab_id: 2,
            drives_asked: 0,
            drives_shown: 0,
            bin_watch: None,
            subfolders: false,
            hidden: false,
            searches_stopped: Vec::new(),
        }
    }

    /// A new tab at `path` (not read yet), with the hidden-files choice.
    fn make_tab(&mut self, path: String) -> Tab {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let mut tab = Tab::new(id, path);
        tab.show_hidden = self.settings.show_hidden;
        tab.show_system = self.settings.show_system;
        tab.folders_first = self.settings.folders_first;
        tab.custom = self.custom_order();
        tab.views = Some(self.view_rules());
        tab.set_own_view(self.settings.view);
        tab
    }

    /// Reads a tab's folder if it never was (a tab restored at start, shown for the first time).
    fn ensure_loaded(&mut self, i: usize) {
        if let Some(t) = self.tabs.get(i) {
            if t.loading.is_none() && t.load_ms.is_none() && t.folder.entries.is_empty() && t.folder.error.is_none() {
                self.start_load(i);
            }
        }
    }

    /// Everything to save: the settings, plus the tabs as they are now (the window adds its
    /// position and the column widths).
    pub fn snapshot(&self) -> Settings {
        let mut s = self.settings.clone();
        s.tabs = self
            .tabs
            .iter()
            .map(|t| SavedTab { path: t.path.clone(), sort_column: settings::sort_number(t.sort), sort_descending: t.descending, view: Some(t.own_view) })
            .collect();
        s.active_tab = self.active;
        s.sort_column = settings::sort_number(self.tab().sort);
        s.sort_descending = self.tab().descending;
        s
    }

    /// Turns hidden files on or off, in every tab.
    pub fn set_show_hidden(&mut self, on: bool) {
        self.settings.show_hidden = on;
        for t in &mut self.tabs {
            t.show_hidden = on;
            t.rebuild_view();
        }
        // A search looks again, with or without what's in hidden folders.
        for i in 0..self.tabs.len() {
            if self.tabs[i].is_searching() {
                self.start_search(i, false);
            }
        }
        self.tab_mut().reveal_focus = true;
        self.say(if on { "Showing hidden files" } else { "Hiding hidden files" });
    }

    /// Shows protected operating-system files too, while hidden files are shown ("ShowSystem";
    /// Explorer's "Hide protected operating system files", the other way round).
    pub fn set_show_system(&mut self, on: bool) {
        self.settings.show_system = on;
        for t in &mut self.tabs {
            t.show_system = on;
            t.rebuild_view();
        }
        for i in 0..self.tabs.len() {
            if self.tabs[i].is_searching() {
                self.start_search(i, false);
            }
        }
        self.say(match (on, self.settings.show_hidden) {
            (true, true) => "Showing protected system files",
            (true, false) => "Protected system files show when hidden files do (Ctrl+H)",
            (false, _) => "Hiding protected system files",
        });
    }

    /// The file list's row height for the "Size" setting, in DIPs.
    pub fn row_height(&self) -> f32 {
        row_height(self.settings.size, self.theme.row_h)
    }

    /// Changes the row and text size ("Size"). Every tab keeps the same rows on screen.
    pub fn set_list_size(&mut self, s: ListSize) {
        let (old, new) = (self.row_height(), row_height(s, self.theme.row_h));
        self.settings.size = s;
        // (Rows only: the icon views' tiles don't change with the row size.)
        for t in self.tabs.iter_mut().filter(|t| matches!(t.view_mode, ViewMode::Details | ViewMode::Tree)) {
            t.scroll = t.scroll / old * new;
        }
        self.tab_mut().reveal_focus = true;
    }

    /// Folders above files, or sorted together with them ("FoldersFirst").
    pub fn set_folders_first(&mut self, on: bool) {
        self.settings.folders_first = on;
        for t in &mut self.tabs {
            t.folders_first = on;
            t.rebuild_view();
        }
        self.tab_mut().reveal_focus = true;
    }

    /// The custom sort order to sort with, while it's on ("CustomSort"; "SortRecipe", or the
    /// starter rules when there are none yet).
    pub fn custom_order(&self) -> Option<Arc<Recipe>> {
        self.settings.custom_sort.then(|| sortrecipe::cached(self.settings.sort_recipe.as_deref().unwrap_or(sortrecipe::STARTER)))
    }

    /// The rules as read for their views (-large_icons…): the custom sort order's text, used
    /// whether sorting by it is on or off.
    pub fn view_rules(&self) -> Arc<Recipe> {
        sortrecipe::cached(self.settings.sort_recipe.as_deref().unwrap_or(sortrecipe::STARTER))
    }

    /// Turns the custom sort order on or off, with new rules when `recipe` is given (the sort
    /// editor, the SORT menu, Settings), and sorts every tab again at once. New rules also show
    /// each tab's folder in the view they give it (or the tab's own, when they no longer give one).
    pub fn set_custom_sort(&mut self, on: bool, recipe: Option<String>) {
        if let Some(text) = recipe {
            self.settings.sort_recipe = Some(text);
        }
        self.settings.custom_sort = on;
        let custom = self.custom_order();
        let views = self.view_rules();
        let mut views_changed = false;
        for t in &mut self.tabs {
            t.custom = custom.clone();
            // (Only when the rules changed: turning sorting on or off keeps a view picked by hand.)
            if !t.views.as_ref().is_some_and(|v| Arc::ptr_eq(v, &views)) {
                t.views = Some(views.clone());
                views_changed |= tree::follow_view_rules(t);
            }
            t.rebuild_view();
        }
        if views_changed {
            self.sync_branch_watch();
        }
        self.tab_mut().reveal_focus = true;
    }

    /// A column picked while the custom sort order decides this folder: that means "sort by this",
    /// so the custom order goes off, and the status bar says how to get it back. (In a folder it
    /// has no rules for, the column just sorts that folder, and the custom order stays on.)
    fn custom_sort_off_for_column(&mut self) {
        if self.settings.custom_sort && self.tab().custom_here().is_some() {
            self.set_custom_sort(false, None);
            self.say("Custom sort is off. SORT \u{203a} Custom order turns it on again");
        }
    }

    /// Shows or hides one of the columns next to Name ("modified", "created", "type", "size").
    /// Tabs sorted by a column that goes away are sorted by name again.
    pub fn set_column(&mut self, name: &str, on: bool) {
        self.settings.set_column(name, on);
        let hidden = |key: SortKey| match key {
            SortKey::Date => !self.settings.shows_column("modified"),
            SortKey::Created => !self.settings.shows_column("created"),
            SortKey::Type => !self.settings.shows_column("type"),
            SortKey::Size => !self.settings.shows_column("size"),
            SortKey::Name | SortKey::Folder => false,
        };
        let resort: Vec<usize> = (0..self.tabs.len()).filter(|&i| hidden(self.tabs[i].sort)).collect();
        for i in resort {
            self.tabs[i].set_sort(SortKey::Name, false);
        }
    }

    /// Turns the error log on or off (SETTINGS > Error log).
    pub fn set_error_log(&mut self, on: bool) {
        self.settings.error_log = on;
        log::set_error_log(on);
    }

    /// Whether Ctrl+Shift+T has a tab to bring back.
    pub fn can_reopen_tab(&self) -> bool {
        !self.closed.is_empty()
    }

    /// Tells background threads which window to wake. Picks up anything that finished already.
    pub fn attach_window(&mut self, hwnd: HWND) -> bool {
        self.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        self.icons.set_window(hwnd);
        // The watch started before the window existed: now it can ask to hear about drive ejects.
        self.watcher.window_ready();
        // The skins folder: watched from now on, and listed in the background (app/skins.rs).
        self.skins.window_ready(hwnd);
        self.receive()
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// Shows a short note in the status bar for a few seconds.
    pub fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
    }

    /// The status-bar note, while it is still fresh.
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().filter(|(_, at)| at.elapsed().as_secs_f64() < NOTE_SECONDS).map(|(text, _)| text.as_str())
    }

    /// How long the current note still shows (the window sets a timer to take it away).
    pub fn note_time_left(&self) -> Option<std::time::Duration> {
        let (_, at) = self.note.as_ref()?;
        std::time::Duration::from_secs_f64(NOTE_SECONDS).checked_sub(at.elapsed()).filter(|d| !d.is_zero())
    }

    /// Runs a folder/tab/item command. Returns false for commands that belong to the window
    /// (minimise, theme, path editing...), which window.rs handles itself.
    pub fn run(&mut self, cmd: Command) -> bool {
        // The Recycle Bin's items aren't ordinary files: while it's on screen (or on its way, or
        // just left), file commands do the bin's own thing (app/bin.rs).
        if self.bin_command(&cmd) {
            self.sync_watch();
            return true;
        }
        match cmd {
            Command::Navigate(p) => self.navigate(&p),
            Command::Back => self.back(),
            Command::Forward => self.forward(),
            Command::Up => self.up(),
            Command::Refresh => self.refresh(),
            Command::NewTab => {
                let p = match self.settings.new_tab {
                    NewTab::Same => self.tab().path.clone(),
                    NewTab::Home => shell::home(),
                    NewTab::ThisPc => String::new(),
                };
                self.open_tab(p);
            }
            Command::NewTabAt(p) => self.open_tab(folder::normalize(&p)),
            Command::CloseTab(i) => self.close_tab(i),
            Command::DuplicateTab(i) => self.duplicate_tab(i),
            Command::CloseOtherTabs(i) => {
                if i < self.tabs.len() {
                    // From the right, so each closed tab remembers the place it can go back to.
                    for j in (0..self.tabs.len()).rev().filter(|&j| j != i) {
                        self.close_tab(j);
                    }
                }
            }
            Command::ReopenClosedTab => self.reopen_closed_tab(),
            Command::ToggleHidden => self.set_show_hidden(!self.settings.show_hidden),
            Command::SwitchTab(i) => {
                if i < self.tabs.len() {
                    self.active = i;
                }
            }
            Command::NextTab => self.active = (self.active + 1) % self.tabs.len(),
            Command::PrevTab => self.active = (self.active + self.tabs.len() - 1) % self.tabs.len(),
            Command::OpenSelected => self.open_selected(),
            Command::OpenEntry(i) => self.open_entry(i),
            Command::CopyPath => self.copy_paths(),
            Command::ShowInExplorer => self.show_in_explorer(),
            Command::OpenInNewTabs => self.open_in_new_tabs(),
            Command::NewTabBehind(p) => self.open_tab_behind(folder::normalize(&p)),
            Command::OpenWith => self.open_with(),
            Command::Properties => self.properties(),
            Command::OpenTerminal(dir) => self.open_terminal(dir),
            Command::SortBy(key) => {
                // While the custom sort order decides, no column is the sorted one (its title has no
                // arrow), so a click on one is a first click, not a second that turns it around.
                let custom = self.tab().custom_here().is_some();
                self.custom_sort_off_for_column();
                let t = self.tab_mut();
                // Newest and biggest first feel natural.
                let desc = if t.sort == key && !custom { !t.descending } else { matches!(key, SortKey::Date | SortKey::Created | SortKey::Size) };
                t.set_sort(key, desc);
            }
            Command::SetSort(key, desc) => {
                self.custom_sort_off_for_column();
                self.tab_mut().set_sort(key, desc);
            }
            Command::SetCustomSort(on) => self.set_custom_sort(on, None),
            Command::CopyFiles => self.copy_files(false),
            Command::CutFiles => self.copy_files(true),
            Command::Paste => self.paste(None),
            Command::PasteInto(folder) => self.paste(Some(folder)),
            Command::Delete { permanent } => self.delete(permanent),
            Command::NewFolder => self.new_item(true),
            Command::NewTextFile => self.new_item(false),
            Command::Rename { dir, from, to } => self.rename(dir, from, to),
            Command::SetFilter(text) => self.set_filter(text),
            Command::ToggleSubfolders => self.toggle_subfolders(),
            Command::OpenFileLocation => self.open_file_location(),
            Command::TogglePin(path) => self.toggle_pin(&path),
            Command::SetView(mode) => self.set_view(mode),
            _ => return false,
        }
        // A tab restored at start is read the first time it's shown (this also starts its watch).
        self.ensure_loaded(self.active);
        // The watch follows the active tab (another tab, another folder, a tab closed).
        self.sync_watch();
        true
    }

    // ------------------------------------------------------------ navigation

    /// Go to `input` (anything a person might type) in the current tab. A name or relative path
    /// ("src", "..\Music") is taken from the folder the tab is in, like Explorer's address bar;
    /// "~", "%USERPROFILE%" and "This PC" work too (folder.rs `normalize`), and so do Windows'
    /// "shell:" places ("shell:startup", "shell:Downloads\Setup"). A shell: place that isn't a
    /// folder on disk (Control Panel...) opens in File Explorer. A file opens in
    /// its app, and the tab stays where it is.
    pub fn navigate(&mut self, input: &str) {
        let typed = input.trim().trim_matches('"').trim();
        if typed.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("shell:")) {
            match shell::shell_folder(typed) {
                Some(path) => self.go(&path),
                None => {
                    shell::show_in_explorer(typed, false);
                    self.say("Glint can't show that place, so it opened in File Explorer");
                }
            }
            return;
        }
        self.go(input);
    }

    fn go(&mut self, input: &str) {
        let path = folder::resolve(input, &self.tab().path);
        let t = self.tab_mut();
        if path == t.path {
            self.refresh();
            return;
        }
        let old = std::mem::replace(&mut t.path, path);
        t.back.push(old);
        let forward = std::mem::take(&mut t.forward);
        t.select_after_load = None;
        self.start_load(self.active);
        self.tab_mut().opens_file = Some(forward);
    }

    pub fn back(&mut self) {
        let t = self.tab_mut();
        let Some(prev) = t.back.pop() else { return };
        let old = std::mem::replace(&mut t.path, prev);
        t.select_after_load = child_name(&t.path, &old);
        t.forward.push(old);
        self.start_load(self.active);
    }

    pub fn forward(&mut self) {
        let t = self.tab_mut();
        let Some(next) = t.forward.pop() else { return };
        let old = std::mem::replace(&mut t.path, next);
        t.select_after_load = child_name(&t.path, &old);
        t.back.push(old);
        self.start_load(self.active);
    }

    pub fn up(&mut self) {
        let t = self.tab_mut();
        let Some(parent) = folder::parent(&t.path) else { return };
        let old = std::mem::replace(&mut t.path, parent);
        t.select_after_load = child_name(&t.path, &old);
        t.back.push(old);
        t.forward.clear();
        self.start_load(self.active);
    }

    /// F5: the folder is read again - or, while search results are shown, the search runs again.
    pub fn refresh(&mut self) {
        self.start_load(self.active);
        self.refresh_drives();
    }

    fn open_tab(&mut self, path: String) {
        let at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        let mut tab = self.make_tab(path);
        if let Some(cur) = self.tabs.get(self.active) {
            (tab.sort, tab.descending) = (cur.sort, cur.descending);
            tab.set_own_view(cur.own_view);
        }
        self.tabs.insert(at, tab);
        self.active = at;
        self.start_load(at);
    }

    /// A copy of tab `i` (folder, history, sort) right after it, made active.
    fn duplicate_tab(&mut self, i: usize) {
        let Some(src) = self.tabs.get(i) else { return };
        let (path, back, forward, sort, descending, view) = (src.path.clone(), src.back.clone(), src.forward.clone(), src.sort, src.descending, src.own_view);
        let mut tab = self.make_tab(path);
        (tab.back, tab.forward, tab.sort, tab.descending) = (back, forward, sort, descending);
        tab.set_own_view(view);
        self.tabs.insert(i + 1, tab);
        self.active = i + 1;
        self.start_load(i + 1);
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        self.closed.push(ClosedTab { index: i, path: t.path, back: t.back, forward: t.forward, sort: t.sort, descending: t.descending, view: t.own_view });
        if self.closed.len() > CLOSED_TABS_KEPT {
            self.closed.remove(0);
        }
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Ctrl+Shift+T: the tab closed last comes back where it was, with its history.
    fn reopen_closed_tab(&mut self) {
        let Some(c) = self.closed.pop() else {
            self.say("No closed tabs to bring back");
            return;
        };
        let at = c.index.min(self.tabs.len());
        let mut tab = self.make_tab(c.path);
        (tab.back, tab.forward, tab.sort, tab.descending) = (c.back, c.forward, c.sort, c.descending);
        tab.set_own_view(c.view);
        self.tabs.insert(at, tab);
        self.active = at;
        self.start_load(at);
    }

    /// Starts reading the tab's folder on a background thread. While the tab shows search results
    /// of that folder (F5, a file job changed something in it), the search runs again instead;
    /// going to another folder ends the search.
    fn start_load(&mut self, tab_index: usize) {
        self.tabs[tab_index].opens_file = None;
        if let Some(root) = self.tabs[tab_index].search.as_ref().map(|s| s.root.clone()) {
            if self.tabs[tab_index].path == root {
                self.note = None;
                self.start_search(tab_index, false);
                return;
            }
            self.end_search(tab_index, false);
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.note = None;
        // Watch the active tab's folder from before it is read, so nothing that changes in between
        // is missed (watch.rs; the read waits a moment for the watch to be under way).
        let armed = self.watch_tab(tab_index);
        let t = &mut self.tabs[tab_index];
        if t.path != t.folder.path {
            t.filter.clear();
            t.renamed.clear();
        }
        t.stale = false; // this read brings everything (a change seen while it runs sets it again)
        // A quiet re-read still under way began before this read: if it came in after it, it would
        // put an older listing back (a file deleted meanwhile showing again). It's dropped.
        t.reloading = None;
        t.loading = Some(Pending { generation, started: Instant::now() });
        let (tab_id, path, tx, hwnd) = (t.id, t.path.clone(), self.tx.clone(), self.hwnd.clone());
        let started = shell::background("read folder", move || {
            let _com = ComGuard::init();
            armed.wait();
            let folder = folder::read(&path);
            // Send first, then wake the window (see `attach_window` for why this order matters).
            if tx.send(Loaded::Folder { tab_id, generation, folder, finished: Instant::now() }).is_ok() {
                wake(&hwnd);
            }
        });
        if !started {
            // No thread to be had (the system is out of resources): say so instead of loading forever.
            let t = &mut self.tabs[tab_index];
            t.loading = None;
            let error = Some("Windows couldn't start reading this folder. Try again with F5.".to_string());
            apply_listing(t, Folder { path: t.path.clone(), error, ..Default::default() });
            return;
        }
        // The folders open in the tree view are read again with it (F5, a file job changed them).
        self.read_branches_again(tab_index);
    }

    /// Reads the drive list again in the background (at start, on refresh, when a drive appears).
    pub fn refresh_drives(&mut self) {
        self.drives_asked += 1;
        let (tx, hwnd, asked) = (self.tx.clone(), self.hwnd.clone(), self.drives_asked);
        // If no thread can be had, the drive list just stays as it is until the next refresh.
        shell::background("read drives", move || {
            if tx.send(Loaded::Drives(asked, folder::read_drives())).is_ok() {
                wake(&hwnd);
            }
        });
    }

    /// Takes in finished background work. Returns true if anything visible changed.
    pub fn receive(&mut self) -> bool {
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Loaded::Drives(asked, d) => {
                    if asked < self.drives_shown {
                        continue; // an older read that took longer than a newer one
                    }
                    self.drives_shown = asked;
                    self.drives = d;
                    // This PC, wherever it's open, shows the new list too (a USB stick came or went).
                    for t in self.tabs.iter_mut().filter(|t| t.path.is_empty() && t.folder.path.is_empty() && t.load_ms.is_some()) {
                        if t.reloading.is_some() {
                            t.stale = true;
                        } else if t.loading.is_none() {
                            merge_listing(t, folder::drives_folder(self.drives.clone()), &[], row_height(self.settings.size, self.theme.row_h));
                        }
                    }
                    changed = true;
                }
                Loaded::Reload { tab_id, generation, folder } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab_id) else { continue };
                    if t.reloading != Some(generation) {
                        continue;
                    }
                    t.reloading = None;
                    // Gone elsewhere meanwhile, or a read of it (F5, say) is on its way anyway. While
                    // search results are shown, the folder is read again when the search ends.
                    if t.loading.is_some() || folder.path != t.path || t.is_searching() {
                        continue;
                    }
                    let merging = Instant::now();
                    let read_ms = folder.read_ms;
                    let renamed = std::mem::take(&mut t.renamed);
                    let different = merge_listing(t, folder, &renamed, row_height(self.settings.size, self.theme.row_h));
                    changed |= different;
                    if different {
                        files::rename_appeared(t); // made by Windows' "New ›"
                    }
                    if watch::verbose() {
                        log::line(&format!(
                            "watch: read \"{}\" again: {} items, read {read_ms:.1} ms, merged {:.1} ms, {}",
                            t.path,
                            t.folder.entries.len(),
                            merging.elapsed().as_secs_f64() * 1000.0,
                            if different { "changed" } else { "no change" }
                        ));
                    }
                    if t.folder.error.is_some() {
                        self.watcher.let_go(t.id); // it may be being deleted (see watch.rs)
                    }
                    if t.stale {
                        // Changed again while being read (read again below): keep the renames for
                        // the next merge.
                        t.renamed = renamed;
                    }
                    // A folder that takes long to read isn't read again sooner than four times that
                    // (a folder of 100,000 files that keeps changing would otherwise be read nonstop).
                    let cost = Duration::try_from_secs_f64(read_ms / 1000.0).unwrap_or_default() + merging.elapsed();
                    self.watcher.set_gap(cost.saturating_mul(4));
                }
                Loaded::FileOp { after, outcome } => {
                    self.after_file_op(after, outcome);
                    changed = true;
                }
                Loaded::Search { tab_id, generation, batch } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab_id) else { continue };
                    if t.search.as_ref().is_some_and(|s| s.generation == generation) {
                        let done = batch.done;
                        take_results(t, batch);
                        changed = true;
                        if let Some(s) = t.search.as_ref().filter(|_| done) {
                            log::line(&format!(
                                "search for \u{201c}{}\u{201d} in \"{}\": {} result(s){}, {:.0} ms after asking",
                                s.query,
                                s.root,
                                t.folder.entries.len(),
                                if s.capped { " (the most it shows)" } else { "" },
                                s.started.elapsed().as_secs_f64() * 1000.0
                            ));
                        }
                    }
                }
                Loaded::BinChanged => {
                    self.bin_changed(); // read again below
                    changed = true;
                }
                Loaded::Branch { tab_id, key, generation, folder } => {
                    changed |= self.branch_read(tab_id, &key, generation, folder);
                }
                Loaded::Folder { tab_id, generation, folder, finished } => {
                    let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab_id) else { continue };
                    let Some(p) = t.loading.take_if(|p| p.generation == generation) else { continue };
                    let opens_file = t.opens_file.take();
                    if let Some(forward) = opens_file.filter(|_| folder.error.as_deref() == Some(folder::NOT_A_FOLDER) && folder.path == t.path) {
                        // A file's path was typed (or pasted) into the path bar: it opens in its app,
                        // and the tab goes back to the folder it showed, which is still on screen
                        // (with Forward as it was).
                        shell::open(&folder.path);
                        if let Some(prev) = t.back.pop() {
                            t.path = prev;
                        }
                        t.forward = forward;
                        let name = folder::file_name(&folder.path).to_string();
                        self.say(format!("Opened \u{201c}{name}\u{201d}"));
                        changed = true;
                        continue;
                    }
                    if let Some(s) = t.search.as_mut().filter(|s| s.root == folder.path) {
                        // The folder came in after its search began: it waits with the search (the
                        // status bar shows its drive's free space meanwhile).
                        t.folder.space = folder.space.clone();
                        s.listing = folder;
                        t.load_ms = Some(finished.saturating_duration_since(p.started).as_secs_f64() * 1000.0);
                        continue;
                    }
                    let applying = Instant::now();
                    apply_listing(t, folder);
                    // "Opened in": reading on the background thread + sorting here. Time the result
                    // spent waiting for the UI thread (only at start-up, while the window is being
                    // made) is not the folder's fault, so it's left out.
                    let ms = (finished.saturating_duration_since(p.started) + applying.elapsed()).as_secs_f64() * 1000.0;
                    t.load_ms = Some(ms);
                    log::line(&format!(
                        "opened \"{}\": {} items, read {:.1} ms, ready {:.1} ms after asking",
                        if t.path.is_empty() { "This PC" } else { &t.path },
                        t.folder.entries.len(),
                        t.folder.read_ms,
                        ms
                    ));
                    if t.folder.error.is_some() {
                        self.watcher.let_go(t.id); // it may be being deleted (see watch.rs)
                    }
                    changed = true;
                }
            }
        }
        // News from the folder watch.
        for (tab_id, event) in self.watcher.take() {
            changed |= self.on_watch(tab_id, event);
        }
        // ...and from the watches of the folders open in the tree view.
        self.branch_news();
        // Read again what changed, or changed while it was being read - once per tab, and only
        // when no read of it is under way.
        for i in 0..self.tabs.len() {
            let t = &self.tabs[i];
            if t.stale && t.loading.is_none() && t.reloading.is_none() {
                self.reload(i, Armed::none());
            }
        }
        self.sync_watch();
        changed
    }

    // ------------------------------------------------------------ watching (see watch.rs)

    /// Keeps the folder watch on the active tab's folder. Cheap; called after every command and
    /// whenever background work comes in.
    fn sync_watch(&mut self) {
        let i = self.active;
        if i >= self.tabs.len() {
            return;
        }
        // The Recycle Bin is watched its own way (app/bin.rs), and so are folders open in the tree
        // view (app/tree.rs).
        self.sync_bin_watch();
        self.sync_branch_watch();
        let t = &self.tabs[i];
        if self.hidden || !watch::can_watch(&t.path) {
            self.watcher.stop(); // This PC
            return;
        }
        if self.watcher.is_on(t.id, &t.path) {
            return;
        }
        let armed = self.watcher.watch(t.id, &t.path);
        // Not watched until now (it was in the background): anything may have changed since it
        // was read. Read it again quietly - after the watch is under way, so nothing slips through.
        let t = &mut self.tabs[i];
        if t.loading.is_some() {
            t.stale = true;
        } else {
            self.reload(i, armed);
        }
    }

    /// For a read of tab `i` that is about to start: watches its folder if it's the active tab
    /// (unless it's watched already) and returns the signal the read waits for.
    fn watch_tab(&mut self, i: usize) -> Armed {
        if i != self.active {
            return Armed::none();
        }
        let t = &self.tabs[i];
        if self.hidden || !watch::can_watch(&t.path) {
            self.watcher.stop();
            return Armed::none();
        }
        if self.watcher.is_working(t.id, &t.path) {
            return self.watcher.armed();
        }
        // Not watched, or the watch stopped working (F5 tries again).
        self.watcher.watch(t.id, &t.path)
    }

    /// Reads tab `i`'s folder again in the background and merges it in (`merge_listing`): the
    /// watch saw a change, or the tab wasn't watched while in the background. Nothing on screen
    /// changes until it's there, and then only what changed. If a read of it is under way already,
    /// this one follows when that's done. `armed`: wait for a watch that is just starting.
    fn reload(&mut self, i: usize, armed: Armed) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        if t.is_searching() {
            // Search results are on screen: the folder is read again when the search ends.
            t.stale = false;
            return;
        }
        if t.loading.is_some() || t.reloading.is_some() {
            t.stale = true;
            return;
        }
        if t.folder.path != t.path {
            t.stale = false;
            return; // nothing of this folder on screen yet: its first read brings everything
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        let t = &mut self.tabs[i];
        t.stale = false;
        t.reloading = Some(generation);
        let (tab_id, path, tx, hwnd) = (t.id, t.path.clone(), self.tx.clone(), self.hwnd.clone());
        let started = shell::background("read folder again", move || {
            let _com = ComGuard::init();
            armed.wait();
            let folder = folder::read(&path);
            if tx.send(Loaded::Reload { tab_id, generation, folder }).is_ok() {
                wake(&hwnd);
            }
        });
        if !started {
            // No thread to be had: the list stays as it is until the next change or F5.
            self.tabs[i].reloading = None;
        }
    }

    /// What the folder watch saw. Returns true if something on screen changed.
    fn on_watch(&mut self, tab_id: u64, event: watch::Event) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) else { return false };
        match event {
            watch::Event::Changed { renamed } => {
                // Read again at the end of `receive` (or once the read under way is done).
                let t = &mut self.tabs[i];
                if t.renamed.len() < 4096 {
                    t.renamed.extend(renamed);
                }
                t.stale = true;
                false
            }
            watch::Event::Moved(to) => {
                let from = self.tabs[i].path.clone();
                self.folder_moved(&from, &to);
                if self.tabs[i].loading.is_some() || self.tabs[i].is_searching() {
                    // The read under way is of the old name; search results have their old paths.
                    self.start_load(i);
                }
                if i == self.active {
                    self.say(format!("This folder was renamed to \u{201c}{}\u{201d}", folder::file_name(&to)));
                }
                // sync_watch (end of `receive`) now watches it under its new name and reads it again.
                true
            }
        }
    }

    /// A folder was renamed (or moved) from `from` to `to`: every tab in it or below it, and their
    /// Back and Forward lists, follow it - as in Explorer. (Glint's own renames can call this too.)
    pub fn folder_moved(&mut self, from: &str, to: &str) {
        for t in &mut self.tabs {
            let shown = t.folder.path.clone();
            let searched = t.search.as_mut().map(|s| [&mut s.root, &mut s.listing.path]).into_iter().flatten();
            for p in [&mut t.path, &mut t.folder.path].into_iter().chain(searched).chain(t.back.iter_mut()).chain(t.forward.iter_mut()) {
                if let Some(moved) = moved_path(p, from, to) {
                    *p = moved;
                }
            }
            // The custom sort order can have rules for one place and not the other: sorted again
            // now (the reading that follows finds the same items, and leaves the order alone).
            if t.folder.path != shown {
                t.rebuild_view();
            }
        }
    }

    /// WM_DEVICECHANGE (window.rs): a drive came or went, or Windows asks Glint to let go of a
    /// drive that is about to be ejected (watch.rs).
    pub fn device_change(&mut self, event: u32, data: isize) {
        if watch::verbose() {
            log::line(&format!("device change {event:#06x}"));
        }
        self.watcher.device_event(event, data);
        // Folders open in the tree view let go of an ejected drive too.
        for w in &self.branch_watch {
            w.device_event(event, data);
        }
        if matches!(event, DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE) {
            self.refresh_drives(); // This PC and the sidebar update when the list arrives
        }
        if event == DBT_DEVICEARRIVAL {
            // The drive the tab was on may be back (a USB stick plugged in again): read it again,
            // which also watches it again.
            let t = self.tab();
            if t.folder.error.is_some() && t.loading.is_none() && t.reloading.is_none() && t.folder.path == t.path && !t.path.is_empty() {
                self.start_load(self.active);
            }
        }
    }

    // ------------------------------------------------------------ items

    pub fn open_entry(&mut self, index: usize) {
        let t = self.tab();
        let Some(e) = t.folder.entries.get(index) else { return };
        let path = e.path_in(&t.folder.path);
        if e.is_container() {
            self.navigate(&path);
        } else {
            shell::open(&path);
        }
    }

    fn open_selected(&mut self) {
        let t = self.tab();
        let mut chosen = t.selected_entries();
        if chosen.is_empty() {
            chosen.extend(t.focus);
        }
        // A folder among them: go into the first one (like Explorer in the same window).
        if let Some(&dir) = chosen.iter().find(|&&i| t.folder.entries[i].is_container()) {
            self.open_entry(dir);
            return;
        }
        for &i in chosen.iter().take(16) {
            self.open_entry(i);
        }
    }

    fn selected_paths(&self) -> Vec<String> {
        let t = self.tab();
        let chosen = t.selected_entries();
        if chosen.is_empty() {
            return vec![t.path.clone()];
        }
        chosen.iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect()
    }

    fn copy_paths(&mut self) {
        // (This PC has no path of its own: nothing is copied over what's on the clipboard.)
        let paths: Vec<String> = self.selected_paths().into_iter().filter(|p| !p.is_empty()).collect();
        if paths.is_empty() {
            self.say("This PC has no path to copy");
            return;
        }
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut _);
        let text = paths.iter().map(|p| if p.contains(' ') { format!("\"{p}\"") } else { p.clone() }).collect::<Vec<_>>().join("\r\n");
        let note = if shell::copy_text(hwnd, &text) {
            if paths.len() == 1 { "Path copied".to_string() } else { format!("{} paths copied", paths.len()) }
        } else {
            "Couldn't copy (the clipboard is busy)".to_string()
        };
        self.say(note);
    }

    fn show_in_explorer(&mut self) {
        let t = self.tab();
        let chosen = t.selected_entries();
        if chosen.is_empty() {
            shell::show_in_explorer(&t.path, false);
        } else {
            // All of them selected there, not just the first. (Search results from several
            // folders: those in the first one's folder, since Explorer shows one folder.)
            let paths = one_folder(chosen.iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect());
            shell::show_items_in_explorer(paths);
        }
    }

    // ------------------------------------------------------------ more ways to open (menu.rs)

    /// The window, for background jobs that open Windows' own windows or leave a note.
    fn owner(&self) -> shell::Owner {
        shell::Owner::new(HWND(self.hwnd.load(Ordering::SeqCst) as *mut _))
    }

    /// Ctrl+Enter, "Open in new tab": the selected folders open in new tabs - one is shown, several
    /// open behind the current tab in their order - and selected files open in their apps.
    fn open_in_new_tabs(&mut self) {
        let t = self.tab();
        let mut chosen = t.selected_entries();
        if chosen.is_empty() {
            chosen.extend(t.focus);
        }
        let (folders, files): (Vec<usize>, Vec<usize>) = chosen.into_iter().partition(|&i| t.folder.entries[i].is_container());
        let folder_paths: Vec<String> = folders.iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect();
        let file_paths: Vec<String> = files.iter().take(16).map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect();
        for p in &file_paths {
            shell::open(p);
        }
        match folder_paths.as_slice() {
            [] => {}
            [one] => self.open_tab(one.clone()),
            many => {
                // Each opens right after the current tab, so the last one goes first.
                for p in many.iter().take(32).rev() {
                    self.open_tab_behind(p.clone());
                }
                self.say(format!("Opened {} tabs", many.len().min(32)));
            }
        }
    }

    /// A new tab at `path` right after the current one, without switching to it (middle-click).
    /// It's read the first time it's shown (`ensure_loaded`), like the tabs restored at start, so
    /// the folder watch stays on the tab on screen.
    fn open_tab_behind(&mut self, path: String) {
        let mut tab = self.make_tab(path);
        if let Some(cur) = self.tabs.get(self.active) {
            (tab.sort, tab.descending) = (cur.sort, cur.descending);
            tab.set_own_view(cur.own_view);
        }
        self.tabs.insert((self.active + 1).min(self.tabs.len()), tab);
    }

    /// Windows' "Open with" window for the one selected file.
    fn open_with(&mut self) {
        let t = self.tab();
        let chosen = t.selected_entries();
        let Some(&i) = chosen.first().filter(|_| chosen.len() == 1) else { return };
        let e = &t.folder.entries[i];
        if !e.is_container() {
            shell::open_with(self.owner(), &e.path_in(&t.folder.path));
        }
    }

    /// Alt+Enter, "Properties": Windows' Properties window for the selection, or for the folder.
    fn properties(&mut self) {
        let t = self.tab();
        if t.path.is_empty() && t.selected_entries().is_empty() {
            return; // This PC itself has none worth showing here
        }
        let all = self.selected_paths();
        // Windows' Properties is for items of one folder (search results can come from several:
        // the others would be looked for in the first one's folder).
        let paths = one_folder(all.clone());
        if paths.len() < all.len() {
            self.say("Properties shows the items of one folder at a time");
        }
        if !shellmenu::properties(self.owner().hwnd(), paths) {
            self.say("Windows couldn't show the properties right now");
        }
    }

    /// Ctrl+`, "Open in terminal". On This PC (no folder), in the home folder.
    fn open_terminal(&mut self, dir: String) {
        let dir = if dir.is_empty() { shell::home() } else { dir };
        shell::open_terminal(self.owner(), &dir, self.settings.terminal);
    }

    // ------------------------------------------------------------ the filter and searching subfolders

    /// The filter box changed: the list shows only what matches, the first match selected (if
    /// nothing else is). With Subfolders on, the subfolders are searched too
    /// (after a short pause, so each key typed doesn't start a new search).
    pub fn set_filter(&mut self, text: String) {
        let i = self.active;
        let t = &mut self.tabs[i];
        if t.filter == text {
            return;
        }
        t.filter = text;
        t.rebuild_view();
        t.scroll = 0.0;
        if self.search_wanted(i) {
            self.start_search(i, true);
        } else if self.tabs[i].is_searching() {
            self.end_search(i, true);
        }
        let t = &mut self.tabs[i];
        if !t.filter.trim().is_empty() && t.selected_entries().is_empty() && !t.view.is_empty() && !t.waiting_long() {
            t.select_only_row(0);
            t.reveal_focus = true;
        }
    }

    /// Whether tab `i` should show search results: Subfolders is on, something is typed in the
    /// filter, and the tab is in a folder that could be read (This PC only holds drives, the
    /// Recycle Bin is filtered as it is; a folder that's gone keeps saying so).
    fn search_wanted(&self, i: usize) -> bool {
        self.tabs.get(i).is_some_and(|t| {
            let listing = t.search.as_ref().map_or(&t.folder, |s| &s.listing);
            let unreadable = listing.error.is_some() && listing.path == t.path;
            self.subfolders && !t.filter.trim().is_empty() && !t.path.is_empty() && !recycle::is_bin(&t.path) && !unreadable
        })
    }

    /// The Subfolders switch (Ctrl+Shift+F, or its button next to the filter).
    fn toggle_subfolders(&mut self) {
        if self.tab().path.is_empty() {
            self.say("Can't search subfolders in This PC");
            return;
        }
        if recycle::is_bin(&self.tab().path) {
            self.say("Can't search subfolders in the Recycle Bin");
            return;
        }
        self.subfolders = !self.subfolders;
        let i = self.active;
        if self.search_wanted(i) {
            self.start_search(i, false);
        } else if self.tabs[i].is_searching() {
            self.end_search(i, true);
        }
        self.say(if self.subfolders { "Searching subfolders too" } else { "Searching this folder only" });
    }

    /// Starts (or starts again) searching tab `i`'s folder for its filter text. `typing`: the
    /// filter was just typed into - wait a moment first, and keep showing the last results
    /// (narrowed down by the filter) until new ones come; otherwise they're cleared at once.
    fn start_search(&mut self, i: usize, typing: bool) {
        let generation = self.next_generation;
        self.next_generation += 1;
        let (tx, hwnd) = (self.tx.clone(), self.hwnd.clone());
        let (show_hidden, show_system) = (self.settings.show_hidden, self.settings.show_system);
        let t = &mut self.tabs[i];
        let (root, query, tab_id) = (t.path.clone(), t.filter.trim().to_string(), t.id);
        let listing = match t.search.take() {
            Some(mut old) => {
                old.cancel();
                std::mem::take(&mut old.listing)
            }
            None => {
                // Search results and the tree view don't mix: the open folders close.
                tree::close_all(t);
                // The folder's own listing waits with the search; the list shows the results.
                let space = t.folder.space.clone();
                let listing = std::mem::replace(&mut t.folder, Folder { path: root.clone(), space, ..Default::default() });
                t.selected.clear();
                (t.focus, t.anchor) = (None, None);
                // What a file job wanted selected (by name) or named belongs to the folder's
                // listing, not to the results.
                (t.pending_select, t.rename_request, t.appear) = (None, None, None);
                t.rebuild_view();
                t.scroll = 0.0;
                listing
            }
        };
        let ask = search::Ask { root, query, generation, show_hidden, show_system, pause: typing };
        let started = search::start(ask, listing, move |batch| {
            if tx.send(Loaded::Search { tab_id, generation, batch }).is_err() {
                return false;
            }
            wake(&hwnd);
            true
        });
        match started {
            Ok(s) => {
                t.search = Some(s);
                if !typing {
                    clear_results(t);
                }
            }
            Err(listing) => {
                // No thread to be had: the folder comes back, filtered as usual.
                t.folder = listing;
                t.selected = vec![false; t.folder.entries.len()];
                (t.focus, t.anchor) = (None, None);
                t.rebuild_view();
                self.say("Windows couldn't start searching right now. Try again.");
            }
        }
    }

    /// Ends tab `i`'s search: the folder's own listing comes back (filtered as the filter says).
    /// Results selected in the folder itself stay selected. `catch_up`: read the folder again
    /// quietly afterwards, since changes weren't followed during the search.
    fn end_search(&mut self, i: usize, catch_up: bool) {
        let t = &mut self.tabs[i];
        let Some(mut s) = t.search.take() else { return };
        s.cancel();
        // What stays selected: results that are right in the folder.
        let keep: Vec<String> = t
            .selected_entries()
            .into_iter()
            .filter_map(|e| t.folder.entries.get(e))
            .filter(|e| fileops::same_path(folder::result_folder(e), &s.root))
            .map(|e| e.lower.clone())
            .collect();
        t.folder = std::mem::take(&mut s.listing);
        t.selected = vec![false; t.folder.entries.len()];
        (t.focus, t.anchor) = (None, None);
        if t.sort == SortKey::Folder {
            (t.sort, t.descending) = (SortKey::Name, false);
        }
        t.rebuild_view();
        t.scroll = 0.0;
        for (e, entry) in t.folder.entries.iter().enumerate() {
            if keep.contains(&entry.lower) && t.pos[e] != u32::MAX {
                t.selected[e] = true;
                t.focus.get_or_insert(e);
            }
        }
        t.anchor = t.focus;
        t.reveal_focus = true;
        if catch_up {
            self.reload(i, Armed::none());
        }
    }

    /// "Open file location" on a search result: its folder opens (without the filter) with the
    /// item selected.
    fn open_file_location(&mut self) {
        let t = self.tab();
        let chosen = t.selected_entries();
        let Some(&index) = chosen.first().or(t.focus.as_ref()) else { return };
        let path = t.folder.entries[index].path_in(&t.folder.path);
        let (Some(dir), name) = (folder::parent(&path), folder::file_name(&path).to_string()) else { return };
        let i = self.active;
        self.tabs[i].filter.clear();
        self.end_search(i, true);
        if fileops::same_path(&dir, &self.tabs[i].path) {
            // Right in this folder: it's in the listing that just came back.
            let t = &mut self.tabs[i];
            t.clear_selection();
            if let Some(row) = t.folder.entries.iter().position(|e| e.lower == name.to_lowercase()).and_then(|e| t.row_of(e)) {
                t.select_only_row(row);
                t.reveal_focus = true;
            }
        } else {
            self.navigate(&dir);
            self.tabs[i].select_after_load = Some(name);
        }
    }

    // ------------------------------------------------------------ names

    /// What a folder is called on screen: "This PC", "Local Disk (C:)", "Downloads", "Recycle Bin".
    pub fn display_name(&self, path: &str) -> String {
        if path.is_empty() {
            return "This PC".into();
        }
        if recycle::is_bin(path) {
            return recycle::NAME.into();
        }
        if let Some(d) = self.drive_for_root(path) {
            return d.name.clone();
        }
        if path.len() <= 3 && path.as_bytes().get(1) == Some(&b':') {
            return format!("Local Disk ({}:)", &path[..1]);
        }
        folder::file_name(path).to_string()
    }

    fn drive_for_root(&self, path: &str) -> Option<&Drive> {
        let p = path.trim_end_matches('\\');
        if p.len() != 2 || !p.ends_with(':') {
            return None;
        }
        self.drives.iter().find(|d| d.root[..2].eq_ignore_ascii_case(p))
    }

    /// The steps of a path for the path bar, from This PC down:
    /// [("This PC", ""), ("Local Disk (C:)", "C:\"), ("Users", "C:\Users"), ...].
    pub fn path_steps(&self, path: &str) -> Vec<(String, String)> {
        if recycle::is_bin(path) {
            // One step (Up still goes to This PC).
            return vec![(recycle::NAME.to_string(), path.to_string())];
        }
        let mut chain = vec![path.to_string()];
        let mut cur = path.to_string();
        while let Some(p) = folder::parent(&cur) {
            chain.push(p.clone());
            cur = p;
        }
        chain.reverse();
        chain.into_iter().map(|p| (self.display_name(&p), p)).collect()
    }
}

/// If `child` is directly inside `parent`, its name (so going up can select where you came from).
fn child_name(parent: &str, child: &str) -> Option<String> {
    (folder::parent(child).as_deref() == Some(parent)).then(|| {
        if parent.is_empty() {
            child.to_string() // a drive root; matched against drive entries by target below
        } else {
            folder::file_name(child).to_string()
        }
    })
}

/// The file list's row height for a "Size" setting, in DIPs: the skin's row height (`small`, 28
/// in AMD Radeon) at Small, a little more at Medium and Large.
pub fn row_height(s: ListSize, small: f32) -> f32 {
    match s {
        ListSize::Small => small,
        ListSize::Medium => small + size::ROW_EXTRA_MEDIUM,
        // Never too low for the big icons.
        ListSize::Large => (small + size::ROW_EXTRA_LARGE).max(size::ICON_LARGE + 8.0),
    }
}

/// The paths that are in the same folder as the first one (all of them, normally; search results
/// can come from several folders, and Windows' own windows and menus work on one folder's items).
fn one_folder(paths: Vec<String>) -> Vec<String> {
    let Some(dir) = paths.first().and_then(|p| folder::parent(p)) else { return paths };
    paths.into_iter().filter(|p| folder::parent(p).is_some_and(|d| fileops::same_path(&d, &dir))).collect()
}

/// Takes search results into the tab (search.rs): they join the list in their sorted place. The
/// first ones of a new search replace the previous search's; whatever was selected and is found
/// again stays selected (by full path, since names repeat across folders). With nothing selected,
/// the first match is, as when filtering.
fn take_results(t: &mut Tab, batch: search::Batch) {
    let Some(s) = t.search.as_mut() else { return };
    let replace = !s.fresh;
    (s.fresh, s.done, s.capped) = (true, batch.done, batch.capped);
    let keep: std::collections::HashSet<String> =
        if replace { t.selected_entries().into_iter().map(|e| t.item_key(e)).collect() } else { Default::default() };
    if replace {
        clear_results(t);
    }
    let start = t.folder.entries.len();
    t.folder.entries.extend(batch.entries);
    t.selected.resize(t.folder.entries.len(), false);
    for e in start..t.folder.entries.len() {
        if !keep.is_empty() && keep.contains(&t.item_key(e)) {
            t.selected[e] = true;
            t.focus.get_or_insert(e);
        }
    }
    t.rebuild_view();
    if t.anchor.is_none() {
        t.anchor = t.focus;
    }
    if t.selected_entries().is_empty() && !t.view.is_empty() {
        t.select_only_row(0);
    }
}

/// No search results (yet).
fn clear_results(t: &mut Tab) {
    t.folder.entries.clear();
    t.selected.clear();
    (t.focus, t.anchor) = (None, None);
    t.rebuild_view();
    t.scroll = 0.0;
}

/// Puts a freshly read folder into its tab, keeping selection and scroll on a refresh.
fn apply_listing(t: &mut Tab, folder: Folder) {
    let refresh = folder.path == t.folder.path && !t.folder.entries.is_empty();
    // Found again by their key (`Entry::key`): the name - or, in the Recycle Bin, where each item is
    // kept, since it can hold several items of the same name.
    let keep: Vec<String> = if refresh { t.selected_entries().iter().map(|&i| t.folder.entries[i].key()).collect() } else { Vec::new() };
    let focus_name = t.focus.filter(|_| refresh).and_then(|i| t.folder.entries.get(i)).map(|e| e.key());
    if folder.path != t.folder.path {
        t.tree.clear(); // another folder: nothing in it is open in the tree yet
        // Another folder: the view the rules give it, or the tab's own.
        let view = t.view_for(&folder.path);
        if t.view_mode != view {
            t.view_mode = view;
            t.lines = None; // (a step means something else in another view)
        }
    }
    t.folder = folder;
    // The folders open in the tree view: their items come after the folder's own again.
    tree::graft_entries(&mut t.tree, &mut t.folder);
    t.selected = vec![false; t.folder.entries.len()];
    t.focus = None;
    t.anchor = None;
    if !refresh {
        t.scroll = 0.0;
    }
    t.rebuild_view();
    // Names are found again through one index of the new listing (lower-case name, or a drive's
    // root, -> entry). Searching the listing once per name would freeze the window for seconds
    // when 10,000 selected files are refreshed: 10,000 x 10,000 comparisons.
    let mut index: HashMap<String, usize> = HashMap::new();
    if !keep.is_empty() || focus_name.is_some() || t.select_after_load.is_some() {
        index.reserve(t.folder.entries.len());
        for (i, e) in t.folder.entries.iter().enumerate() {
            index.entry(e.lower.clone()).or_insert(i);
            if let Some(target) = &e.target {
                index.entry(target.to_lowercase()).or_insert(i);
            }
        }
    }
    let find = |name: &str| index.get(&name.to_lowercase()).copied();
    for name in &keep {
        if let Some(i) = find(name) {
            t.selected[i] = true;
        }
    }
    if let Some(name) = focus_name {
        t.focus = find(&name);
        t.anchor = t.focus;
    }
    if let Some(name) = t.select_after_load.take() {
        if let Some(i) = find(&name) {
            t.selected[i] = true;
            t.focus = Some(i);
            t.anchor = Some(i);
            t.reveal_focus = true;
        }
    }
    // After a paste, rename or delete: the new items (app/files.rs).
    files::select_pending(t);
    files::rename_appeared(t);
}

/// Puts a fresh read of the folder a tab already shows into it (the folder changed, or the tab
/// wasn't watched while in the background), so that nothing jumps:
/// - the selection, the cursor, the Shift-anchor and the row at the top of the list stay on the
///   same items, found again by name - or by their new name when they were renamed (`renamed`:
///   old, new, lower case);
/// - new items appear in their sorted place, deleted ones vanish;
/// - the filter, the sort and the scroll position stay.
///
/// Returns false when nothing in the list changed (only the free space, say). `row_h`: the list's
/// row height (the "Size" setting).
fn merge_listing(t: &mut Tab, folder: Folder, renamed: &[(String, String)], row_h: f32) -> bool {
    // (Only the folder's own items count: the tree view's open folders have watches of their own.)
    let same = if t.tree.is_open() { tree::same_own_listing(&t.folder, &folder) } else { same_listing(&t.folder, &folder) };
    if same {
        t.folder.space = folder.space;
        t.folder.read_ms = folder.read_ms;
        return false;
    }
    // What to find again: drives by their root ("e:\" keeps its place when the stick's label
    // changes), everything else by its name.
    let key = |e: &Entry| e.target.as_deref().map_or_else(|| e.lower.clone(), str::to_lowercase);
    let key_of = |i: usize| t.folder.entries.get(i).map(key);
    let selected: Vec<String> = t.selected_entries().into_iter().filter_map(key_of).collect();
    let focus = t.focus.and_then(key_of);
    let anchor = t.anchor.and_then(key_of);
    // The row at the top: it stays where it is on screen, even if items came or went above it.
    // (At the very top the list stays at the top, so new items there show.) In the icon and List
    // views a step holds several items (a row of tiles, a column of names): `Tab::lines`.
    let (row_h, per_line) = t.lines.filter(|(h, n)| *h > 0.0 && *n > 0).unwrap_or((row_h, 1));
    let top_line = (t.scroll / row_h).floor().max(0.0) as usize;
    let offset = t.scroll - top_line as f32 * row_h;
    let top = if t.scroll > 0.0 { t.index_at(top_line * per_line).and_then(key_of) } else { None };

    t.folder = folder;
    tree::graft_entries(&mut t.tree, &mut t.folder);
    t.selected = vec![false; t.folder.entries.len()];
    t.focus = None;
    t.anchor = None;
    t.rebuild_view();

    // One index of the new listing (see apply_listing for why).
    let index: HashMap<String, usize> = t.folder.entries.iter().enumerate().map(|(i, e)| (key(e), i)).collect();
    let find = |name: &str| -> Option<usize> {
        // The name itself, or else where renames took it (a -> b -> c), never round in circles.
        let mut name = name;
        for _ in 0..=renamed.len().min(64) {
            if let Some(&i) = index.get(name) {
                return Some(i);
            }
            name = renamed.iter().rev().find(|(old, _)| old == name).map(|(_, new)| new.as_str())?;
        }
        None
    };
    for name in &selected {
        if let Some(i) = find(name).filter(|&i| t.row_of(i).is_some()) {
            t.selected[i] = true;
        }
    }
    t.focus = focus.as_deref().and_then(find);
    t.anchor = anchor.as_deref().and_then(find).or(t.focus);
    if let Some(row) = top.as_deref().and_then(find).and_then(|i| t.row_of(i)) {
        t.scroll = (row / per_line) as f32 * row_h + offset;
    }
    true
}

/// Whether two reads of a folder list the same things, in the same order.
fn same_listing(a: &Folder, b: &Folder) -> bool {
    a.error == b.error && a.entries.len() == b.entries.len() && a.entries.iter().zip(&b.entries).all(|(x, y)| same_entry(x, y))
}

/// Whether two reads found an item the same.
fn same_entry(x: &Entry, y: &Entry) -> bool {
    x.name == y.name && x.kind == y.kind && x.size == y.size && x.modified == y.modified && x.attributes == y.attributes && x.free == y.free && x.target == y.target
}

/// `path` after the folder `from` became `to`: itself, or anything inside it. None if it isn't
/// affected. (Windows paths: case doesn't matter.)
fn moved_path(path: &str, from: &str, to: &str) -> Option<String> {
    if from.is_empty() || path.len() < from.len() || !path.is_char_boundary(from.len()) {
        return None;
    }
    let (head, rest) = path.split_at(from.len());
    let inside = rest.is_empty() || rest.starts_with('\\') || from.ends_with('\\');
    (inside && head.to_lowercase() == from.to_lowercase()).then(|| format!("{to}{rest}"))
}

fn wake(hwnd: &AtomicIsize) {
    let h = hwnd.load(Ordering::SeqCst);
    if h != 0 {
        // SAFETY: posting to a window handle is safe even if the window is gone (the call just fails).
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_LOADED, WPARAM(0), LPARAM(0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::EntryKind;

    fn file(name: &str) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: EntryKind::File,
            size: 1,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: folder::extension(name),
            type_name: Arc::from("File"),
            target: None,
            free: 0,
        }
    }

    fn listing(names: &[&str]) -> Folder {
        Folder { path: "C:\\x".into(), entries: names.iter().map(|n| file(n)).collect(), ..Default::default() }
    }

    fn tab_with(names: &[&str]) -> Tab {
        let mut t = Tab::new(1, "C:\\x".into());
        apply_listing(&mut t, listing(names));
        t
    }

    fn shown(t: &Tab) -> Vec<&str> {
        (0..t.view.len()).filter_map(|r| t.entry_at(r)).map(|e| e.name.as_str()).collect()
    }

    fn selected(t: &Tab) -> Vec<&str> {
        t.selected_entries().iter().map(|&i| t.folder.entries[i].name.as_str()).collect()
    }

    fn focused(t: &Tab) -> Option<&str> {
        t.focus.map(|i| t.folder.entries[i].name.as_str())
    }

    #[test]
    fn merging_keeps_selection_and_cursor_by_name() {
        let mut t = tab_with(&["b.txt", "d.txt", "f.txt"]);
        t.select_only_row(1); // d.txt
        t.select_range_to(2, true); // d.txt and f.txt, cursor on f.txt, anchor on d.txt
        // A file came before them, one went; the listing comes back in another order.
        assert!(merge_listing(&mut t, listing(&["f.txt", "a.txt", "d.txt"]), &[], size::ROW_H));
        assert_eq!(shown(&t), vec!["a.txt", "d.txt", "f.txt"], "new items in their sorted place, deleted ones gone");
        assert_eq!(selected(&t), vec!["d.txt", "f.txt"]);
        assert_eq!(focused(&t), Some("f.txt"));
        assert_eq!(t.anchor.map(|i| t.folder.entries[i].name.as_str()), Some("d.txt"));
    }

    #[test]
    fn merging_follows_renamed_items() {
        let mut t = tab_with(&["a.txt", "b.txt"]);
        t.select_only_row(0);
        let renamed = vec![("a.txt".to_string(), "a2.txt".to_string()), ("a2.txt".to_string(), "z.txt".to_string())];
        merge_listing(&mut t, listing(&["b.txt", "z.txt"]), &renamed, size::ROW_H);
        assert_eq!(selected(&t), vec!["z.txt"], "renamed twice, still selected");
        assert_eq!(focused(&t), Some("z.txt"));
        // Renames that go round in circles end.
        let circle = vec![("q".to_string(), "r".to_string()), ("r".to_string(), "q".to_string())];
        let mut gone = tab_with(&["q"]);
        gone.select_only_row(0);
        merge_listing(&mut gone, listing(&["other"]), &circle, size::ROW_H);
        assert!(selected(&gone).is_empty());
    }

    #[test]
    fn merging_keeps_the_rows_on_screen_still() {
        let names: Vec<String> = (10..60).map(|i| format!("f{i}.txt")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut t = tab_with(&refs);
        t.scroll = 10.0 * size::ROW_H + 5.0; // f20.txt at the top, a little scrolled into it
        let mut more = refs.clone();
        more.extend(["a1.txt", "a2.txt", "a3.txt"]); // three new items above it
        merge_listing(&mut t, listing(&more), &[], size::ROW_H);
        assert_eq!(t.entry_at((t.scroll / size::ROW_H) as usize).map(|e| e.name.as_str()), Some("f20.txt"));
        assert_eq!(t.scroll, 13.0 * size::ROW_H + 5.0);
        // At the very top it stays at the top, so new items there show.
        t.scroll = 0.0;
        let mut even_more = more.clone();
        even_more.push("a0.txt");
        merge_listing(&mut t, listing(&even_more), &[], size::ROW_H);
        assert_eq!(t.scroll, 0.0);
        assert_eq!(t.entry_at(0).map(|e| e.name.as_str()), Some("a0.txt"));
    }

    #[test]
    fn merging_the_same_listing_changes_nothing() {
        let mut t = tab_with(&["a.txt", "b.txt"]);
        t.select_only_row(1);
        let mut again = listing(&["a.txt", "b.txt"]);
        again.space = Some(folder::Space { drive: "C:".into(), free: 42 });
        assert!(!merge_listing(&mut t, again, &[], size::ROW_H));
        assert_eq!(selected(&t), vec!["b.txt"]);
        assert_eq!(t.folder.space.as_ref().map(|s| s.free), Some(42), "the free space is still updated");
        // An item that changed size is a change.
        let mut bigger = listing(&["a.txt", "b.txt"]);
        bigger.entries[0].size = 99;
        assert!(merge_listing(&mut t, bigger, &[], size::ROW_H));
        assert_eq!(selected(&t), vec!["b.txt"]);
    }

    #[test]
    fn a_folder_that_vanishes_and_comes_back() {
        let mut t = tab_with(&["a.txt"]);
        t.select_only_row(0);
        let gone = Folder { path: "C:\\x".into(), error: Some("This folder doesn't exist (any more).".into()), ..Default::default() };
        assert!(merge_listing(&mut t, gone, &[], size::ROW_H));
        assert!(t.view.is_empty() && t.folder.error.is_some());
        assert!(merge_listing(&mut t, listing(&["a.txt", "new.txt"]), &[], size::ROW_H));
        assert_eq!(shown(&t), vec!["a.txt", "new.txt"]);
        assert!(t.folder.error.is_none());
    }

    fn result(path: &str) -> Entry {
        Entry { target: Some(path.to_string()), ..file(folder::file_name(path)) }
    }

    fn selected_paths(t: &Tab) -> Vec<String> {
        t.selected_entries().iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect()
    }

    #[test]
    fn search_results_stream_in_and_keep_the_selection_by_path() {
        let mut t = Tab::new(1, "C:\\x".into());
        t.folder.path = "C:\\x".into();
        t.search = Some(search::test_search("C:\\x"));
        let batch = |paths: &[&str], done: bool| search::Batch { entries: paths.iter().map(|p| result(p)).collect(), done, capped: false };
        take_results(&mut t, batch(&["C:\\x\\b\\r.txt", "C:\\x\\a\\r.txt"], false));
        assert_eq!(t.view.len(), 2);
        assert_eq!(selected_paths(&t), vec!["C:\\x\\a\\r.txt"], "the first match is selected");
        // Two results with the same name are told apart by their paths.
        assert_ne!(t.item_key(0), t.item_key(1));
        assert_eq!(t.item_key(0), "c:\\x\\b\\r.txt");
        // The second is picked; more come in: it stays the one selected.
        t.select_only_row(1);
        take_results(&mut t, batch(&["C:\\x\\c\\r.txt"], true));
        assert_eq!(selected_paths(&t), vec!["C:\\x\\b\\r.txt"]);
        assert!(t.search.as_ref().is_some_and(|s| s.done && !s.capped));
        // A new search's first results replace the old ones; what is found again stays selected.
        if let Some(s) = t.search.as_mut() {
            s.fresh = false;
        }
        take_results(&mut t, batch(&["C:\\x\\b\\r.txt", "C:\\x\\d\\r.txt"], false));
        assert_eq!(t.folder.entries.len(), 2);
        assert_eq!(selected_paths(&t), vec!["C:\\x\\b\\r.txt"]);
        // Outside a search, an item's key is its name.
        let plain = tab_with(&["A.txt"]);
        assert_eq!(plain.item_key(0), "a.txt");
    }

    #[test]
    fn only_items_of_one_folder_go_to_windows() {
        let paths = vec!["C:\\a\\1.txt".to_string(), "C:\\b\\2.txt".to_string(), "c:\\A\\3.txt".to_string()];
        assert_eq!(one_folder(paths), vec!["C:\\a\\1.txt", "c:\\A\\3.txt"]);
        assert!(one_folder(Vec::new()).is_empty());
    }

    #[test]
    fn row_heights_follow_the_size_setting() {
        assert_eq!(row_height(ListSize::Small, size::ROW_H), size::ROW_H);
        assert_eq!((row_height(ListSize::Medium, size::ROW_H), row_height(ListSize::Large, size::ROW_H)), (34.0, 44.0), "AMD Radeon: as before skins");
        assert!(row_height(ListSize::Medium, 24.0) > row_height(ListSize::Small, 24.0));
        assert!(row_height(ListSize::Large, 20.0) >= size::ICON_LARGE + 8.0, "room for the big icons");
    }

    #[test]
    fn tabs_follow_a_renamed_folder() {
        assert_eq!(moved_path("C:\\a\\b", "C:\\a\\b", "C:\\a\\c").as_deref(), Some("C:\\a\\c"));
        assert_eq!(moved_path("C:\\a\\b\\deep", "c:\\A\\B", "C:\\a\\c").as_deref(), Some("C:\\a\\c\\deep"), "case doesn't matter");
        assert_eq!(moved_path("C:\\a\\bb", "C:\\a\\b", "C:\\a\\c"), None, "a folder that only starts the same");
        assert_eq!(moved_path("C:\\a", "C:\\a\\b", "C:\\a\\c"), None);
        assert_eq!(moved_path("", "C:\\a", "C:\\b"), None);
        assert_eq!(moved_path("C:\\é\\x", "C:\\é", "C:\\e").as_deref(), Some("C:\\e\\x"));
    }

    #[test]
    fn this_pc_has_no_path_to_copy() {
        let mut a = AppState::for_tests(Settings::default());
        a.tabs[0].path = String::new();
        a.copy_paths();
        assert_eq!(a.note(), Some("This PC has no path to copy"));
    }

    #[test]
    fn a_column_clicked_while_the_custom_order_sorts_is_a_first_click() {
        let mut a = AppState::for_tests(Settings { custom_sort: true, ..Default::default() });
        a.hidden = true; // (no folder watch in a test)
        let custom = a.custom_order();
        a.tabs[0] = tab_with(&["b.txt", "a.txt", "c.txt"]);
        a.tabs[0].custom = custom;
        // Under the custom order the tab is still sorted by name, A to Z, but no column shows it.
        a.run(Command::SortBy(SortKey::Name));
        assert!(a.tab().custom.is_none());
        assert_eq!(shown(a.tab()), ["a.txt", "b.txt", "c.txt"], "A to Z, as a first click on Name sorts");
        // Clicked again: turned around.
        a.run(Command::SortBy(SortKey::Name));
        assert_eq!(shown(a.tab()), ["c.txt", "b.txt", "a.txt"]);
    }

    #[test]
    fn a_folder_the_custom_order_has_no_rules_for_sorts_by_its_column() {
        // Rules for another folder only: this one keeps its column, and a click on one sorts it
        // without turning the custom order off (it still decides in that other folder).
        let recipe = "in C:\\elsewhere:\n-a_to_z".to_string();
        let mut a = AppState::for_tests(Settings { custom_sort: true, sort_recipe: Some(recipe), ..Default::default() });
        a.hidden = true; // (no folder watch in a test)
        let custom = a.custom_order();
        a.tabs[0] = tab_with(&["b.txt", "a.txt", "c.txt"]);
        a.tabs[0].custom = custom;
        a.tabs[0].set_sort(SortKey::Name, true);
        assert!(a.tab().custom_here().is_none());
        assert_eq!(shown(a.tab()), ["c.txt", "b.txt", "a.txt"], "Z to A, as its column says");
        a.run(Command::SortBy(SortKey::Name));
        assert!(a.settings.custom_sort && a.tab().custom.is_some(), "still on for the other folder");
        assert_eq!(shown(a.tab()), ["a.txt", "b.txt", "c.txt"], "a second click on Name turns it around");
    }

    #[test]
    fn a_folder_with_its_own_rules_is_sorted_by_them() {
        // Only this folder's rules (in brackets): they decide here, and a column click turns the
        // custom order off, as with rules for every folder.
        let recipe = "in C:\\x (\n-z_to_a\n)\n# nothing for other folders".to_string();
        let mut a = AppState::for_tests(Settings { custom_sort: true, sort_recipe: Some(recipe), ..Default::default() });
        a.hidden = true; // (no folder watch in a test)
        let custom = a.custom_order();
        a.tabs[0] = tab_with(&["b.txt", "a.txt", "c.txt"]);
        a.tabs[0].custom = custom;
        a.tabs[0].rebuild_view();
        assert!(a.tab().custom_here().is_some());
        assert_eq!(shown(a.tab()), ["c.txt", "b.txt", "a.txt"], "Z to A, as its rules say");
        // Renamed: the folder has no rules under its new name, so its column sorts it again.
        a.folder_moved("C:\\x", "C:\\y");
        assert!(a.tab().custom_here().is_none());
        assert_eq!(shown(a.tab()), ["a.txt", "b.txt", "c.txt"]);
        a.folder_moved("C:\\y", "C:\\x");
        assert_eq!(shown(a.tab()), ["c.txt", "b.txt", "a.txt"]);
        a.run(Command::SortBy(SortKey::Name));
        assert!(!a.settings.custom_sort && a.tab().custom.is_none());
        assert_eq!(shown(a.tab()), ["a.txt", "b.txt", "c.txt"], "A to Z, as a first click on Name sorts");
    }

    /// A tab at C:\\x with these view rules (whether custom sort is on or off).
    fn with_views(rules: &str, custom_sort: bool) -> AppState {
        let mut a = AppState::for_tests(Settings { custom_sort, sort_recipe: Some(rules.into()), ..Default::default() });
        a.hidden = true; // (no folder watch in a test)
        let views = a.view_rules();
        a.tabs[0].views = Some(views);
        a
    }

    fn open(a: &mut AppState, path: &str) {
        apply_listing(a.tab_mut(), Folder { path: path.into(), entries: vec![file("a.txt")], ..Default::default() });
    }

    #[test]
    fn a_folder_with_a_view_rule_is_shown_that_way() {
        let mut a = with_views("in C:\\pics (\n-large_icons\n)\n", false);
        open(&mut a, "C:\\x");
        assert_eq!(a.tab().view_mode, ViewMode::Details, "no rule: the tab's own");
        open(&mut a, "C:\\pics");
        assert_eq!(a.tab().view_mode, ViewMode::Large, "with custom sort off too");
        open(&mut a, "C:\\pics\\2026");
        assert_eq!(a.tab().view_mode, ViewMode::Details, "subfolders don't get it");
        // The VIEW button in a folder without a rule: the tab's own view, for every such folder.
        a.set_view(ViewMode::List);
        assert_eq!((a.tab().view_mode, a.tab().own_view), (ViewMode::List, ViewMode::List));
        open(&mut a, "C:\\pics");
        assert_eq!(a.tab().view_mode, ViewMode::Large);
        // In the folder with the rule: just for now.
        a.set_view(ViewMode::Tree);
        assert_eq!((a.tab().view_mode, a.tab().own_view), (ViewMode::Tree, ViewMode::List));
        assert!(a.note().is_some_and(|n| n.contains("for now")));
        open(&mut a, "C:\\x");
        assert_eq!(a.tab().view_mode, ViewMode::List);
        open(&mut a, "C:\\pics");
        assert_eq!(a.tab().view_mode, ViewMode::Large, "the rule again");
        // Saved with the tab: its own view, not the rule's.
        assert_eq!(a.snapshot().tabs[0].view, Some(ViewMode::List));
        // A refresh keeps a view picked by hand.
        a.set_view(ViewMode::Medium);
        open(&mut a, "C:\\pics");
        assert_eq!(a.tab().view_mode, ViewMode::Medium);
    }

    #[test]
    fn new_view_rules_show_at_once() {
        let mut a = with_views("-a_to_z\n", true);
        open(&mut a, "C:\\x");
        // Typed in the editor: the folder on screen follows.
        a.set_custom_sort(true, Some("-a_to_z\nin C:\\x (\n-extra_large_icons\n)\n".into()));
        assert_eq!(a.tab().view_mode, ViewMode::ExtraLarge);
        // Custom sort turned off (a column click): views stay, and so does one picked by hand.
        a.set_view(ViewMode::List);
        a.set_custom_sort(false, None);
        assert_eq!(a.tab().view_mode, ViewMode::List);
        // The rule taken out: the tab's own view again.
        a.set_custom_sort(false, Some("-a_to_z\n".into()));
        assert_eq!(a.tab().view_mode, ViewMode::Details);
    }

    #[test]
    fn settings_view_leaves_folders_with_a_view_rule_alone() {
        let mut a = with_views("in C:\\pics (\n-large_icons\n)\n", false);
        open(&mut a, "C:\\pics");
        a.set_view_everywhere(ViewMode::List);
        assert_eq!((a.tab().view_mode, a.tab().own_view), (ViewMode::Large, ViewMode::List));
        open(&mut a, "C:\\x");
        assert_eq!(a.tab().view_mode, ViewMode::List);
        // A view for every folder: everything but the folders with their own.
        let mut a = with_views("-details\nin C:\\pics (\n-large_icons\n)\n", false);
        a.set_view_everywhere(ViewMode::Medium);
        open(&mut a, "C:\\x");
        assert_eq!(a.tab().view_mode, ViewMode::Details);
        // The Recycle Bin keeps the tab's own.
        open(&mut a, crate::recycle::LOCATION);
        assert_eq!(a.tab().view_mode, ViewMode::Medium);
    }
}
