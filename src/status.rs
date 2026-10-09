//! The status bar along the bottom: how many items and what's selected on the left; free space,
//! how fast the folder opened and how much memory Glint uses on the right - so the speed is visible.

use std::cell::Cell;
use std::time::Instant;

use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX, PROCESS_MEMORY_COUNTERS_EX2};
use windows::Win32::System::Threading::GetCurrentProcess;

use crate::app::AppState;
use crate::folder::{format_count, format_size};
use crate::pathbar::{fill_box_paint, outline_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{size, Theme};
use crate::ui::{Component, Ctx, Rect};

const PAD: f32 = 12.0;

#[derive(Default)]
pub struct StatusBar {
    rect: Rect,
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
        let mut s = if let Some(search) = &tab.search {
            // Searching subfolders (search.rs): how many found, and whether it's still looking.
            let mut s = if shown == 1 { "1 result".to_string() } else { format!("{} results", format_count(shown)) };
            if !search.done {
                s.push_str("  \u{00b7}  searching\u{2026}");
            } else if search.capped {
                s.push_str(&format!("  \u{00b7}  showing the first {}", format_count(crate::search::LIMIT)));
            }
            s
        } else if shown != total {
            format!("{} of {} items", format_count(shown), format_count(total))
        } else if total == 1 {
            "1 item".into()
        } else {
            format!("{} items", format_count(total))
        };
        let (n, bytes) = tab.selection_summary();
        if n > 0 {
            s.push_str(&format!("  \u{00b7}  {} selected", format_count(n)));
            if bytes > 0 {
                s.push_str(&format!(" ({})", format_size(bytes)));
            }
        }
        if tab.loading.is_some() && tab.path != tab.folder.path {
            s.push_str("  \u{00b7}  loading\u{2026}");
        }
        // The custom sort order decides (and for this folder, its own "in …:" rules).
        if let Some(recipe) = tab.custom.as_ref().filter(|_| !tab.folder.path.is_empty()) {
            match recipe.folder_label(&tab.folder.path) {
                Some(label) => s.push_str(&format!("  \u{00b7}  Custom sort ({label})")),
                None => s.push_str("  \u{00b7}  Custom sort"),
            }
        }
        s
    }

    fn right_text(app: &AppState) -> String {
        let tab = app.tab();
        let mut parts = Vec::new();
        // Free space and opening time are the shown folder's: left out while a slow one is coming.
        if !tab.waiting_long() {
            if let Some(space) = &tab.folder.space {
                parts.push(format!("{} {} free", space.drive, format_size(space.free)));
            }
            // The Recycle Bin: how much it holds.
            if let Some(bin) = &tab.folder.bin {
                parts.push(bin.size_text());
            }
            if let Some(t) = tab.load_ms {
                parts.push(format!("opened in {}", ms(t)));
            }
        }
        if let Some(t) = app.startup_ms {
            parts.push(format!("started in {}", ms(t)));
        }
        let mem = shown_memory();
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
        r.fill_paint(self.rect, 0.0, &theme.statusbar_paint);
        r.fill_rect(Rect::new(self.rect.x, self.rect.y, self.rect.w, 1.0), theme.status_line);
        let style = TextStyle::new(theme.status_text);
        let right = Self::right_text(app);
        let right_w = r.measure(&right, &style).min(self.rect.w * 0.6);
        let inner = self.rect.inset(PAD, 0.0);
        r.text(&right, Rect::new(inner.right() - right_w - 1.0, inner.y, right_w + 1.0, inner.h), &style.align(Align::Right));
        r.text(&Self::left_text(app), Rect::new(inner.x, inner.y, inner.w - right_w - 16.0, inner.h), &style);
    }
}
