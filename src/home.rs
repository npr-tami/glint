//! Home's page, laid out like File Explorer's Home: "Quick access" with a tile for each standard
//! folder (Desktop, Downloads, Documents, Pictures, Music, Videos) and each pinned folder, then
//! "Recent" and "Favorites" as two pill buttons over a list of files with the folder each is in
//! under its name.
//!
//! The items are the tab's own entries (recent.rs reads them; `Parts` says where each part
//! starts), so selecting, opening, Ctrl+C, Delete and the right-click menus work as in any folder.
//! The list view (list.rs) hands its painting and its mouse and keys here while Home is on screen.

use crate::app::{AppState, Tab};
use crate::fileops::same_path;
use crate::folder::{self, Entry};
use crate::recent::Parts;
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{size, Theme};
use crate::ui::{Point, Rect};

/// Room left and right of everything.
const PAD: f32 = 20.0;
/// A section's title line (the chevron and "Quick access", or the pills).
const HEAD_H: f32 = 40.0;
const TILE_H: f32 = 66.0;
const TILE_GAP: f32 = 4.0;
/// Narrower than this, a tile goes to the next line (at most three to a line).
const TILE_MIN_W: f32 = 230.0;
const MAX_COLUMNS: usize = 3;
const SECTION_GAP: f32 = 18.0;
/// The column titles over the list ("Name", "Date accessed").
const COLUMNS_H: f32 = 30.0;
/// A row of the list: two lines of text.
pub const ROW_H: f32 = 48.0;
/// The date column's width (left out when the list is narrow).
const DATE_W: f32 = 180.0;
const PILL_H: f32 = 30.0;

/// What is at a place on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    /// One of the tab's entries (a tile or a row).
    Item(usize),
    /// The chevron and title that fold "Quick access".
    Quick,
    /// The chevron that folds the lists below.
    Lists,
    /// The "Recent" and "Favorites" pills.
    Recent,
    Favorites,
}

/// Where everything goes, in page coordinates (0 at the top of the page, before scrolling).
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub spots: Vec<(Spot, Rect)>,
    /// Where the column titles are (when the list shows).
    pub columns: Option<Rect>,
    /// How tall the whole page is.
    pub height: f32,
}

impl Layout {
    /// The items in the page's order (tiles, then rows).
    pub fn items(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        self.spots.iter().filter_map(|&(s, r)| if let Spot::Item(i) = s { Some((i, r)) } else { None })
    }

    pub fn spot_at(&self, p: Point) -> Option<Spot> {
        self.spots.iter().find(|(_, r)| r.contains(p)).map(|&(s, _)| s)
    }

    pub fn rect_of(&self, index: usize) -> Option<Rect> {
        self.items().find(|&(i, _)| i == index).map(|(_, r)| r)
    }
}

/// What Home remembers between frames.
#[derive(Clone, Debug)]
pub struct Home {
    pub quick_open: bool,
    pub lists_open: bool,
    /// Favorites on show instead of Recent.
    pub favorites: bool,
    /// The last frame's layout (for the mouse and the keys).
    pub layout: Layout,
}

impl Default for Home {
    fn default() -> Self {
        Home { quick_open: true, lists_open: true, favorites: false, layout: Layout::default() }
    }
}

/// Lays the page out `width` wide. `shown(i)`: whether entry `i` is in the tab's view (hidden files
/// may be left out).
pub fn layout(width: f32, parts: Parts, count: usize, home: &Home, shown: impl Fn(usize) -> bool) -> Layout {
    let mut l = Layout::default();
    let inner = (width - 2.0 * PAD).max(100.0);
    let mut y = 4.0;
    l.spots.push((Spot::Quick, Rect::new(PAD - 6.0, y + 4.0, 150.0, HEAD_H - 8.0)));
    y += HEAD_H;
    if home.quick_open {
        let columns = ((inner / TILE_MIN_W) as usize).clamp(1, MAX_COLUMNS);
        let tile_w = (inner - TILE_GAP * (columns - 1) as f32) / columns as f32;
        let tiles: Vec<usize> = (0..parts.recent.min(count)).filter(|&i| shown(i)).collect();
        for (n, &i) in tiles.iter().enumerate() {
            let (c, row) = (n % columns, n / columns);
            let r = Rect::new(PAD + c as f32 * (tile_w + TILE_GAP), y + row as f32 * (TILE_H + TILE_GAP), tile_w, TILE_H);
            l.spots.push((Spot::Item(i), r));
        }
        let lines = tiles.len().div_ceil(columns);
        y += lines as f32 * (TILE_H + TILE_GAP);
    }
    y += SECTION_GAP;
    let pill_y = y + (HEAD_H - PILL_H) / 2.0;
    l.spots.push((Spot::Lists, Rect::new(PAD - 6.0, pill_y, 24.0, PILL_H)));
    l.spots.push((Spot::Recent, Rect::new(PAD + 24.0, pill_y, 96.0, PILL_H)));
    l.spots.push((Spot::Favorites, Rect::new(PAD + 128.0, pill_y, 110.0, PILL_H)));
    y += HEAD_H + 6.0;
    if home.lists_open {
        l.columns = Some(Rect::new(PAD, y, inner, COLUMNS_H));
        y += COLUMNS_H + 2.0;
        let range = if home.favorites { parts.favorites..count } else { parts.recent..parts.favorites.min(count) };
        for i in range.filter(|&i| shown(i)) {
            l.spots.push((Spot::Item(i), Rect::new(PAD, y, inner, ROW_H)));
            y += ROW_H;
        }
    }
    l.height = y + 16.0;
    l
}

/// The item to move to from `from` with an arrow key: `dx`/`dy` is -1, 0 or 1. Up and down go to
/// the nearest item on the line above or below; left and right to the one before or after.
pub fn step(l: &Layout, from: Option<usize>, dx: i32, dy: i32) -> Option<usize> {
    let items: Vec<(usize, Rect)> = l.items().collect();
    let Some(at) = from.and_then(|f| items.iter().position(|&(i, _)| i == f)) else { return items.first().map(|&(i, _)| i) };
    let cur = items[at].1;
    if dy == 0 {
        let to = at as i64 + dx as i64;
        return items.get(to.max(0) as usize).map(|&(i, _)| i).or(Some(items[at].0));
    }
    let cx = cur.x + cur.w / 2.0;
    let line = items.iter().filter(|(_, r)| if dy > 0 { r.y > cur.y + 1.0 } else { r.y < cur.y - 1.0 }).map(|(_, r)| r.y).fold(None, |best: Option<f32>, y| {
        Some(match best {
            Some(b) if (dy > 0 && b <= y) || (dy < 0 && b >= y) => b,
            _ => y,
        })
    });
    let Some(line) = line else { return Some(items[at].0) };
    items
        .iter()
        .filter(|(_, r)| (r.y - line).abs() < 1.0)
        .min_by(|a, b| (a.1.x + a.1.w / 2.0 - cx).abs().total_cmp(&(b.1.x + b.1.w / 2.0 - cx).abs()))
        .map(|&(i, _)| i)
}

/// The folder a file is in, shown short as Explorer does: inside the user's folder it's what comes
/// after it ("Downloads", "Pictures\Screenshots"), elsewhere the whole path.
pub fn short_folder(dir: &str, user: &str) -> String {
    let user = user.trim_end_matches('\\');
    if !user.is_empty() && dir.len() > user.len() + 1 && same_path(&dir[..user.len()], user) && dir[user.len()..].starts_with('\\') {
        return dir[user.len() + 1..].trim_end_matches('\\').to_string();
    }
    dir.to_string()
}

/// The text under an item's name: the folder it's in, or for a standard folder "Stored locally".
fn under_name(e: &Entry, index: usize, parts: Parts, user: &str) -> String {
    if index < parts.pinned {
        return "Stored locally".into();
    }
    let path = e.target.as_deref().unwrap_or(&e.name);
    match folder::parent(path) {
        Some(dir) => short_folder(&dir, user),
        None => path.to_string(),
    }
}

/// Draws the page into `area` scrolled by `scroll`, and keeps its layout in `home`.
#[allow(clippy::too_many_arguments)]
pub fn paint(r: &mut Renderer, theme: &Theme, app: &AppState, tab: &Tab, home: &mut Home, area: Rect, scroll: f32, mouse: Option<Point>, scale: f32) {
    let parts = tab.folder.home.as_deref().copied().unwrap_or_default();
    home.layout = layout(area.w - 8.0, parts, tab.folder.entries.len(), home, |i| tab.row_of(i).is_some());
    let l = &home.layout;
    let at = |rc: Rect| Rect::new(area.x + rc.x, area.y + rc.y - scroll, rc.w, rc.h);
    let hover = mouse.filter(|p| area.contains(*p)).and_then(|p| l.spot_at(Point::new(p.x - area.x, p.y - area.y + scroll)));
    let user = std::env::var("USERPROFILE").unwrap_or_default();
    let cut = app.cut_names(&tab.folder.path);
    // (The renderer makes every text the skin's size: these are sizes of the standard one.)
    let font = size::FONT;
    // 98 / 2000 wrote nothing smaller than their 8 pt, the skin's text size.
    let small = if theme.era.classic() { font } else { font - 1.0 };
    let hide_ext = app.settings.hide_known_extensions;
    let name_of = |e: &'_ Entry| -> String { crate::list::shown_name(e, hide_ext).to_string() };

    for &(spot, rc) in &l.spots {
        let b = at(rc);
        if b.bottom() < area.y || b.y > area.bottom() {
            continue;
        }
        let hovered = hover == Some(spot);
        if theme.era.old() && paint_era_spot(r, theme, home, spot, b, area, hovered) {
            continue;
        }
        match spot {
            Spot::Quick => {
                if hovered {
                    r.fill_paint(b, theme.radius.min(4.0), &theme.hover_paint);
                }
                chevron(r, Point::new(b.x + 12.0, b.y + b.h / 2.0), home.quick_open, theme.text);
                r.text("Quick access", Rect::new(b.x + 24.0, b.y, b.w - 24.0, b.h), &TextStyle::new(theme.text_strong).size(font + 2.0).semibold());
            }
            Spot::Lists => {
                if hovered {
                    r.fill_paint(b, theme.radius.min(4.0), &theme.hover_paint);
                }
                chevron(r, Point::new(b.x + 12.0, b.y + b.h / 2.0), home.lists_open, theme.text);
            }
            Spot::Recent | Spot::Favorites => {
                let on = (spot == Spot::Favorites) == home.favorites;
                let radius = b.h / 2.0;
                let ink = if on { theme.on_accent } else { theme.text };
                if on {
                    r.fill_round_rect(b, radius, theme.accent);
                } else {
                    r.fill_round_rect(b, radius, if hovered { theme.button_hover } else { theme.chip });
                    r.stroke_round_rect(b.inset(0.5, 0.5), radius - 0.5, 1.0, theme.line);
                }
                let c = Point::new(b.x + 20.0, b.y + b.h / 2.0);
                let label = if spot == Spot::Recent {
                    clock(r, c, ink);
                    "Recent"
                } else {
                    star(r, c, ink);
                    "Favorites"
                };
                r.text(label, Rect::new(b.x + 32.0, b.y, b.w - 36.0, b.h), &TextStyle::new(ink).size(font).semibold());
            }
            Spot::Item(i) => {
                let Some(e) = tab.folder.entries.get(i) else { continue };
                let selected = tab.is_selected(i);
                let is_tile = i < parts.recent;
                let faded = cut.is_some_and(|c| c.contains(&e.lower));
                if selected {
                    r.fill_paint(b, if is_tile { theme.radius.min(6.0) } else { 0.0 }, &theme.accent_tint_paint);
                    if !is_tile {
                        r.fill_rect(Rect::new(b.x, b.y, size::SELECT_EDGE, b.h), theme.select_edge);
                    }
                } else if hovered {
                    r.fill_paint(b, if is_tile { theme.radius.min(6.0) } else { 0.0 }, &theme.hover_paint);
                }
                // (An older Windows' selection is dark: faded names on it take its own colour.)
                let name_color = if (faded || e.is_hidden()) && !(selected && theme.era.old()) {
                    theme.muted
                } else if selected {
                    theme.selection_text
                } else {
                    theme.text_strong
                };
                let muted = if selected { theme.selection_details } else { theme.muted };
                let sub = under_name(e, i, parts, &user);
                if is_tile {
                    let pic = Rect::new(b.x + 10.0, b.y + (b.h - 48.0) / 2.0, 48.0, 48.0);
                    crate::list::picture(r, theme, app, &tab.folder.path, e, pic, scale);
                    let x = pic.right() + 12.0;
                    let w = b.right() - x - 8.0;
                    // (Older Windows wrote the names plainly, in the list's font.)
                    let name_style = if theme.era.old() { TextStyle::new(name_color).size(font) } else { TextStyle::new(name_color).size(font + 1.0).semibold() };
                    r.text(&e.name, Rect::new(x, b.y + 8.0, w, 20.0), &name_style);
                    r.text(&sub, Rect::new(x, b.y + 27.0, w, 16.0), &TextStyle::new(muted).size(small));
                    pin(r, Point::new(x + 5.0, b.y + 51.0), muted);
                } else {
                    let ir = Rect::new(b.x + 12.0, b.y + (b.h - size::ICON_LARGE) / 2.0, size::ICON_LARGE, size::ICON_LARGE);
                    if !crate::eraicons::draw_entry(r, theme.icons, &tab.folder.path, e, ir) {
                        let icon = app.icons.for_entry_sized(&tab.folder.path, e, true);
                        r.icon(&app.icons, icon, ir);
                    }
                    if faded {
                        r.fill_rect(ir, theme.panel.with_alpha(0.55));
                    }
                    let x = ir.right() + 12.0;
                    let date_w = if b.w > 520.0 { DATE_W } else { 0.0 };
                    let w = b.right() - date_w - x - 12.0;
                    r.text(&name_of(e), Rect::new(x, b.y + 6.0, w, 20.0), &TextStyle::new(name_color).size(font));
                    r.text(&sub, Rect::new(x, b.y + 25.0, w, 16.0), &TextStyle::new(muted).size(small));
                    if date_w > 0.0 && e.modified != 0 {
                        let date = folder::format_date_as(e.modified, app.settings.date_format);
                        r.text(&date, Rect::new(b.right() - date_w, b.y, date_w - 8.0, b.h), &TextStyle::new(muted).size(font));
                    }
                }
            }
        }
    }

    if let Some(c) = l.columns {
        let b = at(c);
        if theme.era.old() && b.bottom() >= area.y && b.y <= area.bottom() {
            // An older Windows' column titles (retrolist.rs).
            let date_w = if b.w > 520.0 { DATE_W } else { 0.0 };
            let date = if home.favorites { "Date modified" } else { "Date accessed" };
            let h = b.center(b.w, 22.0);
            crate::retrolist::header_band(r, theme, h);
            let name = Rect::new(h.x, h.y, h.w - date_w, h.h);
            crate::retrolist::header_item(r, theme, name, Rect::new(name.x + 12.0, name.y, name.w - 16.0, name.h), "Name", false, None, false);
            if date_w > 0.0 {
                let cell = Rect::new(name.right(), h.y, date_w, h.h);
                crate::retrolist::header_item(r, theme, cell, cell.inset(8.0, 0.0), date, false, None, false);
            }
            if theme.era.classic() {
                // 98 / 2000: the list in a sunken box, as their list views were.
                let last = l.spots.iter().filter(|&&(s, rc)| matches!(s, Spot::Item(_)) && rc.y > c.y).map(|&(_, rc)| at(rc).bottom()).fold(b.bottom() + 40.0, f32::max);
                let edge = Rect::new(h.x - 2.0, h.y - 2.0, h.w + 4.0, last - h.y + 4.0);
                crate::retro::edge(r, theme, edge, crate::retro::Edge::Sunken, theme.toolbar);
            }
        } else if b.bottom() >= area.y && b.y <= area.bottom() {
            let style = TextStyle::new(theme.muted).size(font);
            r.text("Name", Rect::new(b.x + 12.0, b.y, 200.0, b.h), &style);
            if b.w > 520.0 {
                let date = if home.favorites { "Date modified" } else { "Date accessed" };
                r.text(date, Rect::new(b.right() - DATE_W, b.y, DATE_W - 8.0, b.h), &style);
            }
            r.fill_rect(Rect::new(b.x, b.bottom(), b.w, 1.0), theme.line);
        }
        // Nothing in the list: say how things get there.
        let empty = !l.spots.iter().any(|&(s, rc)| matches!(s, Spot::Item(_)) && rc.y > c.y);
        if empty && tab.folder.error.is_none() {
            let text = if home.favorites { "Right-click a file or folder and pick Add to Favorites to see it here." } else { "Files you open show up here." };
            r.text(text, Rect::new(b.x + 12.0, b.bottom() + 8.0, b.w - 24.0, 24.0), &TextStyle::new(theme.muted).size(font).align(Align::Left));
        }
    }
}

/// Draws a heading, fold or switch of the page as an older Windows would (retrolist.rs); false for
/// the spots drawn the usual way (the items).
fn paint_era_spot(r: &mut Renderer, theme: &Theme, home: &Home, spot: Spot, b: Rect, area: Rect, hovered: bool) -> bool {
    let font = size::FONT;
    // The page's width, from the left edge of the headings to the right.
    let right = area.x + area.w - 8.0 - PAD;
    match spot {
        Spot::Quick => {
            crate::retrolist::fold(r, theme, Point::new(b.x + 12.0, b.y + b.h / 2.0), home.quick_open, hovered);
            let row = Rect::new(b.x + 6.0, b.y, right - b.x - 6.0, b.h);
            crate::retrolist::section_title(r, theme, row, Rect::new(b.x + 24.0, b.y, b.w, b.h), "Quick access", font + 1.0);
        }
        Spot::Lists => {
            let strip_x = b.right();
            let strip = Rect::new(strip_x, b.y - 1.0, (right - strip_x).max(0.0), b.h + 2.0);
            crate::retrolist::list_switch_strip(r, theme, strip);
            crate::retrolist::fold(r, theme, Point::new(b.x + 12.0, b.y + b.h / 2.0), home.lists_open, hovered);
        }
        Spot::Recent | Spot::Favorites => {
            let on = (spot == Spot::Favorites) == home.favorites;
            let rect = match theme.era {
                crate::theme::Era::Xp => Rect::new(b.x, b.bottom() - 23.0, b.w - 2.0, 23.0),
                _ => b.center(b.w - 2.0, 24.0),
            };
            let clock = |r: &mut Renderer, at: Rect| crate::retrolist::clock(r, theme, at);
            let star = |r: &mut Renderer, at: Rect| crate::retrolist::favorite(r, theme, at);
            if spot == Spot::Recent {
                crate::retrolist::list_switch(r, theme, rect, "Recent", on, hovered, &clock);
            } else {
                crate::retrolist::list_switch(r, theme, rect, "Favorites", on, hovered, &star);
            }
        }
        Spot::Item(_) => return false,
    }
    true
}

/// ˅ when open, › when folded.
fn chevron(r: &mut Renderer, c: Point, open: bool, color: crate::theme::Color) {
    let points = if open {
        [Point::new(c.x - 4.0, c.y - 2.0), Point::new(c.x, c.y + 2.0), Point::new(c.x + 4.0, c.y - 2.0)]
    } else {
        [Point::new(c.x - 2.0, c.y - 4.0), Point::new(c.x + 2.0, c.y), Point::new(c.x - 2.0, c.y + 4.0)]
    };
    r.polyline(&points, 1.4, color);
}

/// Points around a circle.
fn ring(c: Point, radius: f32, n: usize) -> Vec<Point> {
    (0..=n).map(|k| {
        let a = k as f32 / n as f32 * std::f32::consts::TAU;
        Point::new(c.x + radius * a.cos(), c.y + radius * a.sin())
    }).collect()
}

/// Recent's clock.
fn clock(r: &mut Renderer, c: Point, color: crate::theme::Color) {
    r.polyline(&ring(c, 6.5, 24), 1.3, color);
    r.polyline(&[Point::new(c.x, c.y - 4.0), Point::new(c.x, c.y), Point::new(c.x + 3.0, c.y + 1.5)], 1.3, color);
}

/// Favorites' star.
fn star(r: &mut Renderer, c: Point, color: crate::theme::Color) {
    let points: Vec<Point> = (0..=10)
        .map(|k| {
            let a = k as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
            let radius = if k % 2 == 0 { 7.0 } else { 3.0 };
            Point::new(c.x + radius * a.cos(), c.y + radius * a.sin())
        })
        .collect();
    r.polyline(&points, 1.2, color);
}

/// The small pin under a Quick access tile's name.
fn pin(r: &mut Renderer, c: Point, color: crate::theme::Color) {
    let head = [Point::new(c.x - 3.0, c.y - 5.0), Point::new(c.x + 3.0, c.y - 5.0), Point::new(c.x + 2.0, c.y - 1.0), Point::new(c.x + 4.0, c.y + 1.0), Point::new(c.x - 4.0, c.y + 1.0), Point::new(c.x - 2.0, c.y - 1.0), Point::new(c.x - 3.0, c.y - 5.0)];
    r.polyline(&head, 1.1, color);
    r.line(Point::new(c.x, c.y + 1.0), Point::new(c.x, c.y + 6.0), 1.1, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_show_short_inside_the_user_folder() {
        let user = "C:\\Users\\me";
        assert_eq!(short_folder("C:\\Users\\me\\Downloads", user), "Downloads");
        assert_eq!(short_folder("c:\\users\\ME\\Pictures\\Screenshots", user), "Pictures\\Screenshots");
        assert_eq!(short_folder("C:\\Users\\mel\\Documents", user), "C:\\Users\\mel\\Documents");
        assert_eq!(short_folder("D:\\Work", user), "D:\\Work");
        assert_eq!(short_folder("C:\\Users\\me", user), "C:\\Users\\me");
    }

    /// Six tiles in three columns, then the pills and the Recent rows; Favorites swaps the rows; a
    /// folded section leaves its items out.
    #[test]
    fn the_page_is_laid_out_like_explorers() {
        let parts = Parts { pinned: 6, recent: 7, favorites: 10 };
        let mut home = Home::default();
        let l = layout(900.0, parts, 12, &home, |_| true);
        let items: Vec<(usize, Rect)> = l.items().collect();
        assert_eq!(items.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(items[0].1.y, items[2].1.y, "three to a line");
        assert!(items[3].1.y > items[0].1.y);
        assert_eq!(items[6].1.y, items[3].1.y + TILE_H + TILE_GAP, "the pinned folder after them");
        assert_eq!(items[8].1.h, ROW_H);
        // Down from the second tile: the tile under it; from the last tile line: the first row.
        assert_eq!(step(&l, Some(1), 0, 1), Some(4));
        assert_eq!(step(&l, Some(6), 0, 1), Some(7));
        assert_eq!(step(&l, Some(7), 0, -1), Some(6));
        assert_eq!(step(&l, Some(9), 0, 1), Some(9), "the last stays");
        home.favorites = true;
        let l = layout(900.0, parts, 12, &home, |i| i != 11);
        assert_eq!(l.items().map(|(i, _)| i).filter(|&i| i >= 7).collect::<Vec<_>>(), [10]);
        home.quick_open = false;
        home.lists_open = false;
        let l = layout(900.0, parts, 12, &home, |_| true);
        assert_eq!(l.items().count(), 0);
        assert!(l.spot_at(Point::new(PAD + 30.0, l.spots[2].1.y + 5.0)) == Some(Spot::Recent));
    }
}
