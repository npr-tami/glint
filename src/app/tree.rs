//! The tree view: the list as in Details, but a folder opens in
//! place under its ▸ arrow (a click on the arrow, or the Right key) with its items indented below
//! it, and closes again (▾, the Left key). list.rs draws it; this is the state's side.
//!
//! - **Where the items are.** A folder opened in the tree is read on a background thread like any
//!   folder (`folder::read`). Its items then join the tab's listing after the folder's own, each
//!   with its full path as `target` - as search results do, so everything that works on files
//!   (opening, copy, delete, rename, dragging, the menus, Properties) already acts on the very file.
//!   In a real folder only those items have a `target`, which is how the folder's own are told
//!   apart (`own`).
//! - **Order.** `Tab::rebuild_view` sorts everything as usual (by the column, folders first or not,
//!   the filter), then `nest` puts each open folder's items right under it: sorted per level, and
//!   hidden while their folder is (closed, filtered out).
//! - **Selection** is kept by `Entry::key` (the full path, for items of open folders) whenever the
//!   listing is put together again (`regraft`).
//! - **Fresh.** The open folders of the tab on screen are watched (watch.rs, the MAX_WATCHED most
//!   recently opened) and read again when they change; F5 and Glint's own file jobs read them again
//!   with the folder. A folder that went away (deleted, renamed) closes.
//! - **Where.** Only in a real folder's own listing: This PC, the Recycle Bin and search results
//!   show as Details. The open folders close when another folder comes on screen, when a search
//!   starts and when the tab changes to another view.

use std::collections::{HashMap, HashSet};

use super::{wake, AppState, Loaded, Tab};
use crate::folder::{self, Entry, Folder};
use crate::recycle;
use crate::settings::ViewMode;
use crate::shell::{self, ComGuard};
use crate::watch::{self, Armed, Event, Watcher};

/// At most this many open folders of the tab on screen are watched: the most recently opened.
const MAX_WATCHED: usize = 16;
/// At most this many folders can be open at once in one tab.
const MAX_OPEN: usize = 256;

/// A folder open in the tree.
#[derive(Debug)]
struct Branch {
    /// Its full path, as on disk.
    path: String,
    /// The same as `key` makes it: how it's found.
    key: String,
    /// Its items once read, each with its full path as `target` (None while the first read is on
    /// its way).
    items: Option<Vec<Entry>>,
    /// The read under way (its generation).
    reading: Option<u64>,
    /// It changed while it was being read: read it once more when that's done.
    stale: bool,
}

/// The folders open in a tab's tree view, in the order they were opened.
#[derive(Debug, Default)]
pub struct Tree {
    open: Vec<Branch>,
}

impl Tree {
    /// Whether any folder is open.
    pub fn is_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// Closes every folder (their items stay in the listing until it's put together again).
    pub fn clear(&mut self) {
        self.open.clear();
    }

    /// Whether the folder with this full path is open.
    pub fn has(&self, path: &str) -> bool {
        let k = key(path);
        self.open.iter().any(|b| b.key == k)
    }

    /// Whether this folder is open but its items haven't come in yet.
    pub fn waiting(&self, path: &str) -> bool {
        let k = key(path);
        self.open.iter().any(|b| b.key == k && b.items.is_none())
    }
}

/// How a folder's path is compared: lower case, without a trailing backslash ("c:" for C:\).
fn key(path: &str) -> String {
    path.trim_end_matches('\\').to_lowercase()
}

/// Whether an entry of a real folder's listing is the folder's own (not an item of a folder open
/// in the tree, which carries its full path).
fn own(e: &Entry) -> bool {
    e.target.is_none()
}

/// Puts the open folders' items after the folder's own items, again: after a new read of the
/// folder, or when a folder opened, closed or was read again. A folder that isn't in the listing
/// any more (deleted, renamed, inside one that closed) closes too. Selection is the caller's.
pub(super) fn graft_entries(tree: &mut Tree, folder: &mut Folder) {
    if !tree.is_open() {
        return;
    }
    folder.entries.retain(own);
    if folder.error.is_some() {
        tree.clear();
        return;
    }
    // Parents first: a folder's items go in once the folder itself is in the listing.
    let mut present: HashSet<String> = folder.entries.iter().filter(|e| e.is_container()).map(|e| key(&e.path_in(&folder.path))).collect();
    let mut placed = vec![false; tree.open.len()];
    loop {
        let mut more = false;
        for (i, b) in tree.open.iter().enumerate() {
            if placed[i] || !present.contains(&b.key) {
                continue;
            }
            placed[i] = true;
            more = true;
            for e in b.items.iter().flatten() {
                if e.is_container() {
                    present.insert(key(e.target.as_deref().unwrap_or_default()));
                }
                folder.entries.push(e.clone());
            }
        }
        if !more {
            break;
        }
    }
    let mut i = 0;
    tree.open.retain(|_| {
        i += 1;
        placed[i - 1]
    });
}

/// `view` (the shown entries, sorted) in tree order: each open folder's items right after it, and
/// only those whose folder is shown. Depth first, with a stack (folders can nest deep).
pub(super) fn nest(folder: &Folder, tree: &Tree, view: &mut Vec<u32>) {
    let mut under: HashMap<String, Vec<u32>> = HashMap::new();
    for &i in view.iter() {
        let e = &folder.entries[i as usize];
        let parent = match &e.target {
            None => String::new(),
            Some(t) => folder::parent(t).map(|p| key(&p)).unwrap_or_default(),
        };
        under.entry(parent).or_default().push(i);
    }
    let mut out = Vec::with_capacity(view.len());
    let mut stack = vec![under.remove("").unwrap_or_default().into_iter()];
    while let Some(level) = stack.last_mut() {
        let Some(i) = level.next() else {
            stack.pop();
            continue;
        };
        out.push(i);
        let e = &folder.entries[i as usize];
        if e.is_container() {
            let k = key(&e.path_in(&folder.path));
            if tree.open.iter().any(|b| b.key == k) {
                if let Some(items) = under.remove(&k) {
                    stack.push(items.into_iter());
                }
            }
        }
    }
    *view = out;
}

/// Whether a new read of a folder lists the same as the folder's own items in `a` (the items of
/// its open folders left aside).
pub(super) fn same_own_listing(a: &Folder, b: &Folder) -> bool {
    a.error == b.error && a.entries.iter().filter(|e| own(e)).count() == b.entries.len() && a.entries.iter().filter(|e| own(e)).zip(&b.entries).all(|(x, y)| super::same_entry(x, y))
}

/// Puts the listing together again (`graft_entries`), keeping the selection, the cursor and the
/// Shift-anchor on the same items (by `Tab::item_key`). Items of folders that closed are gone,
/// and a cursor that was in one goes to the folder.
fn regraft(t: &mut Tab, closed: Option<&str>) {
    let selected: HashSet<String> = t.selected_entries().into_iter().map(|i| t.item_key(i)).collect();
    let focus = t.focus.map(|i| t.item_key(i));
    let anchor = t.anchor.map(|i| t.item_key(i));
    if t.tree.is_open() {
        graft_entries(&mut t.tree, &mut t.folder);
    } else {
        t.folder.entries.retain(own);
    }
    t.selected = vec![false; t.folder.entries.len()];
    t.rebuild_view();
    let index: HashMap<String, usize> = (0..t.folder.entries.len()).map(|i| (t.item_key(i), i)).collect();
    for k in &selected {
        if let Some(&i) = index.get(k).filter(|&&i| t.row_of(i).is_some()) {
            t.selected[i] = true;
        }
    }
    // A cursor inside a folder that closed goes to the folder itself.
    let inside = |k: &str| closed.is_some_and(|c| k.starts_with(&format!("{}\\", key(c))));
    let folder_key = closed.map(|c| t.item_key_of_path(c));
    let place = |k: Option<String>| k.map(|k| if inside(&k) { folder_key.clone().unwrap_or(k) } else { k }).and_then(|k| index.get(&k).copied());
    t.focus = place(focus);
    t.anchor = place(anchor).or(t.focus);
    if let Some(f) = t.focus.filter(|&f| closed.is_some() && !t.is_selected(f) && t.selected_entries().is_empty()) {
        t.selected[f] = true;
    }
}

/// Closes every folder open in the tab's tree: only the folder's own items stay.
pub(super) fn close_all(t: &mut Tab) {
    if t.tree.is_open() {
        t.tree.clear();
        regraft(t, None);
    }
}

impl Tab {
    /// Whether the tree view can open folders here: the tab is in the tree view and shows a real
    /// folder's own listing (not This PC, the Recycle Bin, search results or an error).
    pub fn tree_here(&self) -> bool {
        self.view_mode == ViewMode::Tree
            && !self.path.is_empty()
            && self.folder.path == self.path
            && !self.is_searching()
            && !recycle::is_bin(&self.path)
            && self.folder.error.is_none()
    }

    /// How deep entry `index` sits in the tree: 0 for the folder's own items, 1 for the items of
    /// a folder open under one of them, and so on.
    pub fn depth(&self, index: usize) -> usize {
        if !self.tree.is_open() {
            return 0;
        }
        let Some(t) = self.folder.entries.get(index).and_then(|e| e.target.as_deref()) else { return 0 };
        let root = self.folder.path.trim_end_matches('\\');
        match t.get(..root.len()) {
            Some(head) if head.eq_ignore_ascii_case(root) => t[root.len()..].trim_start_matches('\\').matches('\\').count(),
            _ => 0,
        }
    }

    /// The view the view rules give a folder, if any. (The Recycle Bin and This PC have none.)
    fn ruled_view(&self, folder: &str) -> Option<ViewMode> {
        self.views.as_ref().filter(|_| !recycle::is_bin(folder)).and_then(|r| r.view_for(folder))
    }

    /// The view for a folder shown in this tab: the one the view rules give it, else the tab's own.
    pub fn view_for(&self, folder: &str) -> ViewMode {
        self.ruled_view(folder).unwrap_or(self.own_view)
    }

    /// Whether the view rules give the folder on screen a view of its own.
    pub fn view_ruled(&self) -> bool {
        self.ruled_view(&self.folder.path).is_some()
    }

    /// The tab's own view, shown now too (a new or restored tab, before its folder is read).
    pub fn set_own_view(&mut self, mode: ViewMode) {
        self.own_view = mode;
        self.view_mode = mode;
    }

    /// Whether entry `index` is a folder open in the tree.
    pub fn is_expanded(&self, index: usize) -> bool {
        self.tree.is_open() && self.folder.entries.get(index).is_some_and(|e| e.is_container() && self.tree.has(&e.path_in(&self.folder.path)))
    }

    /// The folder entry `index` is in, when that's a folder open in the tree (for the Left key).
    pub fn tree_parent(&self, index: usize) -> Option<usize> {
        let e = self.folder.entries.get(index)?;
        let parent = key(&folder::parent(e.target.as_deref()?)?);
        self.folder.entries.iter().position(|f| f.is_container() && key(&f.path_in(&self.folder.path)) == parent)
    }

    /// `Tab::item_key` of the item at `path` in this listing: its name for the folder's own items,
    /// its full path for the others.
    fn item_key_of_path(&self, path: &str) -> String {
        match folder::parent(path) {
            Some(p) if key(&p) == key(&self.folder.path) => folder::file_name(path).to_lowercase(),
            _ => path.to_lowercase(),
        }
    }
}

/// The watch of a folder open in the tree view of the tab on screen.
pub(super) struct BranchWatch {
    tab_id: u64,
    key: String,
    watcher: Watcher,
    /// Set once the watch is under way (the first read waits for it).
    armed: Armed,
}

impl BranchWatch {
    /// WM_DEVICECHANGE: let go of a drive that is being ejected (see watch.rs).
    pub(super) fn device_event(&self, event: u32, data: isize) {
        self.watcher.device_event(event, data);
    }
}

impl AppState {
    /// The VIEW button, Ctrl+Shift+1 to 6, right-click > View: the tab on screen shows its items
    /// this way (new tabs opened from it too). Leaving the tree view closes its open folders.
    ///
    /// In a folder the view rules give a view (-large_icons in its "in … (" rules), the pick is just
    /// for now: the rules' view comes back when the folder is opened again. Elsewhere it's the
    /// tab's own view, for every folder the rules say nothing about.
    pub fn set_view(&mut self, mode: ViewMode) {
        let t = self.tab_mut();
        if t.view_mode == mode {
            return;
        }
        change_view(t, mode);
        let ruled = t.view_ruled();
        if !ruled {
            t.own_view = mode;
        }
        let n = ViewMode::ALL.iter().position(|(v, _)| *v == mode).unwrap_or(0) + 1;
        if ruled {
            self.say(format!("{} (Ctrl+Shift+{n}) for now: the view rules give this folder its own view", mode.name()));
        } else {
            self.say(format!("{} (Ctrl+Shift+{n})", mode.name()));
        }
    }

    /// The Settings window's "View": every open tab shows its items this way (and new windows and
    /// tabs without a view of their own do, from `settings.view`).
    /// Folders the view rules give a view keep it.
    pub fn set_view_everywhere(&mut self, mode: ViewMode) {
        self.settings.view = mode;
        for t in &mut self.tabs {
            t.own_view = mode;
            follow_view_rules(t);
        }
        self.sync_branch_watch();
    }

    /// Opens or closes the folder at entry `index` of the tab on screen in the tree view (a click
    /// on its arrow). Returns whether something changed.
    pub fn tree_toggle(&mut self, index: usize) -> bool {
        if self.tab().is_expanded(index) {
            self.tree_close(index)
        } else {
            self.tree_open(index)
        }
    }

    /// Opens the folder at entry `index` in the tree view: it's read in the background, and its
    /// items appear under it when they're in.
    pub fn tree_open(&mut self, index: usize) -> bool {
        let i = self.active;
        let t = &mut self.tabs[i];
        if !t.tree_here() || t.waiting_long() {
            return false;
        }
        let Some(e) = t.folder.entries.get(index).filter(|e| e.is_container()) else { return false };
        let path = e.path_in(&t.folder.path);
        if t.tree.has(&path) {
            return false;
        }
        if t.tree.open.len() >= MAX_OPEN {
            self.say("Too many open folders. Close some first (Left key).");
            return false;
        }
        let k = key(&path);
        t.tree.open.push(Branch { path, key: k.clone(), items: None, reading: None, stale: false });
        // Watched before it's read, so nothing that changes in between is missed (as in start_load).
        self.sync_branch_watch();
        let armed = self.branch_watch.iter().find(|w| w.key == k).map(|w| w.armed.clone()).unwrap_or_default();
        self.read_branch(i, &k, armed);
        true
    }

    /// Closes the folder at entry `index` in the tree view, and every folder open inside it.
    pub fn tree_close(&mut self, index: usize) -> bool {
        let t = self.tab_mut();
        let Some(path) = t.folder.entries.get(index).filter(|e| e.is_container()).map(|e| e.path_in(&t.folder.path)) else { return false };
        let k = key(&path);
        let below = format!("{k}\\");
        let before = t.tree.open.len();
        t.tree.open.retain(|b| b.key != k && !b.key.starts_with(&below));
        if t.tree.open.len() == before {
            return false;
        }
        regraft(t, Some(&path));
        self.sync_branch_watch();
        true
    }

    /// Reads a folder open in tab `i`'s tree view (again) on a background thread; it arrives as
    /// `Loaded::Branch` (`branch_read`). One read at a time per folder: a change meanwhile reads it
    /// once more afterwards.
    fn read_branch(&mut self, i: usize, k: &str, armed: Armed) {
        let generation = self.next_generation;
        self.next_generation += 1;
        let (tx, hwnd) = (self.tx.clone(), self.hwnd.clone());
        let Some(t) = self.tabs.get_mut(i) else { return };
        let tab_id = t.id;
        let Some(b) = t.tree.open.iter_mut().find(|b| b.key == k) else { return };
        if b.reading.is_some() {
            b.stale = true;
            return;
        }
        b.reading = Some(generation);
        let (path, k) = (b.path.clone(), k.to_string());
        let started = shell::background("read folder (tree)", move || {
            let _com = ComGuard::init();
            armed.wait();
            let folder = folder::read(&path);
            if tx.send(Loaded::Branch { tab_id, key: k, generation, folder }).is_ok() {
                wake(&hwnd);
            }
        });
        if !started {
            // No thread to be had: the folder closes again (it can be opened again).
            b.reading = None;
            if b.items.is_none() {
                let k = b.key.clone();
                t.tree.open.retain(|b| b.key != k);
                self.say("Windows couldn't start reading that folder right now. Try again.");
            }
        }
    }

    /// F5, or a file job changed something: the folders open in tab `i`'s tree are read again too
    /// (with the folder itself; not when another folder is on its way).
    pub(super) fn read_branches_again(&mut self, i: usize) {
        let Some(t) = self.tabs.get(i).filter(|t| t.tree.is_open() && t.path == t.folder.path) else { return };
        let keys: Vec<String> = t.tree.open.iter().map(|b| b.key.clone()).collect();
        for k in keys {
            self.read_branch(i, &k, Armed::none());
        }
    }

    /// A folder open in a tab's tree was read: its items join the listing (or, if it can't be
    /// read, it closes with a note). Returns whether the list changed.
    pub(super) fn branch_read(&mut self, tab_id: u64, k: &str, generation: u64, read: Folder) -> bool {
        let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) else { return false };
        let t = &mut self.tabs[i];
        let Some(b) = t.tree.open.iter_mut().find(|b| b.key == k && b.reading == Some(generation)) else { return false };
        b.reading = None;
        if let Some(err) = read.error {
            let name = folder::file_name(&b.path).to_string();
            let path = b.path.clone();
            // (Said only when it was just opened: one that went away while open simply closes.)
            let just_opened = b.items.is_none();
            let below = format!("{k}\\");
            t.tree.open.retain(|b| b.key != k && !b.key.starts_with(&below));
            regraft(t, Some(&path));
            if just_opened && i == self.active {
                self.say(format!("\u{201c}{name}\u{201d} can't be opened here: {err}"));
            }
            self.sync_branch_watch();
            return true;
        }
        let dir = b.path.clone();
        let items: Vec<Entry> = read
            .entries
            .into_iter()
            .map(|mut e| {
                e.target = Some(folder::join(&dir, &e.name));
                e
            })
            .collect();
        // Nothing changed (a watch's re-read after a change elsewhere in it): nothing to do.
        let same = b.items.as_ref().is_some_and(|old| old.len() == items.len() && old.iter().zip(&items).all(|(x, y)| super::same_entry(x, y)));
        b.items = Some(items);
        let again = std::mem::take(&mut b.stale);
        if !same {
            regraft(t, None);
        }
        // What a file job made in it (a rename, a paste) is selected once it's in.
        super::files::select_pending(t);
        if again {
            self.read_branch(i, k, Armed::none());
        }
        !same
    }

    /// Keeps a watch on each folder open in the tree of the tab on screen (the most recently
    /// opened ones), and none on anything else - nothing at all while Glint is in the background.
    pub(super) fn sync_branch_watch(&mut self) {
        let t = &self.tabs[self.active.min(self.tabs.len() - 1)];
        let wanted: Vec<(String, String)> = if self.hidden || !t.tree_here() {
            Vec::new()
        } else {
            t.tree.open.iter().rev().filter(|b| watch::can_watch(&b.path)).take(MAX_WATCHED).map(|b| (b.key.clone(), b.path.clone())).collect()
        };
        let tab_id = t.id;
        // (A watch stops when it's dropped.)
        self.branch_watch.retain(|w| w.tab_id == tab_id && wanted.iter().any(|(k, _)| *k == w.key));
        let mut fresh = Vec::new();
        for (k, path) in wanted {
            if self.branch_watch.iter().any(|w| w.key == k) {
                continue;
            }
            let mut watcher = Watcher::new(self.hwnd.clone());
            let armed = watcher.watch(tab_id, &path);
            fresh.push((k.clone(), armed.clone()));
            self.branch_watch.push(BranchWatch { tab_id, key: k, watcher, armed });
        }
        // A folder that wasn't watched until now (its tab was in the background, Glint was hidden)
        // may have changed meanwhile: read it again, once its watch is under way. (One just opened
        // is being read already.)
        let i = self.active.min(self.tabs.len() - 1);
        for (k, armed) in fresh {
            if self.tabs[i].tree.open.iter().any(|b| b.key == k && b.items.is_some() && b.reading.is_none()) {
                self.read_branch(i, &k, armed);
            }
        }
    }

    /// What the open folders' watches saw: each folder that changed is read again.
    pub(super) fn branch_news(&mut self) {
        let mut changed: Vec<(u64, String)> = Vec::new();
        for w in &mut self.branch_watch {
            if w.watcher.take().iter().any(|(_, e)| matches!(e, Event::Changed { .. })) {
                changed.push((w.tab_id, w.key.clone()));
            }
        }
        for (tab_id, k) in changed {
            if let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) {
                self.read_branch(i, &k, Armed::none());
            }
        }
    }
}

/// Shows the tab's folder in the view it should have now (the rules', or the tab's own). Returns
/// whether that changed it.
pub(super) fn follow_view_rules(t: &mut Tab) -> bool {
    let want = t.view_for(&t.folder.path);
    if t.view_mode == want {
        return false;
    }
    change_view(t, want);
    true
}

/// A tab changes its view: the scroll starts at the top (it means something else in another
/// view), the cursor is shown, and leaving the tree view closes its open folders.
fn change_view(t: &mut Tab, mode: ViewMode) {
    if t.view_mode == ViewMode::Tree {
        close_all(t);
    }
    t.view_mode = mode;
    t.scroll = 0.0;
    t.lines = None;
    t.reveal_focus = true;
}

#[cfg(test)]
mod tests {
    use super::super::{apply_listing, merge_listing};
    use super::*;
    use crate::folder::EntryKind;
    use crate::theme::size;
    use std::sync::Arc;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind,
            size: 1,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: if kind == EntryKind::File { folder::extension(name) } else { String::new() },
            type_name: Arc::from("File"),
            target: None,
            free: 0,
        }
    }

    fn listing(path: &str, names: &[&str]) -> Folder {
        let entries = names.iter().map(|n| if let Some(d) = n.strip_suffix('/') { entry(d, EntryKind::Folder) } else { entry(n, EntryKind::File) }).collect();
        Folder { path: path.into(), entries, ..Default::default() }
    }

    fn tab() -> Tab {
        let mut t = Tab::new(1, "C:\\x".into());
        t.set_own_view(ViewMode::Tree);
        apply_listing(&mut t, listing("C:\\x", &["b.txt", "src/", "a.txt", "docs/"]));
        t
    }

    /// What the list shows, indented two spaces per level.
    fn shown(t: &Tab) -> Vec<String> {
        (0..t.view.len()).map(|r| t.index_at(r).unwrap()).map(|i| format!("{}{}", "  ".repeat(t.depth(i)), t.folder.entries[i].name)).collect()
    }

    /// Opens the folder `name` (top level) the way `tree_open` does, with its items already read.
    fn open(t: &mut Tab, path: &str, names: &[&str]) {
        let read = listing(path, names);
        let items = read.entries.into_iter().map(|mut e| {
            e.target = Some(folder::join(path, &e.name));
            e
        });
        t.tree.open.push(Branch { path: path.into(), key: key(path), items: Some(items.collect()), reading: None, stale: false });
        regraft(t, None);
    }

    #[test]
    fn open_folders_show_their_items_under_them_sorted_per_level() {
        let mut t = tab();
        assert!(t.tree_here());
        assert_eq!(shown(&t), ["docs", "src", "a.txt", "b.txt"]);
        open(&mut t, "C:\\x\\src", &["main.rs", "lib/", "build.rs"]);
        assert_eq!(shown(&t), ["docs", "src", "  lib", "  build.rs", "  main.rs", "a.txt", "b.txt"]);
        open(&mut t, "C:\\x\\src\\lib", &["z.rs", "a.rs"]);
        assert_eq!(shown(&t), ["docs", "src", "  lib", "    a.rs", "    z.rs", "  build.rs", "  main.rs", "a.txt", "b.txt"]);
        // Items of open folders act on their own file: their full path.
        let main = t.folder.entries.iter().position(|e| e.name == "main.rs").unwrap();
        assert_eq!(t.folder.entries[main].path_in(&t.folder.path), "C:\\x\\src\\main.rs");
        assert_eq!(t.item_key(main), "c:\\x\\src\\main.rs", "told apart from a main.rs in the folder itself");
        let lib = t.folder.entries.iter().position(|e| e.name == "lib").unwrap();
        assert!(t.is_expanded(lib));
        assert_eq!(t.tree_parent(main).map(|i| t.folder.entries[i].name.as_str()), Some("src"));
        // Sorting newest first or Z to A keeps every level under its folder.
        t.set_sort(crate::folder::SortKey::Name, true);
        assert_eq!(shown(&t), ["src", "  lib", "    z.rs", "    a.rs", "  main.rs", "  build.rs", "docs", "b.txt", "a.txt"]);
    }

    #[test]
    fn the_filter_and_closing_keep_things_tidy() {
        let mut t = tab();
        open(&mut t, "C:\\x\\src", &["main.rs", "notes.txt"]);
        t.filter = "txt".into();
        t.rebuild_view();
        assert_eq!(shown(&t), ["a.txt", "b.txt"], "items of a folder the filter hides are hidden with it");
        t.filter.clear();
        t.rebuild_view();
        // The cursor on an item inside a folder that closes goes to the folder.
        let main = t.folder.entries.iter().position(|e| e.name == "main.rs").unwrap();
        let row = t.row_of(main).unwrap();
        t.select_only_row(row);
        let src = t.folder.entries.iter().position(|e| e.name == "src").unwrap();
        t.tree.open.clear();
        regraft(&mut t, Some("C:\\x\\src"));
        assert_eq!(shown(&t), ["docs", "src", "a.txt", "b.txt"]);
        assert_eq!(t.focus, Some(src));
        assert!(t.is_selected(src));
    }

    #[test]
    fn a_new_read_of_the_folder_keeps_the_open_folders() {
        let mut t = tab();
        open(&mut t, "C:\\x\\src", &["main.rs"]);
        let main = t.folder.entries.iter().position(|e| e.name == "main.rs").unwrap();
        t.select_only_row(t.row_of(main).unwrap());
        // The folder changed (a file came): merged, the open folder still open, the item still selected.
        assert!(merge_listing(&mut t, listing("C:\\x", &["b.txt", "src/", "a.txt", "docs/", "c.txt"]), &[], size::ROW_H));
        assert_eq!(shown(&t), ["docs", "src", "  main.rs", "a.txt", "b.txt", "c.txt"]);
        assert_eq!(t.selected_entries().iter().map(|&i| t.folder.entries[i].name.as_str()).collect::<Vec<_>>(), ["main.rs"]);
        // The same read again changes nothing, though the listing holds more than the folder's own.
        assert!(!merge_listing(&mut t, listing("C:\\x", &["b.txt", "src/", "a.txt", "docs/", "c.txt"]), &[], size::ROW_H));
        // The open folder was deleted: it closes and its items go.
        assert!(merge_listing(&mut t, listing("C:\\x", &["b.txt", "a.txt"]), &[], size::ROW_H));
        assert_eq!(shown(&t), ["a.txt", "b.txt"]);
        assert!(!t.tree.is_open());
        // Another folder: nothing is open there.
        open(&mut t, "C:\\x\\src", &["main.rs"]);
        apply_listing(&mut t, listing("C:\\y", &["src/"]));
        assert!(!t.tree.is_open());
    }

    #[test]
    fn leaving_the_tree_view_closes_its_folders() {
        let mut t = tab();
        open(&mut t, "C:\\x\\docs", &["readme.md"]);
        assert_eq!(t.folder.entries.len(), 5);
        change_view(&mut t, ViewMode::Large);
        assert_eq!(t.folder.entries.len(), 4, "only the folder's own items stay");
        assert!(!t.tree.is_open() && t.view_mode == ViewMode::Large);
        assert!(!t.tree_here(), "folders open only in the tree view");
    }
}
