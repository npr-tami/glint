//! The sidebar's places as the app keeps them: folders pinned to the sidebar (Ctrl+D, "Pin to
//! sidebar", or dropped on "Pinned") with the names given to them, and the Quick access places
//! taken out of the sidebar. All of it lives in the settings ("Pinned", "PinNames",
//! "HiddenPlaces"), so the window saves it a moment after a change.

use super::AppState;
use crate::fileops::same_path;
use crate::folder;
use crate::recycle;

impl AppState {
    /// Whether this folder is pinned to the sidebar.
    pub fn is_pinned(&self, path: &str) -> bool {
        self.settings.pinned.iter().any(|p| same_path(p, path))
    }

    /// What a pinned folder is called in the sidebar: the name given to it (right-click > Rename),
    /// or its own name ("Local Disk (C:)" for a drive).
    pub fn pin_name(&self, path: &str) -> String {
        self.settings.pin_names.iter().find(|(p, _)| same_path(p, path)).map_or_else(|| self.display_name(path), |(_, name)| name.clone())
    }

    /// Ctrl+D, and "Pin to sidebar" / "Unpin from sidebar" in the right-click menus: pins the
    /// folder, or unpins it if it's pinned, and says so in the status bar.
    pub fn toggle_pin(&mut self, path: &str) {
        if !can_pin(path) {
            self.say("Only folders and drives can be pinned to the sidebar.");
            return;
        }
        let name = self.display_name(path);
        if self.is_pinned(path) {
            self.unpin(path);
            self.say(format!("Unpinned {name}"));
        } else if self.pin_folder(path) {
            self.say(format!("Pinned {name}"));
        }
    }

    /// Pins a folder (or a drive) to the sidebar, at the end of "Pinned". For dropping folders on
    /// "Pinned": the caller makes sure `path` is a folder (dropping knows what it dropped); the
    /// disk isn't asked here. Returns false when it was pinned already (or can't be).
    pub fn pin_folder(&mut self, path: &str) -> bool {
        let path = folder::normalize(path);
        if !can_pin(&path) || self.is_pinned(&path) {
            return false;
        }
        self.settings.pinned.push(path);
        true
    }

    /// Takes a folder out of "Pinned" (and forgets the name given to it).
    pub fn unpin(&mut self, path: &str) {
        self.settings.pinned.retain(|p| !same_path(p, path));
        self.settings.pin_names.retain(|p, _| !same_path(p, path));
    }

    /// Moves a pinned folder one place up (`up`) or down in "Pinned".
    pub fn move_pin(&mut self, path: &str, up: bool) {
        let pins = &mut self.settings.pinned;
        let Some(i) = pins.iter().position(|p| same_path(p, path)) else { return };
        let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|&j| j < pins.len()) };
        if let Some(j) = j {
            pins.swap(i, j);
        }
    }

    /// The name a pinned folder shows (right-click > Rename). Only the sidebar shows it - the folder
    /// keeps its own name. An empty name, or the folder's own, means no name of its own any more.
    pub fn rename_pin(&mut self, path: &str, name: &str) {
        if !self.is_pinned(path) {
            return;
        }
        let name = name.trim();
        let own = self.display_name(path);
        self.settings.pin_names.retain(|p, _| !same_path(p, path));
        if !name.is_empty() && name != own {
            self.settings.pin_names.insert(path.to_string(), name.to_string());
        }
    }

    /// Whether a Quick access place (by its id: "home", "desktop", ..., "recycle") was taken out of
    /// the sidebar.
    pub fn is_place_hidden(&self, id: &str) -> bool {
        self.settings.hidden_places.iter().any(|h| h == id)
    }

    /// Right-click > Remove from Quick access (`hidden`), or Show in Quick access > (not `hidden`).
    pub fn set_place_hidden(&mut self, id: &str, hidden: bool) {
        self.settings.hidden_places.retain(|h| h != id);
        if hidden {
            self.settings.hidden_places.push(id.to_string());
        }
    }
}

/// Whether a path can be pinned: a folder or a drive - not This PC, not the Recycle Bin.
pub fn can_pin(path: &str) -> bool {
    !path.trim().is_empty() && !recycle::is_bin(path)
}

#[cfg(test)]
mod tests {
    use super::super::AppState;
    use crate::settings::Settings;

    fn app() -> AppState {
        AppState::for_tests(Settings::default())
    }

    #[test]
    fn pinning_unpinning_and_order() {
        let mut a = app();
        a.toggle_pin("C:\\Work");
        a.toggle_pin("D:\\Music");
        assert!(a.pin_folder("E:\\Photos"));
        assert!(!a.pin_folder("c:\\work\\"), "pinned already (case and the last \\ don't matter)");
        assert!(!a.pin_folder(""), "This PC can't be pinned");
        assert!(!a.pin_folder(crate::recycle::LOCATION), "nor the Recycle Bin");
        assert_eq!(a.settings.pinned, vec!["C:\\Work", "D:\\Music", "E:\\Photos"]);
        a.move_pin("E:\\Photos", true);
        a.move_pin("C:\\Work", true); // already first: stays
        assert_eq!(a.settings.pinned, vec!["C:\\Work", "E:\\Photos", "D:\\Music"]);
        a.move_pin("D:\\Music", false); // already last: stays
        assert_eq!(a.settings.pinned, vec!["C:\\Work", "E:\\Photos", "D:\\Music"]);
        a.rename_pin("C:\\WORK", "Job");
        assert_eq!(a.pin_name("c:\\work"), "Job");
        a.rename_pin("C:\\Work", "  Work ");
        assert_eq!(a.pin_name("C:\\Work"), "Work");
        assert!(a.settings.pin_names.is_empty(), "the folder's own name is no name of its own");
        a.rename_pin("C:\\Work", "Job");
        a.toggle_pin("C:\\Work");
        assert!(!a.is_pinned("C:\\Work"));
        assert!(a.settings.pin_names.is_empty(), "its name goes with it");
        a.rename_pin("C:\\Elsewhere", "Nope");
        assert!(a.settings.pin_names.is_empty(), "only pinned folders get names");
    }

    #[test]
    fn quick_access_places_come_and_go() {
        let mut a = app();
        a.set_place_hidden("music", true);
        a.set_place_hidden("recycle", true);
        a.set_place_hidden("music", true);
        assert_eq!(a.settings.hidden_places, vec!["recycle", "music"]);
        assert!(a.is_place_hidden("music"));
        a.set_place_hidden("music", false);
        assert!(!a.is_place_hidden("music") && a.is_place_hidden("recycle"));
    }
}
