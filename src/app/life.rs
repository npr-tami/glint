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
        self.bin_watch = None; // the Recycle Bin's watch too (app/bin.rs)
        self.skins.pause(); // and the skins folder's (app/skins.rs)
        self.branch_watch.clear(); // and those of folders open in the tree view (app/tree.rs)
        self.searches_stopped.clear();
        for i in 0..self.tabs.len() {
            if self.tabs[i].search.as_ref().is_some_and(|s| !s.done) {
                self.searches_stopped.push(self.tabs[i].id);
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
            let (fresh, active) = settings::initial_tabs(&self.settings, open.map(|t| t.folder.as_str()), &self.settings.home_place());
            let old = std::mem::take(&mut self.tabs);
            for saved in &fresh {
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
                self.closed.push(ClosedTab { index, path: t.path, back: t.back, forward: t.forward, sort: t.sort, descending: t.descending, view: t.own_view });
            }
            let extra = self.closed.len().saturating_sub(CLOSED_TABS_KEPT);
            self.closed.drain(..extra);
            self.active = active.min(self.tabs.len() - 1);
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
        for id in std::mem::take(&mut self.searches_stopped) {
            if let Some(i) = self.tabs.iter().position(|t| t.id == id).filter(|&i| self.search_wanted(i) && !self.tabs[i].is_searching()) {
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
        assert_eq!(a.searches_stopped, vec![a.tab().id], "to search again when the window is back");
        // A search that has finished does no more work: its results stay.
        let mut b = AppState::for_tests(Settings::default());
        let mut done = search::test_search("C:\\x");
        done.done = true;
        b.tab_mut().search = Some(done);
        b.go_to_background();
        assert!(b.tab().is_searching() && b.searches_stopped.is_empty());
    }
}
