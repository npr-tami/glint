//! The file list's right-click menus: one for items (right-click on a row, or the Apps key with
//! something selected) and one for the folder's empty space. Like Explorer, right-clicking an item
//! that isn't selected selects just it first; right-clicking the empty space clears the selection.
//!
//! Each menu is Glint's own items, in Explorer's order, with the items other apps add to Windows'
//! menu (Open with Code, Git Bash, 7-Zip, Send to...) merged in near the end - and "Show more
//! options" for Windows' whole menu. Holding Shift (Shift+right-click, Shift+F10) opens Windows'
//! whole menu straight away, with its extra items. shellmenu.rs gets Windows' items, popup.rs
//! shows the menu.
//!
//! Items for working with files (cut, copy, paste, rename, delete, new folder) send the commands
//! app/files.rs carries out. The same commands picked from Windows' own menu are done by Glint too
//! (`command_for_verb`), so they get Glint's question, selection and name box.
//!
//! The Recycle Bin has menus of its own: Restore, Delete permanently, Go to
//! where it was, Copy where it was on items; Restore all and Empty Recycle Bin on the empty space.
//! They carry no Windows items (Windows' menu for a deleted item would act on where it's kept).

use crate::app::{can_pin, AppState, SortKey};
use crate::fileops::clipboard_has_files;
use crate::folder::EntryKind;
use crate::recycle;
use crate::settings::ViewMode;
use crate::shellmenu::{ShellItem, PICTURE_UNKNOWN};
use crate::ui::{Command, Effect, FilesMenu, Menu, MenuAbout, MenuItem, Point, ShellRequest, ShellTarget, MORE_OPTIONS, SHELL_SLOT};

/// More items than this, and Windows isn't asked (its menu for 50,000 files takes long to build).
const MAX_WINDOWS_ITEMS: usize = 2000;

/// The menus' tags (for `menu_pick`).
pub const MENU_ITEMS: u32 = 1;
pub const MENU_BACKGROUND: u32 = 2;

// The picks. Glint's own ids stay below shellmenu::SHELL_FIRST (0x1000), where Windows' begin.
const PICK_OPEN: u32 = 1;
const PICK_NEW_TABS: u32 = 2;
const PICK_OPEN_WITH: u32 = 3;
const PICK_CUT: u32 = 4;
const PICK_COPY: u32 = 5;
const PICK_PASTE_INTO: u32 = 6;
const PICK_COPY_PATH: u32 = 7;
const PICK_RENAME: u32 = 8;
const PICK_DELETE: u32 = 9;
const PICK_DELETE_FOREVER: u32 = 10;
const PICK_TERMINAL: u32 = 11;
const PICK_EXPLORER: u32 = 12;
const PICK_PROPERTIES: u32 = 13;
const PICK_REFRESH: u32 = 20;
const PICK_PASTE: u32 = 21;
const PICK_NEW_FOLDER: u32 = 22;
const PICK_NEW_TEXT: u32 = 23;
const PICK_HIDDEN: u32 = 24;
const PICK_SELECT_ALL: u32 = 25;
const PICK_TERMINAL_HERE: u32 = 26;
const PICK_NEW_WINDOW: u32 = 27;
const PICK_SORT_NAME: u32 = 30;
const PICK_SORT_DATE: u32 = 31;
const PICK_SORT_TYPE: u32 = 32;
const PICK_SORT_SIZE: u32 = 33;
const PICK_ASCENDING: u32 = 34;
const PICK_DESCENDING: u32 = 35;
const PICK_PIN: u32 = 36;
const PICK_PIN_HERE: u32 = 37;
// The Recycle Bin's.
const PICK_RESTORE: u32 = 40;
const PICK_DELETE_FROM_BIN: u32 = 41;
const PICK_LOOK: u32 = 42;
const PICK_GO_WHERE_IT_WAS: u32 = 43;
const PICK_COPY_WHERE_IT_WAS: u32 = 44;
const PICK_RESTORE_ALL: u32 = 45;
const PICK_EMPTY_BIN: u32 = 46;
const PICK_BIN_EXPLORER: u32 = 47;
// Added with the list options (search results, the Date created column).
const PICK_LOCATION: u32 = 48;
const PICK_SORT_CREATED: u32 = 49;
// Added with the views: View › (from PICK_VIEW, in `ViewMode::ALL`'s order).
const PICK_VIEW: u32 = 60;

/// Windows' items Glint already has (or that make no sense in Glint), by their verb: left out when
/// Windows' items are merged into Glint's menu. Windows' whole menu still has them.
const SKIP_VERBS: [&str; 19] = [
    "open", "opennew", "opennewwindow", "opennewprocess", "opennewtab", "explore", "find", "openas", "cut", "copy", "paste",
    "delete", "rename", "properties", "copyaspath", "pintohome", "unpinfromhome", "refresh", "glint",
];

/// Verbs of Windows Terminal's "Open in Terminal" (its Windows 11 item, and the registry one older
/// versions and many guides add). Its text is matched too, in any language (see `opens_terminal`).
const TERMINAL_VERBS: [&str; 3] = ["openterminalhere", "wt", "openinterminal"];

/// Windows' items about File Explorer's own view (Glint has its own sorting and views), by their
/// text as `label_key` gives it: left out too when merging.
const SKIP_TEXTS: [&str; 7] = ["view", "sort by", "group by", "arrange icons by", "line up icons", "refresh", "customize this folder"];

/// Whether shift-less Windows extras are wanted, and whether to skip Glint's menu altogether.
#[derive(Clone, Copy, Debug, Default)]
pub struct How {
    /// Shift was held: Windows adds its extra items.
    pub extended: bool,
    /// Windows' whole menu straight away (Shift+right-click, Shift+F10).
    pub whole: bool,
}

/// The menu for the selected items (there must be some), at `at`.
pub fn for_items(app: &AppState, at: Point, how: How) -> Effect {
    if recycle::is_bin(&app.tab().folder.path) {
        return bin_items(app, at);
    }
    let t = app.tab();
    let chosen = t.selected_entries();
    let Some(&first_index) = chosen.first() else { return for_background(app, at, how) };
    let first = &t.folder.entries[first_index];
    let single = chosen.len() == 1;
    let folders = chosen.iter().filter(|&&i| t.folder.entries[i].is_container()).count();
    // This PC lists drives: nothing to cut, rename or delete there.
    let files = !t.folder.path.is_empty() && first.kind != EntryKind::Drive;
    let mut items = vec![MenuItem::new(PICK_OPEN, "Open\tEnter")];
    if folders > 0 {
        items.push(MenuItem::new(PICK_NEW_TABS, if folders > 1 { "Open in new tabs\tCtrl+Enter" } else { "Open in new tab\tCtrl+Enter" }));
    }
    if single && !first.is_container() {
        items.push(MenuItem::new(PICK_OPEN_WITH, "Open with\u{2026}"));
    }
    if single && t.is_searching() {
        // A search result: go to the folder it's in, with it selected.
        items.push(MenuItem::new(PICK_LOCATION, "Open file location"));
    }
    items.push(MenuItem::separator());
    if files {
        items.push(MenuItem::new(PICK_CUT, "Cut\tCtrl+X"));
        items.push(MenuItem::new(PICK_COPY, "Copy\tCtrl+C"));
        if single && first.is_container() && clipboard_has_files() {
            items.push(MenuItem::new(PICK_PASTE_INTO, "Paste into folder"));
        }
    }
    items.push(MenuItem::new(PICK_COPY_PATH, if single { "Copy path\tCtrl+Shift+C" } else { "Copy paths\tCtrl+Shift+C" }));
    if files {
        items.push(MenuItem::separator());
        items.push(MenuItem::new(PICK_RENAME, "Rename\tF2").disabled(!single));
        items.push(MenuItem::new(PICK_DELETE, "Delete\tDel"));
        items.push(MenuItem::new(PICK_DELETE_FOREVER, "Delete permanently\tShift+Del"));
    }
    items.push(MenuItem::separator());
    if single && first.is_container() {
        let path = first.path_in(&t.folder.path);
        items.push(MenuItem::new(PICK_PIN, if app.is_pinned(&path) { "Unpin from sidebar" } else { "Pin to sidebar" }));
        items.push(MenuItem::new(PICK_TERMINAL, "Open in terminal"));
    }
    if first.kind != EntryKind::Drive {
        items.push(MenuItem::new(PICK_EXPLORER, "Show in File Explorer"));
    }
    items.push(MenuItem::new(PICK_PROPERTIES, "Properties\tAlt+Enter"));
    finish(items, MENU_ITEMS, PICK_OPEN, at, how, app, || {
        // Windows' menu is for these very items, so only when the listing on screen is the
        // folder's own (not one still on its way) - and only for items of one folder: Windows makes
        // its menu from the first item's folder, and would look for the others there (search
        // results can come from many folders, and two of them may hold files of the same name).
        (t.folder.path == t.path && chosen.len() <= MAX_WINDOWS_ITEMS)
            .then(|| chosen.iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect::<Vec<String>>())
            .filter(|paths| one_folder(paths))
            .map(ShellTarget::Items)
    })
}

/// The menu for the folder's empty space, at `at`.
pub fn for_background(app: &AppState, at: Point, how: How) -> Effect {
    if recycle::is_bin(&app.tab().folder.path) {
        return bin_background(app, at);
    }
    let t = app.tab();
    let real = !t.path.is_empty();
    // While the custom sort order decides here, no column is checked (as in the SORT menu).
    let custom = t.custom_here().is_some();
    let key = |id, label, k| MenuItem::new(id, label).checked(t.sort == k && !custom);
    let sort = vec![
        key(PICK_SORT_NAME, "Name", SortKey::Name),
        key(PICK_SORT_DATE, "Date modified", SortKey::Date),
        key(PICK_SORT_CREATED, "Date created", SortKey::Created),
        key(PICK_SORT_TYPE, "Type", SortKey::Type),
        key(PICK_SORT_SIZE, "Size", SortKey::Size),
        MenuItem::separator(),
        MenuItem::new(PICK_ASCENDING, "Ascending").checked(!t.descending && !custom),
        MenuItem::new(PICK_DESCENDING, "Descending").checked(t.descending && !custom),
    ];
    let mut items = vec![view_menu(app), MenuItem::submenu("Sort by", sort), MenuItem::new(PICK_REFRESH, "Refresh\tF5"), MenuItem::separator()];
    if real {
        items.push(MenuItem::new(PICK_PASTE, "Paste\tCtrl+V").disabled(!clipboard_has_files()));
        items.push(MenuItem::new(PICK_NEW_FOLDER, "New folder\tCtrl+Shift+N"));
        items.push(MenuItem::new(PICK_NEW_TEXT, "New text file"));
        items.push(MenuItem::separator());
    }
    items.push(MenuItem::new(PICK_HIDDEN, "Show hidden files\tCtrl+H").checked(app.settings.show_hidden));
    items.push(MenuItem::new(PICK_SELECT_ALL, "Select all\tCtrl+A").disabled(t.view.is_empty() || t.waiting_long()));
    items.push(MenuItem::separator());
    if real {
        let pinned = app.is_pinned(&t.path);
        items.push(MenuItem::new(PICK_PIN_HERE, if pinned { "Unpin from sidebar\tCtrl+D" } else { "Pin to sidebar\tCtrl+D" }).disabled(!can_pin(&t.path)));
        items.push(MenuItem::new(PICK_TERMINAL_HERE, "Open in terminal\tCtrl+`"));
        items.push(MenuItem::new(PICK_COPY_PATH, "Copy folder path\tCtrl+Shift+C"));
    }
    items.push(MenuItem::new(PICK_EXPLORER, "Open in File Explorer"));
    items.push(MenuItem::new(PICK_NEW_WINDOW, "New window\tCtrl+N"));
    if real {
        items.push(MenuItem::new(PICK_PROPERTIES, "Properties\tAlt+Enter"));
    }
    finish(items, MENU_BACKGROUND, 0, at, how, app, || Some(ShellTarget::Background(t.path.clone())))
}

/// "View ›": the six views, the tab's checked, with Explorer's keys (Ctrl+Shift+1 to 6).
fn view_menu(app: &AppState) -> MenuItem {
    let now = app.tab().view_mode;
    let items = ViewMode::ALL
        .iter()
        .enumerate()
        .map(|(i, (v, name))| MenuItem::new(PICK_VIEW + i as u32, &format!("{name}\tCtrl+Shift+{}", i + 1)).checked(now == *v))
        .collect();
    MenuItem::submenu("View", items)
}

/// Whether all these paths are in the same folder.
fn one_folder(paths: &[String]) -> bool {
    let dir = |p: &String| crate::folder::parent(p).map(|d| d.to_lowercase());
    paths.first().map(dir).is_some_and(|first| first.is_some() && paths.iter().all(|p| dir(p) == first))
}

/// What a menu opened now is about: the tab, its folder and the selected items (see `MenuAbout`;
/// search results by their full path, `Tab::item_key`).
fn about(app: &AppState) -> MenuAbout {
    let t = app.tab();
    MenuAbout { tab_id: t.id, folder: t.folder.path.clone(), names: t.selected_entries().iter().map(|&i| t.item_key(i)).collect() }
}

/// The Recycle Bin's menu for the selected items: Restore (in bold),
/// Delete permanently, looking at a file as it is, and where each came from.
fn bin_items(app: &AppState, at: Point) -> Effect {
    let t = app.tab();
    let chosen = t.selected_entries();
    let Some(&first) = chosen.first() else { return bin_background(app, at) };
    let one_file = chosen.len() == 1 && !t.folder.entries[first].is_container();
    let mut items = vec![MenuItem::new(PICK_RESTORE, "Restore"), MenuItem::new(PICK_DELETE_FROM_BIN, "Delete permanently\tDel"), MenuItem::separator()];
    if one_file {
        items.push(MenuItem::new(PICK_LOOK, "Open\tEnter"));
    }
    items.push(MenuItem::new(PICK_GO_WHERE_IT_WAS, "Open original location"));
    items.push(MenuItem::new(PICK_COPY_WHERE_IT_WAS, if chosen.len() == 1 { "Copy original location\tCtrl+Shift+C" } else { "Copy original locations\tCtrl+Shift+C" }));
    bin_menu(items, MENU_ITEMS, PICK_RESTORE, at, app)
}

/// The Recycle Bin's menu for its empty space.
fn bin_background(app: &AppState, at: Point) -> Effect {
    let t = app.tab();
    let any = !t.folder.entries.is_empty() && !t.waiting_long();
    // While the custom sort order decides here too, no column is checked.
    let custom = t.custom_here().is_some();
    let key = |id, label, k| MenuItem::new(id, label).checked(t.sort == k && !custom);
    let sort = vec![
        key(PICK_SORT_NAME, "Name", SortKey::Name),
        key(PICK_SORT_DATE, "Date deleted", SortKey::Date),
        key(PICK_SORT_TYPE, "Type", SortKey::Type),
        key(PICK_SORT_SIZE, "Size", SortKey::Size),
        MenuItem::separator(),
        MenuItem::new(PICK_ASCENDING, "Ascending").checked(!t.descending && !custom),
        MenuItem::new(PICK_DESCENDING, "Descending").checked(t.descending && !custom),
    ];
    let items = vec![
        MenuItem::new(PICK_RESTORE_ALL, "Restore all").disabled(!any),
        MenuItem::new(PICK_EMPTY_BIN, "Empty Recycle Bin").disabled(!any),
        MenuItem::separator(),
        view_menu(app),
        MenuItem::submenu("Sort by", sort),
        MenuItem::new(PICK_REFRESH, "Refresh\tF5"),
        MenuItem::new(PICK_SELECT_ALL, "Select all\tCtrl+A").disabled(t.view.is_empty() || t.waiting_long()),
        MenuItem::separator(),
        MenuItem::new(PICK_BIN_EXPLORER, "Open in File Explorer"),
    ];
    bin_menu(items, MENU_BACKGROUND, 0, at, app)
}

/// A Recycle Bin menu: Glint's items only (a pick still acts on what was right-clicked).
fn bin_menu(items: Vec<MenuItem>, tag: u32, default_item: u32, at: Point, app: &AppState) -> Effect {
    Effect::FilesMenu(FilesMenu { menu: Menu { tag, at, items, right_aligned: false }, default_item, windows: None, about: about(app) })
}

/// Adds Windows' part (the slot for its items, and "Show more options") and wraps it all up. Other
/// apps' items go into Glint's menu when the "ShellItemsInMenu" setting says so; Windows' whole
/// menu is always under "Show more options".
fn finish(
    mut items: Vec<MenuItem>,
    tag: u32,
    default_item: u32,
    at: Point,
    how: How,
    app: &AppState,
    target: impl FnOnce() -> Option<ShellTarget>,
) -> Effect {
    let merged = app.settings.shell_items_in_menu;
    let target = target();
    if target.is_some() {
        if merged || how.whole {
            items.push(MenuItem::new(SHELL_SLOT, ""));
        }
        items.push(MenuItem::separator());
        items.push(MenuItem::new(MORE_OPTIONS, "Show more options\tShift+F10"));
    }
    let windows = target.map(|target| ShellRequest { target, extended: how.extended, full: how.whole });
    Effect::FilesMenu(FilesMenu { menu: Menu { tag, at, items, right_aligned: false }, default_item, windows, about: about(app) })
}

/// What picking `id` does.
pub fn pick(id: u32, app: &mut AppState) -> Effect {
    let cmd = match id {
        PICK_OPEN => Command::OpenSelected,
        PICK_NEW_TABS => Command::OpenInNewTabs,
        PICK_OPEN_WITH => Command::OpenWith,
        PICK_CUT => Command::CutFiles,
        PICK_COPY => Command::CopyFiles,
        PICK_PASTE_INTO => match selected_folder(app) {
            Some(f) => Command::PasteInto(f),
            None => return Effect::None,
        },
        PICK_COPY_PATH => Command::CopyPath,
        PICK_RENAME => Command::BeginRename,
        PICK_DELETE => Command::Delete { permanent: false },
        PICK_DELETE_FOREVER => Command::Delete { permanent: true },
        PICK_TERMINAL => match selected_folder(app) {
            Some(f) => Command::OpenTerminal(f),
            None => return Effect::None,
        },
        PICK_TERMINAL_HERE => Command::OpenTerminal(app.tab().path.clone()),
        PICK_EXPLORER => Command::ShowInExplorer,
        PICK_NEW_WINDOW => Command::NewWindow,
        PICK_PROPERTIES => Command::Properties,
        PICK_REFRESH => Command::Refresh,
        PICK_PASTE => Command::Paste,
        PICK_NEW_FOLDER => Command::NewFolder,
        PICK_NEW_TEXT => Command::NewTextFile,
        PICK_HIDDEN => Command::ToggleHidden,
        PICK_SELECT_ALL => {
            let t = app.tab_mut();
            if !t.waiting_long() {
                t.select_all();
            }
            return Effect::Redraw;
        }
        n if (PICK_VIEW..PICK_VIEW + ViewMode::ALL.len() as u32).contains(&n) => Command::SetView(ViewMode::ALL[(n - PICK_VIEW) as usize].0),
        PICK_SORT_NAME => Command::SetSort(SortKey::Name, app.tab().descending),
        PICK_SORT_DATE => Command::SetSort(SortKey::Date, app.tab().descending),
        PICK_SORT_CREATED => Command::SetSort(SortKey::Created, app.tab().descending),
        PICK_LOCATION => Command::OpenFileLocation,
        PICK_SORT_TYPE => Command::SetSort(SortKey::Type, app.tab().descending),
        PICK_SORT_SIZE => Command::SetSort(SortKey::Size, app.tab().descending),
        PICK_ASCENDING => Command::SetSort(app.tab().sort, false),
        PICK_DESCENDING => Command::SetSort(app.tab().sort, true),
        PICK_PIN => match selected_folder(app) {
            Some(f) => Command::TogglePin(f),
            None => return Effect::None,
        },
        PICK_PIN_HERE => Command::TogglePin(app.tab().path.clone()),
        // The Recycle Bin's (app/bin.rs). Only when it's what the tab shows.
        PICK_RESTORE..=PICK_BIN_EXPLORER if !app.bin_on_screen() => return Effect::None,
        PICK_RESTORE => return bin(app, AppState::bin_restore_selected),
        PICK_DELETE_FROM_BIN => return bin(app, AppState::bin_delete_selected),
        PICK_LOOK => Command::OpenSelected,
        PICK_GO_WHERE_IT_WAS => return bin(app, AppState::bin_go_to_where_it_was),
        PICK_COPY_WHERE_IT_WAS => return bin(app, AppState::bin_copy_where_it_was),
        PICK_RESTORE_ALL => return bin(app, AppState::bin_restore_all),
        PICK_EMPTY_BIN => return bin(app, AppState::bin_empty),
        PICK_BIN_EXPLORER => Command::ShowInExplorer,
        _ => return Effect::None,
    };
    Effect::Command(cmd)
}

/// Runs one of the Recycle Bin's commands.
fn bin(app: &mut AppState, what: fn(&mut AppState)) -> Effect {
    what(app);
    Effect::Redraw
}

/// The one selected item, when it's a folder (or drive): its path.
fn selected_folder(app: &AppState) -> Option<String> {
    let t = app.tab();
    match t.selected_entries().as_slice() {
        [one] if t.folder.entries[*one].is_container() => Some(t.folder.entries[*one].path_in(&t.folder.path)),
        _ => None,
    }
}

/// Whether Windows' command `verb` may be one Glint does itself (see `command_for_verb`).
pub fn is_glint_verb(verb: &str) -> bool {
    GLINT_VERBS.iter().any(|v| v.eq_ignore_ascii_case(verb))
}

/// The verbs `command_for_verb` knows.
const GLINT_VERBS: [&str; 9] = ["rename", "open", "explore", "opennewtab", "cut", "copy", "delete", "paste", "newfolder"];

/// A command picked from Windows' menu that Glint does itself, by its verb, so it behaves the same
/// as Glint's own item: renaming happens in Glint's name box, opening a folder opens it in Glint (not in
/// File Explorer), and cut, copy, paste, delete and New > Folder go through app/files.rs, with
/// Glint's question before deleting, the new items selected afterwards, and the faded cut items.
/// `shift`: Shift was held when it was picked (Delete then deletes for good, as in Explorer).
pub fn command_for_verb(verb: &str, app: &AppState, shift: bool) -> Option<Command> {
    let t = app.tab();
    let chosen = t.selected_entries();
    let all_folders = !chosen.is_empty() && chosen.iter().all(|&i| t.folder.entries[i].is_container());
    // Files Glint can work on: in a real folder, and not drives.
    let files = !t.path.is_empty() && chosen.iter().all(|&i| t.folder.entries[i].kind != EntryKind::Drive);
    match verb.to_ascii_lowercase().as_str() {
        "rename" if chosen.len() == 1 && files => Some(Command::BeginRename),
        "open" | "explore" if all_folders => Some(Command::OpenSelected),
        "opennewtab" if all_folders => Some(Command::OpenInNewTabs),
        "cut" if files && !chosen.is_empty() => Some(Command::CutFiles),
        "copy" if files && !chosen.is_empty() => Some(Command::CopyFiles),
        "delete" if files && !chosen.is_empty() => Some(Command::Delete { permanent: shift }),
        // The empty space's Paste, or "Paste" on one folder (into it).
        "paste" if !t.path.is_empty() && chosen.is_empty() => Some(Command::Paste),
        "paste" if files => selected_folder(app).map(Command::PasteInto),
        "newfolder" if !t.path.is_empty() => Some(Command::NewFolder),
        _ => None,
    }
}

/// A menu label reduced for comparing: no '&', no accelerator text after a tab, no "…", lower case.
/// "&Open with…\tCtrl+O" -> "open with".
pub fn label_key(label: &str) -> String {
    let text = label.split('\t').next().unwrap_or("");
    text.replace("&&", "\u{1}").replace('&', "").replace('\u{1}', "&").replace('\u{2026}', "").replace("...", "").trim().to_lowercase()
}

/// Whether one of Windows' items goes into Glint's menu: not a separator, not something Glint has
/// itself (by verb, or by the same text as one of Glint's items, `glint_keys`), and not empty.
pub fn merge_item(item: &ShellItem, glint_keys: &[String]) -> bool {
    if item.is_separator() {
        return false;
    }
    if !item.verb.is_empty() && SKIP_VERBS.iter().any(|v| v.eq_ignore_ascii_case(&item.verb)) {
        return false;
    }
    // Windows Terminal's "Open in Terminal" ("Åpne i Terminal"...) next to Glint's own "Open in
    // terminal" (which starts the terminal chosen in Settings, Windows Terminal by default).
    if opens_terminal(item) && glint_keys.iter().any(|k| k == GLINT_TERMINAL) {
        return false;
    }
    if item.is_owner_drawn() {
        return true; // no text to compare (and rare)
    }
    let key = label_key(&item.text);
    !key.is_empty() && !glint_keys.contains(&key) && !SKIP_TEXTS.contains(&key.as_str())
}

/// Glint's own "Open in terminal", as `label_key` gives it.
const GLINT_TERMINAL: &str = "open in terminal";

/// Whether one of Windows' items opens a terminal in the folder: Windows Terminal's "Open in
/// Terminal", by its verb or by its text in any language (Windows keeps the name "Terminal" in all
/// of them: "Åpne i Terminal", "Im Terminal öffnen", "Ouvrir dans le Terminal").
fn opens_terminal(item: &ShellItem) -> bool {
    if TERMINAL_VERBS.iter().any(|v| v.eq_ignore_ascii_case(&item.verb)) {
        return true;
    }
    // By its text only when it names no command of its own: an item with another verb ("Open in
    // Terminal as administrator", a terminal's own extras) does something Glint's item doesn't.
    item.verb.is_empty() && label_key(&item.text).split(|c: char| !c.is_alphanumeric()).any(|word| word == "terminal")
}

/// Which of Windows' items repeat an earlier one: the same text with the same command (verb) or
/// the same picture. Some apps add their item twice - PowerToys' "Rename with PowerRename" comes
/// once from its old menu handler and once from its Windows 11 one, and Windows lists both. The
/// repeats are left out (shellmenu.rs), so no item shows twice, in Glint's menu or Windows' own:
/// the later copy, unless the later one has more entries in its submenu.
/// The text always has to match: an app that gives all its items one verb (some do) keeps them
/// all, and so does anything else that only looks alike.
pub fn repeats(items: &[ShellItem]) -> Vec<usize> {
    let same = |a: &ShellItem, b: &ShellItem| {
        if a.is_separator() || b.is_separator() {
            return false;
        }
        let key = label_key(&b.text);
        if key.is_empty() || label_key(&a.text) != key {
            return false;
        }
        let verbs = !b.verb.is_empty() && a.verb.eq_ignore_ascii_case(&b.verb);
        verbs || a.picture == b.picture || a.picture == PICTURE_UNKNOWN || b.picture == PICTURE_UNKNOWN
    };
    // Of two copies, the one whose submenu has more entries stays (an app's Windows 11 copy brings
    // its submenu nearly empty and fills it only when it opens); otherwise the first.
    let mut out: Vec<usize> = Vec::new();
    for i in 0..items.len() {
        let Some(j) = (0..i).find(|&j| !out.contains(&j) && same(&items[j], &items[i])) else { continue };
        let drop = if items[j].submenu != 0 && items[i].sub_items > items[j].sub_items { j } else { i };
        out.push(drop);
    }
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_menus_are_only_for_items_of_one_folder() {
        let paths = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(one_folder(&paths(&["C:\\a\\1.txt", "c:\\A\\2.txt"])));
        // Search results from two folders: Windows would look for the second in the first's folder.
        assert!(!one_folder(&paths(&["C:\\a\\notes.txt", "C:\\b\\notes.txt"])));
        assert!(!one_folder(&[]));
    }

    #[test]
    fn sort_by_checks_nothing_while_the_custom_order_sorts() {
        let checked = |custom_sort: bool, recipe: Option<&str>| {
            let settings = crate::settings::Settings { custom_sort, sort_recipe: recipe.map(str::to_string), ..Default::default() };
            let mut app = AppState::for_tests(settings);
            app.tabs[0].folder.path = "C:\\t".into();
            app.tabs[0].custom = app.custom_order();
            let Effect::FilesMenu(m) = for_background(&app, Point::new(0.0, 0.0), How::default()) else { panic!("no menu") };
            let sort = m.menu.items.into_iter().find(|i| i.label == "Sort by").expect("Sort by").submenu;
            sort.into_iter().filter(|i| i.checked).map(|i| i.label).collect::<Vec<_>>()
        };
        assert_eq!(checked(false, None), ["Name", "Ascending"]);
        assert!(checked(true, None).is_empty(), "the custom order decides, not a column");
        assert!(checked(true, Some("in C:\\t (\n-newest_first\n)")).is_empty(), "its rules for this folder decide");
        // Rules only for another folder: this one sorts by its column.
        assert_eq!(checked(true, Some("in C:\\elsewhere (\n-newest_first\n)")), ["Name", "Ascending"]);
    }

    #[test]
    fn labels_compare_without_decoration() {
        assert_eq!(label_key("&Open with\u{2026}\tCtrl+O"), "open with");
        assert_eq!(label_key("Open in &Terminal"), "open in terminal");
        assert_eq!(label_key("Fish && Chips..."), "fish & chips");
        assert_eq!(label_key("  Send to "), "send to");
        assert_eq!(label_key(""), "");
    }

    #[test]
    fn windows_items_glint_has_are_left_out() {
        let glint: Vec<String> = ["Open\tEnter", "Open with\u{2026}", "Open in terminal", "Copy path\tCtrl+Shift+C"].iter().map(|l| label_key(l)).collect();
        let item = |text: &str, verb: &str| ShellItem { text: text.into(), verb: verb.into(), ..Default::default() };
        // Same verb as one of Glint's own.
        assert!(!merge_item(&item("&Delete", "delete"), &glint));
        assert!(!merge_item(&item("Copy as path", "CopyAsPath"), &glint));
        // Same text: Windows' "Open with ›" submenu and Windows Terminal's own item.
        assert!(!merge_item(&item("Open wit&h", ""), &glint));
        assert!(!merge_item(&item("Open in Terminal", ""), &glint));
        // File Explorer's view items.
        assert!(!merge_item(&item("&View", ""), &glint));
        assert!(!merge_item(&item("Line up Icons", ""), &glint));
        // Other apps' items stay.
        assert!(merge_item(&item("Open with Code", ""), &glint));
        assert!(merge_item(&item("Send to", ""), &glint));
        assert!(merge_item(&item("Scan with Microsoft Defender...", ""), &glint));
        // Nothing to show.
        assert!(!merge_item(&item("", ""), &glint));
        let sep = ShellItem { ftype: 0x800, ..Default::default() };
        assert!(!merge_item(&sep, &glint));
        // Drawn by the app itself: no text, but kept.
        let drawn = ShellItem { ftype: 0x100, ..Default::default() };
        assert!(merge_item(&drawn, &glint));
    }

    #[test]
    fn windows_terminal_goes_when_glint_has_open_in_terminal() {
        let item = |text: &str, verb: &str| ShellItem { text: text.into(), verb: verb.into(), ..Default::default() };
        let with: Vec<String> = ["Open in terminal\tCtrl+`", "Properties\tAlt+Enter"].iter().map(|l| label_key(l)).collect();
        let without: Vec<String> = ["Open\tEnter", "Properties\tAlt+Enter"].iter().map(|l| label_key(l)).collect();
        // In any language (Norwegian, German, English), and by its verb.
        for t in [item("Åpne i &Terminal", ""), item("Im Terminal öffnen", ""), item("Open in Terminal", ""), item("Something", "OpenTerminalHere")] {
            assert!(!merge_item(&t, &with), "{t:?}");
        }
        // Without Glint's own item it stays (a file's menu has no "Open in terminal").
        assert!(merge_item(&item("Åpne i Terminal", ""), &without));
        // Not every word with "terminal" in it, nor a terminal item with a command of its own.
        assert!(merge_item(&item("Open Git Bash here", ""), &with));
        assert!(merge_item(&item("Terminalserver tools", ""), &with));
        assert!(merge_item(&item("Open in Terminal as administrator", "runasterminal"), &with));
        // Windows' own items Glint has: by verb in every language.
        for (text, verb) in [("Klipp ut", "cut"), ("Kopier", "copy"), ("Slett", "delete"), ("Gi nytt navn", "rename"), ("Egenskaper", "properties"), ("Kopier som bane", "copyaspath"), ("Fest til Hurtigtilgang", "pintohome"), ("Løsne fra Hurtigtilgang", "unpinfromhome")] {
            assert!(!merge_item(&item(text, verb), &without), "{text}");
        }
    }

    #[test]
    fn an_item_windows_lists_twice_shows_once() {
        let item = |text: &str, verb: &str, picture: u64| ShellItem { text: text.into(), verb: verb.into(), picture, ..Default::default() };
        let sep = ShellItem { ftype: 0x800, ..Default::default() };
        let items = vec![
            item("Rename with PowerRename", "", 77),
            item("Open with Code", "", 5),
            sep.clone(),
            sep.clone(),
            item("Rename with &PowerRename", "", 77), // the same text and picture: a repeat
            item("Scan with Microsoft Defender...", "", PICTURE_UNKNOWN),
            item("Scan with Microsoft Defender…", "", 9), // a picture that can't be compared counts as the same
            item("Send to", "", 0),
            item("Send to", "", 3), // the same name, another picture: another item
            item("Share", "Windows.ModernShare", 4),
            item("&Share", "windows.modernshare", 5), // the same text and command, another picture
            item("Scan with Tool", "tool", 8),
            item("Shred with Tool", "tool", 8), // one verb for all of an app's items: both stay
        ];
        assert_eq!(repeats(&items), vec![4, 6, 10]);
        assert!(repeats(&[sep.clone(), sep]).is_empty(), "lines are tidied, not counted as repeats");
        // An archiver listed twice: its Windows 11 copy first, with a submenu still empty, then its
        // classic copy with the entries. The classic one stays.
        let sub = |text: &str, picture: u64, sub_items: u32| ShellItem { text: text.into(), picture, submenu: 1, sub_items, ..Default::default() };
        assert_eq!(repeats(&[sub("WinRAR", 6, 0), sub("WinRAR", 6, 9)]), vec![0]);
        assert_eq!(repeats(&[sub("WinRAR", 6, 9), sub("WinRAR", 6, 0)]), vec![1]);
        // A placeholder that stays put doesn't make a copy win.
        assert_eq!(repeats(&[sub("WinRAR", 6, 1), sub("WinRAR", 6, 2)]), vec![0]);
        assert_eq!(repeats(&[sub("WinRAR", 6, 2), sub("WinRAR", 6, 2)]), vec![1]);
        assert_eq!(repeats(&[sub("Send to", 0, 0), sub("Send to", 0, 0)]), vec![1], "both empty: the first stays");
    }
}
