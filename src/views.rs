//! Where the file list (list.rs) puts its items in each view - the arithmetic only, so it can be
//! tested on its own:
//!
//! - **Details** and **Tree**: one row per item, top to bottom, under the column titles
//!   (`Flow::Rows`).
//! - **List**: names in columns, top to bottom and then left to right, scrolling sideways, as in
//!   Explorer's List (`Flow::Columns`).
//! - **Medium / Large / Extra large icons**: tiles with a 48-, 96- or 256-DIP picture and the name
//!   under it, left to right and then top to bottom, spread to fill the width (`Flow::Grid`).
//!
//! An item is a "row" everywhere: its place in the tab's `view` (app.rs). The scroll position is the
//! tab's `scroll`: down for rows and tiles, sideways for the List view. A "line" is what one step
//! of scrolling moves by: a row, a row of tiles, a column of names.

use std::ops::Range;

use crate::settings::ViewMode;
use crate::ui::Rect;

/// Room kept free for the scrollbar: on the right, or at the bottom in the List view.
pub const SCROLL_GUTTER: f32 = 14.0;
/// Space around the tiles of the icon views, and before the List view's first column.
pub const GRID_PAD: f32 = 8.0;
/// Space between two tiles (half of it on each side of a tile).
pub const TILE_GAP: f32 = 4.0;
/// One line of a tile's name.
pub const NAME_LINE_H: f32 = 16.0;
/// Space in a tile above its picture and below its name, and between the two.
pub const TILE_PAD: f32 = 8.0;
pub const TILE_TEXT_GAP: f32 = 6.0;
/// A column of the List view, at the normal text size.
pub const LIST_COLUMN_W: f32 = 250.0;

/// How the items flow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// One item per row, top to bottom (Details, Tree).
    Rows,
    /// Names in columns: top to bottom, then left to right; scrolls sideways (List).
    Columns,
    /// Tiles in rows: left to right, then top to bottom (the icon views).
    Grid,
}

impl Flow {
    pub fn of(mode: ViewMode) -> Flow {
        match mode {
            ViewMode::Details | ViewMode::Tree => Flow::Rows,
            ViewMode::List => Flow::Columns,
            ViewMode::Medium | ViewMode::Large | ViewMode::ExtraLarge => Flow::Grid,
        }
    }
}

/// The size of an icon view's tiles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// The picture's box (square), in DIPs.
    pub picture: f32,
    /// The narrowest a tile gets (they're spread to fill the width).
    pub min_w: f32,
    /// Lines of the name under the picture.
    pub lines: usize,
}

impl Tile {
    pub fn of(mode: ViewMode) -> Tile {
        match mode {
            ViewMode::Large => Tile { picture: 96.0, min_w: 136.0, lines: 2 },
            ViewMode::ExtraLarge => Tile { picture: 256.0, min_w: 280.0, lines: 2 },
            _ => Tile { picture: 48.0, min_w: 104.0, lines: 2 },
        }
    }

    /// The whole tile's height: room, picture, room, the name's lines, room.
    pub fn h(&self) -> f32 {
        TILE_PAD + self.picture + TILE_TEXT_GAP + self.lines as f32 * NAME_LINE_H + TILE_PAD
    }
}

/// A key that moves the cursor (see `Geometry::step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    Left,
    Right,
}

/// Where the items of one listing sit in the list, for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub flow: Flow,
    /// Where the items scroll (under the column titles when there are some).
    pub area: Rect,
    /// One item's cell.
    pub cell_w: f32,
    pub cell_h: f32,
    /// Items in a row of tiles or a column of names; 1 for rows.
    pub per_line: usize,
    /// How many items there are.
    pub count: usize,
    /// Space before the first line and around the tiles.
    pub pad: f32,
}

impl Geometry {
    /// Details and Tree: rows `row_h` high, as wide as the list.
    pub fn rows(area: Rect, row_h: f32, count: usize) -> Geometry {
        Geometry { flow: Flow::Rows, area, cell_w: area.w, cell_h: row_h.max(1.0), per_line: 1, count, pad: 0.0 }
    }

    /// List: columns of names `col_w` wide, `row_h` per name, as many names per column as fit
    /// above the scrollbar's room at the bottom.
    pub fn columns(area: Rect, row_h: f32, col_w: f32, count: usize) -> Geometry {
        let row_h = row_h.max(1.0);
        let usable = area.h - SCROLL_GUTTER - 2.0 * 2.0;
        let per_line = ((usable / row_h).floor() as usize).max(1);
        Geometry { flow: Flow::Columns, area, cell_w: col_w.max(40.0), cell_h: row_h, per_line, count, pad: 2.0 }
    }

    /// The icon views: as many tiles across as fit (at least `tile.min_w` wide each), spread to
    /// fill the width.
    pub fn grid(area: Rect, tile: Tile, count: usize) -> Geometry {
        let usable = area.w - SCROLL_GUTTER - 2.0 * GRID_PAD;
        let per_line = ((usable / tile.min_w).floor() as usize).max(1);
        let cell_w = (usable / per_line as f32).max(tile.min_w);
        Geometry { flow: Flow::Grid, area, cell_w, cell_h: tile.h(), per_line, count, pad: GRID_PAD }
    }

    /// Whether it scrolls sideways (the List view).
    pub fn sideways(&self) -> bool {
        self.flow == Flow::Columns
    }

    /// How many lines there are (rows, rows of tiles, columns of names).
    pub fn lines(&self) -> usize {
        self.count.div_ceil(self.per_line.max(1))
    }

    /// The length of one line in the direction the list scrolls.
    pub fn line(&self) -> f32 {
        if self.sideways() {
            self.cell_w
        } else {
            self.cell_h
        }
    }

    /// The length of everything, in the direction the list scrolls.
    pub fn content(&self) -> f32 {
        match self.flow {
            // (A little room under the last row, as before there were views.)
            Flow::Rows => self.count as f32 * self.cell_h + 4.0,
            Flow::Columns | Flow::Grid => 2.0 * self.pad + self.lines() as f32 * self.line(),
        }
    }

    /// The length of what's on screen, in the direction the list scrolls.
    pub fn view(&self) -> f32 {
        if self.sideways() {
            self.area.w
        } else {
            self.area.h
        }
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content() - self.view()).max(0.0)
    }

    /// The line item `row` is in, and its place in that line.
    fn place(&self, row: usize) -> (usize, usize) {
        let n = self.per_line.max(1);
        (row / n, row % n)
    }

    /// Where item `row`'s cell is on screen, with the list scrolled by `scroll`.
    pub fn cell(&self, row: usize, scroll: f32) -> Rect {
        let (line, k) = self.place(row);
        let a = self.area;
        match self.flow {
            Flow::Rows => Rect::new(a.x, a.y + row as f32 * self.cell_h - scroll, a.w, self.cell_h),
            Flow::Columns => Rect::new(a.x + self.pad + line as f32 * self.cell_w - scroll, a.y + self.pad + k as f32 * self.cell_h, self.cell_w, self.cell_h),
            Flow::Grid => Rect::new(a.x + self.pad + k as f32 * self.cell_w, a.y + self.pad + line as f32 * self.cell_h - scroll, self.cell_w, self.cell_h),
        }
    }

    /// The tile drawn in a cell of the icon views (the space between tiles is empty space).
    pub fn tile(cell: Rect) -> Rect {
        cell.inset(TILE_GAP / 2.0, TILE_GAP / 2.0)
    }

    /// The item at line `line`, place `k` (None past the end).
    fn item(&self, line: usize, k: usize) -> Option<usize> {
        (k < self.per_line).then_some(line * self.per_line + k).filter(|&r| r < self.count)
    }

    /// The item under `p` (None over empty space: past the last item, between tiles, below the
    /// List view's columns).
    pub fn row_at(&self, p: crate::ui::Point, scroll: f32) -> Option<usize> {
        let a = self.area;
        if !a.contains(p) {
            return None;
        }
        match self.flow {
            Flow::Rows => {
                let row = ((p.y - a.y + scroll) / self.cell_h).floor();
                (row >= 0.0 && (row as usize) < self.count).then_some(row as usize)
            }
            Flow::Columns => {
                let (x, y) = (p.x - a.x - self.pad + scroll, p.y - a.y - self.pad);
                if x < 0.0 || y < 0.0 {
                    return None;
                }
                self.item((x / self.cell_w) as usize, (y / self.cell_h) as usize)
            }
            Flow::Grid => {
                let (x, y) = (p.x - a.x - self.pad, p.y - a.y - self.pad + scroll);
                if x < 0.0 || y < 0.0 {
                    return None;
                }
                let row = self.item((y / self.cell_h) as usize, (x / self.cell_w) as usize)?;
                Self::tile(self.cell(row, scroll)).contains(p).then_some(row)
            }
        }
    }

    /// The items whose cells are (partly) on screen.
    pub fn visible(&self, scroll: f32) -> Range<usize> {
        let line = self.line();
        let first = ((scroll - self.pad) / line).floor().max(0.0) as usize;
        let last = ((scroll + self.view() - self.pad) / line).ceil().max(0.0) as usize;
        (first * self.per_line).min(self.count)..(last.saturating_mul(self.per_line)).min(self.count)
    }

    /// The items whose tiles (or rows) touch `r`, a rectangle on screen with the list scrolled by
    /// `scroll` (selecting with a rectangle dragged over the empty space).
    pub fn touching(&self, r: Rect, scroll: f32) -> Vec<usize> {
        let meets = |a: Rect, b: Rect| a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom();
        let range = self.visible_in(r, scroll);
        range
            .filter(|&row| {
                let cell = self.cell(row, scroll);
                meets(if self.flow == Flow::Grid { Self::tile(cell) } else { cell }, r)
            })
            .collect()
    }

    /// The items whose lines fall within `r` (in the direction the list scrolls).
    fn visible_in(&self, r: Rect, scroll: f32) -> Range<usize> {
        let (from, len) = if self.sideways() { (r.x - self.area.x, r.w) } else { (r.y - self.area.y, r.h) };
        let line = self.line();
        let first = ((from + scroll - self.pad) / line).floor().max(0.0) as usize;
        let last = ((from + len + scroll - self.pad) / line).ceil().max(0.0) as usize + 1;
        (first * self.per_line).min(self.count)..(last.saturating_mul(self.per_line)).min(self.count)
    }

    /// Where to scroll so item `row` is fully on screen (`scroll` when it already is).
    pub fn reveal(&self, row: usize, scroll: f32) -> f32 {
        let (line, _) = self.place(row);
        let (start, margin) = match self.flow {
            Flow::Rows => (row as f32 * self.cell_h, 4.0),
            _ => (self.pad + line as f32 * self.line(), self.pad),
        };
        let end = start + self.line();
        let to = if start < scroll {
            if line == 0 {
                0.0
            } else {
                start - if self.flow == Flow::Rows { 0.0 } else { margin }
            }
        } else if end > scroll + self.view() {
            end - self.view() + margin
        } else {
            scroll
        };
        to.clamp(0.0, self.max_scroll())
    }

    /// Where to scroll to put item `row` in the middle (far away: going up to a big folder).
    pub fn centre(&self, row: usize) -> f32 {
        let (line, _) = self.place(row);
        let start = self.pad + line as f32 * self.line();
        (start - self.view() / 2.0 + self.line() / 2.0).clamp(0.0, self.max_scroll())
    }

    /// Whether item `row` is fully on screen.
    pub fn shows(&self, row: usize, scroll: f32) -> bool {
        (self.reveal(row, scroll) - scroll).abs() < 0.5
    }

    /// The item an arrow key moves to from `row`: up and down a column, left and right along a row
    /// of tiles (on to the next row at its end, as in Explorer), and sideways a whole column in the
    /// List view. A step past the end goes to the last item when there's a line further on.
    pub fn step(&self, row: usize, key: Step) -> usize {
        if self.count == 0 {
            return 0;
        }
        let last = self.count - 1;
        let row = row.min(last);
        let n = self.per_line.max(1);
        let back = |by: usize| row.checked_sub(by).unwrap_or(row);
        let on = |by: usize| {
            if row + by <= last {
                row + by
            } else if row / n < last / n {
                last // into the shorter last line
            } else {
                row
            }
        };
        match (self.flow, key) {
            (Flow::Rows, Step::Up) | (Flow::Columns, Step::Up) | (Flow::Grid, Step::Left) => back(1),
            (Flow::Rows, Step::Down) | (Flow::Columns, Step::Down) | (Flow::Grid, Step::Right) => (row + 1).min(last),
            (Flow::Grid, Step::Up) | (Flow::Columns, Step::Left) => back(n),
            (Flow::Grid, Step::Down) | (Flow::Columns, Step::Right) => on(n),
            (Flow::Rows, Step::Left | Step::Right) => row,
        }
    }

    /// How many items a page (PgUp / PgDn) moves by: a screenful of lines.
    pub fn page(&self) -> usize {
        let lines = (self.view() / self.line()).floor() as usize;
        match self.flow {
            Flow::Rows => lines.max(2) - 1,
            _ => lines.max(1) * self.per_line,
        }
    }

    /// The first item at least partly on screen (where paging starts without a cursor).
    pub fn first_shown(&self, scroll: f32) -> usize {
        let line = ((scroll - self.pad) / self.line()).ceil().max(0.0) as usize;
        (line * self.per_line).min(self.count.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Point;

    const AREA: Rect = Rect::new(100.0, 50.0, 614.0, 400.0);

    #[test]
    fn rows_are_as_they_always_were() {
        let g = Geometry::rows(AREA, 28.0, 100);
        assert_eq!(g.content(), 100.0 * 28.0 + 4.0);
        assert_eq!(g.cell(2, 10.0), Rect::new(100.0, 50.0 + 56.0 - 10.0, 614.0, 28.0));
        assert_eq!(g.row_at(Point::new(150.0, 50.0 + 29.0), 0.0), Some(1));
        assert_eq!(g.row_at(Point::new(150.0, 50.0 + 1.0), 28.0), Some(1), "scrolled by a row");
        assert_eq!(Geometry::rows(AREA, 28.0, 1).row_at(Point::new(150.0, 50.0 + 40.0), 0.0), None, "past the last item");
        assert_eq!((g.step(5, Step::Up), g.step(5, Step::Down), g.step(5, Step::Left)), (4, 6, 5));
        assert_eq!(g.page(), 13);
        // Revealing a row below the screen puts it at the bottom, with a little room.
        let s = g.reveal(20, 0.0);
        assert_eq!(s, 21.0 * 28.0 - 400.0 + 4.0);
        assert!(g.shows(20, s));
    }

    #[test]
    fn tiles_fill_the_width_and_wrap() {
        let tile = Tile::of(ViewMode::Large);
        let g = Geometry::grid(AREA, tile, 10);
        // 614 - 14 - 16 = 584 wide: four tiles of at least 136 fit, spread to 146 each.
        assert_eq!(g.per_line, 4);
        assert_eq!(g.cell_w, 146.0);
        assert_eq!(g.lines(), 3);
        assert_eq!(g.cell(5, 0.0), Rect::new(100.0 + 8.0 + 146.0, 50.0 + 8.0 + tile.h(), 146.0, tile.h()));
        // In a tile it's the item; between tiles it's empty space.
        let c = g.cell(5, 0.0);
        assert_eq!(g.row_at(Point::new(c.x + 20.0, c.y + 20.0), 0.0), Some(5));
        assert_eq!(g.row_at(Point::new(c.x + 0.5, c.y + 20.0), 0.0), None);
        assert_eq!(g.row_at(Point::new(AREA.x + 8.0 + 3.0 * 146.0 + 40.0, AREA.y + 8.0 + 2.0 * tile.h() + 20.0), 0.0), None, "past the last item");
        // Arrows move in two directions; down from a full row into the shorter last row.
        assert_eq!((g.step(5, Step::Left), g.step(5, Step::Right), g.step(5, Step::Up), g.step(5, Step::Down)), (4, 6, 1, 9));
        assert_eq!(g.step(7, Step::Down), 9, "the last row has only two: the last item");
        assert_eq!(g.step(9, Step::Down), 9);
        assert_eq!(g.step(3, Step::Right), 4, "on to the next row at the end of one");
        assert_eq!(g.step(0, Step::Up), 0);
        // What's on screen: whole rows of tiles.
        assert_eq!(g.visible(0.0), 0..10);
        let tall = Geometry::grid(AREA, tile, 100);
        assert_eq!(tall.visible(tile.h() * 3.0 + 8.0), 12..24);
        assert_eq!(tall.page(), 8, "two rows of four");
    }

    #[test]
    fn the_list_view_flows_down_then_across() {
        let g = Geometry::columns(AREA, 28.0, 250.0, 40);
        // 400 - 14 - 4 = 382 high: 13 names per column.
        assert_eq!(g.per_line, 13);
        assert_eq!(g.lines(), 4);
        assert!(g.sideways());
        assert_eq!(g.cell(14, 0.0), Rect::new(100.0 + 2.0 + 250.0, 50.0 + 2.0 + 28.0, 250.0, 28.0));
        assert_eq!(g.cell(14, 100.0).x, 100.0 + 2.0 + 250.0 - 100.0, "scrolls sideways");
        assert_eq!(g.row_at(Point::new(100.0 + 260.0, 50.0 + 40.0), 0.0), Some(14));
        assert_eq!(g.row_at(Point::new(100.0 + 260.0, 50.0 + 380.0), 0.0), None, "below the names, above the scrollbar");
        assert_eq!((g.step(14, Step::Up), g.step(14, Step::Down), g.step(14, Step::Left), g.step(14, Step::Right)), (13, 15, 1, 27));
        assert_eq!(g.step(30, Step::Right), 39, "into the shorter last column");
        assert_eq!(g.content(), 4.0 + 4.0 * 250.0);
        assert_eq!(g.max_scroll(), 1004.0 - 614.0);
        assert_eq!(g.reveal(39, 0.0), 1004.0 - 614.0, "the last column comes into view");
    }

    #[test]
    fn a_dragged_rectangle_touches_tiles() {
        let tile = Tile::of(ViewMode::Medium);
        let g = Geometry::grid(AREA, tile, 30);
        let (a, b) = (g.cell(0, 0.0), g.cell(g.per_line + 1, 0.0));
        let band = Rect::new(a.x + 10.0, a.y + 10.0, b.x + 10.0 - (a.x + 10.0), b.y + 10.0 - (a.y + 10.0));
        let mut hit = g.touching(band, 0.0);
        hit.sort_unstable();
        assert_eq!(hit, vec![0, 1, g.per_line, g.per_line + 1]);
        assert!(g.touching(Rect::new(AREA.x, AREA.y, 3.0, 3.0), 0.0).is_empty(), "the margin touches nothing");
    }

    #[test]
    fn nothing_breaks_on_an_empty_or_tiny_list() {
        for g in [Geometry::rows(AREA, 28.0, 0), Geometry::columns(Rect::new(0.0, 0.0, 10.0, 10.0), 28.0, 250.0, 0), Geometry::grid(Rect::new(0.0, 0.0, 10.0, 10.0), Tile::of(ViewMode::ExtraLarge), 0)] {
            assert_eq!(g.visible(0.0), 0..0);
            assert_eq!(g.step(0, Step::Down), 0);
            assert_eq!(g.max_scroll(), (g.content() - g.view()).max(0.0));
            assert!(g.per_line >= 1);
        }
    }
}
