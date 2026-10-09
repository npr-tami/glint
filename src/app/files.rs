//! The file commands of the app state: copy, cut, paste, delete, rename, new folder and new text
//! file (Ctrl+C / Ctrl+X / Ctrl+V, Del, Shift+Del, F2, Ctrl+Shift+N, and the right-click menu).
//!
//! The work itself is done by fileops.rs on background threads. This part decides what to do -
//! nothing in This PC, a question first before deleting for good - and when a job is done, reads
//! the changed folders again with the new or renamed items selected (and a new folder's name box
//! open). The folder watch (watch.rs) sees the same changes and reads the folder again too; that
//! read is merged in by name, so it keeps what was selected here.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;

use super::{wake, AppState, Loaded, Tab};
use crate::fileops::{self, check_name, dotted_extension, items_text, same_path, Job, NameCheck, Outcome};
use crate::folder::{file_name, join, parent, EntryKind};
use crate::recycle;
use crate::ui::MenuAbout;

/// The status-bar note for file commands in This PC.
pub const THIS_PC_NOTE: &str = "This PC only holds drives. Open a drive or folder first.";

/// How long a name box waits for its item to turn up in the list (a new folder being read).
const RENAME_WAIT: Duration = Duration::from_secs(3);
/// How long Glint waits for the item Windows' "New ›" makes, to open its name box.
const APPEAR_WAIT: Duration = Duration::from_secs(3);

/// A question for the person before something that can't easily be undone. ask.rs shows it; the
/// answer comes back through [`AppState::answer`].
pub struct Question {
    /// Tells questions apart (a new one starts with its yes button in focus).
    pub id: u64,
    pub title: String,
    pub text: String,
    /// The label of the yes button ("DELETE").
    pub yes: &'static str,
    job: Job,
    after: AfterJob,
}

/// What the tab that started a job wants once it's done.
#[derive(Clone, Debug)]
pub(super) struct AfterJob {
    tab_id: u64,
    /// After a delete: the item the selection moves to.
    select_next: Option<String>,
    /// After a rename: the old name.
    renamed_from: Option<String>,
}

impl AfterJob {
    pub(super) fn new(tab_id: u64) -> AfterJob {
        AfterJob { tab_id, select_next: None, renamed_from: None }
    }
}

/// The items Glint cut to the clipboard, drawn faded while they're still there (as Explorer does).
pub(super) struct CutMark {
    /// The clipboard's change counter right after they were put there.
    seq: u32,
    folder: String,
    /// Their names in lower case.
    names: HashSet<String>,
}

/// Items to select when a tab's next listing of `dir` arrives (after a paste, rename or delete).
#[derive(Clone, Debug)]
pub(super) struct PendingSelect {
    dir: String,
    names: Vec<String>,
    /// Select only these (otherwise they are added to what stays selected).
    exclusive: bool,
}

/// Windows' own "New ›" (in Windows' items of the empty-space menu) is making something in `dir`:
/// the names that were there before, to find the one new item.
pub(super) struct Appear {
    dir: String,
    before: HashSet<String>,
    until: Instant,
}

/// Open the name box on an item of the tab's folder `dir`: a new folder, or a rename to try again
/// with what was typed (`text`). The file list does it on its next frame (list.rs).
#[derive(Clone, Debug)]
pub struct RenameRequest {
    pub dir: String,
    pub name: String,
    pub text: Option<String>,
    made: Instant,
}

impl RenameRequest {
    fn new(dir: &str, name: &str, text: Option<&str>) -> RenameRequest {
        RenameRequest { dir: dir.to_string(), name: name.to_string(), text: text.map(str::to_string), made: Instant::now() }
    }

    /// Too old to act on: the item never showed up.
    pub fn expired(&self) -> bool {
        self.made.elapsed() > RENAME_WAIT
    }
}

impl AppState {
    /// The selected items' full paths, to work on. Never the folder itself, never drives.
    fn chosen(&self) -> Vec<String> {
        let t = self.tab();
        t.selected_entries()
            .into_iter()
            .map(|i| &t.folder.entries[i])
            .filter(|e| e.kind != EntryKind::Drive)
            .map(|e| e.path_in(&t.folder.path))
            .collect()
    }

    /// File commands work in real folders only: This PC holds drives, and the Recycle Bin's rows are
    /// deleted items (app/bin.rs deals with those). Nothing happens either while the list says
    /// "Loading…" (the rows on screen aren't the folder's).
    fn files_here(&mut self) -> bool {
        if self.tab().path.is_empty() {
            self.say(THIS_PC_NOTE);
            return false;
        }
        if recycle::is_bin(&self.tab().path) || recycle::is_bin(&self.tab().folder.path) {
            return false;
        }
        !self.tab().waiting_long()
    }

    /// Ctrl+C / Ctrl+X: the selected items go on the clipboard as files, to be pasted here, in
    /// another folder or in File Explorer.
    pub(super) fn copy_files(&mut self, cut: bool) {
        if !self.files_here() {
            return;
        }
        let paths = self.chosen();
        if paths.is_empty() {
            self.say(if cut { "Select what to cut first." } else { "Select what to copy first." });
            return;
        }
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut _);
        let Some(seq) = fileops::put_files(hwnd, &paths, cut) else {
            self.say("The clipboard is busy. Try again.");
            return;
        };
        let t = self.tab();
        // Faded by name in their folder - not search results, whose names can repeat across the
        // folders they come from, nor items of folders open in the tree view (app/tree.rs).
        let own = t.selected_entries().iter().all(|&i| !t.tree.is_open() || t.folder.entries[i].target.is_none());
        self.cut = (cut && !t.is_searching() && own).then(|| CutMark {
            seq,
            folder: t.folder.path.clone(),
            names: t.selected_entries().iter().map(|&i| t.folder.entries[i].lower.clone()).collect(),
        });
        self.say(format!("{} {}", if cut { "Cut" } else { "Copied" }, items_text(&paths)));
    }

    /// Ctrl+V: the files on the clipboard (from Glint or File Explorer) are copied here - or moved,
    /// when they were cut. `into`: a folder in the list instead (right-click > Paste into folder).
    pub(super) fn paste(&mut self, into: Option<String>) {
        if !self.files_here() {
            return;
        }
        if !fileops::clipboard_has_files() {
            self.say("There are no files on the clipboard to paste.");
            return;
        }
        let dest = into.unwrap_or_else(|| self.tab().path.clone());
        let after = AfterJob::new(self.tab().id);
        self.start_job(Job::Paste { dest }, after);
    }

    /// Del (to the Recycle Bin) and Shift+Del (`permanent`), asking first as the "Ask before
    /// deleting" setting says.
    pub(super) fn delete(&mut self, permanent: bool) {
        if !self.files_here() {
            return;
        }
        let paths = self.chosen();
        if paths.is_empty() {
            return;
        }
        let what = items_text(&paths);
        let after = AfterJob { select_next: self.next_after_selection(), ..AfterJob::new(self.tab().id) };
        let job = Job::Delete { paths, recycle: !permanent };
        let ask = self.settings.ask_before_deleting(permanent);
        if !ask {
            self.start_job(job, after);
        } else if permanent {
            self.ask(format!("Delete {what} permanently?"), "This can't be undone.".into(), "DELETE", job, after);
        } else {
            self.ask(format!("Move {what} to the Recycle Bin?"), String::new(), "DELETE", job, after);
        }
    }

    /// The item the selection goes to when the selected ones are deleted: the first one below them
    /// that isn't selected, else the one just above them (like Explorer).
    fn next_after_selection(&self) -> Option<String> {
        let t = self.tab();
        let picked = |row: usize| t.index_at(row).is_some_and(|i| t.is_selected(i));
        let first = (0..t.view.len()).find(|&r| picked(r))?;
        let last = (first..t.view.len()).rev().find(|&r| picked(r))?;
        let row = (last + 1..t.view.len()).find(|&r| !picked(r)).or(first.checked_sub(1))?;
        // (Selected again by name in the folder: not an item of a folder open in the tree view.)
        t.entry_at(row).filter(|e| !(t.tree.is_open() && e.target.is_some())).map(|e| e.name.clone())
    }

    /// Ctrl+Shift+N (`folder`) and New text file: made here, then selected with its name box open.
    pub(super) fn new_item(&mut self, folder: bool) {
        if !self.files_here() {
            return;
        }
        // The new item must be in sight to be named, and the filter might hide it (or search
        // results be on screen instead of the folder).
        if !self.tab().filter.is_empty() {
            self.set_filter(String::new());
        }
        let t = self.tab_mut();
        let dir = t.path.clone();
        let after = AfterJob::new(t.id);
        self.start_job(if folder { Job::NewFolder { dir } } else { Job::NewTextFile { dir } }, after);
    }

    /// What was typed in the name box: `from` in `dir` becomes `to`.
    pub(super) fn rename(&mut self, dir: String, from: String, to: String) {
        if dir.is_empty() {
            self.say(THIS_PC_NOTE);
            return;
        }
        let name = match check_name(&to) {
            NameCheck::Empty => return,
            NameCheck::Bad(why) => {
                // Say why, and open the box again with what was typed, so it can be fixed.
                self.say(why);
                self.reopen_rename(&dir, &from, &to);
                return;
            }
            NameCheck::Good(name) => name,
        };
        if name == from {
            return;
        }
        let after = AfterJob { renamed_from: Some(from.clone()), ..AfterJob::new(self.tab().id) };
        let job = Job::Rename { path: join(&dir, &from), new_name: name.clone() };
        // Explorer asks before a file's extension changes: the file may then open in another app,
        // or not at all.
        let t = self.tab();
        // Found by its full path: search results come from many folders, and names repeat.
        let path = join(&dir, &from);
        let is_folder = t.folder.entries.iter().any(|e| e.is_container() && same_path(&e.path_in(&t.folder.path), &path));
        let (old_ext, new_ext) = (dotted_extension(&from), dotted_extension(&name));
        if !is_folder && !old_ext.is_empty() && old_ext != new_ext {
            let text = if new_ext.is_empty() {
                format!("Without \u{201c}{old_ext}\u{201d}, the file may not open correctly.")
            } else {
                format!("From \u{201c}{old_ext}\u{201d} to \u{201c}{new_ext}\u{201d}: the file may not open correctly.")
            };
            self.ask("Change the file type?".into(), text, "CHANGE", job, after);
            return;
        }
        self.start_job(job, after);
    }

    /// Opens the name box on `name` in `dir` again (when the active tab is there) with `typed`.
    fn reopen_rename(&mut self, dir: &str, name: &str, typed: &str) {
        let t = self.tab_mut();
        // Not over search results: the box finds its item by name, and names repeat there.
        if same_path(&t.path, dir) && !t.is_searching() {
            t.rename_request = Some(RenameRequest::new(dir, name, Some(typed)));
        }
    }

    pub(super) fn ask(&mut self, title: String, text: String, yes: &'static str, job: Job, after: AfterJob) {
        self.next_question += 1;
        self.question = Some(Question { id: self.next_question, title, text, yes, job, after });
    }

    /// The person answered the question (ask.rs): yes does what was asked about.
    pub fn answer(&mut self, yes: bool) {
        let Some(q) = self.question.take() else { return };
        if yes {
            self.start_job(q.job, q.after);
        }
    }

    pub(super) fn start_job(&mut self, job: Job, after: AfterJob) {
        let (tx, hwnd) = (self.tx.clone(), self.hwnd.clone());
        let started = fileops::start(job, move |outcome| {
            if tx.send(Loaded::FileOp { after, outcome }).is_ok() {
                wake(&hwnd);
            }
        });
        if !started {
            self.say("Windows couldn't start this right now. Try again.");
        }
    }

    /// A job finished: read the changed folders again (in every tab showing them), with what was
    /// made selected in the tab that asked.
    pub(super) fn after_file_op(&mut self, after: AfterJob, outcome: Outcome) {
        if outcome.moved_cut {
            self.cut = None;
        }
        // A folder Glint renamed or moved (cut and paste): tabs inside it (and Back / Forward)
        // follow it, as they do when it's renamed elsewhere (watch.rs).
        if let (Some(from), Some((dir, names))) = (&after.renamed_from, &outcome.created) {
            if let [to] = names.as_slice() {
                self.folder_moved(&join(dir, from), &join(dir, to));
            }
        }
        for (from, to) in &outcome.moved_folders {
            self.folder_moved(from, to);
        }
        let mut reread = Vec::new();
        for (i, t) in self.tabs.iter_mut().enumerate() {
            // Search results of a folder in which something changed (anywhere below it): the search
            // runs again (start_load does that). Nothing is selected by name there - names repeat.
            if let Some(s) = &t.search {
                if outcome.changed.iter().any(|f| fileops::is_same_or_inside(f, &s.root)) {
                    reread.push(i);
                }
                continue;
            }
            // (Also when it changed a folder open in the tab's tree view: it's read again with it.)
            if !outcome.changed.iter().any(|f| same_path(f, &t.path) || t.tree.has(f)) {
                continue;
            }
            if t.id == after.tab_id {
                plan_selection(t, &after, &outcome);
            }
            reread.push(i);
        }
        for i in reread {
            self.start_load(i);
        }
        if !outcome.changed.is_empty() {
            self.refresh_drives(); // free space changed
        }
        if let Some((path, typed)) = &outcome.retry {
            if let Some(dir) = parent(path) {
                self.reopen_rename(&dir, file_name(path), typed);
            }
        }
        if let Some(note) = outcome.note {
            self.say(note); // after the reading, which clears notes
        }
    }

    /// Windows' own "New ›" was picked (window.rs): the one item it makes here in the next few
    /// seconds is selected with its name box open, as Explorer does. If
    /// more than one item turns up at once, Glint doesn't guess.
    pub fn rename_what_appears(&mut self) {
        let t = self.tab_mut();
        // Not over search results: they aren't the folder's listing the new item is looked for in.
        if t.path.is_empty() || !same_path(&t.folder.path, &t.path) || t.is_searching() {
            return;
        }
        let before = t.folder.entries.iter().map(|e| e.lower.clone()).collect();
        t.appear = Some(Appear { dir: t.path.clone(), before, until: Instant::now() + APPEAR_WAIT });
    }

    /// Before a pick from a right-click menu for files runs (window.rs): the selection goes back to
    /// the items the menu was opened for, if it changed while the menu waited for Windows' items or
    /// was open (a paste that finished meanwhile selects what it made; a Delete picked then must
    /// delete what was right-clicked, not that). Items that are gone meanwhile stay unselected.
    /// False when the tab on screen isn't the menu's any more or shows another folder: the pick is
    /// then dropped.
    pub fn reselect_for_menu(&mut self, about: &MenuAbout) -> bool {
        let ok = reselect(self.tab_mut(), about);
        if !ok {
            self.say("The folder changed while the menu was open, so nothing was done.");
        }
        ok
    }

    /// The lower-case names of the items cut to the clipboard from `folder`, while they're still on
    /// it (the list draws them faded).
    pub fn cut_names(&self, folder: &str) -> Option<&HashSet<String>> {
        let c = self.cut.as_ref()?;
        (c.folder == folder && fileops::clipboard_sequence() == c.seq).then_some(&c.names)
    }

    /// Something else was put on the clipboard (WM_CLIPBOARDUPDATE). True if faded items should
    /// look normal again.
    pub fn clipboard_changed(&mut self) -> bool {
        if self.cut.as_ref().is_some_and(|c| c.seq != fileops::clipboard_sequence()) {
            self.cut = None;
            return true;
        }
        false
    }
}

/// Decides what the tab that started a job selects when its folder has been read again.
fn plan_selection(t: &mut Tab, after: &AfterJob, outcome: &Outcome) {
    // (Made in the folder - or in a folder open in its tree view, app/tree.rs.)
    let created = outcome.created.as_ref().filter(|(dir, names)| (same_path(dir, &t.path) || t.tree.has(dir)) && !names.is_empty());
    let into = created.map_or_else(|| t.path.clone(), |(dir, _)| dir.clone());
    let pending = |names: Vec<String>, exclusive: bool| Some(PendingSelect { dir: into.clone(), names, exclusive });
    if let Some((dir, names)) = created {
        if outcome.rename_new && same_path(dir, &t.path) {
            t.pending_select = pending(names.clone(), true);
            t.rename_request = Some(RenameRequest::new(dir, &names[0], None));
        } else if let Some(from) = &after.renamed_from {
            // Renamed with Enter: it stays selected under its new name. (After a click elsewhere,
            // the selection is already where that click put it.)
            if t.selected_entries().iter().any(|&i| t.folder.entries[i].name == *from) {
                t.pending_select = pending(names.clone(), false);
            }
        } else {
            t.pending_select = pending(names.clone(), true);
        }
    } else if let Some(next) = after.select_next.as_ref().filter(|_| outcome.ok) {
        t.pending_select = pending(vec![next.clone()], true);
    }
}

/// See `AppState::reselect_for_menu`. Items are told apart by `Tab::item_key` (`Entry::key`): the
/// name, or where it really is for search results and Recycle Bin items, which can share a name.
fn reselect(t: &mut Tab, about: &MenuAbout) -> bool {
    if t.id != about.tab_id || t.folder.path != about.folder || t.waiting_long() {
        return false;
    }
    let wanted: HashSet<&str> = about.names.iter().map(String::as_str).collect();
    let now = t.selected_entries();
    if now.len() == wanted.len() && now.iter().all(|&i| wanted.contains(t.item_key(i).as_str())) {
        return true;
    }
    t.clear_selection();
    let mut first = None;
    for row in 0..t.view.len() {
        let Some(i) = t.index_at(row) else { continue };
        if wanted.contains(t.item_key(i).as_str()) {
            t.selected[i] = true;
            first.get_or_insert(i);
        }
    }
    if !t.focus.is_some_and(|f| t.is_selected(f)) {
        t.focus = first;
        t.anchor = first;
    }
    true
}

/// After a listing of a tab came in (read, or merged in after the folder watch saw a change): the
/// item Windows' "New ›" made gets selected and its name box (see `rename_what_appears`).
pub(super) fn rename_appeared(t: &mut Tab) {
    let Some(a) = t.appear.take() else { return };
    if Instant::now() > a.until || !same_path(&a.dir, &t.folder.path) {
        return; // too late, or the tab went elsewhere
    }
    // (The folder's own items only: not those of folders open in the tree view.)
    let own = |i: usize| !t.tree.is_open() || t.folder.entries[i].target.is_none();
    let added: Vec<usize> = (0..t.folder.entries.len()).filter(|&i| own(i) && !a.before.contains(&t.folder.entries[i].lower)).collect();
    match added.as_slice() {
        [] => t.appear = Some(a), // not there yet
        [one] => {
            if let Some(row) = t.row_of(*one) {
                t.select_only_row(row);
                t.reveal_focus = true;
                t.rename_request = Some(RenameRequest::new(&a.dir, &t.folder.entries[*one].name, None));
            }
        }
        _ => {} // more than one thing changed: don't guess
    }
}

/// After a tab's listing arrived (apply_listing in app.rs): selects what a finished job asked for,
/// puts the cursor on the first of them and scrolls there.
pub(super) fn select_pending(t: &mut Tab) {
    let Some(p) = t.pending_select.take() else { return };
    // Made in a folder open in the tree view (app/tree.rs): its items carry their full path.
    let in_tree = !same_path(&p.dir, &t.folder.path) && t.tree.has(&p.dir);
    if !same_path(&p.dir, &t.folder.path) && !in_tree {
        return; // the person went elsewhere meanwhile
    }
    let wanted: HashSet<String> = p.names.iter().map(|n| if in_tree { join(&p.dir, n).to_lowercase() } else { n.to_lowercase() }).collect();
    // Only what the list shows: a hidden file pasted while hidden files are hidden (or one the
    // filter hides) must not end up selected where no one can see it - Del would delete it too.
    // (By name in the folder itself: not items of folders open in the tree view, which can share
    // a name with them.)
    let mine = |i: usize| {
        let e = &t.folder.entries[i];
        match (in_tree, &e.target) {
            (true, Some(full)) => wanted.contains(&full.to_lowercase()),
            (true, None) => false,
            (false, _) => (!t.tree.is_open() || e.target.is_none()) && wanted.contains(&e.lower),
        }
    };
    let found: Vec<usize> = (0..t.folder.entries.len()).filter(|&i| mine(i) && t.row_of(i).is_some()).collect();
    let Some(first) = found.iter().copied().min_by_key(|&i| t.row_of(i).unwrap_or(usize::MAX)) else {
        if in_tree {
            t.pending_select = Some(p); // that folder's new read is still on its way
        }
        return;
    };
    if p.exclusive {
        t.clear_selection();
    }
    for &i in &found {
        t.selected[i] = true;
    }
    t.focus = Some(first);
    t.anchor = Some(first);
    t.reveal_focus = true;
}

#[cfg(test)]
mod tests {
    use super::super::apply_listing;
    use super::*;
    use crate::folder::{extension, Entry, Folder};
    use std::sync::Arc;

    fn entry(name: &str, attributes: u32) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: EntryKind::File,
            size: 1,
            modified: 0,
            created: 0,
            attributes,
            ext: extension(name),
            type_name: Arc::from("File"),
            target: None,
            free: 0,
        }
    }

    fn listing(names: &[&str]) -> Folder {
        Folder { path: "C:\\x".into(), entries: names.iter().map(|n| entry(n, 0x20)).collect(), ..Default::default() }
    }

    #[test]
    fn the_item_windows_new_menu_makes_gets_the_name_box() {
        let mut t = Tab::new(1, "C:\\x".into());
        apply_listing(&mut t, listing(&["a.txt"]));
        t.appear = Some(Appear { dir: "C:\\x".into(), before: ["a.txt".to_string()].into(), until: Instant::now() + APPEAR_WAIT });
        // Not there yet: still waiting.
        rename_appeared(&mut t);
        assert!(t.appear.is_some() && t.rename_request.is_none());
        // It turned up: selected, with its name box asked for.
        t.folder = listing(&["a.txt", "New Bitmap image.bmp"]);
        t.selected = vec![false; 2];
        t.rebuild_view();
        rename_appeared(&mut t);
        assert!(t.appear.is_none());
        assert_eq!(t.rename_request.as_ref().map(|r| r.name.as_str()), Some("New Bitmap image.bmp"));
        assert_eq!(t.selected_entries().len(), 1);
        // Two new items at once: no guessing.
        let mut u = Tab::new(2, "C:\\x".into());
        apply_listing(&mut u, listing(&["a.txt"]));
        u.appear = Some(Appear { dir: "C:\\x".into(), before: ["a.txt".to_string()].into(), until: Instant::now() + APPEAR_WAIT });
        u.folder = listing(&["a.txt", "b.txt", "c.txt"]);
        u.selected = vec![false; 3];
        u.rebuild_view();
        rename_appeared(&mut u);
        assert!(u.appear.is_none() && u.rename_request.is_none());
    }

    #[test]
    fn a_menu_pick_acts_on_what_was_right_clicked() {
        // "old.txt" was right-clicked; while the menu was open a paste finished and selected its copy.
        let mut t = Tab::new(1, "C:\\x".into());
        apply_listing(&mut t, listing(&["new.txt", "old.txt"]));
        t.select_only_row(1);
        let about = MenuAbout { tab_id: 1, folder: "C:\\x".into(), names: vec!["old.txt".into()] };
        t.select_only_row(0);
        assert!(reselect(&mut t, &about));
        let picked: Vec<&str> = t.selected_entries().iter().map(|&i| t.folder.entries[i].name.as_str()).collect();
        assert_eq!(picked, vec!["old.txt"], "Delete must delete what was right-clicked, not the new copy");
        assert_eq!(t.focus.map(|i| t.folder.entries[i].name.as_str()), Some("old.txt"));
        // Gone meanwhile: nothing stays selected (so nothing is deleted).
        let gone = MenuAbout { names: vec!["vanished.txt".into()], ..about.clone() };
        assert!(reselect(&mut t, &gone));
        assert!(t.selected_entries().is_empty());
        // Another folder on screen by now: the pick is dropped.
        let elsewhere = MenuAbout { folder: "C:\\y".into(), ..about };
        assert!(!reselect(&mut t, &elsewhere));
    }

    #[test]
    fn a_menu_pick_on_search_results_acts_on_the_very_file() {
        // Two search results with the same name, in different folders; the second was right-clicked.
        let result = |path: &str| Entry { target: Some(path.to_string()), ..entry(file_name(path), 0x20) };
        let mut t = Tab::new(1, "C:\\x".into());
        t.folder = Folder { path: "C:\\x".into(), entries: vec![result("C:\\x\\a\\notes.txt"), result("C:\\x\\b\\notes.txt")], ..Default::default() };
        t.search = Some(crate::search::test_search("C:\\x"));
        t.selected = vec![false; 2];
        t.rebuild_view();
        let b = t.row_of(1).unwrap();
        t.select_only_row(b);
        let about = MenuAbout { tab_id: 1, folder: "C:\\x".into(), names: t.selected_entries().iter().map(|&i| t.item_key(i)).collect() };
        // Meanwhile the selection moved to the other one.
        t.select_only_row(t.row_of(0).unwrap());
        assert!(reselect(&mut t, &about));
        let picked: Vec<String> = t.selected_entries().iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect();
        assert_eq!(picked, vec!["C:\\x\\b\\notes.txt"], "only the file right-clicked, not its namesake");
        // A menu opened on the folder (names) doesn't match search results (paths): nothing is picked.
        let by_name = MenuAbout { names: vec!["notes.txt".into()], ..about };
        assert!(reselect(&mut t, &by_name));
        assert!(t.selected_entries().is_empty());
    }

    #[test]
    fn only_items_on_screen_are_selected_after_a_job() {
        // A paste brought a normal file and a hidden one, and hidden files aren't shown.
        let mut t = Tab::new(1, "C:\\x".into());
        t.pending_select = Some(PendingSelect { dir: "C:\\x".into(), names: vec!["a.txt".into(), "secret.txt".into()], exclusive: true });
        let listing = Folder { path: "C:\\x".into(), entries: vec![entry("secret.txt", 0x2), entry("a.txt", 0x20)], ..Default::default() };
        apply_listing(&mut t, listing);
        let picked: Vec<&str> = t.selected_entries().iter().map(|&i| t.folder.entries[i].name.as_str()).collect();
        assert_eq!(picked, vec!["a.txt"], "the hidden copy must not be selected (Del would delete it unseen)");
        assert_eq!(t.focus.map(|i| t.folder.entries[i].name.as_str()), Some("a.txt"));
    }
}
