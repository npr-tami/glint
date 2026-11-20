//! The status bar along the bottom: how many items and what's selected on the left; free space,
//! how fast the folder opened and how much memory Glint uses on the right - so the speed is visible.

use std::cell::Cell;
use std::time::Instant;

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX, PROCESS_MEMORY_COUNTERS_EX2};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::app::AppState;
use crate::eraicons::{DriveKind, Role};
use crate::folder::{format_count, format_size};
use crate::pathbar::{fill_box_paint, outline_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{size, Theme};
use crate::ui::{Component, CursorKind, Ctx, Effect, MouseButton, MouseEvent, Point, Rect};

const PAD: f32 = 12.0;
/// 98 / 2000: the last pane, saying where the folder is (My Computer, or Local intranet).
const ZONE_W: f32 = 130.0;

#[derive(Default)]
pub struct StatusBar {
    rect: Rect,
    /// Where the left text was drawn at the last frame (a click there, while a column sorts the
    /// folder for now, goes back to the custom order).
    left: Rect,
}

/// Glint's memory use, three ways, in bytes.
#[derive(Clone, Copy, Debug, Default)]
pub struct Memory {
    /// Task Manager's "Memory" column: RAM holding Glint's own data, not counting shared system
    /// DLLs. Only Windows 10 21H2 / Windows 11 and later report it.
    pub private_working_set: Option<u64>,
    /// Memory Glint has asked Windows for (some of it may not be in RAM at all).
    pub private: u64,
    /// All RAM the process touches, including shared DLLs that other programs use as well (and,
    /// under Wine, Wine's and Linux's graphics libraries - which is why the number is big there).
    pub working_set: u64,
}

impl Memory {
    /// The number to show: what Task Manager shows, or the closest thing older Windows has.
    pub fn shown(&self) -> u64 {
        self.private_working_set.unwrap_or(self.private)
    }
}

/// Asks Windows how much memory Glint uses right now.
pub fn memory() -> Memory {
    // On Windows, PagefileUsage is the same number as PrivateUsage; Wine fills only the former
    // (with Linux's "anonymous resident memory").
    let private = |private: usize, pagefile: usize| if private > 0 { private } else { pagefile } as u64;
    // SAFETY (both calls): the out-struct is valid and its true size is passed in `cb`; the larger
    // structs start with the smaller ones, so the pointer cast matches what the function writes.
    // GetCurrentProcess is a pseudo-handle that needs no closing.
    unsafe {
        let mut ex2 = PROCESS_MEMORY_COUNTERS_EX2 { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32, ..Default::default() };
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut ex2 as *mut _ as *mut PROCESS_MEMORY_COUNTERS, ex2.cb).is_ok() {
            return Memory {
                private_working_set: Some(ex2.PrivateWorkingSetSize as u64).filter(|&v| v > 0),
                private: private(ex2.PrivateUsage, ex2.PagefileUsage),
                working_set: ex2.WorkingSetSize as u64,
            };
        }
        // Windows before 10 21H2 doesn't know the EX2 struct and refuses it.
        let mut ex = PROCESS_MEMORY_COUNTERS_EX { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut ex as *mut _ as *mut PROCESS_MEMORY_COUNTERS, ex.cb).is_ok() {
            return Memory { private_working_set: None, private: private(ex.PrivateUsage, ex.PagefileUsage), working_set: ex.WorkingSetSize as u64 };
        }
    }
    Memory::default()
}

/// One line for the log: "private working set 12.3 MB, private 20.1 MB, working set 40.2 MB".
pub fn memory_report() -> String {
    let m = memory();
    let mb = |v: u64| v as f64 / 1_048_576.0;
    let pws = m.private_working_set.map_or("n/a".to_string(), |v| format!("{:.1} MB", mb(v)));
    format!("memory: private working set {pws}, private {:.1} MB, working set {:.1} MB", mb(m.private), mb(m.working_set))
}

thread_local! {
    /// The memory figure the status bar shows and when it was taken: asked again at most once a
    /// second, so the number doesn't flicker while scrolling.
    static SHOWN_MEMORY: Cell<Option<(Instant, u64)>> = const { Cell::new(None) };
}

fn shown_memory() -> u64 {
    SHOWN_MEMORY.with(|c| match c.get() {
        Some((at, v)) if at.elapsed().as_secs_f64() < 1.0 => v,
        _ => {
            let v = memory().shown();
            c.set(Some((Instant::now(), v)));
            v
        }
    })
}

/// "1.8 ms" / "42 ms".
fn ms(v: f64) -> String {
    if v < 10.0 {
        format!("{v:.1} ms")
    } else {
        format!("{v:.0} ms")
    }
}

/// Duplicates in `view` (each set's files share `free`): how many sets, and the bytes deleting the
/// extra copies would free.
pub fn dupes_summary(entries: &[crate::folder::Entry], view: &[u32]) -> (usize, u64) {
    let mut sets: std::collections::HashMap<u64, (usize, u64)> = std::collections::HashMap::new();
    for &r in view {
        let e = &entries[r as usize];
        let set = sets.entry(e.free).or_default();
        set.0 += 1;
        set.1 = e.size;
    }
    let extra: u64 = sets.values().map(|&(n, size)| size * (n as u64).saturating_sub(1)).sum();
    (sets.len(), extra)
}

impl StatusBar {
    fn left_text(app: &AppState) -> String {
        let tab = app.tab();
        if let Some(note) = app.note() {
            return note.to_string();
        }
        if tab.waiting_long() {
            return "Loading\u{2026}".into(); // the list shows no rows of the old folder either
        }
        let shown = tab.view.len();
        // Hidden files that aren't shown aren't counted (like Explorer); a filter shows "3 of 12".
        let total = tab.visible_total;
        let mut s = if let Some(search) = tab.search.as_ref().filter(|s| s.is_dupes()) {
            // Duplicates (dupes.rs): how many sets, and what deleting the extra copies would free.
            let (sets, extra) = tab.dupes_summary.unwrap_or_else(|| dupes_summary(&tab.folder.entries, &tab.view));
            let mut s = match sets {
                // Nothing to show so far, or none found: the list says the same words (dupes.rs).
                0 if !search.done => crate::dupes::CHECKING.to_string(),
                0 if tab.folder.entries.is_empty() => crate::dupes::NONE_FOUND.to_string(),
                1 => "1 set of duplicates".to_string(),
                // (0 of them: it found some, and the search bar hides them all.)
                n => format!("{} sets of duplicates", format_count(n)),
            };
            if extra > 0 {
                s.push_str(&format!("  \u{00b7}  {} could be freed", format_size(extra)));
            }
            if sets > 0 && !search.done {
                s.push_str("  \u{00b7}  still looking\u{2026}");
            } else if search.capped {
                s.push_str(&format!("  \u{00b7}  showing the first {} files", format_count(crate::search::LIMIT)));
            } else if let Some(d) = search.dupes.filter(|d| d.smallest_compared > 0) {
                s.push_str(&format!("  \u{00b7}  files under {} skipped", format_size(d.smallest_compared)));
            }
            s.push_str("  \u{00b7}  Esc to go back");
            s
        } else if let Some(search) = &tab.search {
            // Searching subfolders (search.rs): how many found, and whether it's still looking.
            let mut s = if shown == 1 { "1 result".to_string() } else { format!("{} results", format_count(shown)) };
            if !search.done {
                s.push_str("  \u{00b7}  searching\u{2026}");
            } else if search.capped {
                s.push_str(&format!("  \u{00b7}  showing the first {}", format_count(crate::search::LIMIT)));
            }
            s
        } else if !app.settings.status_count {
            String::new()
        } else if shown != total {
            format!("{} of {} items", format_count(shown), format_count(total))
        } else if total == 1 {
            "1 item".into()
        } else {
            format!("{} items", format_count(total))
        };
        // The Recycle Bin: how much it holds, right after its items ("9 items  ·  185.2 MB in
        // Recycle Bin"). Said once, here: the bin has no size of the list below.
        if let Some(held) = tab.folder.bin.as_ref().and_then(|bin| bin.size_text()) {
            s.push_str(&format!("  \u{00b7}  {held}"));
        }
        let (n, bytes) = tab.selection_summary();
        if n > 0 && app.settings.status_selected {
            s.push_str(&format!("  \u{00b7}  {} selected", format_count(n)));
            if bytes > 0 {
                s.push_str(&format!(" ({})", format_size(bytes)));
            }
        }
        // The size of the files in the list right now, when it's asked for: what's inside folders
        // isn't counted (folders have no size here), and it follows the list as the search bar
        // narrows it or the tree view opens folders. Not in the Recycle Bin, which says what it holds.
        if app.settings.status_total && tab.search.is_none() && !crate::recycle::is_bin(&tab.folder.path) {
            let sum: u64 = tab.view.iter().filter_map(|&i| tab.folder.entries.get(i as usize)).filter(|e| e.kind == crate::folder::EntryKind::File).map(|e| e.size).sum();
            s.push_str(&format!("  \u{00b7}  {}", format_size(sum)));
        }
        if tab.loading.is_some() && tab.path != tab.folder.path {
            s.push_str("  \u{00b7}  loading\u{2026}");
        }
        // (With the number of items left out, the first thing has no dot before it.)
        let s = s.strip_prefix("  \u{00b7}  ").map(str::to_string).unwrap_or(s);
        let mut s = s;
        // (Where the folder rules sort the folder, nothing is said: the list shows it.)
        if tab.column_now && tab.custom_here().is_none() {
            // A column picked here sorts it for now (the custom sort order comes back after).
            s.push_str("  \u{00b7}  Column for now");
        }
        s
    }

    fn right_text(app: &AppState) -> String {
        let tab = app.tab();
        let mut parts = Vec::new();
        // Free space and opening time are the shown folder's: left out while a slow one is coming.
        if !tab.waiting_long() {
            if let Some(space) = tab.folder.space.as_ref().filter(|_| app.settings.status_free) {
                parts.push(format!("{} {} free", space.drive, format_size(space.free)));
            }
            // An empty Recycle Bin says so (what a full one holds is on the left, after its items).
            if let Some(empty) = tab.folder.bin.as_ref().and_then(|bin| bin.empty_text()) {
                parts.push(empty.to_string());
            }
            if let Some(t) = tab.load_ms.filter(|_| app.settings.status_opened) {
                parts.push(format!("opened in {}", ms(t)));
            }
        }
        if let Some(t) = app.startup_ms.filter(|_| app.settings.status_started) {
            parts.push(format!("started in {}", ms(t)));
        }
        let mem = if app.settings.status_memory { shown_memory() } else { 0 };
        if mem > 0 {
            parts.push(format!("{} RAM", format_size(mem)));
        }
        parts.join("  \u{00b7}  ")
    }
}

/// With the status bar switched off ("Show the status bar"): a note ("Path copied", "Restore it
/// first") shows in a small box at the bottom of `area` (the file list) while it's fresh.
pub fn paint_note(r: &mut Renderer, theme: &Theme, app: &AppState, area: Rect) {
    let Some(note) = app.note() else { return };
    let style = TextStyle::new(theme.text);
    let w = (r.measure(note, &style) + 2.0 * PAD).min(area.w - 16.0);
    if w <= 2.0 * PAD {
        return;
    }
    let b = Rect::new(area.x + (area.w - w) / 2.0, area.bottom() - size::STATUS_H - 10.0, w, size::STATUS_H);
    // In the skin's shape: leaning a little in a slanted skin, round-cornered in a rounded one.
    let lean = theme.lean(6.0);
    fill_box_paint(r, theme, b, lean, &theme.toolbar_paint);
    outline_box(r, theme, b, lean, 1.0, theme.line);
    r.text(note, b.inset(PAD, 0.0), &style.align(Align::Center));
}

impl Component for StatusBar {
    fn layout(&mut self, rect: Rect, _ctx: &Ctx, _app: &AppState) {
        self.rect = rect;
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, _ctx: &Ctx) {
        // A required fix that couldn't be installed: red, whatever the skin, until the next start.
        if let Some(version) = &app.update_needed {
            r.fill_rect(self.rect, crate::theme::Color::hex(0xc42b1c));
            let style = TextStyle::new(crate::theme::Color::hex(0xffffff)).semibold();
            let text = format!("Update needed: Glint {version} fixes a problem in this version. It will try again next time Glint starts.");
            r.text(&text, self.rect.inset(PAD, 0.0), &style);
            return;
        }
        if theme.era.old() {
            // An older Windows' status bar (retrolist.rs): its panes, the text in them.
            let style = TextStyle::new(crate::retrolist::status_text(theme));
            let right = Self::right_text(app);
            let right_w = r.measure(&right, &style).min(self.rect.w * 0.6);
            let grip = if theme.era == crate::theme::Era::Win7 { 0.0 } else { 14.0 };
            let zone = if theme.era.classic() && self.rect.w > 3.0 * ZONE_W { ZONE_W } else { 0.0 };
            let inner = Rect::new(self.rect.x + PAD, self.rect.y, self.rect.w - 2.0 * PAD - grip - zone, self.rect.h);
            let split = inner.right() - right_w - PAD - 2.0;
            crate::retrolist::status_bar(r, theme, self.rect, split, zone);
            if zone > 0.0 {
                // As their Explorer did: My Computer for the PC's own folders, Local intranet for
                // a network place.
                let network = app.tab().path.starts_with("\\\\");
                let pane = Rect::new(self.rect.right() - 16.0 - zone, self.rect.y, zone, self.rect.h);
                let icon = Rect::new(pane.x + 5.0, (pane.y + (pane.h - size::ICON) / 2.0).round() + 1.0, size::ICON, size::ICON);
                let role = if network { Role::Drive(DriveKind::Network) } else { Role::Computer };
                crate::eraicons::draw(r, theme.icons, role, icon);
                let words = if network { "Local intranet" } else { "My Computer" };
                r.text(words, Rect::new(icon.right() + 5.0, pane.y, (pane.right() - icon.right() - 8.0).max(0.0), pane.h), &style);
            }
            r.text(&right, Rect::new(inner.right() - right_w - 1.0, inner.y, right_w + 1.0, inner.h), &style.align(Align::Right));
            self.left = Rect::new(inner.x, inner.y, (split - inner.x - 8.0).max(0.0), inner.h);
            r.text(&Self::left_text(app), self.left, &style);
            return;
        }
        r.fill_paint(self.rect, 0.0, &theme.through(&theme.statusbar_paint));
        r.fill_rect(Rect::new(self.rect.x, self.rect.y, self.rect.w, 1.0), theme.status_line);
        if theme.bevels {
            let well = self.rect.inset(2.0, 3.0);
            r.stroke_rect(Rect::new(well.x, well.y, well.w, well.h), 1.0, theme.statusbar.darker(0.4));
            r.fill_rect(Rect::new(well.x + 1.0, well.bottom() - 1.0, well.w - 1.0, 1.0), crate::theme::Color::hex(0xffffff));
            r.fill_rect(Rect::new(well.right() - 1.0, well.y + 1.0, 1.0, well.h - 1.0), crate::theme::Color::hex(0xffffff));
        }
        let style = TextStyle::new(theme.status_text);
        let right = Self::right_text(app);
        let right_w = r.measure(&right, &style).min(self.rect.w * 0.6);
        let inner = self.rect.inset(PAD, 0.0);
        r.text(&right, Rect::new(inner.right() - right_w - 1.0, inner.y, right_w + 1.0, inner.h), &style.align(Align::Right));
        self.left = Rect::new(inner.x, inner.y, inner.w - right_w - 16.0, inner.h);
        r.text(&Self::left_text(app), self.left, &style);
    }

    fn cursor(&self, p: Point, app: &AppState) -> CursorKind {
        if self.back_to_custom(p, app) {
            CursorKind::Hand
        } else {
            CursorKind::Arrow
        }
    }

    fn mouse_down(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if e.button == MouseButton::Left && self.back_to_custom(e.pos, app) {
            app.back_to_custom_order();
            return Effect::Redraw;
        }
        Effect::None
    }
}

impl StatusBar {
    /// Whether `p` is on "Sorted by Name." or "Column for now": a column sorts the folder for now,
    /// in place of the custom sort order.
    fn back_to_custom(&self, p: Point, app: &AppState) -> bool {
        app.update_needed.is_none() && app.custom_order_waits() && self.left.contains(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn never_says_folder_rules() {
        let text = |recipe: &str, column_now: bool| {
            let mut app = AppState::for_tests(Settings { custom_sort: true, sort_recipe: Some(recipe.into()), ..Default::default() });
            app.tabs[0].folder.path = "C:\\t".into();
            app.tabs[0].custom = app.custom_order();
            app.tabs[0].column_now = column_now;
            StatusBar::left_text(&app)
        };
        assert_eq!(text("-a_to_z", false), "0 items");
        assert_eq!(text("in C:\\t (\n-newest_first\n)", false), "0 items");
        // Rules for a folder with odd letters in its name: nothing said either.
        let odd = "C:\\t\\Fotos #1 \u{130}\u{f1} \u{1F600}";
        let odd_text = |recipe: &str| {
            let mut app = AppState::for_tests(Settings { custom_sort: true, sort_recipe: Some(recipe.into()), ..Default::default() });
            app.tabs[0].folder.path = odd.into();
            app.tabs[0].custom = app.custom_order();
            StatusBar::left_text(&app)
        };
        assert_eq!(odd_text(&format!("in {odd} (\n-newest_first\n)")), "0 items");
        // Rules only for another folder: this one sorts by its column, and says nothing.
        let elsewhere = text("in C:\\elsewhere (\n-newest_first\n)", false);
        assert_eq!(elsewhere, "0 items");
        // A column picked for now, in place of the rules: said as before.
        assert_eq!(text("-a_to_z", true), "0 items  \u{00b7}  Column for now");
        // The old words are gone everywhere.
        for t in [text("-a_to_z", false), text("in C:\\t (\n-newest_first\n)", false), text("-a_to_z", true), elsewhere] {
            assert!(!t.contains("Custom sort") && !t.contains("Folder rules"), "{t}");
        }
    }

    fn entry(name: &str, kind: crate::folder::EntryKind, size: u64) -> crate::folder::Entry {
        crate::folder::Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind,
            size,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: crate::folder::extension(name),
            type_name: std::sync::Arc::from("File"),
            target: None,
            free: 0,
        }
    }

    /// The state with one tab showing `entries` as the folder C:\t.
    fn showing(settings: Settings, entries: Vec<crate::folder::Entry>) -> AppState {
        let mut app = AppState::for_tests(settings);
        app.tabs[0].path = "C:\\t".into();
        crate::app::apply_listing(&mut app.tabs[0], crate::folder::Folder { path: "C:\\t".into(), entries, ..Default::default() });
        app
    }

    /// The size on the status bar ("4 items  ·  9 KB", no label) is the size of the files in the list right now: what's in folders isn't
    /// counted, and it shrinks as the search bar narrows the list. Only when it's asked for.
    #[test]
    fn the_size_is_the_size_of_the_files_in_the_list() {
        use crate::folder::EntryKind::{File, Folder};
        let entries = || vec![entry("a.txt", File, 4096), entry("b #1 \u{130}\u{f1} \u{1F600}.txt", File, 5120), entry("Sub", Folder, 0), entry("Sized", Folder, 1 << 30)];
        let mut app = showing(Settings { status_total: true, ..Default::default() }, entries());
        assert_eq!(StatusBar::left_text(&app), "4 items  \u{00b7}  9 KB");
        // The search bar narrows the list, and the size with it.
        app.tabs[0].filter = "a.txt".into();
        app.tabs[0].rebuild_view();
        assert_eq!(StatusBar::left_text(&app), "1 of 4 items  \u{00b7}  4 KB");
        app.tabs[0].filter = "\u{1F600}".into();
        app.tabs[0].rebuild_view();
        assert_eq!(StatusBar::left_text(&app), "1 of 4 items  \u{00b7}  5 KB");
        app.tabs[0].filter = "nothing like it".into();
        app.tabs[0].rebuild_view();
        assert_eq!(StatusBar::left_text(&app), "0 of 4 items  \u{00b7}  0 B");
        // With what's selected before it, and without the number of items.
        app.tabs[0].filter.clear();
        app.tabs[0].rebuild_view();
        app.tabs[0].selected[0] = true;
        assert_eq!(StatusBar::left_text(&app), "4 items  \u{00b7}  1 selected (4 KB)  \u{00b7}  9 KB");
        app.settings.status_count = false;
        app.tabs[0].selected[0] = false;
        assert_eq!(StatusBar::left_text(&app), "9 KB");
        // Not asked for (as Glint starts out): not said. An empty folder holds nothing.
        assert_eq!(StatusBar::left_text(&showing(Settings::default(), entries())), "4 items");
        assert_eq!(StatusBar::left_text(&showing(Settings { status_total: true, ..Default::default() }, Vec::new())), "0 items  \u{00b7}  0 B");
        // Search results have no such size, as before.
        let mut app = showing(Settings { status_total: true, ..Default::default() }, entries());
        let mut search = crate::search::test_search("C:\\t");
        search.done = true;
        app.tabs[0].search = Some(search);
        assert_eq!(StatusBar::left_text(&app), "4 results");
    }

    /// The state with one tab showing the Recycle Bin, holding `sizes` (one item each).
    fn bin_showing(settings: Settings, sizes: &[u64]) -> AppState {
        let mut app = AppState::for_tests(settings);
        let entries: Vec<_> = sizes.iter().enumerate().map(|(i, &size)| entry(&format!("deleted {i} \u{1F600}.txt"), crate::folder::EntryKind::File, size)).collect();
        let listing = crate::recycle::Listing { items: vec![crate::recycle::BinItem::default(); entries.len()], total: sizes.iter().sum() };
        app.tabs[0].path = crate::recycle::LOCATION.into();
        let folder = crate::folder::Folder { path: crate::recycle::LOCATION.into(), entries, bin: Some(std::sync::Arc::new(listing)), ..Default::default() };
        crate::app::apply_listing(&mut app.tabs[0], folder);
        app
    }

    /// In the Recycle Bin: the items, then what the bin holds, once: "9 items  ·  185.2 MB in
    /// Recycle Bin". Never the size of the list (or the older "Folder"), and not again on the right.
    #[test]
    fn the_recycle_bin_says_what_it_holds_once() {
        // 185.2 MB in nine items.
        let mut sizes = vec![1_048_576u64; 8];
        sizes.push(194_196_275 - 8 * 1_048_576);
        let quiet = Settings { status_free: false, status_opened: false, status_started: false, status_memory: false, ..Default::default() };
        let app = bin_showing(quiet.clone(), &sizes);
        assert_eq!(StatusBar::left_text(&app), "9 items  \u{00b7}  185.2 MB in Recycle Bin");
        assert_eq!(StatusBar::right_text(&app), "", "the size isn't said twice");
        // The size of the list asked for: the bin still says only what it holds.
        let mut app = bin_showing(Settings { status_total: true, ..quiet.clone() }, &sizes);
        assert_eq!(StatusBar::left_text(&app), "9 items  \u{00b7}  185.2 MB in Recycle Bin");
        // What's selected comes after; the search bar narrows the items, not what the bin holds.
        app.tabs[0].selected[0] = true;
        assert_eq!(StatusBar::left_text(&app), "9 items  \u{00b7}  185.2 MB in Recycle Bin  \u{00b7}  1 selected (1.0 MB)");
        app.tabs[0].selected[0] = false;
        app.tabs[0].filter = "deleted 8".into();
        app.tabs[0].rebuild_view();
        assert_eq!(StatusBar::left_text(&app), "1 of 9 items  \u{00b7}  185.2 MB in Recycle Bin");
        // One item.
        let app = bin_showing(quiet.clone(), &[4096]);
        assert_eq!(StatusBar::left_text(&app), "1 item  \u{00b7}  4 KB in Recycle Bin");
        // The parts switched off stay off: no number of items, nothing about what's selected.
        let mut app = bin_showing(Settings { status_count: false, status_selected: false, ..quiet.clone() }, &sizes);
        app.tabs[0].selected[0] = true;
        assert_eq!(StatusBar::left_text(&app), "185.2 MB in Recycle Bin");
        // Every setting on, a huge bin, items of no size: said once, on the left only.
        for sizes in [vec![u64::MAX / 4, 1], vec![0, 0, 0]] {
            let app = bin_showing(Settings { status_total: true, ..Default::default() }, &sizes);
            let (left, right) = (StatusBar::left_text(&app), StatusBar::right_text(&app));
            assert_eq!(left.matches("in Recycle Bin").count(), 1, "{left}");
            assert!(!right.contains("Recycle Bin"), "{right}");
            assert_eq!(left.matches("  \u{00b7}  ").count(), 1, "the items, then what the bin holds: {left}");
            assert!(!left.contains("Folder"), "{left}");
        }
    }

    /// An empty Recycle Bin says so where it always did, and no size.
    #[test]
    fn an_empty_recycle_bin_says_so() {
        let quiet = Settings { status_free: false, status_opened: false, status_started: false, status_memory: false, status_total: true, ..Default::default() };
        let app = bin_showing(quiet, &[]);
        assert_eq!(StatusBar::left_text(&app), "0 items");
        assert_eq!(StatusBar::right_text(&app), "The Recycle Bin is empty.");
        // A bin that couldn't be read holds nothing to tell: no size of any kind.
        let mut app = AppState::for_tests(Settings { status_total: true, ..Default::default() });
        app.tabs[0].path = crate::recycle::LOCATION.into();
        let folder = crate::folder::Folder { path: crate::recycle::LOCATION.into(), error: Some("Failed to read the Recycle Bin.".into()), ..Default::default() };
        crate::app::apply_listing(&mut app.tabs[0], folder);
        assert_eq!(StatusBar::left_text(&app), "0 items");
        assert!(!StatusBar::right_text(&app).contains("Recycle Bin"));
    }

    /// The status line of a duplicate search that is `done`, `capped` and skipped files under
    /// `smallest` bytes.
    fn dupes_text(done: bool, capped: bool, smallest: u64) -> String {
        let mut app = AppState::for_tests(Settings::default());
        let mut search = crate::search::test_search("C:\\t");
        (search.done, search.capped) = (done, capped);
        search.dupes = Some(crate::dupes::Summary { smallest_compared: smallest, ..Default::default() });
        app.tabs[0].search = Some(search);
        StatusBar::left_text(&app)
    }

    /// The sets and the bytes to free are worked out with the view, and follow it (results coming
    /// in, a filter typed, a sort), always the same as counting them on the spot.
    #[test]
    fn duplicates_summary_follows_the_view() {
        let mut app = AppState::for_tests(Settings::default());
        let mut search = crate::search::test_search("C:\\t");
        search.done = true;
        search.dupes = Some(crate::dupes::Summary::default());
        app.tabs[0].search = Some(search);
        let file = |name: &str, set: u64, size: u64| crate::folder::Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: crate::folder::EntryKind::File,
            size,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: crate::folder::extension(name),
            type_name: std::sync::Arc::from("File"),
            target: Some(format!("C:\\t\\{set}\\{name}")),
            free: set,
        };
        let tab = &mut app.tabs[0];
        tab.folder.path = "C:\\t".into();
        tab.folder.entries = vec![file("a.mp4", 1, 1_048_576), file("b.mp4", 1, 1_048_576), file("c.mp4", 1, 1_048_576), file("x \u{1F600}.txt", 2, 10), file("y.txt", 2, 10)];
        tab.selected = vec![false; 5];
        tab.rebuild_view();
        assert_eq!(tab.dupes_summary, Some((2, 2 * 1_048_576 + 10)));
        let text = StatusBar::left_text(&app);
        assert!(text.starts_with("2 sets of duplicates  \u{00b7}  2.0 MB could be freed"), "{text}");
        let tab = &mut app.tabs[0];
        tab.filter = "mp4".into();
        tab.rebuild_view();
        assert_eq!(tab.dupes_summary, Some((1, 2 * 1_048_576)));
        assert!(StatusBar::left_text(&app).starts_with("1 set of duplicates"), "{}", StatusBar::left_text(&app));
        let tab = &mut app.tabs[0];
        tab.filter = "nothing like it".into();
        tab.rebuild_view();
        assert_eq!(tab.dupes_summary, Some((0, 0)));
        // (It found some: the search bar hiding them all isn't "No duplicate files found.")
        assert_eq!(StatusBar::left_text(&app), "0 sets of duplicates  \u{00b7}  Esc to go back");
        // The same as counting on the spot, whatever the view.
        let tab = &mut app.tabs[0];
        tab.filter.clear();
        tab.rebuild_view();
        assert_eq!(tab.dupes_summary, Some(dupes_summary(&tab.folder.entries, &tab.view)));
        // Not a duplicate search: nothing kept.
        tab.search = None;
        tab.rebuild_view();
        assert_eq!(tab.dupes_summary, None);
    }

    /// Nothing to show: "Checking for duplicate files…" while it runs, "No duplicate files found."
    /// when it ended with none - the words the file list says too (dupes.rs keeps them for both).
    #[test]
    fn a_duplicate_search_with_nothing_to_show_says_what_the_list_says() {
        assert_eq!(dupes_text(false, false, 0), "Checking for duplicate files\u{2026}  \u{00b7}  Esc to go back");
        assert_eq!(dupes_text(true, false, 0), "No duplicate files found.  \u{00b7}  Esc to go back");
        assert!(dupes_text(false, false, 0).starts_with(crate::dupes::CHECKING));
        assert!(dupes_text(true, false, 0).starts_with(crate::dupes::NONE_FOUND));
        // What it adds about a search that stopped early or skipped small files still follows.
        assert_eq!(dupes_text(true, true, 0), "No duplicate files found.  \u{00b7}  showing the first 20,000 files  \u{00b7}  Esc to go back");
        assert_eq!(dupes_text(true, false, 4096), "No duplicate files found.  \u{00b7}  files under 4 KB skipped  \u{00b7}  Esc to go back");
        // The old words are gone.
        for text in [dupes_text(false, false, 0), dupes_text(true, false, 0)] {
            assert!(!text.contains("Looking for duplicates") && !text.contains("No duplicates found"), "{text}");
        }
        // Found some, the search bar hides them all, and it still runs: still checking.
        let mut app = AppState::for_tests(Settings::default());
        let mut search = crate::search::test_search("C:\\t");
        search.dupes = Some(crate::dupes::Summary::default());
        app.tabs[0].search = Some(search);
        let tab = &mut app.tabs[0];
        tab.folder.path = "C:\\t".into();
        tab.folder.entries = vec![entry("a.txt", crate::folder::EntryKind::File, 10), entry("b.txt", crate::folder::EntryKind::File, 10)];
        tab.selected = vec![false; 2];
        tab.filter = "zzz".into();
        tab.rebuild_view();
        assert_eq!(StatusBar::left_text(&app), "Checking for duplicate files\u{2026}  \u{00b7}  Esc to go back");
    }

    #[test]
    fn a_duplicate_search_that_skipped_small_files_says_so() {
        let text = dupes_text(true, false, 4096);
        assert!(text.contains("files under 4 KB skipped"), "{text}");
        // Nothing skipped, nothing said; still looking, nothing yet; the cap note comes first.
        assert!(!dupes_text(true, false, 0).contains("skipped"));
        let capped = dupes_text(true, true, 4096);
        assert!(capped.contains("showing the first 20,000 files") && !capped.contains("skipped"), "{capped}");
        assert!(text.ends_with("Esc to go back"));
    }

    #[test]
    fn a_name_search_says_it_stopped_only_when_it_was_capped() {
        let text = |done: bool, capped: bool| {
            let mut app = AppState::for_tests(Settings::default());
            let mut search = crate::search::test_search("C:\\t");
            (search.done, search.capped) = (done, capped);
            app.tabs[0].search = Some(search);
            StatusBar::left_text(&app)
        };
        assert_eq!(text(true, false), "0 results");
        assert_eq!(text(true, true), "0 results  \u{00b7}  showing the first 20,000");
        assert_eq!(text(false, false), "0 results  \u{00b7}  searching\u{2026}");
    }
}
