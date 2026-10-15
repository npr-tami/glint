//! The Recycle Bin's commands in the app state: restore, delete for good, empty, go to or copy
//! where an item was - and what every other command does while the bin is on screen.
//!
//! The bin isn't a folder: its rows are deleted items, kept as `$R…` files inside `$Recycle.Bin`
//! (recycle.rs). Copying, cutting, renaming or opening those as if they were ordinary files would
//! work on the wrong things, so while the bin is on screen (or on its way, or just left), the file
//! commands come here first (`bin_command`). The work runs as file jobs (fileops.rs threads); a job
//! carries the exact items it acts on (where each is kept), never just names - the bin can hold
//! three "notes.txt".

use std::sync::atomic::Ordering;

use super::files::AfterJob;
use super::{wake, AppState, Loaded};
use crate::fileops::Job;
use crate::folder::format_count;
use crate::recycle::{self, BinItem, BinJob};
use crate::shell;
use crate::ui::Command;

/// What a file command in the Recycle Bin says instead of acting on its items.
const RESTORE_FIRST: &str = "Restore it first (right-click > Restore).";

/// Files that do something when opened (programs, scripts, shortcuts, registry files): not
/// opened straight from the Recycle Bin.
const RUNS: &[&str] = &[
    "exe", "com", "scr", "pif", "msi", "msix", "msixbundle", "appx", "appxbundle", "appinstaller", "bat", "cmd", "ps1", "vbs", "vbe", "js", "jse",
    "wsf", "wsh", "hta", "cpl", "reg", "inf", "lnk", "url", "jar", "application",
];

impl AppState {
    /// Whether the tab on screen shows the Recycle Bin, with its listing on screen (not another
    /// folder still on its way, or still shown while the bin is).
    pub fn bin_on_screen(&self) -> bool {
        let t = self.tab();
        recycle::is_bin(&t.path) && recycle::is_bin(&t.folder.path) && t.folder.bin.is_some()
    }

    /// Runs `cmd` the Recycle Bin's way when the bin is involved: on screen, or on its way, or just
    /// left (its rows still shown). Returns false when the command isn't about the bin's items (it
    /// then runs as usual).
    pub(super) fn bin_command(&mut self, cmd: &Command) -> bool {
        let t = self.tab();
        if !recycle::is_bin(&t.path) && !recycle::is_bin(&t.folder.path) {
            return false;
        }
        let ready = self.bin_on_screen();
        match cmd {
            Command::OpenSelected | Command::OpenEntry(_) => {
                let chosen = match cmd {
                    Command::OpenEntry(i) => vec![*i],
                    _ => self.tab().selected_entries(),
                };
                if ready {
                    self.bin_look_at(&chosen);
                }
            }
            Command::OpenInNewTabs | Command::NewTabBehind(_) | Command::OpenWith => {
                if ready {
                    self.say("Restore it to open it (right-click > Restore).");
                }
            }
            Command::CopyFiles | Command::CutFiles | Command::BeginRename | Command::Rename { .. } => self.say(RESTORE_FIRST),
            Command::Paste | Command::PasteInto(_) | Command::NewFolder | Command::NewTextFile => {
                self.say("To put things in the Recycle Bin, delete them (Del).");
            }
            Command::Delete { .. } => {
                if ready {
                    self.bin_delete_selected();
                }
            }
            Command::CopyPath => {
                if ready {
                    self.bin_copy_where_it_was();
                }
            }
            Command::ShowInExplorer => shell::show_in_explorer("shell:RecycleBinFolder", false),
            Command::Properties => {
                // The bin's own Properties (how big it may get on each drive); deleted items have none
                // worth showing.
                if self.tab().selected_entries().is_empty() && !crate::shellmenu::properties(self.owner().hwnd(), vec![recycle::LOCATION.to_string()]) {
                    self.say("Windows couldn't show the properties right now");
                }
            }
            Command::OpenTerminal(dir) if recycle::is_bin(dir) => self.open_terminal(String::new()),
            _ => return false,
        }
        true
    }

    /// The bin's items for these entries of the listing on screen (none unless the bin is on screen).
    fn bin_items(&self, entries: &[usize]) -> Vec<BinItem> {
        if !self.bin_on_screen() {
            return Vec::new();
        }
        let t = self.tab();
        let Some(bin) = t.folder.bin.as_ref().filter(|b| b.items.len() == t.folder.entries.len()) else { return Vec::new() };
        entries.iter().filter_map(|&i| bin.items.get(i).cloned()).collect()
    }

    /// The selected items of the bin on screen.
    fn bin_chosen(&self) -> Vec<BinItem> {
        self.bin_items(&self.tab().selected_entries())
    }

    /// Enter / double-click in the bin: a deleted file can be looked at as it is (opened where it's
    /// kept); a folder has to be restored first, and so does a program or script, which would run.
    fn bin_look_at(&mut self, chosen: &[usize]) {
        let t = self.tab();
        match chosen {
            [one] => match t.folder.entries.get(*one) {
                Some(e) if RUNS.contains(&e.ext.as_str()) => self.say("Restore it to run it (right-click > Restore)."),
                Some(e) if !e.is_container() => match self.bin_items(chosen).first().and_then(|i| i.real.clone()) {
                    Some(real) => shell::open(&real),
                    None => self.say("Windows didn't say where this item is kept, so Glint can't open it."),
                },
                Some(_) => self.say("Restore it to open it (right-click > Restore)."),
                None => {}
            },
            [] => {}
            _ => self.say("Restore them to open them (right-click > Restore)."),
        }
    }

    /// Right-click > Restore: the selected items go back where they came from (with a number added
    /// when the name is taken there now).
    pub fn bin_restore_selected(&mut self) {
        let items = self.bin_chosen();
        self.bin_restore(items);
    }

    /// The empty space's "Restore all": everything in the bin goes back.
    pub fn bin_restore_all(&mut self) {
        let all: Vec<usize> = (0..self.tab().folder.entries.len()).collect();
        let items = self.bin_items(&all);
        self.bin_restore(items);
    }

    fn bin_restore(&mut self, items: Vec<BinItem>) {
        if items.is_empty() {
            return;
        }
        let after = AfterJob::new(self.tab().id);
        self.start_job(Job::Bin(BinJob::Restore(items)), after);
    }

    /// Del, Shift+Del and "Delete permanently" in the bin: the selected items are deleted for good,
    /// after a question (unless "Ask before deleting" is "never").
    pub fn bin_delete_selected(&mut self) {
        let items = self.bin_chosen();
        if items.is_empty() {
            return;
        }
        let what = names_text(&items);
        let after = AfterJob::new(self.tab().id);
        let job = Job::Bin(BinJob::Delete(items));
        if self.settings.ask_before_deleting(true) {
            self.ask(format!("Delete {what} permanently?"), "This can't be undone.".into(), "DELETE", job, after);
        } else {
            self.start_job(job, after);
        }
    }

    /// "Empty Recycle Bin": Windows asks first (its own question, with the number of items) and
    /// shows its progress.
    pub fn bin_empty(&mut self) {
        let after = AfterJob::new(self.tab().id);
        let job = Job::Bin(BinJob::Empty { owner: self.hwnd.load(Ordering::SeqCst) });
        if recycle::test_dir().is_some() {
            // The pretend bin has no Windows question: Glint asks.
            let text = "This can't be undone.".to_string();
            self.ask("Empty the Recycle Bin?".into(), text, "EMPTY", job, after);
        } else {
            self.start_job(job, after);
        }
    }

    /// "Go to where it was": the folder the (first) selected item was deleted from.
    pub fn bin_go_to_where_it_was(&mut self) {
        let Some(item) = self.bin_chosen().into_iter().next() else { return };
        if item.location.is_empty() {
            self.say("Windows didn't say where this item was.");
            return;
        }
        self.navigate(&item.location);
    }

    /// "Copy where it was": the full paths the selected items were deleted from, as text.
    pub fn bin_copy_where_it_was(&mut self) {
        let items = self.bin_chosen();
        if items.is_empty() {
            return;
        }
        let lines: Vec<&str> = items.iter().map(|i| if i.original.is_empty() { i.location.as_str() } else { i.original.as_str() }).collect();
        let hwnd = self.owner().hwnd();
        let ok = shell::copy_text(hwnd, &lines.join("\r\n"));
        self.say(match (ok, lines.len()) {
            (false, _) => "Couldn't copy (the clipboard is busy)".to_string(),
            (true, 1) => "Original location copied".to_string(),
            (true, n) => format!("{} original locations copied", format_count(n)),
        });
    }

    /// While the active tab shows the bin, Windows tells Glint when it changes (recycle.rs); other
    /// tabs showing it are read again when they're shown. Called from `sync_watch`.
    pub(super) fn sync_bin_watch(&mut self) {
        let t = self.tab();
        // (Nothing is watched while Glint is hidden in the background: app/life.rs.)
        if !recycle::is_bin(&t.path) || self.hidden {
            self.bin_watch = None;
            return;
        }
        if self.bin_watch.as_ref().is_some_and(|(id, _)| *id == t.id) {
            return;
        }
        let (tab_id, tx, hwnd) = (t.id, self.tx.clone(), self.hwnd.clone());
        let watch = recycle::Watch::start(move || {
            if tx.send(Loaded::BinChanged).is_ok() {
                wake(&hwnd);
            }
        });
        self.bin_watch = Some((tab_id, watch));
        // Not watched until now: it may have changed since it was read (a read on its way is new
        // enough).
        let i = self.active;
        let t = &self.tabs[i];
        if t.loading.is_none() && t.folder.path == t.path {
            self.reload(i, crate::watch::Armed::none());
        }
    }

    /// Windows said the bin changed: every tab showing it reads it again (quietly, keeping the
    /// selection on the same items).
    pub(super) fn bin_changed(&mut self) {
        for t in self.tabs.iter_mut().filter(|t| recycle::is_bin(&t.path)) {
            t.stale = true;
        }
    }
}

/// "“notes.txt”" for one item, "3 items" for more - by the names they had.
fn names_text(items: &[BinItem]) -> String {
    match items {
        [one] => {
            let name = crate::folder::file_name(&one.original);
            if name.is_empty() { "this item".to_string() } else { format!("\u{201c}{name}\u{201d}") }
        }
        _ => format!("{} items", format_count(items.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::{Entry, EntryKind, Folder};
    use crate::settings::Settings;
    use crate::ui::MenuAbout;
    use std::sync::Arc;

    /// A bin listing with three "notes.txt" deleted from three folders, plus one folder.
    fn bin_listing() -> Folder {
        let item = |n: usize, name: &str, from: &str, dir: bool| {
            let real = format!("C:\\$Recycle.Bin\\S-1-5-21\\$R{n:06}");
            let e = Entry {
                name: name.into(),
                lower: name.to_lowercase(),
                kind: if dir { EntryKind::Folder } else { EntryKind::File },
                size: 10,
                modified: n as u64,
                created: 0,
                attributes: 0,
                ext: String::new(),
                type_name: Arc::from("x"),
                target: Some(real.clone()),
                free: 0,
            };
            let original = format!("{from}\\{name}");
            (e, BinItem { original, location: from.into(), real: Some(real), info: None, deleted: n as u64 })
        };
        let pairs = [
            item(1, "notes.txt", "C:\\a", false),
            item(2, "notes.txt", "C:\\b", false),
            item(3, "notes.txt", "C:\\c", false),
            item(4, "Project", "C:\\a", true),
        ];
        let (entries, items): (Vec<Entry>, Vec<BinItem>) = pairs.into_iter().unzip();
        Folder { path: recycle::LOCATION.into(), entries, bin: Some(Arc::new(recycle::Listing { items, total: 40 })), ..Default::default() }
    }

    /// The state with the bin on screen.
    fn in_bin() -> AppState {
        let mut a = AppState::for_tests(Settings::default());
        let t = a.tab_mut();
        t.path = recycle::LOCATION.into();
        super::super::apply_listing(t, bin_listing());
        a
    }

    fn chosen_from(a: &AppState) -> Vec<String> {
        a.bin_chosen().into_iter().map(|i| i.location).collect()
    }

    #[test]
    fn items_of_the_same_name_are_never_mixed_up() {
        let mut a = in_bin();
        let t = a.tab_mut();
        let row_b = (0..t.view.len()).find(|&r| t.entry_at(r).is_some_and(|e| e.target.as_deref().is_some_and(|x| x.ends_with("$R000002")))).unwrap();
        t.select_only_row(row_b);
        assert_eq!(chosen_from(&a), vec!["C:\\b"]);
        // A right-click menu was opened for it; meanwhile another "notes.txt" got selected.
        let about = MenuAbout { tab_id: a.tab().id, folder: recycle::LOCATION.into(), names: vec!["c:\\$recycle.bin\\s-1-5-21\\$r000002".into()] };
        let t = a.tab_mut();
        let other = (0..t.view.len()).find(|&r| r != row_b && t.entry_at(r).is_some_and(|e| e.name == "notes.txt")).unwrap();
        t.select_only_row(other);
        assert!(a.reselect_for_menu(&about));
        assert_eq!(chosen_from(&a), vec!["C:\\b"], "exactly the one right-clicked, not every notes.txt");
        // A fresh read of the bin (F5, a change Windows told about) keeps it, and only it.
        super::super::apply_listing(a.tab_mut(), bin_listing());
        assert_eq!(chosen_from(&a), vec!["C:\\b"]);
        // Delete asks about that one item.
        assert!(a.run(Command::Delete { permanent: false }));
        let q = a.question.take().expect("a question first");
        assert_eq!(q.title, "Delete \u{201c}notes.txt\u{201d} permanently?");
    }

    #[test]
    fn file_commands_never_touch_bin_items_as_files() {
        let mut a = in_bin();
        a.tab_mut().select_all();
        for cmd in [Command::CopyFiles, Command::CutFiles, Command::Paste, Command::NewFolder, Command::OpenInNewTabs] {
            assert!(a.run(cmd.clone()), "{cmd:?} is the bin's");
            assert!(a.question.is_none() && a.cut_names(recycle::LOCATION).is_none(), "{cmd:?} did nothing to the items");
        }
        // Leaving the bin: the path says Downloads, but the rows on screen are still the bin's.
        // Del must not send those (where they're kept) to the bin, or delete them.
        a.tab_mut().path = "C:\\Users\\me\\Downloads".into();
        assert!(a.run(Command::Delete { permanent: false }));
        assert!(a.run(Command::Delete { permanent: true }));
        assert!(a.question.is_none(), "nothing asked, nothing deleted");
        assert!(a.bin_chosen().is_empty(), "the bin's commands only work with the bin on screen");
    }

    #[test]
    fn only_items_inside_folders_go_to_the_bin() {
        use crate::fileops::is_full_item_path;
        assert!(is_full_item_path("C:\\x\\a.txt"));
        assert!(is_full_item_path("\\\\server\\share\\x"));
        assert!(!is_full_item_path("C:\\"), "a whole drive");
        assert!(!is_full_item_path("\\\\server\\share"), "a whole share");
        assert!(!is_full_item_path("a.txt"), "a bare name");
        assert!(!is_full_item_path(""));
        assert!(!is_full_item_path(recycle::LOCATION));
    }
}
