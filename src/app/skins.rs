//! The app state's part in skins (skin.rs): the skin in use - in its light or dark version -,
//! picking one, and news from the skins folder (WM_APP_SKINS).
//!
//! `settings.skin` is always the skin on screen: switching light or dark switches it to its light
//! or dark version (`skin::Library::twin`), so Settings shows what you see. The look is made from
//! the skin's text again whenever that text changes (the customiser, a file edited in Notepad), and
//! `theme_generation` goes up so the windows follow (window.rs `sync_theme`).

use super::{row_height, AppState};
use crate::settings::{self, ThemeMode};
use crate::skin;

impl AppState {
    /// The look follows `settings.skin` (after a pick, a change to the skin's text, new files). A
    /// skin whose file is gone looks like AMD Radeon until it's back.
    pub fn apply_skin(&mut self) {
        let mut theme = self.skins.theme(&self.settings.skin, self.dark);
        theme.logo_style = self.settings.logo;
        if theme == self.theme {
            return;
        }
        // Another row height: the same rows stay on screen (as when the "Size" setting changes).
        let (old, new) = (self.row_height(), row_height(self.settings.size, theme.row_h));
        if old != new && old > 0.0 {
            // (Only where items are rows: icons and the List view don't change with it.)
            for t in self.tabs.iter_mut().filter(|t| matches!(t.view_mode, settings::ViewMode::Details | settings::ViewMode::Tree)) {
                t.scroll = t.scroll / old * new;
            }
            self.tab_mut().reveal_focus = true;
        }
        self.dark = theme.dark;
        self.theme = theme;
        self.theme_generation += 1;
    }

    /// Light or dark (Ctrl+Shift+L, Settings' "Light or dark", Windows switching with "Same as
    /// Windows"): the skin's light or dark version. False when it has none (the look stays).
    pub fn set_dark(&mut self, dark: bool) -> bool {
        self.settings.skin = self.skins.twin(&self.settings.skin, dark);
        self.apply_skin();
        self.dark == dark
    }

    /// A skin picked (Settings' drop-down, the customiser's list). "Light or dark" follows the
    /// pick: picking a light skin means light - unless it's "Same as Windows" and Windows is light
    /// anyway.
    pub fn pick_skin(&mut self, id: &str) {
        let dark = self.skins.find(id).map_or(self.dark, |e| e.dark);
        let windows_agrees = self.settings.theme == ThemeMode::System && settings::dark_for(ThemeMode::System) == dark;
        if !windows_agrees {
            self.settings.theme = if dark { ThemeMode::Dark } else { ThemeMode::Light };
        }
        self.settings.skin = id.to_string();
        self.apply_skin();
    }

    /// News about skins (WM_APP_SKINS): a file in the folder changed (list it again), a listing is
    /// ready (the skin in use may look different now), or the file worker finished (problems go to
    /// the status bar).
    pub fn skins_event(&mut self, event: usize) {
        match event {
            skin::EVENT_CHANGED => self.skins.refresh(),
            skin::EVENT_LISTED => {
                // A picture the skin names that wasn't there when the skin was read (copied in next
                // to it just now, say) is found by reading the skin again.
                let picture = std::path::Path::new(&self.theme.wallpaper);
                let picture_due = !self.theme.wallpaper.is_empty() && picture.starts_with(skin::folder()) && !picture.exists();
                if self.skins.take_listing() || picture_due {
                    // The skin in use by its id as the folder now has it (a file whose name changed
                    // only in capitals is the same skin; Settings compares ids exactly).
                    if let Some(id) = self.skins.find(&self.settings.skin).map(|e| e.id.clone()).filter(|id| *id != self.settings.skin) {
                        self.settings.skin = id;
                    }
                    self.apply_skin();
                }
            }
            skin::EVENT_WRITTEN => {
                let (problems, moved) = self.skins.take_written();
                for problem in problems {
                    self.say(problem);
                }
                // A skin whose new name was another file's moved to a free name: still the one in use.
                if let Some((_, new)) = moved.iter().find(|(old, _)| *old == self.settings.skin) {
                    self.settings.skin = new.clone();
                    self.apply_skin();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::AppState;
    use crate::settings::{ListSize, Settings, ThemeMode};
    use crate::skin;
    use crate::theme::Shape;

    #[test]
    fn light_and_dark_switch_to_the_skins_other_version() {
        let mut app = AppState::for_tests(Settings { skin: "nvidia".into(), ..Default::default() });
        app.apply_skin();
        assert_eq!(app.theme.shape, Shape::Straight);
        let generation = app.theme_generation;
        assert!(app.set_dark(false));
        assert_eq!(app.settings.skin, "nvidia-light");
        assert!(!app.dark && !app.theme.dark);
        assert!(app.theme_generation > generation, "the windows hear about it");
        assert!(app.set_dark(true));
        assert_eq!(app.settings.skin, "nvidia");
        // Picking a light skin means light (the "Light or dark" setting follows).
        app.settings.theme = ThemeMode::Dark;
        app.pick_skin(skin::DEFAULT_LIGHT);
        assert_eq!((app.settings.theme, app.settings.skin.as_str(), app.dark), (ThemeMode::Light, skin::DEFAULT_LIGHT, false));
        assert_eq!(app.theme.shape, Shape::Slanted);
    }

    #[test]
    fn a_skin_without_a_light_version_stays_and_a_missing_one_looks_like_amd_radeon() {
        let mut app = AppState::for_tests(Settings { skin: "file:gone".into(), ..Default::default() });
        app.apply_skin();
        assert_eq!(app.theme, skin::built_in_theme(skin::DEFAULT));
        assert!(!app.set_dark(false), "no light version of a skin that isn't there");
        assert_eq!(app.settings.skin, "file:gone", "the choice is kept for when the file is back");
    }

    #[test]
    fn a_new_row_height_keeps_the_same_rows_on_screen() {
        let mut app = AppState::for_tests(Settings { size: ListSize::Small, ..Default::default() });
        app.tabs[0].scroll = 280.0; // 10 rows of 28
        app.settings.skin = "nvidia".into();
        app.apply_skin();
        assert_eq!(app.tabs[0].scroll, 280.0, "NVIDIA has AMD Radeon's row height");
        app.skins.add_for_tests("file:Short", "name: Short\nrow-height: 24\n");
        app.pick_skin("file:Short");
        assert_eq!(app.row_height(), 24.0);
        assert_eq!(app.tabs[0].scroll, 240.0, "still 10 rows down");
    }
}
