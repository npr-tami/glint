//! Disk space's page (Ctrl+Shift+U), drawn in the file list's place while a tab shows it: the
//! drive's name and how full it is, SCAN AGAIN, then three tabs:
//! - Biggest: the biggest folders, each with a bar; clicking one goes into it (or opens it when
//!   nothing big is inside), and the path above goes back up;
//! - Changes: the folders that grew or shrank most since the last scan;
//! - By type: how much videos, games, pictures and so on take;
//! and at the bottom the space no folder shows (Recycle Bin, hibernation and page files, the rest).
//!
//! The look itself is diskspace.rs; the app keeps one per drive (app/disk.rs).

use std::sync::atomic::Ordering;

use crate::app::disk::DiskLook;
use crate::app::AppState;
use crate::diskspace::{self, Kind, Look};
use crate::folder;
use crate::kit;
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{size, Color, Theme};
use crate::ui::{Command, CursorKind, Effect, MouseButton, MouseEvent, Point, Rect};

const PAD: f32 = 24.0;
const TITLE_H: f32 = 34.0;
const SUB_H: f32 = 22.0;
const TABS_H: f32 = 36.0;
const CRUMBS_H: f32 = 32.0;
const ROW_H: f32 = 44.0;
const BAR_H: f32 = 8.0;
const SIZE_W: f32 = 96.0;
const BUTTON_W: f32 = 112.0;
const BUTTON_H: f32 = 28.0;
/// One wheel notch scrolls this far.
const NOTCH: f32 = 48.0;

const GREW: Color = Color::hex(0xf0b44c);
const SHRANK: Color = Color::hex(0x5fb4f0);

/// The three tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Biggest,
    Grew,
    Types,
}

impl Page {
    const ALL: [Page; 3] = [Page::Biggest, Page::Grew, Page::Types];

    fn label(self) -> &'static str {
        match self {
            Page::Biggest => "Biggest",
            Page::Grew => "Changes",
            Page::Types => "By type",
        }
    }
}

/// What is at a place on the page.
#[derive(Clone, Debug, PartialEq)]
enum Spot {
    LookAgain,
    Tab(Page),
    /// A step of the path over Biggest: the folder inside the drive ("" is the drive).
    Crumb(String),
    /// A row: what clicking it does.
    Row(Click),
}

#[derive(Clone, Debug, PartialEq)]
enum Click {
    /// Go into this folder (inside the drive) on Biggest.
    Into(String),
    /// Open this folder in the tab.
    Open(String),
    /// Nothing (By type's rows).
    Nothing,
}

/// One row as drawn.
struct Row {
    name: String,
    sub: String,
    /// The bar's length, 0 to 1.
    part: f32,
    /// The bar's colour; None: the skin's accent.
    bar: Option<Color>,
    size: String,
    size_color: Option<Color>,
    click: Click,
}

/// What the page remembers between frames.
#[derive(Default)]
pub struct DiskPage {
    page: Page,
    /// Biggest: the folder on show, inside the drive ("" is the drive itself).
    at: String,
    /// The drive the page last showed (the folder on show is forgotten for another drive).
    drive: String,
    scroll: f32,
    height: f32,
    mouse: Option<Point>,
    spots: Vec<(Spot, Rect)>,
    area: Rect,
}

impl DiskPage {
    fn spot_at(&self, p: Point) -> Option<&Spot> {
        if !self.area.contains(p) {
            return None;
        }
        self.spots.iter().find(|(_, r)| r.contains(p)).map(|(s, _)| s)
    }

    fn max_scroll(&self) -> f32 {
        (self.height - self.area.h).max(0.0)
    }

    pub fn cursor(&self, p: Point) -> CursorKind {
        match self.spot_at(p) {
            Some(Spot::Row(Click::Nothing)) | None => CursorKind::Arrow,
            Some(_) => CursorKind::Hand,
        }
    }

    pub fn mouse_move(&mut self, e: &MouseEvent) -> Effect {
        let before = self.mouse.and_then(|p| self.spot_at(p).cloned());
        self.mouse = Some(e.pos);
        let now = self.spot_at(e.pos).cloned();
        if before != now { Effect::Redraw } else { Effect::None }
    }

    pub fn mouse_leave(&mut self) -> Effect {
        let had = self.mouse.and_then(|p| self.spot_at(p).cloned()).is_some();
        self.mouse = None;
        if had { Effect::Redraw } else { Effect::None }
    }

    pub fn wheel(&mut self, e: &MouseEvent) -> Effect {
        let to = (self.scroll - e.wheel * NOTCH).clamp(0.0, self.max_scroll());
        if (to - self.scroll).abs() < 0.5 {
            return Effect::None;
        }
        self.scroll = to;
        Effect::Redraw
    }

    pub fn mouse_down(&mut self, e: &MouseEvent, app: &AppState) -> Effect {
        if e.button != MouseButton::Left {
            return Effect::None;
        }
        let Some(drive) = diskspace::drive_of_location(&app.tab().path) else { return Effect::None };
        match self.spot_at(e.pos).cloned() {
            Some(Spot::LookAgain) => Effect::Command(Command::DiskLookAgain(drive)),
            Some(Spot::Tab(p)) => {
                self.page = p;
                self.scroll = 0.0;
                Effect::Redraw
            }
            Some(Spot::Crumb(rel)) | Some(Spot::Row(Click::Into(rel))) => {
                self.page = Page::Biggest;
                self.at = rel;
                self.scroll = 0.0;
                Effect::Redraw
            }
            Some(Spot::Row(Click::Open(rel))) => Effect::Command(Command::Navigate(real_path(&drive, &rel))),
            _ => Effect::None,
        }
    }

    /// Backspace or Left on Biggest goes up a folder; Escape back to the drive. True when used.
    pub fn key(&mut self, vk: u16) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_BACK, VK_ESCAPE, VK_LEFT};
        if self.page != Page::Biggest || self.at.is_empty() {
            return false;
        }
        if vk == VK_BACK.0 || vk == VK_LEFT.0 {
            self.at = folder::parent(&self.at).filter(|p| !p.ends_with(':')).unwrap_or_default();
            self.scroll = 0.0;
            return true;
        }
        if vk == VK_ESCAPE.0 {
            self.at.clear();
            self.scroll = 0.0;
            return true;
        }
        false
    }

    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, area: Rect) {
        self.area = area;
        self.spots.clear();
        let path = &app.tab().path;
        let Some(drive) = diskspace::drive_of_location(path) else { return };
        if self.drive != drive {
            self.drive = drive.clone();
            self.at.clear();
            self.scroll = 0.0;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        let font = size::FONT;
        let w = (area.w - 2.0 * PAD).max(160.0);
        let x = area.x + PAD;
        let top = area.y - self.scroll;
        let mut y = 16.0;
        let hover = self.mouse.and_then(|p| self.spot_at(p).cloned());
        let drive_name = app.display_name(&drive);

        // The drive's name, and SCAN AGAIN on the right.
        r.text(&drive_name, Rect::new(x, top + y, w - BUTTON_W - 12.0, TITLE_H), &TextStyle::new(theme.text_strong).size(font + 7.0).semibold());
        let look = app.disks.looks.get(&drive);
        let looking = matches!(look, Some(DiskLook::Looking { .. }) | None);
        if !looking {
            let b = Rect::new(x + w - BUTTON_W, top + y + 3.0, BUTTON_W, BUTTON_H);
            let look_btn = kit::Look { hover: hover == Some(Spot::LookAgain), enabled: true, ..kit::Look::default() };
            kit::button(r, theme, b, "SCAN AGAIN", look_btn);
            self.spots.push((Spot::LookAgain, b));
        }
        y += TITLE_H;

        match look {
            None | Some(DiskLook::Looking { .. }) => {
                let (counted, used) = match look {
                    Some(DiskLook::Looking { counted, used, .. }) => (counted.load(Ordering::Relaxed), *used),
                    _ => (0, 0),
                };
                let text = if used > 0 {
                    format!("Scanning every folder\u{2026} {} of {} counted.", folder::format_size(counted.min(used)), folder::format_size(used))
                } else {
                    "Scanning every folder\u{2026}".to_string()
                };
                r.text(&text, Rect::new(x, top + y, w, SUB_H), &TextStyle::new(theme.muted).size(font));
                y += SUB_H + 12.0;
                let track = Rect::new(x, top + y, w, BAR_H);
                r.fill_round_rect(track, BAR_H / 2.0, theme.chip);
                let part = if used > 0 { (counted as f32 / used as f32).clamp(0.02, 1.0) } else { 0.02 };
                r.fill_round_rect(Rect::new(track.x, track.y, track.w * part, BAR_H), BAR_H / 2.0, theme.accent);
                y += BAR_H + 16.0;
                r.text("A big drive can take a few minutes. You can keep using Glint meanwhile.", Rect::new(x, top + y, w, SUB_H), &TextStyle::new(theme.muted).size(font));
                y += SUB_H;
            }
            Some(DiskLook::Failed) => {
                r.text("Failed to scan this drive. Is it plugged in?", Rect::new(x, top + y, w, SUB_H), &TextStyle::new(theme.muted).size(font));
                y += SUB_H;
            }
            Some(DiskLook::Done { look, then, grew }) => {
                let mut sub = format!("{} used \u{b7} {} free", folder::format_size(look.used()), folder::format_size(look.free));
                if let Some(t) = then {
                    let change = look.used() as i64 - t.used as i64;
                    if change.unsigned_abs() >= 1024 * 1024 * 1024 {
                        let word = if change > 0 { "less" } else { "more" };
                        sub.push_str(&format!(" \u{b7} {} {word} free than on {}", folder::format_size(change.unsigned_abs()), date_of(t.at)));
                    }
                }
                r.text(&sub, Rect::new(x, top + y, w, SUB_H), &TextStyle::new(theme.muted).size(font));
                y += SUB_H + 10.0;

                // The tabs.
                let mut tx = x;
                for p in Page::ALL {
                    let style = TextStyle::new(if p == self.page { theme.text_strong } else { theme.muted }).size(font + 2.0);
                    let tw = r.measure(p.label(), &style) + 4.0;
                    let rect = Rect::new(tx, top + y, tw, TABS_H);
                    if hover == Some(Spot::Tab(p)) && p != self.page {
                        r.text(p.label(), rect, &TextStyle { color: theme.text, ..style });
                    } else {
                        r.text(p.label(), rect, &style);
                    }
                    if p == self.page {
                        r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 2.0), theme.accent);
                    }
                    self.spots.push((Spot::Tab(p), rect));
                    tx += tw + 26.0;
                }
                r.fill_rect(Rect::new(x, top + y + TABS_H, w, 1.0), theme.line);
                y += TABS_H + 8.0;

                let rows = match self.page {
                    Page::Biggest => {
                        // A folder that isn't kept any more (a new look): back to the drive.
                        if look.tree.find(&self.at).is_none() {
                            self.at.clear();
                        }
                        if !self.at.is_empty() {
                            y = self.crumbs(r, theme, &drive_name, x, top, y, w, hover.as_ref());
                        }
                        biggest(look, &self.at)
                    }
                    Page::Grew => {
                        let note = match then {
                            None => Some("Glint remembers this scan. Next time, this shows which folders got bigger or smaller since today.".to_string()),
                            Some(t) if grew.is_empty() => Some(format!("No folder got bigger or smaller by 100 MB or more since {}.", date_of(t.at))),
                            Some(t) => Some(format!("Since {}:", date_of(t.at))),
                        };
                        if let Some(note) = note {
                            r.text(&note, Rect::new(x, top + y, w, SUB_H + 6.0), &TextStyle::new(theme.muted).size(font));
                            y += SUB_H + 10.0;
                        }
                        grew_rows(grew)
                    }
                    Page::Types => types(look),
                };
                for row in rows {
                    let rect = Rect::new(x, top + y, w, ROW_H);
                    let spot = Spot::Row(row.click.clone());
                    let hovered = row.click != Click::Nothing && self.mouse.is_some_and(|p| rect.contains(p) && area.contains(p));
                    self.paint_row(r, theme, &row, rect, hovered);
                    self.spots.push((spot, rect));
                    y += ROW_H;
                }
                if self.page != Page::Grew {
                    y += 18.0;
                    y = self.hidden(r, theme, &look.hidden, x, top, y, w);
                }
            }
        }
        self.height = y + 24.0;
    }

    /// The path over Biggest: "Local Disk (C:) › Users › me", each step clickable.
    #[allow(clippy::too_many_arguments)]
    fn crumbs(&mut self, r: &mut Renderer, theme: &Theme, drive_name: &str, x: f32, top: f32, y: f32, w: f32, hover: Option<&Spot>) -> f32 {
        let mut steps = vec![(drive_name.to_string(), String::new())];
        let mut rel = String::new();
        for part in self.at.split('\\').filter(|p| !p.is_empty()) {
            rel = if rel.is_empty() { part.to_string() } else { format!("{rel}\\{part}") };
            steps.push((part.to_string(), rel.clone()));
        }
        let mut cx = x;
        let last = steps.len() - 1;
        for (i, (name, rel)) in steps.into_iter().enumerate() {
            let here = i == last;
            let spot = Spot::Crumb(rel);
            let color = if here { theme.text_strong } else if hover == Some(&spot) { theme.accent } else { theme.text };
            let style = TextStyle::new(color).size(size::FONT);
            let tw = r.measure(&name, &style).min(w / 3.0) + 2.0;
            let rect = Rect::new(cx, top + y, tw, CRUMBS_H);
            r.text(&name, rect, &style);
            if !here {
                self.spots.push((spot, rect));
                r.text("\u{203a}", Rect::new(rect.right() + 6.0, top + y, 12.0, CRUMBS_H), &TextStyle::new(theme.muted).size(size::FONT));
            }
            cx = rect.right() + 22.0;
            if cx > x + w {
                break;
            }
        }
        y + CRUMBS_H + 4.0
    }

    fn paint_row(&self, r: &mut Renderer, theme: &Theme, row: &Row, b: Rect, hovered: bool) {
        if hovered {
            r.fill_paint(b, theme.radius.min(4.0), &theme.hover_paint);
        }
        let font = size::FONT;
        let bar_w = (b.w * 0.34).clamp(80.0, 300.0);
        let size_x = b.right() - SIZE_W - 8.0;
        let bar_x = size_x - 16.0 - bar_w;
        let text_w = (bar_x - b.x - 24.0).max(40.0);
        r.text(&row.name, Rect::new(b.x + 10.0, b.y + 4.0, text_w, 20.0), &TextStyle::new(theme.text_strong).size(font));
        r.text(&row.sub, Rect::new(b.x + 10.0, b.y + 22.0, text_w, 16.0), &TextStyle::new(theme.muted).size(font - 1.0));
        let track = Rect::new(bar_x, b.y + (b.h - BAR_H) / 2.0, bar_w, BAR_H);
        r.fill_round_rect(track, BAR_H / 2.0, theme.chip);
        if row.part > 0.0 {
            r.fill_round_rect(Rect::new(track.x, track.y, (track.w * row.part).max(BAR_H), BAR_H), BAR_H / 2.0, row.bar.unwrap_or(theme.accent));
        }
        let color = row.size_color.unwrap_or(theme.text);
        r.text(&row.size, Rect::new(size_x, b.y, SIZE_W, b.h), &TextStyle::new(color).size(font).align(Align::Right));
        r.fill_rect(Rect::new(b.x, b.bottom() - 1.0, b.w, 1.0), theme.line.with_alpha(theme.line.a * 0.5));
    }

    /// The box at the bottom: the space no folder shows, and what it is.
    #[allow(clippy::too_many_arguments)]
    fn hidden(&mut self, r: &mut Renderer, theme: &Theme, h: &diskspace::Hidden, x: f32, top: f32, y: f32, w: f32) -> f32 {
        if h.total() < 1024 * 1024 {
            return y;
        }
        let b = Rect::new(x, top + y, w, 58.0);
        r.fill_round_rect(b, theme.radius.min(6.0), theme.chip);
        let head = format!("{} no folder shows", folder::format_size(h.total()));
        r.text(&head, Rect::new(b.x + 14.0, b.y + 8.0, b.w - 28.0, 20.0), &TextStyle::new(theme.text_strong).size(size::FONT).semibold());
        let mut parts = Vec::new();
        for (name, bytes) in [("Recycle Bin", h.recycle_bin), ("Hibernation file", h.hibernation), ("Page file", h.page_file), ("Restore points and system files", h.other)] {
            if bytes >= 1024 * 1024 {
                parts.push(format!("{name} {}", folder::format_size(bytes)));
            }
        }
        r.text(&parts.join(" \u{b7} "), Rect::new(b.x + 14.0, b.y + 30.0, b.w - 28.0, 18.0), &TextStyle::new(theme.muted).size(size::FONT - 1.0));
        y + b.h
    }
}

/// A folder inside `drive` ("C:\\") as a real path.
fn real_path(drive: &str, rel: &str) -> String {
    if rel.is_empty() { drive.to_string() } else { folder::join(drive, rel) }
}

/// Seconds since 1970 as the user's short date ("24/10/2026").
fn date_of(at: u64) -> String {
    let filetime = (at + 11_644_473_600) * 10_000_000;
    let full = folder::format_date(filetime);
    full.split(' ').next().unwrap_or(&full).to_string()
}

/// Biggest's rows for the folder `at` inside the drive: its kept folders, then what's left.
fn biggest(look: &Look, at: &str) -> Vec<Row> {
    let Some(node) = look.tree.find(at) else { return Vec::new() };
    let used = look.used().max(1) as f32;
    let top = node.kids.first().map(|k| k.size).unwrap_or(0).max(node.rest()).max(1) as f32;
    let mut rows: Vec<Row> = node
        .kids
        .iter()
        .map(|k| {
            let rel = if at.is_empty() { k.name.clone() } else { format!("{at}\\{}", k.name) };
            let share = k.size as f32 / used * 100.0;
            let sub = if share >= 1.0 { format!("{share:.0}% of the used space") } else { "Folder".to_string() };
            Row {
                name: k.name.clone(),
                sub,
                part: k.size as f32 / top,
                bar: None,
                size: folder::format_size(k.size),
                size_color: None,
                click: if k.kids.is_empty() { Click::Open(rel) } else { Click::Into(rel) },
            }
        })
        .collect();
    let rest = node.rest();
    if rest > 0 {
        let name = if at.is_empty() { "Files on the drive itself and small folders".to_string() } else { "Files here and small folders".to_string() };
        rows.push(Row { name, sub: "Open the folder to see them".into(), part: rest as f32 / top, bar: Some(Color::hex(0x5a5a5a)), size: folder::format_size(rest), size_color: None, click: Click::Open(at.to_string()) });
    }
    rows
}

fn grew_rows(changes: &[diskspace::Change]) -> Vec<Row> {
    let top = changes.iter().map(|c| c.change.unsigned_abs()).max().unwrap_or(1).max(1) as f32;
    changes
        .iter()
        .map(|c| {
            let (name, parent) = match c.rel.rfind('\\') {
                Some(i) => (c.rel[i + 1..].to_string(), c.rel[..i].to_string()),
                None => (c.rel.clone(), String::new()),
            };
            let up = c.change > 0;
            let color = if up { GREW } else { SHRANK };
            let sign = if up { "+" } else { "\u{2212}" };
            Row {
                name,
                sub: if parent.is_empty() { "On the drive itself".into() } else { parent },
                part: c.change.unsigned_abs() as f32 / top,
                bar: Some(color),
                size: format!("{sign}{}", folder::format_size(c.change.unsigned_abs())),
                size_color: Some(color),
                click: Click::Open(c.rel.clone()),
            }
        })
        .collect()
}

fn types(look: &Look) -> Vec<Row> {
    let kinds = look.kinds_by_size();
    let top = kinds.first().map(|k| k.1).unwrap_or(1).max(1) as f32;
    kinds
        .into_iter()
        .map(|(k, bytes)| Row { name: k.name().into(), sub: k.examples().into(), part: bytes as f32 / top, bar: Some(kind_color(k)), size: folder::format_size(bytes), size_color: None, click: Click::Nothing })
        .collect()
}

fn kind_color(k: Kind) -> Color {
    Color::hex(match k {
        Kind::Videos => 0x5fb4f0,
        Kind::Games => 0x76b900,
        Kind::Apps => 0xa98ae0,
        Kind::Windows => 0x8a8a8a,
        Kind::Pictures => 0xe88aa8,
        Kind::Music => 0x4fc3a1,
        Kind::Documents => 0xe0d36a,
        Kind::Archives => 0xe0a33a,
        Kind::Other => 0x5a5a5a,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diskspace::Node;

    const GB: u64 = 1024 * 1024 * 1024;

    fn look() -> Look {
        let tree = Node {
            name: "C:".into(),
            size: 500 * GB,
            kids: vec![
                Node { name: "Users".into(), size: 300 * GB, kids: vec![Node { name: "me".into(), size: 290 * GB, kids: vec![] }] },
                Node { name: "Games".into(), size: 150 * GB, kids: vec![] },
            ],
        };
        Look { root: "C:\\".into(), tree, total: 1000 * GB, free: 400 * GB, ..Look::default() }
    }

    #[test]
    fn biggest_goes_into_folders_with_big_folders_and_opens_the_others() {
        let rows = biggest(&look(), "");
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Users", "Games", "Files on the drive itself and small folders"]);
        assert_eq!(rows[0].click, Click::Into("Users".into()));
        assert_eq!(rows[1].click, Click::Open("Games".into()));
        assert_eq!(rows[2].click, Click::Open(String::new()));
        assert_eq!(rows[0].part, 1.0);
        let inside = biggest(&look(), "Users");
        assert_eq!(inside[0].click, Click::Open("Users\\me".into()));
        assert_eq!(real_path("C:\\", "Users\\me"), "C:\\Users\\me");
        assert_eq!(real_path("C:\\", ""), "C:\\");
    }

    #[test]
    fn what_grew_shows_plus_and_minus() {
        let rows = grew_rows(&[diskspace::Change { rel: "Users\\me\\Videos".into(), change: 160 * GB as i64 }, diskspace::Change { rel: "Games".into(), change: -(20 * GB as i64) }]);
        assert_eq!(rows[0].name, "Videos");
        assert_eq!(rows[0].sub, "Users\\me");
        assert!(rows[0].size.starts_with('+'));
        assert!(rows[1].size.starts_with('\u{2212}'));
        assert_eq!(rows[1].sub, "On the drive itself");
    }
}
