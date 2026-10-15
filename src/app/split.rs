//! Split view (Ctrl+\): two folders side by side. Each half shows a tab: the left or right
//! one Glint works in is the active tab (the path bar, the toolbar and the keys are for it), and
//! the other half shows `AppState::split`, another tab. A click in the other half swaps them over
//! (window.rs), and F6 does it from the keyboard. Dragging from one half into the other copies or
//! moves, as any drag into a folder does.
//!
//! The other half's folder is watched too (its own watch, `split_watch`), so it stays up to date
//! while Glint works in the first.

use super::files::AfterJob;
use super::AppState;
use crate::fileops::{DropHow, Job};
use crate::{archive, folder, recycle, watch};

impl AppState {
    /// The tab shown in the other half, while split view is on (never the active tab).
    pub fn split_index(&self) -> Option<usize> {
        let id = self.split?;
        self.tabs.iter().position(|t| t.id == id).filter(|&i| i != self.active)
    }

    /// Split view on or off. On: the other half shows the next tab - a new one, of the same folder,
    /// when there's only this one.
    pub(super) fn toggle_split(&mut self) {
        if self.split.take().is_some() {
            self.split_watch.stop();
            self.say("Split view off");
            return;
        }
        let i = self.active;
        let other = if self.tabs.len() > 1 {
            if i + 1 < self.tabs.len() { i + 1 } else { i - 1 }
        } else {
            let mut tab = self.make_tab(self.tab().path.clone());
            let cur = &self.tabs[i];
            (tab.sort, tab.descending) = (cur.sort, cur.descending);
            tab.set_own_view(cur.own_view);
            let at = self.insert_tab(i + 1, tab);
            self.start_load(at);
            at
        };
        self.split = Some(self.tabs[other].id);
        self.split_last = self.tab().id;
        self.ensure_loaded(other);
        self.sync_split_watch();
        self.say("Split view: click a side to work in it (F6 switches)");
    }

    /// F6, or a click in the other half: Glint works in that half now.
    pub fn focus_other_pane(&mut self) {
        let Some(other) = self.split_index() else { return };
        self.split = Some(self.tab().id);
        self.active = other;
        self.split_last = self.tab().id;
        // Each watch stays on its folder, and each half's folder sizes with it: they just change places.
        self.swap_halves();
        self.sync_watch();
    }

    /// After anything that may have changed the tabs: the other half still shows a tab that isn't
    /// the active one. Picking the other half's tab in the tab row swaps the halves; closing it
    /// shows a neighbour instead (or ends split view when there's no other tab).
    pub(super) fn fix_split(&mut self) {
        let Some(id) = self.split else { return };
        let active = self.tabs.get(self.active).map(|t| t.id).unwrap_or(0);
        let neighbour = (if self.active + 1 < self.tabs.len() { Some(self.active + 1) } else { self.active.checked_sub(1) }).map(|n| self.tabs[n].id);
        if id == active {
            // The other half's tab was picked: the tab Glint was in goes to the other half - or,
            // when that tab was just closed, a neighbour does.
            let last = self.split_last;
            self.split = if self.tabs.iter().any(|t| t.id == last) { (last != active).then_some(last) } else { neighbour };
            self.swap_halves();
        } else if !self.tabs.iter().any(|t| t.id == id) {
            self.split = neighbour;
        }
        self.split_last = active;
        if self.split.is_none() {
            self.split_watch.stop();
        }
    }

    /// At start ("Reopen split view"): split view again, with `folder` on the other side - a tab
    /// already showing it, or a new one after the current tab (read when it's first shown).
    pub fn reopen_split(&mut self, folder: String) {
        // (Not the active tab itself: with both sides on one folder the other side is a second tab.)
        let other = match self.tabs.iter().enumerate().position(|(i, t)| i != self.active && t.path == folder) {
            Some(i) => i,
            None => {
                let mut tab = self.make_tab(folder);
                let cur = &self.tabs[self.active];
                (tab.sort, tab.descending) = (cur.sort, cur.descending);
                tab.set_own_view(cur.own_view);
                self.insert_tab(self.active + 1, tab)
            }
        };
        self.split = Some(self.tabs[other].id);
        self.split_last = self.tab().id;
    }

    /// Right-click > "Open in other side": the folder opens in split view's other half, and Glint
    /// stays in this one.
    pub(super) fn open_in_other_side(&mut self) {
        let Some(other) = self.split_index() else { return };
        let t = self.tab();
        let Some(i) = t.selected_entries().first().copied().or(t.focus) else { return };
        let e = &t.folder.entries[i];
        if !e.is_container() {
            return;
        }
        let path = e.path_in(&t.folder.path);
        let keep = std::mem::replace(&mut self.active, other);
        self.go(&path);
        self.active = keep;
    }

    /// Right-click > "Copy to other side" / "Move to other side": the selected items go into the
    /// folder split view's other half shows, as if dragged there.
    pub(super) fn send_to_other_side(&mut self, moving: bool) {
        let Some(other) = self.split_index() else { return };
        if !self.files_here() {
            return;
        }
        let dest = self.tabs[other].path.clone();
        if dest.is_empty() || folder::is_place(&dest) || archive::split(&dest).is_some() || recycle::is_bin(&dest) {
            self.say("The other side isn't a folder files can go into");
            return;
        }
        let sources = self.chosen();
        if sources.is_empty() {
            return;
        }
        let how = if moving { DropHow::Move } else { DropHow::Copy };
        let after = AfterJob::new(self.tabs[other].id);
        self.start_job(Job::Drop { sources, dest, how }, after);
    }

    fn swap_halves(&mut self) {
        std::mem::swap(&mut self.watcher, &mut self.split_watch);
        self.sizer.swap(0, 1);
        self.sizing.swap(0, 1);
    }

    /// The other half's folder is watched (and read again quietly when the watch is new, as the
    /// active tab's is: `sync_watch`).
    pub(super) fn sync_split_watch(&mut self) {
        let Some(i) = self.split_index() else {
            self.split_watch.stop();
            return;
        };
        self.ensure_loaded(i);
        let t = &self.tabs[i];
        if self.hidden || !watch::can_watch(&t.path) {
            self.split_watch.stop();
            return;
        }
        if self.split_watch.is_on(t.id, &t.path) {
            return;
        }
        let armed = self.split_watch.watch(t.id, &t.path);
        let t = &mut self.tabs[i];
        if t.loading.is_some() {
            t.stale = true;
        } else {
            self.reload(i, armed);
        }
    }
}
