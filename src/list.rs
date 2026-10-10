//! The file list: a column header and the rows, drawn by us and "virtualized" - only the rows on
//! screen are drawn, so 10 rows or 100,000 rows cost the same per frame.
//!
//! What it does: click to select, Ctrl/Shift-click to select more, double-click or Enter to open,
//! Ctrl+Enter or middle-click to open a folder in a new tab, right-click (or the Apps key, Shift+F10)
//! for the menus in menu.rs, arrows/Home/End/PgUp/PgDn (Shift extends), type letters to jump,
//! smooth (eased) wheel scrolling, a thin scrollbar you can drag, click a column title to sort
//! (again to reverse), drag a column title's left edge to resize it.
//!
//! The scroll position, selection and cursor live in the tab (app.rs), so every tab keeps its own.
//!
//! Working with files (app/files.rs does the deciding, fileops.rs the work): Ctrl+C / Ctrl+X /
//! Ctrl+V, Del / Shift+Del, Ctrl+Shift+N, Ctrl+Shift+C, and the items in the right-click menu.
//! Renaming (F2, or a slow second click on a selected name) opens a name box over the name: a real
//! Windows text box like the path box, with the name selected up to its extension. Enter renames,
//! Esc cancels, and clicking or scrolling elsewhere renames too, as in Explorer. Items cut to the
//! clipboard are drawn faded until they're pasted.
//!
//! The list's options (settings.json; right-click a column title for most of
//! them):
//! - which columns show next to Name ("Columns": Date modified, Date created, Type, Size), plus a
//!   Folder column while subfolders are searched (search.rs);
//! - folders above files or mixed with them ("FoldersFirst");
//! - how dates and sizes look ("DateFormat", "SizeFormat"; folder.rs formats them);
//! - the row and text size ("Size": small, medium with more room, large with bigger text and
//!   32-pixel icons);
//! - file name extensions left out for types Windows knows ("HideKnownExtensions"); renaming keeps
//!   the extension then;
//! - one click opens instead of two ("SingleClick"), and double-clicking the empty space goes up
//!   ("DoubleClickUp").
//!
//! Views (the VIEW button, Ctrl+Shift+1 to 6; settings.json "View" and each tab's own): Details (the
//! rows above), List (names in columns flowing left to right, scrolling sideways), Medium, Large and
//! Extra large icons (tiles with a 48-, 96- or 256-DIP picture and the name under it: thumbnails of
//! pictures, videos and PDFs, Windows' big icons for the rest, from thumbs.rs), and Tree (Details,
//! with folders opening in place under a ▸ arrow; app/tree.rs). views.rs does the arithmetic of
//! where items go; everything above works the same in every view: the arrows move in two directions
//! among tiles, the name box sits under a tile's picture, and dragging a rectangle over the empty
//! space selects what it touches.
//!
//! Drag and drop (dragdrop.rs, window/drag.rs): pressing a selected item and moving the mouse past
//! Windows' drag distance drags the selection out - with the right button too, whose menu then
//! comes from the program dropped into. So a plain click on one of several selected items selects
//! only that one when the button comes up (not when it goes down), Ctrl-clicking a selected item
//! unselects it on the way up, and the right-click menu opens when the button comes up, as in
//! Explorer. Something dragged over the list goes into the folder row under the mouse (which is
//! highlighted) or into the folder on screen; near the top or bottom edge the list scrolls.

use std::collections::BTreeMap;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, VK_A, VK_C, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F2, VK_HOME, VK_INSERT, VK_LEFT, VK_N, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP, VK_V, VK_X,
};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, SystemParametersInfoW, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
// Dragging items out, and things dragged in (dragdrop.rs).
use crate::ui::{DragOut, DropTarget};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};

use crate::app::{AppState, SortKey, Tab, THIS_PC_NOTE};
use crate::fileops;
use crate::folder::{self, Entry, EntryKind};
use crate::frame;
use crate::icons;
use crate::recycle::{self, BinItem};
use crate::pathbar::{fill_box_paint, outline_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::settings::{DateFormat, ListSize, SizeFormat, ViewMode};
use crate::thumbs;
use crate::views::{Flow, Geometry, Step, Tile, LIST_COLUMN_W, NAME_LINE_H, SCROLL_GUTTER, TILE_PAD, TILE_TEXT_GAP};
use crate::textbox::{TextBox, TextEvent, TextEventKind};
use crate::theme::{size, ButtonStyle, Paint, Theme};
use crate::ui::{Anim, Command, Component, Ctx, CursorKind, Effect, Menu, MenuItem, MouseButton, MouseEvent, Point, Rect};
// The right-click menus (menu.rs) and their keys.
use crate::menu;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_APPS, VK_F10, VK_SHIFT};

/// How long a wheel scroll glides, in seconds.
const SCROLL_TIME: f64 = 0.12;
/// How far into the glide the first frame starts (about one frame at 120 Hz).
const FRAME_HEAD_START: f64 = 0.008;
/// Space left of the icon (the red selection edge sits in it).
const ROW_PAD: f32 = 12.0;
const MIN_COL: f32 = 48.0;
/// The tree view: how far each level is pulled in, and the room for the ▸ arrow before the icon.
const TREE_INDENT: f32 = 16.0;
const TREE_ARROW: f32 = 16.0;
const MIN_NAME: f32 = 120.0;
/// How close (DIPs) to a column edge the mouse must be to resize it.
const EDGE_GRAB: f32 = 4.0;
/// With one-click opening: a click whose button comes up further than this from where it went
/// down (DIPs) doesn't open (it was a drag).
const CLICK_SLOP: f32 = 4.0;

/// The name box's id (text boxes 300-399 belong to the file list).
const RENAME_BOX: u16 = 300;
/// The window timer that starts renaming after a slow second click; window.rs hands it to
/// [`ListView::slow_click_due`].
pub const SLOW_CLICK_TIMER: usize = 10;

/// The column titles' right-click menu (its tag; the files menus are menu.rs's 1 and 2).
const MENU_HEADER: u32 = 10;
// Its items.
const HEAD_MODIFIED: u32 = 1;
const HEAD_CREATED: u32 = 2;
const HEAD_TYPE: u32 = 3;
const HEAD_SIZE: u32 = 4;
const HEAD_FOLDERS_FIRST: u32 = 10;
const HEAD_EXTENSIONS: u32 = 11;
const HEAD_DATE_DEFAULT: u32 = 20;
const HEAD_DATE_RELATIVE: u32 = 21;
const HEAD_DATE_ISO: u32 = 22;
const HEAD_SIZE_AUTO: u32 = 30;
const HEAD_SIZE_KB: u32 = 31;
const HEAD_ROWS_SMALL: u32 = 40;
const HEAD_ROWS_MEDIUM: u32 = 41;
const HEAD_ROWS_LARGE: u32 = 42;

/// A column of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Col {
    Name,
    Modified,
    Created,
    Type,
    Size,
    /// Where a search result is, below the folder searched.
    Folder,
    /// This PC: a drive's free space.
    Free,
    /// The Recycle Bin's "Original location": the folder an item was deleted from.
    Location,
}

impl Col {
    /// Its name in settings.json ("Columns", "ColumnWidthMap").
    fn key(self) -> &'static str {
        match self {
            Col::Name => "name",
            Col::Modified => "modified",
            Col::Created => "created",
            Col::Type => "type",
            Col::Size => "size",
            Col::Folder => "folder",
            Col::Free => "free",
            Col::Location => "location",
        }
    }

    fn default_width(self) -> f32 {
        match self {
            Col::Size => 90.0,
            Col::Free => 110.0,
            Col::Folder => 220.0,
            Col::Location => 260.0,
            _ => 150.0,
        }
    }

    /// Its title; `bin`: in the Recycle Bin, whose dates are when items were deleted.
    fn title(self, this_pc: bool, bin: bool) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Modified if bin => "Date deleted",
            Col::Modified => "Date modified",
            Col::Created => "Date created",
            Col::Type => "Type",
            Col::Size if this_pc => "Total size",
            Col::Size => "Size",
            Col::Folder => "Folder",
            Col::Free => "Free space",
            Col::Location => "Original location",
        }
    }

    /// What clicking its title sorts by (None: it doesn't sort).
    fn sort_key(self) -> Option<SortKey> {
        Some(match self {
            Col::Name => SortKey::Name,
            // (This PC's "Free space" sorts as Date always did there: drives then go A to Z.)
            Col::Modified | Col::Free => SortKey::Date,
            Col::Created => SortKey::Created,
            Col::Type => SortKey::Type,
            Col::Size => SortKey::Size,
            Col::Folder => SortKey::Folder,
            Col::Location => return None,
        })
    }

    fn right_aligned(self) -> bool {
        self == Col::Size
    }
}

/// The columns next to Name that settings.json can switch on, in the list's order.
const OPTIONAL: [Col; 4] = [Col::Modified, Col::Created, Col::Type, Col::Size];

/// A column where it sits now.
#[derive(Clone, Copy, Debug)]
struct Placed {
    col: Col,
    x: f32,
    w: f32,
}

/// The item whose name is being edited.
struct Rename {
    tab_id: u64,
    /// The listing it's in: the tab's folder (for search results, the folder searched).
    listing: String,
    /// The folder the item itself is in, and its name before editing.
    folder: String,
    name: String,
    /// Its full path: how it's found again (search results can share a name).
    path: String,
    /// Its index in the listing (looked up again by path if the listing is read again).
    index: usize,
    /// With known extensions left out of names: the extension (".pdf") the box doesn't show; it's
    /// put back on what was typed.
    hidden_ext: String,
}

/// A click on the name of the one selected item: if no second click makes it a double-click,
/// renaming starts (Explorer's "slow double-click").
struct SlowClick {
    tab_id: u64,
    index: usize,
    /// The button came up on it and the timer runs.
    armed: bool,
}

/// With "SingleClick" on: the item a click went down on, opened when the button comes up on it.
struct ClickOpen {
    tab_id: u64,
    folder: String,
    index: usize,
    /// The item itself (`Tab::item_key`): a re-read meanwhile may have put another one at `index`.
    key: String,
    at: Point,
}

/// A mouse button went down on the list and hasn't come up yet.
#[derive(Clone, Debug)]
struct Press {
    at: Point,
    button: MouseButton,
    tab_id: u64,
    /// On an item (moving away drags the selection out) or on empty space.
    on_item: bool,
    /// What a left click finishes with when the button comes up without a drag.
    then: Then,
}

/// The rest of a left click, done when the button comes up without a drag. Items by their key
/// (`Tab::item_key`: the name, or where a search result or Recycle Bin item really is), so a re-read
/// of the folder meanwhile can't make it pick another item.
#[derive(Clone, Debug, PartialEq)]
enum Then {
    Nothing,
    /// Select only this item (a plain click on one of several selected items).
    SelectOnly(String),
    /// Unselect this item (Ctrl+click on a selected item).
    Unselect(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    None,
    /// Dragging the scrollbar thumb; `grab` is where on the thumb it was taken (from its top, or
    /// its left in the List view).
    Thumb { grab: f32 },
    /// Resizing the column `col` by its left edge.
    Column { col: Col, start_x: f32, start_w: f32 },
}

/// A rectangle dragged over the empty space, selecting what it touches.
struct Band {
    tab_id: u64,
    /// Where the button went down, in the list's own coordinates (scrolled with it: the scroll
    /// position is added down, or across in the List view).
    start: Point,
    /// Where the mouse is now, on screen.
    now: Point,
    /// What was selected before (with Ctrl, the rectangle adds to it), by entry.
    base: Vec<bool>,
    /// The mouse moved past Windows' drag distance: it's a rectangle, not a click.
    active: bool,
    /// The last frame's time, for scrolling near the edges.
    at: f64,
}

pub struct ListView {
    rect: Rect,
    /// Widths of the columns next to Name as last set (by default or by dragging an edge), by key.
    wanted: BTreeMap<&'static str, f32>,
    /// The columns as they sit now (from the last `place_columns`): Name first, then the others,
    /// squeezed when the window is too narrow for them. Name takes the rest.
    cols: Vec<Placed>,
    /// Row height, icon and text size, from the "Size" setting at the last frame.
    row_h: f32,
    icon: f32,
    font: f32,
    /// The skin's text size as a part of the standard one (the renderer draws every text that much
    /// bigger): columns widen with it.
    skin_text: f32,
    /// How thick the scrollbar's thumb is and the space beside it, from the skin.
    bar_w: f32,
    bar_margin: f32,
    mouse: Option<Point>,
    drag: Drag,
    anim: Anim,
    /// The tab the running animation belongs to.
    anim_tab: u64,
    wheel_lines: f32,
    typed: String,
    typed_at: f64,
    /// The main window and its scale, from the last layout (for the name box and the slow-click timer).
    hwnd: HWND,
    scale: f32,
    rename: Option<Rename>,
    /// The name box, made the first time it's needed.
    name_box: Option<TextBox>,
    /// Where the name box was put last (None: hidden), so it only moves when its row does.
    name_box_at: Option<Rect>,
    slow: Option<SlowClick>,
    click_open: Option<ClickOpen>,
    /// When one click last opened something (the second click of a double-click is then ignored).
    opened_at: Option<f64>,
    /// Whether the window was active at the last mouse move (a click that brings the window to the
    /// front doesn't start renaming).
    was_active: bool,
    /// Keep painting until this time: under Wine a hidden text box stays visible, and a new listing
    /// isn't on screen, until a later frame.
    repaint_until: f64,
    /// Whether the tab was waiting for a listing at the last frame (to notice one arriving).
    was_loading: bool,
    /// A button that went down on the list (see `Press`).
    press: Option<Press>,
    /// Something dragged over the list would go into this folder (full path, lower case): its row
    /// is highlighted.
    drop_row: Option<String>,
    /// How the tab on screen flows (from its view, at the last frame), and its tiles' size.
    flow: Flow,
    tile: Tile,
    /// The tab's view at the last frame (a change stops a scroll glide and a rectangle).
    mode: ViewMode,
    /// A rectangle being dragged over the empty space.
    band: Option<Band>,
    /// The List view: where each name drawn at the last frame ends (row, right edge from its cell's
    /// left), so the empty space beside a short name isn't the item.
    list_hit: Vec<(usize, f32)>,
}

impl Default for ListView {
    fn default() -> Self {
        Self::new()
    }
}

impl ListView {
    pub fn new() -> ListView {
        let mut lines: u32 = 3;
        // SAFETY: writes one u32 into `lines`.
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, Some(&mut lines as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        let all = [Col::Modified, Col::Created, Col::Type, Col::Size, Col::Folder, Col::Free, Col::Location];
        ListView {
            rect: Rect::default(),
            wanted: all.iter().map(|c| (c.key(), c.default_width())).collect(),
            cols: Vec::new(),
            row_h: size::ROW_H,
            icon: size::ICON,
            font: size::FONT,
            skin_text: 1.0,
            bar_w: size::SCROLLBAR_W,
            bar_margin: size::SCROLLBAR_MARGIN,
            mouse: None,
            drag: Drag::None,
            anim: Anim::default(),
            anim_tab: 0,
            // "One screen per notch" comes back as u32::MAX; treat it as a big step.
            wheel_lines: if lines == u32::MAX { 20.0 } else { lines.clamp(1, 20) as f32 },
            typed: String::new(),
            typed_at: 0.0,
            hwnd: HWND::default(),
            scale: 1.0,
            rename: None,
            name_box: None,
            name_box_at: None,
            slow: None,
            click_open: None,
            opened_at: None,
            was_active: false,
            repaint_until: 0.0,
            was_loading: false,
            press: None,
            drop_row: None,
            flow: Flow::Rows,
            tile: Tile::of(ViewMode::Medium),
            mode: ViewMode::Details,
            band: None,
            list_hit: Vec::new(),
        }
    }

    /// The widths of the columns next to Name as last set (by default or by dragging), in DIPs,
    /// by their names in settings.json ("modified", "created", "type", "size", "folder", "free",
    /// "location").
    pub fn column_widths(&self) -> BTreeMap<String, f32> {
        self.wanted.iter().map(|(k, w)| (k.to_string(), *w)).collect()
    }

    /// Puts back widths from settings.json (each kept at least MIN_COL wide); columns it doesn't
    /// name keep theirs. Takes effect at the next frame.
    pub fn set_column_widths(&mut self, widths: &BTreeMap<String, f32>) {
        for (key, w) in self.wanted.iter_mut() {
            if let Some(&saved) = widths.get(*key) {
                *w = if saved.is_finite() { saved.max(MIN_COL) } else { MIN_COL };
            }
        }
    }

    /// Takes the "Size" setting's row height, icon and text size, and the tab's view, for this frame.
    fn take_size(&mut self, app: &AppState) {
        self.row_h = app.row_height();
        let large = app.settings.size == ListSize::Large;
        self.icon = if large { size::ICON_LARGE } else { size::ICON };
        self.font = if large { size::FONT_LARGE } else { size::FONT };
        self.skin_text = app.theme.font_size / size::FONT;
        self.bar_w = app.theme.scrollbar_w;
        self.bar_margin = app.theme.scrollbar_margin();
        let mode = app.tab().view_mode;
        self.flow = Flow::of(mode);
        self.tile = Tile::of(mode);
        if mode != self.mode {
            // Another view: a glide or rectangle under way meant the old one; the name box moves.
            self.mode = mode;
            self.anim.stop();
            self.band = None;
            self.name_box_at = None;
            self.list_hit.clear();
        }
    }

    /// Where the tab's items sit this frame (views.rs).
    fn geo(&self, tab: &Tab) -> Geometry {
        let n = tab.view.len();
        match self.flow {
            Flow::Rows => Geometry::rows(self.rows_area(), self.row_h, n),
            Flow::Columns => Geometry::columns(self.rows_area(), self.row_h, LIST_COLUMN_W * self.text_growth(), n),
            Flow::Grid => Geometry::grid(self.rows_area(), self.tile, n),
        }
    }

    /// The tree view's room before an item's icon: its arrow and its level's indent (0 elsewhere).
    fn tree_offset(tab: &Tab, index: usize) -> f32 {
        if tab.view_mode == ViewMode::Tree && tab.tree_here() {
            TREE_ARROW + tab.depth(index) as f32 * TREE_INDENT
        } else {
            0.0
        }
    }

    /// The tree view: where an item's ▸ arrow is on its row (`line`).
    fn arrow_rect(tab: &Tab, index: usize, line: Rect) -> Rect {
        Rect::new(line.x + ROW_PAD - 4.0 + tab.depth(index) as f32 * TREE_INDENT, line.y, TREE_ARROW + 2.0, line.h)
    }

    /// The tree view: whether `p` is on the arrow of a folder's row (a click there opens or
    /// closes it).
    fn on_arrow(&self, tab: &Tab, p: Point) -> Option<usize> {
        if self.flow != Flow::Rows || tab.view_mode != ViewMode::Tree || !tab.tree_here() {
            return None;
        }
        let row = self.row_at(tab, p)?;
        let index = tab.index_at(row)?;
        let line = self.geo(tab).cell(row, tab.scroll.clamp(0.0, self.max_scroll(tab)));
        (tab.folder.entries.get(index).is_some_and(|e| e.is_container()) && Self::arrow_rect(tab, index, line).contains(p)).then_some(index)
    }

    /// The columns the list shows for the tab on screen: This PC and the Recycle Bin have their
    /// own; elsewhere Name, the ones settings.json asks for, and
    /// Folder for search results.
    fn wanted_columns(app: &AppState) -> Vec<Col> {
        let t = app.tab();
        if Self::is_this_pc(t) {
            return vec![Col::Name, Col::Free, Col::Type, Col::Size];
        }
        if recycle::is_bin(&t.folder.path) {
            return vec![Col::Name, Col::Location, Col::Modified, Col::Size, Col::Type];
        }
        let mut cols = vec![Col::Name];
        cols.extend(OPTIONAL.iter().copied().filter(|c| app.settings.shows_column(c.key())));
        if t.is_searching() {
            cols.push(Col::Folder);
        }
        cols
    }

    fn is_this_pc(t: &Tab) -> bool {
        t.folder.path.is_empty() && !t.folder.entries.is_empty()
    }

    /// Works out where every column sits: the ones next to Name keep the widths last set, squeezed
    /// on narrow windows (so they come back when the window is made wide again); Name takes the rest.
    fn place_columns(&mut self, app: &AppState) {
        let wanted_cols = Self::wanted_columns(app);
        let others: Vec<Col> = wanted_cols.iter().copied().filter(|&c| c != Col::Name).collect();
        // Large rows have bigger text: the columns grow with it, so dates still fit.
        let grow = self.text_growth();
        let mut widths: Vec<f32> = others.iter().map(|c| self.wanted.get(c.key()).copied().unwrap_or(c.default_width()) * grow).collect();
        let avail = self.rect.w - SCROLL_GUTTER - MIN_NAME;
        let total: f32 = widths.iter().sum();
        if total > avail && avail > 0.0 {
            let k = avail / total;
            for w in &mut widths {
                *w = (*w * k).max(MIN_COL.min(*w));
            }
        }
        let end = self.rect.right() - SCROLL_GUTTER;
        let mut x = end;
        let mut placed: Vec<Placed> = Vec::with_capacity(wanted_cols.len());
        for (c, w) in others.iter().zip(&widths).rev() {
            x -= w;
            placed.push(Placed { col: *c, x, w: *w });
        }
        placed.reverse();
        // Name: from the left edge to the first other column (never narrower than MIN_NAME; the
        // others then start further right, and the last ones may be cut off).
        let name_end = x.max(self.rect.x + MIN_NAME);
        let shift = name_end - x;
        for p in &mut placed {
            p.x += shift;
        }
        let mut cols = vec![Placed { col: Col::Name, x: self.rect.x, w: name_end - self.rect.x }];
        cols.extend(placed);
        self.cols = cols;
    }

    /// How much bigger the text is than the normal size (1 for small and medium rows, in a skin with
    /// the standard text size). Column widths
    /// are remembered at the normal size and shown this much wider.
    fn text_growth(&self) -> f32 {
        (self.font / size::FONT * self.skin_text).max(1.0)
    }

    /// The right edge of the Name column.
    fn name_right(&self) -> f32 {
        self.cols.first().map_or(self.rect.right() - SCROLL_GUTTER, |p| p.x + p.w)
    }

    /// The column titles (Details and Tree only: the other views have none).
    fn header(&self) -> Rect {
        let h = if self.flow == Flow::Rows { size::HEADER_H } else { 0.0 };
        Rect::new(self.rect.x, self.rect.y, self.rect.w, h)
    }

    /// The area the items scroll in (under the header and its 1-DIP line, where there is one).
    fn rows_area(&self) -> Rect {
        if self.flow != Flow::Rows {
            return self.rect;
        }
        let top = self.rect.y + size::HEADER_H + 1.0;
        Rect::new(self.rect.x, top, self.rect.w, (self.rect.bottom() - top).max(0.0))
    }

    fn max_scroll(&self, tab: &Tab) -> f32 {
        self.geo(tab).max_scroll()
    }

    /// The item under a point (None over empty space and the scrollbar, and while the list says
    /// "Loading…" instead of showing items).
    fn row_at(&self, tab: &Tab, p: Point) -> Option<usize> {
        if tab.waiting_long() || self.near_track(p) {
            return None;
        }
        let scroll = tab.scroll.clamp(0.0, self.max_scroll(tab));
        let geo = self.geo(tab);
        let row = geo.row_at(p, scroll)?;
        if self.flow == Flow::Columns {
            // Beside a short name is empty space, as in Explorer.
            let cell = geo.cell(row, scroll);
            if let Some((_, right)) = self.list_hit.iter().find(|(r, _)| *r == row) {
                if p.x > cell.x + right {
                    return None;
                }
            }
        }
        Some(row)
    }

    /// The scrollbar's track: on the right, or along the bottom in the List view.
    fn track(&self) -> Rect {
        let area = self.rows_area();
        let (w, m) = (self.bar_w, self.bar_margin);
        if self.flow == Flow::Columns {
            return Rect::new(area.x + m, self.rect.bottom() - w - m, (area.w - 2.0 * m).max(0.0), w);
        }
        Rect::new(self.rect.right() - w - m, area.y + m, w, (area.h - 2.0 * m).max(0.0))
    }

    /// The scrollbar's whole lane: the track and the space beside it (where a skin paints a track).
    fn lane(&self) -> Rect {
        let t = self.track();
        let m = self.bar_margin;
        Rect::new(t.x - m, t.y - m, t.w + 2.0 * m, t.h + 2.0 * m)
    }

    /// Whether `p` is on (or right next to) the scrollbar's track.
    fn near_track(&self, p: Point) -> bool {
        let t = self.track();
        if self.flow == Flow::Columns {
            p.y >= t.y - 2.0 && self.rect.contains(p)
        } else {
            p.x >= t.x - 2.0 && self.rows_area().contains(p)
        }
    }

    /// The scrollbar thumb, if the items don't all fit (and are showing).
    fn thumb(&self, tab: &Tab) -> Option<Rect> {
        let geo = self.geo(tab);
        let max = geo.max_scroll();
        if max <= 0.0 || tab.waiting_long() {
            return None;
        }
        let track = self.track();
        let along = (tab.scroll / max).clamp(0.0, 1.0);
        if geo.sideways() {
            let w = (track.w * geo.view() / geo.content()).clamp(24.0_f32.min(track.w), track.w);
            return Some(Rect::new(track.x + (track.w - w) * along, track.y, w, track.h));
        }
        let h = (track.h * geo.view() / geo.content()).clamp(24.0_f32.min(track.h), track.h);
        Some(Rect::new(track.x, track.y + (track.h - h) * along, track.w, h))
    }

    /// The column whose left edge is under `p` in the header (for resizing). Name has none.
    fn edge_at(&self, p: Point) -> Option<Placed> {
        if self.flow != Flow::Rows || !self.header().contains(p) {
            return None;
        }
        self.cols.iter().skip(1).find(|c| (p.x - c.x).abs() <= EDGE_GRAB).copied()
    }

    fn column_at(&self, p: Point) -> Option<Col> {
        if self.flow != Flow::Rows || !self.header().contains(p) {
            return None;
        }
        let end = self.rect.right() - SCROLL_GUTTER;
        self.cols.iter().find(|c| p.x >= c.x && p.x < (c.x + c.w).min(end)).map(|c| c.col)
    }

    /// Scrolls (without animation) so `row` is fully visible.
    fn reveal(&mut self, tab: &mut Tab, row: usize) {
        tab.scroll = self.geo(tab).reveal(row, tab.scroll);
        self.anim.stop();
    }

    /// Glide the scroll position to `target`.
    fn scroll_to(&mut self, tab: &mut Tab, target: f32, now: f64) -> Effect {
        let target = target.clamp(0.0, self.max_scroll(tab));
        // Start the glide one frame "in the past" so the very next frame already moves (a
        // wheel notch should answer at once, not one frame later).
        self.anim.go(tab.scroll, target, now - FRAME_HEAD_START, SCROLL_TIME);
        self.anim_tab = tab.id;
        Effect::Animate
    }

    /// Moves the keyboard cursor to `row`, selecting like Explorer does.
    fn move_cursor(&mut self, tab: &mut Tab, row: usize, shift: bool, ctrl: bool) {
        if tab.view.is_empty() {
            return;
        }
        let row = row.min(tab.view.len() - 1);
        if shift {
            tab.select_range_to(row, ctrl);
        } else if ctrl {
            tab.focus = tab.index_at(row);
        } else {
            tab.select_only_row(row);
        }
        self.reveal(tab, row);
    }

    /// Whether a name is being typed to jump to it (a key in the last second): then Space is part
    /// of the name rather than the Space preview (window.rs).
    pub fn typing(&self, now: f64) -> bool {
        !self.typed.is_empty() && now - self.typed_at <= 1.0
    }

    /// Type-to-jump: the next name starting with what was typed in the last second.
    fn jump_to_typed(&mut self, tab: &mut Tab, ch: char, now: f64) -> bool {
        if now - self.typed_at > 1.0 {
            self.typed.clear();
        }
        self.typed_at = now;
        self.typed.extend(ch.to_lowercase());
        // Pressing the same letter again and again cycles through names starting with it.
        let first = self.typed.chars().next().unwrap_or(ch);
        let repeating = self.typed.chars().all(|c| c == first);
        let needle: String = if repeating { first.to_string() } else { self.typed.clone() };
        let n = tab.view.len();
        if n == 0 {
            return false;
        }
        let cur = tab.focus_row();
        let start = match cur {
            Some(r) if repeating => r + 1,
            Some(r) => r,
            None => 0,
        };
        for k in 0..n {
            let row = (start + k) % n;
            if tab.entry_at(row).is_some_and(|e| e.lower.starts_with(&needle)) {
                tab.select_only_row(row);
                self.reveal(tab, row);
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------ renaming

    /// Opens the name box on the selected item (F2, Rename in the menu, Command::BeginRename): the
    /// one with the cursor, else the first selected.
    pub fn begin_rename_selected(&mut self, ctx: &Ctx, app: &mut AppState) -> Effect {
        let tab = app.tab();
        if tab.path.is_empty() {
            app.say(THIS_PC_NOTE);
            return Effect::Redraw;
        }
        if recycle::is_bin(&tab.path) || recycle::is_bin(&tab.folder.path) {
            app.say("Restore it first (right-click > Restore).");
            return Effect::Redraw;
        }
        if tab.waiting_long() {
            return Effect::None;
        }
        let chosen = tab.selected_entries();
        let Some(index) = tab.focus.filter(|f| chosen.contains(f)).or(chosen.first().copied()) else { return Effect::None };
        let hide_ext = app.settings.hide_known_extensions;
        self.begin_rename(ctx.now, app.tab_mut(), index, None, hide_ext)
    }

    /// Opens the name box on entry `index`, showing `text` (or its name) with the part before the
    /// extension selected. `hide_ext`: known extensions are left out of names ("HideKnownExtensions"):
    /// the box then holds the name without it, all selected, and the extension is put back after.
    fn begin_rename(&mut self, now: f64, tab: &mut Tab, index: usize, text: Option<&str>, hide_ext: bool) -> Effect {
        let Some(e) = tab.folder.entries.get(index) else { return Effect::None };
        // Not drives, and not the Recycle Bin's deleted items.
        if e.kind == EntryKind::Drive || tab.folder.path.is_empty() || recycle::is_bin(&tab.folder.path) || self.hwnd.is_invalid() {
            return Effect::None;
        }
        let Some(row) = tab.row_of(index) else { return Effect::None }; // hidden by the filter
        let (name, is_folder) = (e.name.clone(), e.is_container());
        let path = e.path_in(&tab.folder.path);
        // Search results and the items of folders open in the tree view are in folders of their
        // own (they carry their full path); everything else is in the tab's folder.
        let folder = if e.target.is_some() { folder::parent(&path).unwrap_or_default() } else { tab.folder.path.clone() };
        let shown = shown_name(e, hide_ext);
        let hidden_ext = name[shown.len()..].to_string();
        // One name at a time: a box still open is closed as it is.
        self.end_rename(now, false, false);
        self.cancel_slow_click();
        tab.select_only_row(row);
        self.reveal(tab, row);
        let start_text = match text {
            // A name typed before (to try again), without the extension the box leaves out.
            Some(t) => strip_extension(t, &hidden_ext).to_string(),
            None => name[..name.len() - hidden_ext.len()].to_string(),
        };
        let hwnd = self.hwnd;
        // The size the list draws names at (the skin's too; tiles' names are always the normal size).
        let font = if self.flow == Flow::Grid { size::FONT } else { self.font } * self.skin_text;
        let b = self.name_box.get_or_insert_with(|| TextBox::new(hwnd, RENAME_BOX));
        b.set_font_size(font);
        b.set_text(&start_text);
        let select = if hidden_ext.is_empty() { fileops::stem_len(&start_text, is_folder) } else { start_text.encode_utf16().count() };
        self.rename = Some(Rename { tab_id: tab.id, listing: tab.folder.path.clone(), folder, name, path, index, hidden_ext });
        self.name_box_at = None;
        self.place_name_box(tab);
        if let Some(b) = &self.name_box {
            b.focus();
            b.select_range(0, select);
        }
        Effect::Redraw
    }

    /// Closes the name box. With `commit`, what was typed becomes a Rename command (unless nothing
    /// changed). `focus_back`: give the keyboard back to the window (after Enter / Esc; not when it
    /// already went elsewhere).
    fn end_rename(&mut self, now: f64, commit: bool, focus_back: bool) -> Option<Command> {
        let r = self.rename.take()?;
        self.name_box_at = None;
        let b = self.name_box.as_mut()?;
        let mut typed = b.text();
        b.hide();
        if focus_back {
            b.give_focus_back();
        }
        // Wine (only) leaves a hidden text box on screen until a later frame replaces it.
        if frame::is_wine() {
            self.repaint_until = now + 0.3;
        }
        if !r.hidden_ext.is_empty() {
            // The extension the box left out goes back on (nothing typed: the name stays).
            let stem = typed.trim().trim_end_matches(['.', ' ']);
            typed = if stem.is_empty() { String::new() } else { format!("{stem}{}", r.hidden_ext) };
        }
        (commit && typed != r.name).then_some(Command::Rename { dir: r.folder, from: r.name, to: typed })
    }

    /// The row of the item being renamed (its index is looked up again when the listing was read
    /// again). None when it's gone.
    fn rename_row(&mut self, tab: &Tab) -> Option<usize> {
        let r = self.rename.as_mut()?;
        if r.tab_id != tab.id || tab.folder.path != r.listing {
            return None;
        }
        let is_it = |e: &Entry| e.name == r.name && e.path_in(&tab.folder.path) == r.path;
        if !tab.folder.entries.get(r.index).is_some_and(is_it) {
            r.index = tab.folder.entries.iter().position(is_it)?;
        }
        tab.row_of(r.index)
    }

    /// Where the name box goes on `row`: over the name (from just left of it to the next column),
    /// or under a tile's picture. None when the item isn't fully in sight.
    fn name_box_rect(&self, tab: &Tab, row: usize) -> Option<Rect> {
        let area = self.rows_area();
        let geo = self.geo(tab);
        let cell = geo.cell(row, tab.scroll.clamp(0.0, geo.max_scroll()));
        let inside = cell.y >= area.y - 0.5 && cell.bottom() <= area.bottom() + 0.5 && cell.x >= area.x - 0.5 && cell.right() <= area.right() + 0.5;
        if !inside {
            return None;
        }
        match self.flow {
            Flow::Rows => {
                let offset = tab.index_at(row).map_or(0.0, |i| Self::tree_offset(tab, i));
                let x = area.x + ROW_PAD + offset + self.icon + 3.0;
                let right = self.name_right() - 6.0;
                let h = (self.font * 2.0).min(self.row_h - 4.0);
                Some(Rect::new(x, cell.y + (self.row_h - h) / 2.0, (right - x).max(60.0), h))
            }
            Flow::Columns => {
                let x = cell.x + ROW_PAD + self.icon + 3.0;
                let h = (self.font * 2.0).min(self.row_h - 4.0);
                Some(Rect::new(x, cell.y + (self.row_h - h) / 2.0, (cell.right() - x - 4.0).max(60.0), h))
            }
            Flow::Grid => {
                let tile = Geometry::tile(cell);
                let y = cell.y + TILE_PAD + self.tile.picture + TILE_TEXT_GAP - 3.0;
                Some(Rect::new(tile.x + 2.0, y, tile.w - 4.0, size::FONT * 2.0))
            }
        }
    }

    /// Moves the name box to its row (only when that moved), or hides it while the row is out of sight.
    fn place_name_box(&mut self, tab: &Tab) {
        let rect = self.rename_row(tab).and_then(|row| self.name_box_rect(tab, row));
        if rect == self.name_box_at {
            return;
        }
        self.name_box_at = rect;
        let scale = self.scale;
        let Some(b) = self.name_box.as_mut() else { return };
        match rect {
            // The text inside the box (2 DIPs in, plus the box's own 3-pixel margin) lines up with the
            // names in the list.
            Some(r) => {
                b.place(r, 2.0, scale);
                if !b.is_visible() {
                    b.show();
                }
            }
            None => b.hide(),
        }
    }

    /// Every frame while renaming: keeps the box on its row. When another folder or tab is on screen
    /// (a mouse's Back button, say) what was typed is kept, as clicking elsewhere would; when the
    /// item is gone, the box closes.
    fn keep_name_box(&mut self, now: f64, app: &mut AppState) {
        let tab = app.tab();
        let Some(elsewhere) = self.rename.as_ref().map(|r| r.tab_id != tab.id || r.listing != tab.path || r.listing != tab.folder.path) else {
            return;
        };
        if elsewhere {
            if let Some(cmd) = self.end_rename(now, true, true) {
                app.run(cmd);
            }
            return;
        }
        if self.rename_row(tab).is_none() {
            let name = self.rename.as_ref().map(|r| r.name.clone()).unwrap_or_default();
            self.end_rename(now, false, true);
            app.say(format!("\u{201c}{name}\u{201d} isn't there any more."));
            return;
        }
        self.place_name_box(tab);
    }

    /// Opens the name box someone asked for (a new folder once it's in the list, a rename to try again).
    fn take_rename_request(&mut self, now: f64, app: &mut AppState) {
        let hide_ext = app.settings.hide_known_extensions;
        let tab = app.tab_mut();
        let Some(req) = &tab.rename_request else { return };
        // (Never over search results: the request names its item, and names repeat there.)
        if req.expired() || !fileops::same_path(&req.dir, &tab.path) || tab.is_searching() {
            tab.rename_request = None;
            return;
        }
        if tab.loading.is_some() || !fileops::same_path(&req.dir, &tab.folder.path) {
            return; // the listing with it is on its way
        }
        let Some(req) = tab.rename_request.take() else { return };
        // (The folder's own item: an item of a folder open in the tree view can have the same name.)
        if let Some(index) = tab.folder.entries.iter().position(|e| e.name == req.name && !(tab.tree.is_open() && e.target.is_some())) {
            self.begin_rename(now, tab, index, req.text.as_deref(), hide_ext);
        }
    }

    /// Under Wine (only): keep painting for a moment, so a frame with news from a background job
    /// (a change the folder watch saw, say) gets on screen without waiting for the mouse to move.
    /// See the end of `tick`. Returns whether it asks for frames.
    pub fn wine_nudge(&mut self, now: f64) -> bool {
        if frame::is_wine() {
            self.repaint_until = self.repaint_until.max(now + 0.3);
            return true;
        }
        false
    }

    /// The slow-click timer ran out (window.rs): rename, if the item is still the lone selected one.
    pub fn slow_click_due(&mut self, ctx: &Ctx, app: &mut AppState) -> Effect {
        let Some(s) = self.slow.take().filter(|s| s.armed && app.question.is_none()) else { return Effect::None };
        let hide_ext = app.settings.hide_known_extensions;
        let tab = app.tab_mut();
        let alone = tab.focus == Some(s.index) && tab.is_selected(s.index) && tab.selection_summary().0 == 1;
        if tab.id != s.tab_id || !alone || self.rename.is_some() {
            return Effect::None;
        }
        self.begin_rename(ctx.now, tab, s.index, None, hide_ext)
    }

    fn cancel_slow_click(&mut self) {
        if self.slow.take().is_some_and(|s| s.armed) && !self.hwnd.is_invalid() {
            // SAFETY: stops a timer on our own window (harmless if it already ran).
            unsafe {
                let _ = KillTimer(Some(self.hwnd), SLOW_CLICK_TIMER);
            }
        }
    }

    /// Whether `p` is over the name of item `row` (not its icon or the other columns) - where a
    /// slow click renames.
    fn on_name(&self, tab: &Tab, row: usize, p: Point) -> bool {
        let geo = self.geo(tab);
        let cell = geo.cell(row, tab.scroll.clamp(0.0, geo.max_scroll()));
        match self.flow {
            Flow::Rows => {
                let offset = tab.index_at(row).map_or(0.0, |i| Self::tree_offset(tab, i));
                let name_x = self.rows_area().x + ROW_PAD + offset + self.icon + 8.0;
                p.x >= name_x - 2.0 && p.x < self.name_right()
            }
            Flow::Columns => p.x >= cell.x + ROW_PAD + self.icon + 6.0,
            Flow::Grid => p.y >= cell.y + TILE_PAD + self.tile.picture + TILE_TEXT_GAP - 2.0,
        }
    }

    // ------------------------------------------------------------ the column titles' menu

    /// Right-click on the column titles: which columns show, and how the list looks.
    fn header_menu(&self, at: Point, app: &AppState) -> Effect {
        let s = &app.settings;
        let col = |id, label: &str, key| MenuItem::new(id, label).checked(s.shows_column(key));
        // Examples in the user's own date and number style.
        let now = now_filetime();
        let earlier = now.saturating_sub(2 * 3600 * 10_000_000);
        let date = |id, label: &str, f: DateFormat, example: u64| {
            MenuItem::new(id, &format!("{label} ({})", folder::format_date_as(example, f)).replace('&', "&&")).checked(s.date_format == f)
        };
        let sample = 3_247_000;
        let sizes = |id, label: &str, f: SizeFormat| MenuItem::new(id, &format!("{label} ({})", folder::format_size_as(sample, f))).checked(s.size_format == f);
        let rows = |id, label: &str, l: ListSize| MenuItem::new(id, label).checked(s.size == l);
        let items = vec![
            MenuItem::new(0, "Name").checked(true).disabled(true),
            col(HEAD_MODIFIED, "Date modified", "modified"),
            col(HEAD_CREATED, "Date created", "created"),
            col(HEAD_TYPE, "Type", "type"),
            col(HEAD_SIZE, "Size", "size"),
            MenuItem::separator(),
            MenuItem::new(HEAD_FOLDERS_FIRST, "Folders first").checked(s.folders_first),
            MenuItem::new(HEAD_EXTENSIONS, "File extensions").checked(!s.hide_known_extensions),
            MenuItem::separator(),
            MenuItem::submenu(
                "Dates",
                vec![
                    MenuItem::new(HEAD_DATE_DEFAULT, &format!("Windows default ({})", folder::format_date(now)).replace('&', "&&"))
                        .checked(s.date_format == DateFormat::Default),
                    date(HEAD_DATE_RELATIVE, "Relative", DateFormat::Relative, earlier),
                    date(HEAD_DATE_ISO, "Year first", DateFormat::Iso, now),
                ],
            ),
            MenuItem::submenu("Sizes", vec![sizes(HEAD_SIZE_AUTO, "Automatic", SizeFormat::Auto), sizes(HEAD_SIZE_KB, "Always KB", SizeFormat::Kb)]),
            MenuItem::submenu(
                "Row size",
                vec![
                    rows(HEAD_ROWS_SMALL, "Small", ListSize::Small),
                    rows(HEAD_ROWS_MEDIUM, "Medium", ListSize::Medium),
                    rows(HEAD_ROWS_LARGE, "Large", ListSize::Large),
                ],
            ),
        ];
        Effect::Menu(Menu { tag: MENU_HEADER, at, items, right_aligned: false })
    }

    /// A pick in the column titles' menu: the choice is changed right in the settings (the window
    /// saves it a moment later).
    fn header_pick(&mut self, item: u32, app: &mut AppState) -> Effect {
        match item {
            HEAD_MODIFIED | HEAD_CREATED | HEAD_TYPE | HEAD_SIZE => {
                let key = OPTIONAL[(item - HEAD_MODIFIED) as usize].key();
                let on = !app.settings.shows_column(key);
                app.set_column(key, on);
            }
            HEAD_FOLDERS_FIRST => {
                let on = !app.settings.folders_first;
                app.set_folders_first(on);
            }
            HEAD_EXTENSIONS => {
                let hide = !app.settings.hide_known_extensions;
                app.settings.hide_known_extensions = hide;
            }
            HEAD_DATE_DEFAULT => app.settings.date_format = DateFormat::Default,
            HEAD_DATE_RELATIVE => app.settings.date_format = DateFormat::Relative,
            HEAD_DATE_ISO => app.settings.date_format = DateFormat::Iso,
            HEAD_SIZE_AUTO => app.settings.size_format = SizeFormat::Auto,
            HEAD_SIZE_KB => app.settings.size_format = SizeFormat::Kb,
            HEAD_ROWS_SMALL => app.set_list_size(ListSize::Small),
            HEAD_ROWS_MEDIUM => app.set_list_size(ListSize::Medium),
            HEAD_ROWS_LARGE => app.set_list_size(ListSize::Large),
            _ => return Effect::None,
        }
        self.take_size(app);
        self.place_columns(app);
        self.name_box_at = None; // the name box (if open) moves with its row
        Effect::Redraw
    }

    // ------------------------------------------------------------ drawing

    fn paint_header(&mut self, r: &mut Renderer, theme: &Theme, tab: &Tab) {
        let header = self.header();
        let hover_col = match self.drag {
            Drag::None => self.mouse.and_then(|p| if self.edge_at(p).is_some() { None } else { self.column_at(p) }),
            _ => None,
        };
        let this_pc = Self::is_this_pc(tab);
        let bin = recycle::is_bin(&tab.folder.path);
        let end = self.rect.right() - SCROLL_GUTTER;
        r.fill_paint(header, 0.0, &theme.list_header);
        for (i, p) in self.cols.clone().iter().enumerate() {
            let cell = Rect::new(p.x, header.y, (p.x + p.w).min(end) - p.x, header.h);
            if cell.w <= 0.0 {
                continue;
            }
            if hover_col == Some(p.col) && p.col.sort_key().is_some() {
                r.fill_paint(cell, 0.0, &theme.hover_paint);
            }
            if i > 0 {
                // The line between two column titles (none in most skins).
                r.fill_rect(Rect::new(cell.x, cell.y, 1.0, cell.h), theme.column_line);
            }
            let label = p.col.title(this_pc, bin);
            let right = p.col.right_aligned();
            let text_rect = if i == 0 { Rect::new(cell.x + ROW_PAD + 4.0, cell.y, cell.w - ROW_PAD - 16.0, cell.h) } else { cell.inset(10.0, 0.0) };
            let style = TextStyle::new(theme.list_header_text).align(if right { Align::Right } else { Align::Left });
            // With liquid buttons the titles are bold, as the sidebar's panel headers are.
            let style = if theme.buttons == ButtonStyle::Liquid { style.bold() } else { style };
            r.text(label, text_rect, &style);
            // (None while the custom sort order decides.)
            if p.col.sort_key() == Some(tab.sort) && tab.custom_here().is_none() && !(this_pc && p.col == Col::Free && tab.sort != SortKey::Date) {
                // A small chevron beside the sorted column's title: up = A to Z / oldest first.
                let w = r.measure(label, &style);
                let cx = if right { text_rect.right() - w - 9.0 } else { (text_rect.x + w + 9.0).min(text_rect.right() - 4.0) };
                let cy = cell.y + cell.h / 2.0;
                let d = if tab.descending { 1.5 } else { -1.5 };
                r.polyline(&[Point::new(cx - 3.5, cy - d), Point::new(cx, cy + d), Point::new(cx + 3.5, cy - d)], 1.3, theme.list_header_text);
            }
        }
        r.fill_rect(Rect::new(header.x, header.bottom(), header.w, 1.0), theme.line);
    }

    /// The text of one cell (not Name's). `bin`: what the Recycle Bin says about the item, in the bin.
    fn cell_text(col: Col, e: &Entry, root: &str, app: &AppState, bin: Option<&BinItem>) -> String {
        let s = &app.settings;
        let drive = e.kind == EntryKind::Drive;
        match col {
            Col::Name => String::new(),
            Col::Modified if !drive => folder::format_date_as(e.modified, s.date_format),
            Col::Created if !drive => folder::format_date_as(e.created, s.date_format),
            Col::Modified | Col::Created => String::new(),
            Col::Type => e.type_name.to_string(),
            Col::Size if drive => if e.size > 0 { folder::format_size(e.size) } else { String::new() },
            // A deleted folder's size is known too (Windows noted it when it was deleted).
            Col::Size if e.kind == EntryKind::File || bin.is_some() => folder::format_size_as(e.size, s.size_format),
            Col::Size => String::new(),
            Col::Free if drive && e.size > 0 => format!("{} free", folder::format_size(e.free)),
            Col::Free => String::new(),
            Col::Folder => relative_folder(e, root),
            Col::Location => bin.map(|b| b.location.clone()).unwrap_or_default(),
        }
    }

    /// Draws the items as the tab's view says, then what goes over them: the name box's frame, a
    /// message instead of items, the rectangle being dragged and the scrollbar.
    fn paint_rows(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, tab: &Tab) {
        let area = self.rows_area();
        let geo = self.geo(tab);
        let scroll = tab.scroll.clamp(0.0, geo.max_scroll());
        // A slow folder on its way: no items of the one we came from (see app::LOADING_DELAY).
        let waiting = tab.waiting_long();
        r.push_clip(area);
        if !waiting {
            match self.flow {
                Flow::Rows => self.paint_lines(r, theme, app, tab, &geo, scroll),
                Flow::Columns => self.paint_names(r, theme, app, tab, &geo, scroll),
                Flow::Grid => self.paint_tiles(r, theme, app, tab, &geo, scroll),
            }
        }
        // The name box is a Windows text box on top; this is the frame around it.
        if let Some(at) = self.name_box_at.filter(|_| self.rename.as_ref().is_some_and(|rn| rn.tab_id == tab.id)) {
            r.fill_rect(at, theme.input);
            r.stroke_rect(at, 1.0, theme.accent);
        }
        // A rectangle dragged over the empty space.
        if let Some(b) = self.band.as_ref().filter(|b| b.active && b.tab_id == tab.id) {
            let rect = self.band_rect(b, scroll);
            r.fill_rect(rect, theme.accent.with_alpha(0.10));
            r.stroke_rect(rect, 1.0, theme.accent.with_alpha(0.75));
        }
        r.pop_clip();

        // A message instead of items: errors, empty folders, no filter matches.
        let message = if waiting {
            Some("Loading\u{2026}".to_string())
        } else if let Some(err) = &tab.folder.error {
            Some(err.clone())
        } else if let Some(s) = tab.search.as_ref().filter(|_| tab.view.is_empty()) {
            Some(if !s.done {
                "Searching\u{2026}".to_string()
            } else if let Some(err) = s.listing.error.as_ref().filter(|_| s.listing.path == tab.path) {
                err.clone() // the folder itself can't be read (gone, no permission)
            } else {
                format!("Nothing called \u{201c}{}\u{201d} here or in the folders inside.", s.query)
            })
        } else if tab.folder.entries.is_empty() && tab.loading.is_some() {
            Some("Loading\u{2026}".to_string())
        } else if tab.folder.entries.is_empty() && tab.folder.path == tab.path {
            Some(if recycle::is_bin(&tab.path) { "The Recycle Bin is empty." } else { "This folder is empty." }.to_string())
        } else if tab.view.is_empty() && !tab.filter.is_empty() {
            Some(format!("No items match \u{201c}{}\u{201d}.", tab.filter))
        } else if tab.view.is_empty() && tab.folder.path == tab.path {
            // Everything here is hidden (see Tab::rebuild_view).
            Some(if tab.show_hidden { "Only protected system files are here." } else { "Only hidden items are here. Ctrl+H shows them." }.to_string())
        } else {
            None
        };
        if let Some(m) = message {
            let line = Rect::new(area.x + ROW_PAD + size::ICON + 8.0, area.y + 6.0, area.w - 60.0, size::ROW_H);
            r.text(&m, line, &TextStyle::new(theme.muted));
        }

        if let Some(thumb) = self.thumb(tab) {
            let state = match self.drag {
                Drag::Thumb { .. } => ThumbState::Dragged,
                _ if self.mouse.is_some_and(|p| self.near_track(p)) => ThumbState::Hover,
                _ => ThumbState::Rest,
            };
            paint_scrollbar(r, theme, self.lane(), thumb, state);
        }
    }

    /// The item under the mouse, to draw highlighted (not while dragging something in the list).
    fn hovered(&self, tab: &Tab) -> Option<usize> {
        if self.drag != Drag::None || self.band.as_ref().is_some_and(|b| b.active) {
            return None;
        }
        self.mouse.and_then(|p| self.row_at(tab, p))
    }

    /// Whether an item is cut to the clipboard (drawn faded until it's pasted, as in Explorer).
    /// Items of folders open in the tree view aren't marked (names repeat across folders).
    fn is_cut(tab: &Tab, cut: Option<&std::collections::HashSet<String>>, e: &Entry) -> bool {
        cut.is_some_and(|c| c.contains(&e.lower)) && !(tab.tree.is_open() && e.target.is_some())
    }

    /// The colour of an item's name.
    fn name_color(theme: &Theme, e: &Entry, selected: bool, is_cut: bool) -> crate::theme::Color {
        if selected && !is_cut {
            theme.selection_text
        } else if e.is_hidden() || is_cut {
            theme.muted
        } else {
            theme.text
        }
    }

    /// Details and Tree: one row per item, with the columns.
    fn paint_lines(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, tab: &Tab, geo: &Geometry, scroll: f32) {
        let row_h = self.row_h;
        let hover = self.hovered(tab);
        let cut = app.cut_names(&tab.folder.path);
        let large = app.settings.size == ListSize::Large;
        let hide_ext = app.settings.hide_known_extensions;
        let end = self.rect.right() - SCROLL_GUTTER;
        let cols = self.cols.clone();
        // The Recycle Bin: where each item came from (its "Original location"), one per entry.
        let bin = tab.folder.bin.as_ref().filter(|b| b.items.len() == tab.folder.entries.len());
        // The tree view: arrows on folders, items of open folders pulled in by their level.
        let tree = tab.view_mode == ViewMode::Tree && tab.tree_here();
        for row in geo.visible(scroll) {
            let Some(index) = tab.index_at(row) else { continue };
            let e = &tab.folder.entries[index];
            let line = geo.cell(row, scroll);
            let y = line.y;
            let selected = tab.is_selected(index);
            // The folder something dragged over the list would go into: outlined in red.
            let dropping = self.drop_row.as_deref().is_some_and(|d| e.is_container() && e.path_in(&tab.folder.path).to_lowercase() == d);
            if selected {
                r.fill_paint(line, 0.0, &theme.accent_tint_paint);
                r.fill_rect(Rect::new(line.x, line.y, size::SELECT_EDGE, line.h), theme.select_edge);
            } else if hover == Some(row) || dropping {
                r.fill_paint(line, 0.0, &theme.hover_paint);
            }
            if dropping {
                r.stroke_rect(line, 1.0, theme.accent);
            }
            let offset = Self::tree_offset(tab, index);
            if tree && e.is_container() {
                let a = Self::arrow_rect(tab, index, line);
                let c = Point::new(a.x + a.w / 2.0, a.y + a.h / 2.0);
                // (Greyed while an opened folder is still being read.)
                let color = if tab.tree.waiting(&e.path_in(&tab.folder.path)) {
                    theme.disabled
                } else if selected || hover == Some(row) {
                    theme.text
                } else {
                    theme.muted
                };
                let points = if tab.is_expanded(index) {
                    [Point::new(c.x - 3.5, c.y - 1.5), Point::new(c.x, c.y + 2.0), Point::new(c.x + 3.5, c.y - 1.5)] // ▾
                } else {
                    [Point::new(c.x - 1.5, c.y - 3.5), Point::new(c.x + 2.0, c.y), Point::new(c.x - 1.5, c.y + 3.5)] // ▸
                };
                r.polyline(&points, 1.3, color);
            }
            let icon = if large { app.icons.for_entry_sized(&tab.folder.path, e, true) } else { app.icons.for_entry(&tab.folder.path, e) };
            let icon_rect = Rect::new(line.x + ROW_PAD + offset, y + (row_h - self.icon) / 2.0, self.icon, self.icon);
            r.icon(&app.icons, icon, icon_rect);
            let is_cut = Self::is_cut(tab, cut, e);
            if is_cut {
                // The row's own background over the icon, half see-through, so it fades into its row.
                let under = if hover == Some(row) && !selected { theme.hover } else { theme.panel };
                r.fill_rect(icon_rect, under.with_alpha(0.55));
                if selected {
                    r.fill_rect(icon_rect, theme.accent_tint.with_alpha(theme.accent_tint.a * 0.55));
                }
            }
            let name_color = Self::name_color(theme, e, selected, is_cut);
            let name_x = line.x + ROW_PAD + offset + self.icon + 8.0;
            let name_right = self.name_right();
            r.text(shown_name(e, hide_ext), Rect::new(name_x, y, name_right - name_x - 8.0, row_h), &TextStyle::new(name_color).size(self.font));
            let detail = TextStyle::new(if selected { theme.selection_details } else { theme.muted }).size(self.font);
            for p in cols.iter().skip(1) {
                let w = (p.x + p.w).min(end) - p.x - 20.0;
                if w <= 0.0 {
                    continue;
                }
                let text = Self::cell_text(p.col, e, &tab.folder.path, app, bin.and_then(|b| b.items.get(index)));
                let style = if p.col.right_aligned() { detail.align(Align::Right) } else { detail };
                r.text(&text, Rect::new(p.x + 10.0, y, w, row_h), &style);
            }
        }
    }

    /// The List view: an icon and the name, in columns flowing left to right. Only the icon and the
    /// name are the item (`list_hit`); beside a short name is empty space.
    fn paint_names(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, tab: &Tab, geo: &Geometry, scroll: f32) {
        let hover = self.hovered(tab);
        let cut = app.cut_names(&tab.folder.path);
        let large = app.settings.size == ListSize::Large;
        let hide_ext = app.settings.hide_known_extensions;
        let area = self.rows_area();
        self.list_hit.clear();
        for row in geo.visible(scroll) {
            let Some(index) = tab.index_at(row) else { continue };
            let e = &tab.folder.entries[index];
            let cell = geo.cell(row, scroll);
            if cell.right() < area.x || cell.x > area.right() {
                continue;
            }
            let selected = tab.is_selected(index);
            let is_cut = Self::is_cut(tab, cut, e);
            let style = TextStyle::new(Self::name_color(theme, e, selected, is_cut)).size(self.font);
            let name = shown_name(e, hide_ext);
            let name_x = cell.x + ROW_PAD + self.icon + 8.0;
            let room = (cell.right() - name_x - 10.0).max(0.0);
            let w = r.measure(name, &style).min(room);
            let item = Rect::new(cell.x + 4.0, cell.y + 1.0, name_x + w + 6.0 - (cell.x + 4.0), cell.h - 2.0);
            self.list_hit.push((row, item.right() - cell.x));
            let dropping = self.drop_row.as_deref().is_some_and(|d| e.is_container() && e.path_in(&tab.folder.path).to_lowercase() == d);
            if selected {
                r.fill_paint(item, 0.0, &theme.accent_tint_paint);
                r.fill_rect(Rect::new(item.x, item.y, size::SELECT_EDGE, item.h), theme.select_edge);
            } else if hover == Some(row) || dropping {
                r.fill_paint(item, 0.0, &theme.hover_paint);
            }
            if dropping {
                r.stroke_rect(item, 1.0, theme.accent);
            }
            let icon = if large { app.icons.for_entry_sized(&tab.folder.path, e, true) } else { app.icons.for_entry(&tab.folder.path, e) };
            let icon_rect = Rect::new(cell.x + ROW_PAD, cell.y + (cell.h - self.icon) / 2.0, self.icon, self.icon);
            r.icon(&app.icons, icon, icon_rect);
            if is_cut {
                r.fill_rect(icon_rect, theme.panel.with_alpha(0.55));
            }
            r.text(name, Rect::new(name_x, cell.y, room, cell.h), &style);
        }
    }

    /// Medium, Large and Extra large icons: a tile per item, its picture above its name.
    fn paint_tiles(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, tab: &Tab, geo: &Geometry, scroll: f32) {
        let hover = self.hovered(tab);
        let cut = app.cut_names(&tab.folder.path);
        let hide_ext = app.settings.hide_known_extensions;
        let side = self.tile.picture;
        for row in geo.visible(scroll) {
            let Some(index) = tab.index_at(row) else { continue };
            let e = &tab.folder.entries[index];
            let cell = geo.cell(row, scroll);
            let tile = Geometry::tile(cell);
            let selected = tab.is_selected(index);
            let dropping = self.drop_row.as_deref().is_some_and(|d| e.is_container() && e.path_in(&tab.folder.path).to_lowercase() == d);
            // In the skin's shape (round corners only in a rounded skin), with the skin's selection
            // edge along the bottom.
            if selected {
                fill_box_paint(r, theme, tile, 0.0, &theme.accent_tint_paint);
                r.fill_rect(Rect::new(tile.x + 8.0, tile.bottom() - 2.0, tile.w - 16.0, 2.0), theme.select_edge);
            } else if hover == Some(row) || dropping {
                fill_box_paint(r, theme, tile, 0.0, &theme.hover_paint);
            }
            if dropping {
                outline_box(r, theme, tile.inset(0.5, 0.5), 0.0, 1.0, theme.accent);
            }
            let pic = Rect::new(cell.x + (cell.w - side) / 2.0, cell.y + TILE_PAD, side, side);
            let drawn = self.paint_picture(r, theme, app, &tab.folder.path, e, pic);
            let is_cut = Self::is_cut(tab, cut, e);
            if is_cut {
                let under = if selected { theme.panel } else if hover == Some(row) { theme.hover } else { theme.panel };
                r.fill_rect(drawn, under.with_alpha(0.55));
            }
            let name = Rect::new(tile.x + 4.0, pic.bottom() + TILE_TEXT_GAP, tile.w - 8.0, self.tile.lines as f32 * NAME_LINE_H + 1.0);
            let style = TextStyle::new(Self::name_color(theme, e, selected, is_cut)).align(Align::Center);
            r.text_wrapped(shown_name(e, hide_ext), name, &style);
        }
    }

    /// An item's picture, in the box `b`: its thumbnail (pictures, videos, PDFs: filling the box,
    /// with a thin frame) or Windows' big icon (at its own size at most) - made in the background
    /// (thumbs.rs), with the list's own icon meanwhile. Returns where it was drawn.
    fn paint_picture(&self, r: &mut Renderer, theme: &Theme, app: &AppState, dir: &str, e: &Entry, b: Rect) -> Rect {
        let scale = self.scale.max(0.5);
        // Pictures are asked for in screen pixels, so they're sharp at any scaling.
        let px = (b.w * scale).round().max(16.0) as u32;
        let path = e.path_in(dir);
        let version = thumbs::version_of(e.modified, e.size);
        if let Some(pic) = thumbs::request_versioned(&path, version, px) {
            // In DIPs, fitted into the box; never blown up past its own size.
            let (w, h) = (pic.width() as f32 / scale, pic.height() as f32 / scale);
            let k = (b.w / w).min(b.h / h).min(1.0);
            let (w, h) = (w * k, h * k);
            // Thumbnails sit on the box's bottom edge, like photos on a shelf; icons in its middle.
            let y = if pic.is_icon() { b.y + (b.h - h) / 2.0 } else { b.bottom() - h };
            let at = Rect::new(b.x + (b.w - w) / 2.0, y, w, h);
            if r.picture(&pic, at, None, 1.0, true) {
                if !pic.is_icon() {
                    r.stroke_rect(Rect::new(at.x - 1.0, at.y - 1.0, at.w + 2.0, at.h + 2.0), 1.0, theme.line);
                }
                return at;
            }
        }
        // Not made yet (or Windows has none): the list's own big icon.
        let icon = app.icons.for_entry_sized(dir, e, true);
        let s = size::ICON_LARGE.min(b.w);
        let at = Rect::new(b.x + (b.w - s) / 2.0, b.y + (b.h - s) / 2.0, s, s);
        r.icon(&app.icons, icon, at);
        at
    }

    /// The right-click menu (menu.rs): for the selected items, or for the folder's empty space.
    /// With Shift held, Windows' whole menu with its extra items, as in Explorer.
    fn context_menu(&mut self, at: Point, on_item: bool, app: &AppState) -> Effect {
        // SAFETY: a plain key-state query.
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        let how = menu::How { extended: shift, whole: shift };
        if on_item {
            menu::for_items(app, at, how)
        } else {
            menu::for_background(app, at, how)
        }
    }

    /// The Apps key and Shift+F10: the right-click menu at the selected item (scrolled into view),
    /// or at the top of the list when nothing is selected. `whole`: Windows' whole menu.
    fn keyboard_menu(&mut self, app: &mut AppState, whole: bool) -> Effect {
        let tab = app.tab_mut();
        if tab.waiting_long() {
            return Effect::None;
        }
        let chosen = tab.selected_entries();
        let row = tab.focus.filter(|f| chosen.contains(f)).or(chosen.first().copied()).and_then(|e| tab.row_of(e));
        let area = self.rows_area();
        let left = area.x + ROW_PAD + self.icon + 8.0;
        let at = match row {
            Some(r) => {
                self.reveal(tab, r);
                let cell = self.geo(tab).cell(r, tab.scroll);
                let x = if self.flow == Flow::Rows { left + 16.0 } else { cell.x + cell.w / 2.0 };
                Point::new(x.clamp(area.x, area.right()), cell.bottom().clamp(area.y, area.bottom()))
            }
            None => Point::new(left, area.y + 4.0),
        };
        let how = menu::How { extended: whole, whole };
        if row.is_some() {
            menu::for_items(app, at, how)
        } else {
            menu::for_background(app, at, how)
        }
    }

    /// The rectangle being dragged, on screen.
    fn band_rect(&self, b: &Band, scroll: f32) -> Rect {
        let start = if self.flow == Flow::Columns { Point::new(b.start.x - scroll, b.start.y) } else { Point::new(b.start.x, b.start.y - scroll) };
        let (x0, x1) = (start.x.min(b.now.x), start.x.max(b.now.x));
        let (y0, y1) = (start.y.min(b.now.y), start.y.max(b.now.y));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Selects what the dragged rectangle touches (with what was selected before, with Ctrl).
    fn band_select(&mut self, app: &mut AppState) {
        let Some(b) = self.band.as_mut().filter(|b| b.active) else { return };
        let tab = app.tab_mut();
        if b.tab_id != tab.id {
            self.band = None;
            return;
        }
        if b.base.len() != tab.folder.entries.len() {
            // The listing changed under the rectangle (the folder watch): start from nothing.
            b.base = vec![false; tab.folder.entries.len()];
        }
        let base = b.base.clone();
        let Some(b) = self.band.as_ref() else { return };
        let scroll = tab.scroll.clamp(0.0, self.max_scroll(tab));
        let rect = self.band_rect(b, scroll);
        let touched = self.geo(tab).touching(rect, scroll);
        tab.selected = base;
        let mut first = None;
        for row in touched {
            if let Some(i) = tab.index_at(row) {
                tab.selected[i] = true;
                first.get_or_insert(i);
            }
        }
        if let Some(i) = first {
            tab.focus = Some(i);
            tab.anchor = Some(i);
        }
    }
}

/// How a scrollbar's thumb looks: at rest, with the mouse near it, or being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbState {
    Rest,
    Hover,
    Dragged,
}

/// Draws a scrollbar (the file list's, the sidebar's): the skin's track along `lane` if it paints
/// one (with a line on the side facing the items), and the thumb in `thumb`: a bar with round ends
/// in the skin's `scrollbar` paint (a gradient shades it across, like a rod), its edge and three
/// grip lines across its middle if the skin has them. Being dragged, a flat thumb takes the accent
/// colour and a shaded one gets a little darker.
pub fn paint_scrollbar(r: &mut Renderer, theme: &Theme, lane: Rect, thumb: Rect, state: ThumbState) {
    let upright = thumb.h >= thumb.w;
    if !theme.scrollbar_track.is_clear() {
        r.fill_paint(lane, 0.0, &theme.scrollbar_track);
        let edge = if upright { Rect::new(lane.x, lane.y, 1.0, lane.h) } else { Rect::new(lane.x, lane.y, lane.w, 1.0) };
        r.fill_rect(edge, theme.line);
    }
    let radius = thumb.w.min(thumb.h) / 2.0;
    let shaded = theme.scroll_thumb_paint.gradient().is_some();
    let paint = match state {
        ThumbState::Dragged if shaded => theme.scroll_thumb_hover_paint.darker(0.1),
        ThumbState::Dragged => Paint::Flat(theme.accent),
        ThumbState::Hover => theme.scroll_thumb_hover_paint,
        ThumbState::Rest => theme.scroll_thumb_paint,
    };
    r.fill_paint(thumb, radius, &paint);
    if theme.scrollbar_border.a > 0.0 {
        r.stroke_round_rect(thumb.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, theme.scrollbar_border);
    }
    let long = if upright { thumb.h } else { thumb.w };
    if theme.scrollbar_grip && long >= 24.0 {
        let c = Point::new(r.snap(thumb.x + thumb.w / 2.0), r.snap(thumb.y + thumb.h / 2.0));
        let half = (thumb.w.min(thumb.h) * 0.28).max(2.0);
        for k in -1..=1 {
            let at = k as f32 * 3.0;
            let line = if upright { Rect::new(c.x - half, c.y + at - 0.5, 2.0 * half, 1.0) } else { Rect::new(c.x + at - 0.5, c.y - half, 1.0, 2.0 * half) };
            r.fill_rect(line, theme.scrollbar_grip_lines);
        }
    }
}

/// An item's name as the list shows it: without its extension when known extensions are left out
/// ("HideKnownExtensions") and Windows knows the type - like File Explorer. A name that is only an
/// extension (".gitignore") stays whole.
fn shown_name(e: &Entry, hide_ext: bool) -> &str {
    if hide_ext && e.kind == EntryKind::File && !e.ext.is_empty() && icons::known_type(&e.ext) {
        if let Some(i) = e.name.rfind('.').filter(|&i| i > 0) {
            return &e.name[..i];
        }
    }
    &e.name
}

/// `text` without `ext` at its end (any capitals), if it ends with it.
fn strip_extension<'a>(text: &'a str, ext: &str) -> &'a str {
    let cut = text.len().checked_sub(ext.len()).filter(|_| !ext.is_empty());
    match cut.and_then(|c| text.get(c..).filter(|end| end.eq_ignore_ascii_case(ext)).map(|_| c)) {
        Some(c) => &text[..c],
        None => text,
    }
}

/// Where a search result is, below the folder searched (`root`): "2024\Taxes", or "" right in it.
fn relative_folder(e: &Entry, root: &str) -> String {
    let dir = folder::result_folder(e);
    let root = root.trim_end_matches('\\');
    match dir.get(..root.len()) {
        Some(head) if fileops::same_path(head, root) && (dir.len() == root.len() || dir[root.len()..].starts_with('\\')) => {
            dir[root.len()..].trim_start_matches('\\').to_string()
        }
        _ => dir.to_string(),
    }
}

/// The time now, as a FILETIME (for the date examples in the column titles' menu).
fn now_filetime() -> u64 {
    // SAFETY: no arguments; returns the time now.
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// How far (DIPs, across and down) the mouse must move with a button held before a drag starts:
/// Windows' own setting (SM_CXDRAG / SM_CYDRAG, 4 pixels by default).
fn drag_distance(scale: f32) -> (f32, f32) {
    // SAFETY: plain system queries.
    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
    let s = scale.max(0.5);
    (cx.max(1) as f32 / s, cy.max(1) as f32 / s)
}

/// How close (DIPs) to the list's top or bottom edge a drag must be to scroll it.
const EDGE_SCROLL_ZONE: f32 = size::ROW_H * 1.25;
/// Scroll speed at the start of that zone and at the very edge, in rows per second.
const EDGE_SCROLL_SLOW: f32 = 4.0;
const EDGE_SCROLL_FAST: f32 = 32.0;

impl ListView {
    /// The selected items, to drag out (dragdrop.rs): only from a real folder whose listing is on
    /// screen (not This PC, not the Recycle Bin, not while another folder is on its way). Search
    /// results are found by their full path; they must all be in one folder (Windows' data object
    /// is made for items of one folder), otherwise the status bar says so and nothing is dragged.
    fn drag_out(&self, p: &Press, app: &mut AppState) -> Option<DragOut> {
        let tab = app.tab();
        let real = tab.id == p.tab_id && !tab.waiting_long() && !tab.folder.path.is_empty() && tab.folder.path == tab.path && !recycle::is_bin(&tab.path);
        if !real {
            return None;
        }
        let paths: Vec<String> = tab
            .selected_entries()
            .into_iter()
            .map(|i| &tab.folder.entries[i])
            .filter(|e| e.kind != EntryKind::Drive)
            .map(|e| e.path_in(&tab.folder.path))
            .collect();
        let folder = paths.first().and_then(|p| folder::parent(p))?;
        if !paths.iter().all(|p| folder::parent(p).is_some_and(|f| fileops::same_path(&f, &folder))) {
            app.say("Drag items from one folder at a time.");
            return None;
        }
        let names = paths.iter().map(|p| folder::file_name(p).to_string()).collect();
        Some(DragOut { folder, names, right: p.button == MouseButton::Right })
    }

    /// While something is dragged over the list (or a rectangle is dragged in it): how fast it
    /// should scroll (DIPs per second, negative = up, or left in the List view) with the mouse at
    /// `p` - faster the nearer the edge, 0 away from the edges or when it can't go further.
    pub fn drag_scroll_speed(&self, p: Point, app: &AppState) -> f32 {
        let area = self.rows_area();
        if p.x < self.rect.x || p.x >= self.rect.right() || p.y < self.rect.y || p.y >= self.rect.bottom() {
            return 0.0;
        }
        self.edge_speed(p, area, app.tab())
    }

    /// How fast to scroll with the mouse at `p` near the edges of `area` (see `drag_scroll_speed`;
    /// a rectangle being dragged also scrolls when the mouse is outside the list).
    fn edge_speed(&self, p: Point, area: Rect, tab: &Tab) -> f32 {
        let geo = self.geo(tab);
        let (pos, start, end) = if geo.sideways() { (p.x, area.x, area.right()) } else { (p.y, area.y, area.bottom()) };
        let (low, high) = (start + EDGE_SCROLL_ZONE, end - EDGE_SCROLL_ZONE);
        let lines_per_s = |depth: f32| EDGE_SCROLL_SLOW + (EDGE_SCROLL_FAST - EDGE_SCROLL_SLOW) * (depth / EDGE_SCROLL_ZONE).clamp(0.0, 1.0);
        // (A row of tiles is tall: scroll by rows' worth of DIPs, not by whole tiles.)
        let step = if self.flow == Flow::Grid { self.row_h } else { geo.line() };
        if pos < low && tab.scroll > 0.0 {
            -lines_per_s(low - pos) * step
        } else if pos > high && tab.scroll < geo.max_scroll() {
            lines_per_s(pos - high) * step
        } else {
            0.0
        }
    }

    /// Scrolls the list by `dy` DIPs (while a drag is at its edge).
    pub fn drag_scroll(&mut self, dy: f32, app: &mut AppState) {
        let max = self.max_scroll(app.tab());
        let tab = app.tab_mut();
        tab.scroll = (tab.scroll + dy).clamp(0.0, max);
        self.anim.stop();
    }

    /// Arrows, PgUp / PgDn, Home / End: the item the cursor goes to (None: the key isn't one of
    /// those). In the tree view, Right opens a folder (or goes into it) and Left closes it (or goes
    /// to the folder an item is in).
    fn cursor_key(&mut self, key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY, app: &mut AppState) -> Option<usize> {
        let tab = app.tab();
        let geo = self.geo(tab);
        let n = tab.view.len();
        let cur = tab.focus_row();
        let last = n.saturating_sub(1);
        // With no cursor yet, paging starts from the first item on screen (as in Explorer).
        let top = geo.first_shown(tab.scroll);
        let page = geo.page();
        let from = |step: Step| Some(cur.map_or(0, |r| geo.step(r, step)));
        match key {
            VK_UP => from(Step::Up),
            VK_DOWN => from(Step::Down),
            VK_LEFT | VK_RIGHT if self.flow == Flow::Rows => {
                if tab.view_mode != ViewMode::Tree || !tab.tree_here() {
                    return None;
                }
                let row = cur?;
                let index = tab.index_at(row)?;
                let folder = tab.folder.entries.get(index).is_some_and(|e| e.is_container());
                let open = tab.is_expanded(index);
                if key == VK_RIGHT {
                    if folder && !open {
                        app.tree_open(index);
                        return Some(row);
                    }
                    // Open already: into its first item (if it has any yet).
                    let next = tab.index_at(row + 1).filter(|&i| open && tab.depth(i) > tab.depth(index));
                    return Some(if next.is_some() { row + 1 } else { row });
                }
                if open {
                    // Closed, it may sit at another place in the listing: found again by its key.
                    let key = tab.item_key(index);
                    app.tree_close(index);
                    let t = app.tab();
                    return (0..t.folder.entries.len()).find(|&i| t.item_key(i) == key).and_then(|i| t.row_of(i));
                }
                tab.tree_parent(index).and_then(|p| tab.row_of(p)).or(Some(row))
            }
            VK_LEFT => from(Step::Left),
            VK_RIGHT => from(Step::Right),
            VK_PRIOR => Some(cur.map_or(top, |r| r.saturating_sub(page))),
            VK_NEXT => Some(cur.map_or(top + page, |r| r + page).min(last)),
            VK_HOME => Some(0),
            VK_END => Some(last),
            _ => None,
        }
    }
}

impl Component for ListView {
    fn layout(&mut self, rect: Rect, ctx: &Ctx, app: &AppState) {
        self.rect = rect;
        self.hwnd = ctx.hwnd;
        if (ctx.scale - self.scale).abs() > 0.001 {
            self.scale = ctx.scale;
            self.name_box_at = None; // the name box needs placing (and its font making) again
        }
        // Keep the name column usable on narrow windows by shrinking the others - from the widths
        // last set, so they come back when the window is made wide again.
        self.take_size(app);
        self.place_columns(app);
        if self.rename.is_some() {
            // (Tiles and columns move when the list's size changes.)
            self.name_box_at = None;
            self.place_name_box(app.tab());
        }
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, _ctx: &Ctx) {
        let tab = app.tab();
        self.take_size(app);
        self.place_columns(app);
        r.fill_paint(self.rect, 0.0, &theme.panel_paint);
        r.push_clip(self.rect);
        if self.flow == Flow::Rows {
            self.paint_header(r, theme, tab);
        }
        self.paint_rows(r, theme, app, tab);
        r.pop_clip();
    }

    fn cursor(&self, p: Point, app: &AppState) -> CursorKind {
        if matches!(self.drag, Drag::Column { .. }) || self.edge_at(p).is_some() {
            CursorKind::SizeWE
        } else if app.settings.single_click && self.row_at(app.tab(), p).is_some() && self.on_arrow(app.tab(), p).is_none() {
            // One click opens: the pointer says so, as in Explorer's single-click mode.
            CursorKind::Hand
        } else {
            CursorKind::Arrow
        }
    }

    fn mouse_move(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.mouse = Some(e.pos);
        self.was_active = ctx.active;
        // Moved far enough from where a button went down on an item: the selection is dragged out.
        if let Some(p) = self.press.as_ref().filter(|p| p.on_item && self.drag == Drag::None) {
            let (dx, dy) = drag_distance(self.scale);
            if (e.pos.x - p.at.x).abs() > dx || (e.pos.y - p.at.y).abs() > dy {
                let p = self.press.take();
                self.cancel_slow_click();
                self.click_open = None; // a drag, not a click: nothing opens
                if let Some(d) = p.and_then(|p| self.drag_out(&p, app)) {
                    return Effect::DragOut(d);
                }
            }
        }
        // A rectangle over the empty space: it selects what it touches.
        if let Some(b) = self.band.as_mut() {
            b.now = e.pos;
            if !b.active {
                let scroll = app.tab().scroll;
                let start = if self.flow == Flow::Columns { Point::new(b.start.x - scroll, b.start.y) } else { Point::new(b.start.x, b.start.y - scroll) };
                let (dx, dy) = drag_distance(self.scale);
                b.active = (e.pos.x - start.x).abs() > dx || (e.pos.y - start.y).abs() > dy;
                b.at = ctx.now;
            }
            if self.band.as_ref().is_some_and(|b| b.active) {
                self.band_select(app);
                // Near an edge (or past it), the list scrolls (see `tick`).
                return Effect::Animate;
            }
        }
        let geo = self.geo(app.tab());
        let tab = app.tab_mut();
        match self.drag {
            Drag::Thumb { grab } => {
                let (track, max) = (self.track(), geo.max_scroll());
                if let Some(thumb) = self.thumb(tab) {
                    let (pos, start, span) = if geo.sideways() { (e.pos.x, track.x, track.w - thumb.w) } else { (e.pos.y, track.y, track.h - thumb.h) };
                    tab.scroll = ((pos - grab - start) / span.max(1.0) * max).clamp(0.0, max);
                    self.anim.stop();
                }
            }
            Drag::Column { col, start_x, start_w } => {
                // Dragging an edge left makes the column right of it wider; the name column gives way.
                let w = (start_w + (start_x - e.pos.x)).max(MIN_COL);
                let others: f32 = self.cols.iter().skip(1).filter(|p| p.col != col).map(|p| p.w).sum();
                let max_w = (self.rect.w - SCROLL_GUTTER - MIN_NAME - others).max(MIN_COL);
                // Kept at the normal text size (see `text_growth`).
                self.wanted.insert(col.key(), w.min(max_w) / self.text_growth());
                self.place_columns(app);
            }
            Drag::None => {}
        }
        Effect::Redraw
    }

    fn mouse_leave(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        self.mouse = None;
        Effect::Redraw
    }

    fn mouse_down(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.mouse = Some(e.pos);
        self.cancel_slow_click();
        self.click_open = None;
        self.band = None;
        let single_click = app.settings.single_click;
        if e.button == MouseButton::Right && self.flow == Flow::Rows && self.header().contains(e.pos) {
            return self.header_menu(e.pos, app);
        }
        // The tree view: a click on a folder's arrow opens or closes it (and selects nothing).
        if e.button == MouseButton::Left {
            if let Some(index) = self.on_arrow(app.tab(), e.pos) {
                app.tree_toggle(index);
                return Effect::Redraw;
            }
        }
        let geo = self.geo(app.tab());
        let tab = app.tab_mut();
        if e.button == MouseButton::Left {
            // Scrollbar: grab the thumb, or page towards the click.
            if self.near_track(e.pos) {
                if let Some(thumb) = self.thumb(tab) {
                    let (pos, start, end) = if geo.sideways() { (e.pos.x, thumb.x, thumb.right()) } else { (e.pos.y, thumb.y, thumb.bottom()) };
                    if pos >= start && pos < end {
                        self.drag = Drag::Thumb { grab: pos - start };
                        return Effect::Redraw;
                    }
                    let page = geo.view() - geo.line();
                    let from = if self.anim.running() { self.anim.target() } else { tab.scroll };
                    let target = from + if pos < start { -page } else { page };
                    return self.scroll_to(tab, target, ctx.now);
                }
                return Effect::None;
            }
            // Header: resize at an edge, otherwise sort.
            if let Some(p) = self.edge_at(e.pos) {
                self.drag = Drag::Column { col: p.col, start_x: e.pos.x, start_w: p.w };
                return Effect::Redraw;
            }
            if let Some(col) = self.column_at(e.pos) {
                return match col.sort_key() {
                    Some(key) => Effect::Command(Command::SortBy(key)),
                    None => Effect::None,
                };
            }
        }
        if !self.rows_area().contains(e.pos) {
            return Effect::None;
        }
        // Middle-click on a folder: it opens in a new tab, behind this one (as in a web browser).
        if e.button == MouseButton::Middle {
            return match self.row_at(tab, e.pos).and_then(|row| tab.entry_at(row)) {
                Some(entry) if entry.is_container() => Effect::Command(Command::NewTabBehind(entry.path_in(&tab.folder.path))),
                _ => Effect::None,
            };
        }
        let tab_id = tab.id;
        let press = |on_item, then| Press { at: e.pos, button: e.button, tab_id, on_item, then };
        match (self.row_at(tab, e.pos), e.button) {
            (Some(row), MouseButton::Left) => {
                // A plain click on the name of the one item already selected may become a rename
                // (if no second click follows; see mouse_up). Not when this click brought the window
                // to the front, nor when one click opens items.
                let plain = !e.mods.shift && !e.mods.ctrl;
                let index = tab.index_at(row);
                let again = plain && index.is_some_and(|i| tab.is_selected(i) && tab.focus == Some(i)) && tab.selection_summary().0 == 1;
                // Pressing a selected item keeps the selection as it is until the button comes up:
                // the items may be dragged out together (dragdrop.rs).
                let picked = index.filter(|&i| tab.is_selected(i));
                let mut then = Then::Nothing;
                if e.mods.shift {
                    tab.select_range_to(row, e.mods.ctrl);
                } else if let (true, Some(i)) = (e.mods.ctrl, picked) {
                    then = Then::Unselect(tab.item_key(i));
                    tab.focus = Some(i);
                } else if e.mods.ctrl {
                    tab.toggle_row(row);
                } else if let Some(i) = picked.filter(|_| tab.selection_summary().0 > 1) {
                    then = Then::SelectOnly(tab.item_key(i));
                    tab.focus = Some(i);
                } else {
                    tab.select_only_row(row);
                }
                if let Some(index) = index.filter(|_| plain && single_click) {
                    // One click opens: when the button comes up on the same item.
                    self.click_open = Some(ClickOpen { tab_id: tab.id, folder: tab.folder.path.clone(), index, key: tab.item_key(index), at: e.pos });
                } else if let Some(index) = index.filter(|_| again && self.was_active && self.on_name(tab, row, e.pos) && !tab.folder.path.is_empty()) {
                    self.slow = Some(SlowClick { tab_id: tab.id, index, armed: false });
                }
                self.press = Some(press(true, then));
                Effect::Redraw
            }
            (Some(row), MouseButton::Right) => {
                if !tab.index_at(row).is_some_and(|i| tab.is_selected(i)) {
                    tab.select_only_row(row);
                }
                // The menu opens when the button comes up (moving first drags the items out).
                self.press = Some(press(true, Then::Nothing));
                Effect::Redraw
            }
            (None, MouseButton::Left) => {
                if !e.mods.ctrl {
                    tab.clear_selection();
                }
                // Dragging from here draws a rectangle that selects what it touches.
                if !tab.waiting_long() {
                    let start = if self.flow == Flow::Columns { Point::new(e.pos.x + tab.scroll, e.pos.y) } else { Point::new(e.pos.x, e.pos.y + tab.scroll) };
                    let base = if e.mods.ctrl { tab.selected.clone() } else { vec![false; tab.folder.entries.len()] };
                    self.band = Some(Band { tab_id: tab.id, start, now: e.pos, base, active: false, at: ctx.now });
                }
                Effect::Redraw
            }
            (None, MouseButton::Right) => {
                tab.clear_selection();
                self.press = Some(press(false, Then::Nothing));
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    fn mouse_up(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if self.drag != Drag::None {
            self.drag = Drag::None;
            return Effect::Redraw;
        }
        if self.band.take().is_some_and(|b| b.active) {
            return Effect::Redraw;
        }
        // The button came up without a drag: the click finishes (or the right-click menu opens).
        // Another button coming up (the capture was lost, say) only forgets the press.
        let mut finished = Effect::None;
        if let Some(p) = self.press.take().filter(|p| p.button == e.button && p.tab_id == app.tab().id) {
            let tab = app.tab_mut();
            let row_of_key = |tab: &Tab, key: &str| (0..tab.view.len()).find(|&r| tab.index_at(r).is_some_and(|i| tab.item_key(i) == key));
            match (p.button, &p.then) {
                (MouseButton::Right, _) => return self.context_menu(e.pos, p.on_item, app),
                (_, Then::SelectOnly(key)) => {
                    if let Some(row) = row_of_key(tab, key) {
                        tab.select_only_row(row);
                    }
                    finished = Effect::Redraw;
                }
                (_, Then::Unselect(key)) => {
                    if let Some(row) = row_of_key(tab, key).filter(|&r| tab.index_at(r).is_some_and(|i| tab.is_selected(i))) {
                        tab.toggle_row(row);
                    }
                    finished = Effect::Redraw;
                }
                (_, Then::Nothing) => {}
            }
        }
        let tab = app.tab();
        let clicked = if e.button == MouseButton::Left { self.row_at(tab, e.pos).and_then(|r| tab.index_at(r)) } else { None };
        // One click opens: the item the button went down on, if it comes up on the very same item
        // of the same listing without having moved away (a drag, a click that slid off).
        if let Some(c) = self.click_open.take() {
            let near = (e.pos.x - c.at.x).abs() <= CLICK_SLOP && (e.pos.y - c.at.y).abs() <= CLICK_SLOP;
            let same_item = clicked == Some(c.index) && tab.item_key(c.index) == c.key;
            if near && same_item && c.tab_id == tab.id && c.folder == tab.folder.path && app.settings.single_click {
                self.opened_at = Some(ctx.now);
                return Effect::Command(Command::OpenEntry(c.index));
            }
            return finished;
        }
        // The first click of a slow double-click came up on its item: rename when the double-click
        // time has passed without a second click.
        let hwnd = self.hwnd;
        match self.slow.as_mut() {
            Some(s) if !s.armed && clicked == Some(s.index) && !hwnd.is_invalid() => {
                s.armed = true;
                // SAFETY: a one-off timer on our own window; window.rs stops it when it fires.
                unsafe {
                    SetTimer(Some(hwnd), SLOW_CLICK_TIMER, GetDoubleClickTime() + 50, None);
                }
            }
            Some(s) if !s.armed => self.slow = None,
            _ => {}
        }
        finished
    }

    fn double_click(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.cancel_slow_click();
        self.press = None;
        self.band = None;
        // With one click opening, the first click already opened: the second must not open whatever
        // is under the mouse now (in the folder just opened, say), nor go up from there.
        self.click_open = None;
        // SAFETY: a plain query.
        let double_time = unsafe { GetDoubleClickTime() } as f64 / 1000.0;
        if self.opened_at.take().is_some_and(|t| ctx.now - t <= double_time + 0.1) {
            return Effect::None;
        }
        if e.button != MouseButton::Left || !self.rows_area().contains(e.pos) || self.near_track(e.pos) {
            return Effect::None;
        }
        // The tree view: the second click on an arrow opens or closes the folder again, as two
        // clicks would.
        if let Some(index) = self.on_arrow(app.tab(), e.pos) {
            app.tree_toggle(index);
            return Effect::Redraw;
        }
        let tab = app.tab();
        match self.row_at(tab, e.pos).and_then(|row| tab.index_at(row)) {
            Some(_) if app.settings.single_click => Effect::None,
            Some(index) => Effect::Command(Command::OpenEntry(index)),
            // The empty space below the items: up a folder, if the setting says so.
            None if app.settings.double_click_up && !tab.waiting_long() && !e.mods.ctrl && !e.mods.shift => Effect::Command(Command::Up),
            None => Effect::None,
        }
    }

    fn wheel(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.cancel_slow_click();
        // Scrolling while renaming keeps what was typed, like clicking elsewhere.
        if self.rename.is_some() {
            return self.end_rename(ctx.now, true, true).map_or(Effect::Redraw, Effect::Command);
        }
        let geo = self.geo(app.tab());
        let tab = app.tab_mut();
        let from = if self.anim.running() && self.anim_tab == tab.id { self.anim.target() } else { tab.scroll };
        // A notch moves the rows the Windows setting says; a column of the List view; at least half
        // a row of tiles.
        let step = match self.flow {
            Flow::Rows => self.wheel_lines * self.row_h,
            Flow::Columns => geo.cell_w,
            Flow::Grid => (self.wheel_lines * size::ROW_H).max(geo.cell_h * 0.5),
        };
        let target = from - e.wheel * step;
        self.scroll_to(tab, target, ctx.now)
    }

    fn key_down(&mut self, ctx: &Ctx, vk: u16, mods: crate::ui::Mods, app: &mut AppState) -> Effect {
        self.cancel_slow_click();
        if app.tab().waiting_long() {
            return Effect::None; // the items on screen aren't this folder's (see paint_rows)
        }
        let key = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk);
        // (Ctrl+arrows and Shift+arrows: moving the cursor without selecting, selecting a range.)
        if let Some(row) = self.cursor_key(key, app) {
            let tab = app.tab_mut();
            self.move_cursor(tab, row, mods.shift, mods.ctrl);
            return Effect::Redraw;
        }
        let tab = app.tab_mut();
        let cur = tab.focus_row();
        match key {
            VK_RETURN if mods.ctrl => Effect::Command(Command::OpenInNewTabs),
            // The right-click menu from the keyboard; Shift+F10 (and Shift+Apps) Windows' whole menu.
            VK_APPS => self.keyboard_menu(app, mods.shift),
            VK_F10 if mods.shift => self.keyboard_menu(app, true),
            VK_RETURN => Effect::Command(Command::OpenSelected),
            VK_A if mods.ctrl => {
                tab.select_all();
                Effect::Redraw
            }
            VK_SPACE if mods.ctrl => {
                if let Some(r) = cur {
                    tab.toggle_row(r);
                }
                Effect::Redraw
            }
            // Esc clears the filter first, then the selection.
            VK_ESCAPE if !tab.filter.is_empty() => Effect::Command(Command::SetFilter(String::new())),
            VK_ESCAPE => {
                tab.clear_selection();
                Effect::Redraw
            }
            // Working with files (app/files.rs). Ctrl+Insert and Shift+Insert are the old copy and
            // paste keys, which Explorer still knows.
            VK_C if mods.ctrl && mods.shift => Effect::Command(Command::CopyPath),
            VK_C | VK_INSERT if mods.ctrl && !mods.shift => Effect::Command(Command::CopyFiles),
            VK_X if mods.ctrl && !mods.shift => Effect::Command(Command::CutFiles),
            VK_V if mods.ctrl && !mods.shift => Effect::Command(Command::Paste),
            VK_INSERT if mods.shift && !mods.ctrl => Effect::Command(Command::Paste),
            VK_DELETE if !mods.ctrl => Effect::Command(Command::Delete { permanent: mods.shift }),
            VK_F2 if !mods.ctrl && !mods.shift => self.begin_rename_selected(ctx, app),
            VK_N if mods.ctrl && mods.shift => Effect::Command(Command::NewFolder),
            _ => Effect::None,
        }
    }

    fn char_input(&mut self, ctx: &Ctx, ch: char, app: &mut AppState) -> Effect {
        // (A space that doesn't go on with a name being typed is the Space preview's.)
        if ch.is_control() || (ch == ' ' && !self.typing(ctx.now)) || app.tab().waiting_long() {
            return Effect::None;
        }
        if self.jump_to_typed(app.tab_mut(), ch, ctx.now) {
            Effect::Redraw
        } else {
            Effect::None
        }
    }

    fn menu_pick(&mut self, _ctx: &Ctx, tag: u32, item: u32, app: &mut AppState) -> Effect {
        if tag == MENU_HEADER {
            return self.header_pick(item, app);
        }
        menu::pick(item, app)
    }

    fn drop_target(&self, p: Point, app: &AppState) -> Option<DropTarget> {
        let tab = app.tab();
        // Only into what's on screen: not while another folder is on its way (the rows aren't
        // that folder's).
        if tab.folder.path != tab.path || tab.waiting_long() {
            return None;
        }
        // The Recycle Bin open in the list: whatever is dropped there is deleted.
        if recycle::is_bin(&tab.path) {
            return Some(DropTarget::RecycleBin);
        }
        // A folder that couldn't be read (gone, no permission) takes nothing.
        if tab.folder.error.is_some() {
            return None;
        }
        // A folder's (or in This PC a drive's) row: into it. Search results by their full path.
        if let Some(e) = self.row_at(tab, p).and_then(|row| tab.entry_at(row)) {
            if e.is_container() {
                return Some(DropTarget::Folder(e.path_in(&tab.folder.path)));
            }
        }
        // Anywhere else (a file's row, empty space): into the folder on screen (the folder
        // searched, for search results). This PC holds only drives.
        (!tab.folder.path.is_empty()).then(|| DropTarget::Folder(tab.folder.path.clone()))
    }

    fn drop_hover(&mut self, target: Option<&DropTarget>) -> Effect {
        let row = match target {
            Some(DropTarget::Folder(path)) => Some(path.to_lowercase()),
            _ => None,
        };
        if row == self.drop_row {
            return Effect::None;
        }
        self.drop_row = row;
        Effect::Redraw
    }

    fn text_event(&mut self, ctx: &Ctx, ev: &TextEvent, _app: &mut AppState) -> Effect {
        if ev.id != RENAME_BOX || self.rename.is_none() {
            return Effect::None;
        }
        let cmd = match ev.kind {
            TextEventKind::Enter => self.end_rename(ctx.now, true, true),
            TextEventKind::Escape => {
                self.end_rename(ctx.now, false, true);
                None
            }
            // Clicking elsewhere (or switching to another program) keeps what was typed, as in
            // Explorer. The message was posted, so the box may have the keyboard back by now.
            TextEventKind::LostFocus if !self.name_box.as_ref().is_some_and(|b| b.has_focus()) => self.end_rename(ctx.now, true, false),
            _ => return Effect::None,
        };
        cmd.map_or(Effect::Redraw, Effect::Command)
    }

    fn tick(&mut self, now: f64, app: &mut AppState) -> bool {
        self.take_size(app);
        let geo = self.geo(app.tab());
        let tab = app.tab_mut();
        if self.anim_tab != tab.id {
            self.anim.stop();
            self.anim_tab = tab.id;
        }
        if let Some(v) = self.anim.value(now) {
            tab.scroll = v;
        }
        let max = geo.max_scroll();
        tab.scroll = tab.scroll.clamp(0.0, max);
        // How a step of scrolling goes in this view, for a re-read that keeps the top item in place.
        tab.lines = Some((geo.line(), geo.per_line));
        if tab.reveal_focus {
            tab.reveal_focus = false;
            if let Some(row) = tab.focus_row() {
                // Centre it if it's out of sight (e.g. going up to a folder with thousands of items).
                if !geo.shows(row, tab.scroll) {
                    tab.scroll = geo.centre(row);
                }
            }
        }
        // A rectangle dragged to (or past) an edge scrolls the list, selecting as it goes.
        let mut banding = false;
        if let Some(b) = self.band.as_mut().filter(|b| b.active) {
            let dt = (now - b.at).clamp(0.0, 0.1) as f32;
            b.at = now;
            let mouse = b.now;
            let speed = self.edge_speed(mouse, self.rows_area(), app.tab());
            if speed != 0.0 {
                let tab = app.tab_mut();
                tab.scroll = (tab.scroll + speed * dt).clamp(0.0, max);
                self.anim.stop();
                self.band_select(app);
                banding = true;
            }
        }
        // Wine (only) shows a frame once the next one is drawn, so the last frame after a listing
        // arrives - the result of a paste or rename, say - would stay off screen until the mouse
        // moves. Paint for a moment instead. Windows shows every frame at once. (Background
        // results that come without a read of the tab - the folder watch's - use `wine_nudge`.)
        let loading = app.tab().loading.is_some();
        if self.was_loading && !loading && frame::is_wine() {
            self.repaint_until = self.repaint_until.max(now + 0.3);
        }
        self.was_loading = loading;
        // The name box follows its item; a new folder's box opens once the folder is read again.
        self.keep_name_box(now, app);
        self.take_rename_request(now, app);
        self.anim.running() || now < self.repaint_until || banding
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn file(name: &str) -> Entry {
        Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: EntryKind::File,
            size: 1,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: folder::extension(name),
            type_name: Arc::from("File"),
            target: None,
            free: 0,
        }
    }

    #[test]
    fn names_keep_their_extension_unless_asked() {
        let e = file("report.pdf");
        assert_eq!(shown_name(&e, false), "report.pdf");
        // A name that is only an extension, or has none, stays whole either way.
        assert_eq!(shown_name(&file(".gitignore"), true), ".gitignore");
        assert_eq!(shown_name(&file("README"), true), "README");
        let folder = Entry { kind: EntryKind::Folder, ..file("my.folder") };
        assert_eq!(shown_name(&folder, true), "my.folder");
    }

    #[test]
    fn search_results_show_where_they_are() {
        let mut e = file("a.txt");
        e.target = Some("C:\\Work\\2024\\Taxes\\a.txt".into());
        assert_eq!(relative_folder(&e, "C:\\Work"), "2024\\Taxes");
        assert_eq!(relative_folder(&e, "c:\\work\\"), "2024\\Taxes", "capitals and a trailing backslash don't matter");
        assert_eq!(relative_folder(&e, "C:\\Wor"), "C:\\Work\\2024\\Taxes", "a folder that only starts the same");
        e.target = Some("C:\\Work\\a.txt".into());
        assert_eq!(relative_folder(&e, "C:\\Work"), "", "right in the folder searched");
        e.target = Some("C:\\a.txt".into());
        assert_eq!(relative_folder(&e, "C:\\"), "");
        e.target = Some("C:\\x\\a.txt".into());
        assert_eq!(relative_folder(&e, "C:\\"), "x");
    }

    #[test]
    fn a_name_typed_before_loses_the_hidden_extension() {
        assert_eq!(strip_extension("draft.PDF", ".pdf"), "draft");
        assert_eq!(strip_extension("draft", ".pdf"), "draft");
        assert_eq!(strip_extension("é.pdf", ".pdf"), "é");
        assert_eq!(strip_extension("x", ""), "x");
    }
}
