//! The app state's part in how Glint runs (see life.rs): hiding in the background, coming back, and
//! folders sent from outside (another glint.exe, a double-clicked folder, Win+E, the tray icon).
//!
//! While hidden nothing is watched (`AppState::hidden`, checked by `sync_watch`), so a hidden Glint
//! does no work when files change. Coming back watches the tab on screen again and reads it again
//! quietly, like a tab that was in the background.

use super::{AppState, ClosedTab, CLOSED_TABS_KEPT};
use crate::folder;
use crate::life::Target;
use crate::settings::{self, Startup};

impl AppState {
    /// The window hides in the background: the folder watch stops, and so does a subfolder search
    /// still under way (it searches again when the window is back).
    pub fn go_to_background(&mut self) {
        self.hidden = true;
        self.watcher.stop();
        self.split_watch.stop(); // the other half's of split view (app/split.rs)
        self.bin_watch = None; // the Recycle Bin's watch too (app/bin.rs)
        self.skins.pause(); // and the skins folder's (app/skins.rs)
        self.branch_watch.clear(); // and those of folders open in the tree view (app/tree.rs)
        self.searches_stopped.clear();
        for i in 0..self.tabs.len() {
            if let Some(s) = self.tabs[i].search.as_ref().filter(|s| !s.done) {
                self.searches_stopped.push((self.tabs[i].id, s.is_dupes()));
                self.end_search(i, false);
            }
        }
    }

    /// Whether the window is hidden in the background.
    pub fn in_background(&self) -> bool {
        self.hidden
    }

    /// The window comes back from the background, with `open` (a folder from outside) if one came.
    /// With "Tabs from last time" the tabs are as they were (and `open` is added); otherwise it opens
    /// what a fresh start would ("When Glint starts, open", or just `open`) -
    /// the old tabs can still be brought back with Ctrl+Shift+T. `seen`: the window was on screen
    /// before (not so for a Glint started hidden with Windows, whose tabs are a fresh start already).
    pub fn back_from_background(&mut self, open: Option<&Target>, seen: bool) {
        self.hidden = false;
        if self.settings.startup != Startup::Restore && (seen || open.is_some()) {
            let mut old = std::mem::take(&mut self.tabs);
            // Pinned tabs stay as they are, first in the row.
            let mut i = 0;
            while i < old.len() {
                if old[i].pinned {
                    self.tabs.push(old.remove(i));
                } else {
                    i += 1;
                }
            }
            let pinned = self.tabs.len();
            // The folder from outside is a pinned tab's: that tab comes forward and no other opens,
            // as at a fresh start. (The saved tabs `initial_tabs` looks through are empty while
            // Glint runs: the pinned tabs open now are looked through here.)
            let opened = open.map(|t| t.folder.as_str());
            let at_pinned = opened.and_then(|dir| self.tabs.iter().position(|t| crate::fileops::same_path(&t.path, dir)));
            let (fresh, active) = match at_pinned {
                Some(i) => (Vec::new(), i),
                None => settings::initial_tabs(&self.settings, opened, &self.settings.home_place()),
            };
            for saved in fresh.iter().filter(|t| !t.pinned) {
                let mut tab = self.make_tab(saved.path.clone());
                tab.sort = settings::sort_key(saved.sort_column);
                tab.descending = saved.sort_descending;
                self.tabs.push(tab);
            }
            if self.tabs.is_empty() {
                // (initial_tabs always gives one; a window without tabs must never happen.)
                let tab = self.make_tab(self.settings.home_place());
                self.tabs.push(tab);
            }
            // From the right, so the first Ctrl+Shift+T brings back the leftmost.
            for (index, t) in old.into_iter().enumerate().rev().filter(|_| seen) {
                self.closed.push(ClosedTab { index, path: t.path, back: t.back, forward: t.forward, sort: t.sort, descending: t.descending, view: t.own_view, pinned: false });
            }
            let extra = self.closed.len().saturating_sub(CLOSED_TABS_KEPT);
            self.closed.drain(..extra);
            // (`active` is a place in `fresh`, whose saved pinned tabs made way for the ones open now.)
            let saved_pinned = fresh.iter().filter(|t| t.pinned).count();
            self.active = match active.checked_sub(saved_pinned) {
                _ if at_pinned.is_some() => active,
                Some(k) => pinned + k,
                // The folder from outside is a pinned tab's: the pinned tab showing it comes forward
                // (or, when none does any more, it opens in a tab of its own).
                None => match self.tabs[..pinned].iter().position(|t| crate::fileops::same_path(&t.path, &fresh[active].path)) {
                    Some(i) => i,
                    None => {
                        let tab = self.make_tab(fresh[active].path.clone());
                        self.tabs.push(tab);
                        self.tabs.len() - 1
                    }
                },
            }
            .min(self.tabs.len() - 1);
            if let Some(name) = open.and_then(|t| t.select.clone()) {
                self.tabs[self.active].select_after_load = Some(name);
            }
            self.start_load(self.active);
        } else if let Some(t) = open {
            self.open_from_outside(t);
        }
        // The tab on screen wasn't watched while hidden: watch it again and read it again quietly.
        self.ensure_loaded(self.active);
        self.sync_watch();
        // The skins folder too: watched again and listed (a skin may have been edited meanwhile).
        self.skins.resume();
        // Searches stopped on the way into the background run again (in tabs that are still there).
        // (A duplicate search too: it isn't "wanted" by the filter, which it cleared.)
        for (id, dupes) in std::mem::take(&mut self.searches_stopped) {
            let Some(i) = self.tabs.iter().position(|t| t.id == id).filter(|&i| !self.tabs[i].is_searching()) else { continue };
            if dupes {
                self.start_search_for(i, false, true);
            } else if self.search_wanted(i) {
                self.start_search(i, false);
            }
        }
    }

    /// A folder from outside, while the window is on screen: it opens in a new tab after the current
    /// one - or, when a tab shows it already, that tab comes forward (double-clicking the same
    /// folder on the desktop again doesn't pile up tabs). `t.select`: a file in it to select.
    pub fn open_from_outside(&mut self, t: &Target) {
        let path = folder::normalize(&t.folder);
        let key = |p: &str| p.trim_end_matches('\\').to_lowercase();
        let open = if key(&self.tab().path) == key(&path) {
            Some(self.active)
        } else {
            self.tabs.iter().position(|tab| key(&tab.path) == key(&path))
        };
        match open {
            Some(i) => {
                self.active = i;
                if t.select.is_some() {
                    self.start_load(i); // read it again, selecting the file
                }
            }
            None => self.open_tab(path),
        }
        if let Some(name) = t.select.clone() {
            self.tab_mut().select_after_load = Some(name);
        }
        self.ensure_loaded(self.active);
        self.sync_watch();
    }

    /// Selects `name` in the active tab once its folder has been read (a file given on the command
    /// line at start).
    pub fn select_when_loaded(&mut self, name: String) {
        self.tab_mut().select_after_load = Some(name);
    }
}

#[cfg(test)]
mod tests {
    use super::super::AppState;
    use crate::search;
    use crate::settings::Settings;

    #[test]
    fn a_search_under_way_stops_in_the_background() {
        let mut a = AppState::for_tests(Settings::default());
        let t = a.tab_mut();
        (t.path, t.folder.path) = ("C:\\x".into(), "C:\\x".into());
        t.search = Some(search::test_search("C:\\x"));
        a.go_to_background();
        assert!(!a.tab().is_searching(), "it could walk a whole drive while nobody looks");
        assert_eq!(a.searches_stopped, vec![(a.tab().id, false)], "to search again when the window is back");
        // A search that has finished does no more work: its results stay.
        let mut b = AppState::for_tests(Settings::default());
        let mut done = search::test_search("C:\\x");
        done.done = true;
        b.tab_mut().search = Some(done);
        b.go_to_background();
        assert!(b.tab().is_searching() && b.searches_stopped.is_empty());
    }

    /// A folder from outside that a pinned tab shows: that tab comes forward, with no second tab
    /// for it (as at a fresh start). Another folder opens in a tab of its own after the pinned ones.
    #[test]
    fn a_pinned_tabs_folder_from_outside_brings_that_tab_forward() {
        use crate::life::Target;
        let target = |f: &str| Target { folder: f.into(), select: Some("a.txt".into()) };
        let mut a = AppState::for_tests(Settings::default());
        a.hidden = true; // (no folder watch in a test)
        let t = a.tab_mut();
        (t.path, t.folder.path, t.pinned) = ("D:\\Work".into(), "D:\\Work".into(), true);
        let other = a.make_tab("D:\\Other".into());
        a.tabs.push(other);
        let first = a.tabs[0].id;
        // Twice, in other capitals and with a backslash at the end the second time.
        for folder in ["D:\\Work", "d:\\work\\"] {
            a.go_to_background();
            a.back_from_background(Some(&target(folder)), true);
            assert_eq!(a.tabs.len(), 1, "no second tab for D:\\Work: {:?}", a.tabs.iter().map(|t| &t.path).collect::<Vec<_>>());
            assert_eq!((a.active, a.tab().id, a.tab().pinned), (0, first, true));
            assert_eq!(a.tab().select_after_load.as_deref(), Some("a.txt"));
        }
        // A folder no pinned tab shows: its own tab, after the pinned one, and that's the one on screen.
        a.go_to_background();
        a.back_from_background(Some(&target("D:\\Else")), true);
        assert_eq!(a.tabs.iter().map(|t| (t.path.as_str(), t.pinned)).collect::<Vec<_>>(), [("D:\\Work", true), ("D:\\Else", false)]);
        assert_eq!(a.active, 1);
    }

    #[test]
    fn a_duplicate_search_runs_again_when_the_window_is_back() {
        let mut a = AppState::for_tests(Settings::default());
        let t = a.tab_mut();
        (t.path, t.folder.path) = ("C:\\x".into(), "C:\\x".into());
        let mut dupes = search::test_search("C:\\x");
        dupes.dupes = Some(Default::default());
        t.search = Some(dupes);
        a.go_to_background();
        assert_eq!(a.searches_stopped, vec![(a.tab().id, true)]);
        a.back_from_background(None, false);
        assert!(a.tab().search.as_ref().is_some_and(|s| s.is_dupes()), "no filter is typed, yet it looks again");
    }
}
