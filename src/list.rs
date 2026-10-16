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
//! - which columns show next to Name ("Columns": Date modified, Date created, Type, Size, and Picture
//!   size and Length, which media.rs reads in the background), plus a Folder column while subfolders
//!   are searched (search.rs);
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
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetFocus, VK_A, VK_C, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F2, VK_HOME, VK_INSERT, VK_LEFT, VK_N, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP, VK_V, VK_X, VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, SystemParametersInfoW, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
// Dragging items out, and things dragged in (dragdrop.rs).
use crate::ui::{DragOut, DropTarget};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};

use crate::app::{AppState, SortKey, Tab, THIS_PC_NOTE};
use crate::fileops;
use crate::home;
use crate::folder::{self, Entry, EntryKind};
use crate::frame;
use crate::icons;
use crate::media;
use crate::recycle::{self, BinItem};
use crate::pathbar::{fill_box_paint, outline_box};
use crate::render::{Align, Renderer, TextStyle, WallDraw};
use crate::settings::{DateFormat, ListSize, SizeFormat, ViewMode};
use crate::thumbs;
use crate::views::{Flow, Geometry, Step, Tile, LIST_COLUMN_W, NAME_LINE_H, SCROLL_GUTTER, TILE_PAD, TILE_TEXT_GAP};
use crate::textbox::{TextBox, TextEvent, TextEventKind};
use crate::theme::{size, ButtonStyle, Color, Paint, Theme, WallpaperFit};
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
/// The window timer that repeats a held scroll bar arrow or track (an older Windows); window.rs
/// hands it to [`ListView::repeat_due`].
pub const BAR_REPEAT_TIMER: usize = 11;

/// The column titles' right-click menu (its tag; the files menus are menu.rs's 1 and 2).
const MENU_HEADER: u32 = 10;
// Its items.
const HEAD_MODIFIED: u32 = 1;
const HEAD_CREATED: u32 = 2;
const HEAD_TYPE: u32 = 3;
const HEAD_SIZE: u32 = 4;
const HEAD_DIMENSIONS: u32 = 5;
const HEAD_LENGTH: u32 = 6;
const HEAD_FOLDERS_FIRST: u32 = 10;
const HEAD_EXTENSIONS: u32 = 11;
const HEAD_FOLDER_SIZES: u32 = 12;
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
    /// Picture size: a picture's or a video's width and height (media.rs).
    Dimensions,
    /// How long a video or a song plays (media.rs).
    Length,
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
            Col::Dimensions => "dimensions",
            Col::Length => "length",
            Col::Folder => "folder",
            Col::Free => "free",
            Col::Location => "location",
        }
    }

    fn default_width(self) -> f32 {
        match self {
            Col::Size => 90.0,
            Col::Dimensions => 110.0,
            Col::Length => 80.0,
            Col::Free => 110.0,
            Col::Folder => 220.0,
            Col::Location => 260.0,
            _ => 150.0,
        }
    }

    /// Its title; `bin`: in the Recycle Bin, whose dates are when items were deleted; `recent`: in
    /// Recent, whose dates are when files were opened.
    fn title(self, this_pc: bool, bin: bool, recent: bool) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Modified if bin => "Date deleted",
            Col::Modified if recent => "Date accessed",
            Col::Modified => "Date modified",
            Col::Created => "Date created",
            Col::Type => "Type",
            Col::Size if this_pc => "Total size",
            Col::Size => "Size",
            Col::Dimensions => "Picture size",
            Col::Length => "Length",
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
            Col::Dimensions => SortKey::Dimensions,
            Col::Length => SortKey::Length,
            Col::Folder => SortKey::Folder,
            Col::Location => return None,
        })
    }

    fn right_aligned(self) -> bool {
        matches!(self, Col::Size | Col::Dimensions | Col::Length)
    }
}

/// The columns next to Name that settings.json can switch on, in the list's order.
const OPTIONAL: [Col; 6] = [Col::Modified, Col::Created, Col::Type, Col::Size, Col::Dimensions, Col::Length];

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

/// What the list draws lit up because of where the mouse is.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct HoverKey {
    /// The item under it (a row, tile or name).
    item: Option<usize>,
    /// The column title under it.
    column: Option<Col>,
    /// Near the scroll bar: its thumb shows.
    track: bool,
    /// Home: the tile, line, chevron or pill under it.
    spot: Option<home::Spot>,
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
    /// It moved away, but nothing can be dragged from here (the Recycle Bin, This PC's drives):
    /// not tried again at every move.
    refused: bool,
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
    /// An older Windows' scroll bar arrow button or track held down (`ListView::repeat` says
    /// which).
    Bar,
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
    /// How long the scroll bar's arrow buttons are (an older Windows; 0 otherwise).
    arrows: f32,
    /// How far in the column titles sit (98 / 2000: inside the list's 2 DIP sunken edge).
    edge_in: f32,
    mouse: Option<Point>,
    /// What the mouse lit up in the last frame painted: a move that changes none of it needs no
    /// new frame.
    painted_hover: HoverKey,
    drag: Drag,
    /// The scroll bar's held arrow or track, repeating (an older Windows).
    repeat: crate::retroctl::Repeat,
    anim: Anim,
    /// The tab the running animation belongs to.
    anim_tab: u64,
    wheel_lines: f32,
    /// Ctrl + wheel notches not yet used (a touchpad sends them in small parts).
    zoom_wheel: f32,
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
    /// The keyboard is in the list (the window is active and no text box has it): 98 / 2000 show
    /// the selection in their colours only then, in grey otherwise.
    has_focus: bool,
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
    /// Home's page (home.rs) and whether it's on screen (at the last frame).
    home: home::Home,
    home_on: bool,
    /// Split view: where the wallpaper lies (both halves); None: this list.
    wall_area: Option<Rect>,
    /// How media.rs tells this list's asks from the other half's (split view), and what this list
    /// last asked it for (Picture size and Length).
    media_id: u32,
    media: MediaRead,
}

/// Numbers for the lists, for media.rs.
static NEXT_LIST: AtomicU32 = AtomicU32::new(1);

/// While picture sizes or lengths come in, a tab sorted by one of them sorts again at most this
/// often (seconds), so its rows don't jump about with every value.
const RESORT_EVERY: f64 = 0.25;

/// What a list last asked media.rs for (the Picture size and Length columns), and how far through
/// its listing handing over the rest has got.
#[derive(Default)]
struct MediaRead {
    /// The tab and the listing (its folder, how many items) asked for, and the columns then: a
    /// change starts the rest again from the top.
    tab: u64,
    listing: String,
    entries: usize,
    cols: media::Columns,
    /// The listing's files may be read (it isn't inside a .zip or .7z), and it's on the PC's own
    /// drives (the rest is read there; elsewhere only while the tab is sorted by one of the columns).
    readable: bool,
    local: bool,
    /// The next entry to look at for the rest, and how many of the rest media.rs still had waiting.
    next: usize,
    waiting: usize,
    /// What was handed over before was for an older listing (said with the next ask).
    restart: bool,
    /// This list asked during the last frame; it has asks at media.rs.
    asked: bool,
    active: bool,
    /// `media::arrived` when the tab was last sorted again by a column, and when that was.
    sorted: u64,
    sorted_at: f64,
    /// The tabs' folders when media.rs was last told which to keep (a hash).
    kept: u64,
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
        let all = [Col::Modified, Col::Created, Col::Type, Col::Size, Col::Dimensions, Col::Length, Col::Folder, Col::Free, Col::Location];
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
            arrows: 0.0,
            edge_in: 0.0,
            mouse: None,
            painted_hover: HoverKey::default(),
            drag: Drag::None,
            repeat: crate::retroctl::Repeat::default(),
            anim: Anim::default(),
            anim_tab: 0,
            // "One screen per notch" comes back as u32::MAX; treat it as a big step.
            wheel_lines: if lines == u32::MAX { 20.0 } else { lines.clamp(1, 20) as f32 },
            zoom_wheel: 0.0,
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
            has_focus: true,
            repaint_until: 0.0,
            was_loading: false,
            press: None,
            drop_row: None,
            flow: Flow::Rows,
            tile: Tile::of(ViewMode::Medium),
            mode: ViewMode::Details,
            band: None,
            list_hit: Vec::new(),
            home: home::Home::default(),
            home_on: false,
            wall_area: None,
            media_id: NEXT_LIST.fetch_add(1, Ordering::Relaxed),
            media: MediaRead::default(),
        }
    }

    /// The widths of the columns next to Name as last set (by default or by dragging), in DIPs,
    /// by their names in settings.json ("modified", "created", "type", "size", "dimensions", "length",
    /// "folder", "free", "location").
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
        self.arrows = crate::retroctl::arrows(&app.theme);
        self.edge_in = if app.theme.era.classic() && app.theme.bevels { 2.0 } else { 0.0 };
        self.home_on = Self::is_home(app.tab());
        let mode = if self.home_on { ViewMode::Details } else { app.tab().view_mode };
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

    /// Whether the tab shows Home's page (home.rs) rather than rows: Home read and on screen.
    fn is_home(tab: &Tab) -> bool {
        crate::recent::is_recent(&tab.folder.path) && tab.folder.home.is_some() && tab.search.is_none() && !tab.waiting_long()
    }

    /// Home: the page under a point (in the page's own coordinates).
    fn home_point(&self, tab: &Tab, p: Point) -> Point {
        Point::new(p.x - self.rect.x, p.y - self.rect.y + tab.scroll.clamp(0.0, self.max_scroll(tab)))
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
        if crate::recent::is_recent(&t.folder.path) {
            return vec![Col::Name, Col::Folder, Col::Modified, Col::Size, Col::Type];
        }
        // Picture size and Length only where something can have them (pictures, videos, songs).
        let here = |c: &Col| match c {
            Col::Dimensions => t.media_here.dims,
            Col::Length => t.media_here.length,
            _ => true,
        };
        let mut cols = vec![Col::Name];
        cols.extend(OPTIONAL.iter().copied().filter(|c| app.settings.shows_column(c.key()) && here(c)));
        if t.is_searching() {
            cols.push(Col::Folder);
        }
        cols
    }

    fn is_this_pc(t: &Tab) -> bool {
        // (A search of every drive from This PC shows files, with the search results' columns.)
        t.folder.path.is_empty() && !t.folder.entries.is_empty() && !t.is_searching()
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
        let h = if self.flow == Flow::Rows && !self.home_on { size::HEADER_H } else { 0.0 };
        // 98 / 2000: inside the list's sunken edge, so its own raised edge shows all round.
        let e = self.edge_in;
        Rect::new(self.rect.x + e, self.rect.y + e, (self.rect.w - 2.0 * e).max(0.0), h)
    }

    /// The area the items scroll in (under the header and its 1-DIP line, where there is one).
    fn rows_area(&self) -> Rect {
        if self.flow != Flow::Rows || self.home_on {
            return self.rect;
        }
        let top = self.rect.y + self.edge_in + size::HEADER_H + 1.0;
        Rect::new(self.rect.x, top, self.rect.w, (self.rect.bottom() - top).max(0.0))
    }

    fn max_scroll(&self, tab: &Tab) -> f32 {
        if self.home_on {
            return (self.home.layout.height - self.rect.h).max(0.0);
        }
        self.geo(tab).max_scroll()
    }

    /// The item under a point (None over empty space and the scrollbar, and while the list says
    /// "Loading…" instead of showing items).
    fn row_at(&self, tab: &Tab, p: Point) -> Option<usize> {
        if tab.waiting_long() || self.near_track(p) {
            return None;
        }
        if self.home_on {
            if !self.rect.contains(p) {
                return None;
            }
            return match self.home.layout.spot_at(self.home_point(tab, p)) {
                Some(home::Spot::Item(i)) => tab.row_of(i),
                _ => None,
            };
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
        // Clear of an older Windows' arrow buttons at the two ends, and inside 98 / 2000's sunken
        // edge round the list.
        let along = m + self.arrows;
        let e = if self.home_on { 0.0 } else { self.edge_in };
        if self.flow == Flow::Columns {
            return Rect::new(area.x + e + along, self.rect.bottom() - e - w - m, (area.w - 2.0 * (e + along)).max(0.0), w);
        }
        Rect::new(self.rect.right() - e - w - m, area.y + along, w, (area.h - e - 2.0 * along).max(0.0))
    }

    /// The scrollbar's whole lane: the track and the space beside it (where a skin paints a track),
    /// and an older Windows' arrow buttons.
    fn lane(&self) -> Rect {
        let t = self.track();
        let (m, a) = (self.bar_margin, self.arrows);
        if self.flow == Flow::Columns {
            return Rect::new(t.x - m - a, t.y - m, t.w + 2.0 * (m + a), t.h + 2.0 * m);
        }
        Rect::new(t.x - m, t.y - m - a, t.w + 2.0 * m, t.h + 2.0 * (m + a))
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
        self.thumb_at(tab, tab.scroll)
    }

    /// Where the thumb is once the list is scrolled to `scroll`.
    fn thumb_at(&self, tab: &Tab, scroll: f32) -> Option<Rect> {
        if self.home_on {
            let (max, track) = (self.max_scroll(tab), self.track());
            if max <= 0.0 {
                return None;
            }
            let h = (track.h * self.rect.h / self.home.layout.height).clamp(24.0_f32.min(track.h), track.h);
            return Some(Rect::new(track.x, track.y + (track.h - h) * (scroll / max).clamp(0.0, 1.0), track.w, h));
        }
        let geo = self.geo(tab);
        let max = geo.max_scroll();
        if max <= 0.0 || tab.waiting_long() {
            return None;
        }
        let track = self.track();
        let along = (scroll / max).clamp(0.0, 1.0);
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
        if self.home_on {
            if let Some(r) = tab.index_at(row).and_then(|i| self.home.layout.rect_of(i)) {
                let top = tab.scroll.min(r.y - 8.0).max(r.bottom() + 8.0 - self.rect.h);
                tab.scroll = top.clamp(0.0, self.max_scroll(tab));
            }
            self.anim.stop();
            return;
        }
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

    /// Where the scroll position is heading (where it is, when not gliding).
    fn scroll_target(&self, tab: &Tab) -> f32 {
        if self.anim.running() {
            self.anim.target()
        } else {
            tab.scroll
        }
    }

    /// The mouse went down on an older Windows' scroll bar arrow or track: the first step now, the
    /// next ones on BAR_REPEAT_TIMER while the button is held.
    fn press_bar(&mut self, tab: &mut Tab, hold: crate::retroctl::Hold, now: f64) -> Effect {
        self.drag = Drag::Bar;
        let wait = self.repeat.press(hold, now);
        crate::retroctl::repeat_timer(self.hwnd, BAR_REPEAT_TIMER, Some(wait));
        match self.bar_step(tab, hold, now) {
            Effect::None => Effect::Redraw,
            e => e,
        }
    }

    /// One step of a held scroll bar arrow (a line) or track (a page, while the thumb hasn't
    /// reached the mouse).
    fn bar_step(&mut self, tab: &mut Tab, hold: crate::retroctl::Hold, now: f64) -> Effect {
        use crate::retroctl::Hold;
        let geo = self.geo(tab);
        let from = self.scroll_target(tab);
        let amount = match hold {
            Hold::Arrow(_) if self.home_on => crate::kit::ARROW_STEP,
            Hold::Arrow(_) => geo.line(),
            Hold::Page(way) => {
                let on = self.mouse.zip(self.thumb_at(tab, from)).is_some_and(|(p, t)| crate::retroctl::page_on(self.lane(), t, way, p));
                if !on {
                    return Effect::None;
                }
                if self.home_on {
                    (self.rect.h - crate::kit::ARROW_STEP).max(crate::kit::ARROW_STEP)
                } else {
                    geo.view() - geo.line()
                }
            }
            Hold::None => return Effect::None,
        };
        self.scroll_to(tab, from + hold.way().sign() * amount, now)
    }

    /// BAR_REPEAT_TIMER fired: the held scroll bar arrow or track takes its next step; the timer
    /// stops at the end.
    pub fn repeat_due(&mut self, ctx: &Ctx, app: &mut AppState) -> Effect {
        let tab = app.tab_mut();
        let room = crate::retroctl::Room::of(self.scroll_target(tab), self.max_scroll(tab)).toward(self.repeat.hold.way());
        let (hold, steps, next) = self.repeat.fire(room, ctx.now);
        crate::retroctl::repeat_timer(self.hwnd, BAR_REPEAT_TIMER, next);
        let mut effect = Effect::Redraw;
        for _ in 0..steps {
            if let e @ Effect::Animate = self.bar_step(tab, hold, ctx.now) {
                effect = e;
            }
        }
        effect
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
        let find = |needle: &str, start: usize| (0..n).map(|k| (start + k) % n).find(|&row| tab.entry_at(row).is_some_and(|e| e.lower.starts_with(needle)));
        let mut found = find(&needle, start);
        if found.is_none() && self.typed.chars().count() > 1 {
            // No name starts with the letters typed together: the last letter starts again on its
            // own (File Explorer just stops here), going on from the item selected now.
            self.typed = ch.to_lowercase().collect();
            found = find(&self.typed, cur.map_or(0, |r| r + 1));
        }
        let Some(row) = found else { return false };
        tab.select_only_row(row);
        self.reveal(tab, row);
        true
    }

    // ------------------------------------------------------------ renaming

    /// Opens the name box on the selected item (F2, Rename in the menu, Command::BeginRename): the
    /// one with the cursor, else the first selected.
    pub fn begin_rename_selected(&mut self, ctx: &Ctx, app: &mut AppState) -> Effect {
        let tab = app.tab();
        // (What a search of every drive found there can be renamed, like any search result.)
        if tab.path.is_empty() && !tab.is_searching() {
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
        // Several selected: they're renamed all at once, with a form saying how (app/forms.rs).
        if chosen.len() > 1 && !crate::recent::is_recent(&tab.folder.path) {
            return Effect::Command(Command::RenameMany);
        }
        let Some(index) = tab.focus.filter(|f| chosen.contains(f)).or(chosen.first().copied()) else { return Effect::None };
        let hide_ext = app.settings.hide_known_extensions;
        self.begin_rename(ctx.now, app.tab_mut(), index, None, hide_ext)
    }

    /// Opens the name box on entry `index`, showing `text` (or its name) with the part before the
    /// extension selected. `hide_ext`: known extensions are left out of names ("HideKnownExtensions"):
    /// the box then holds the name without it, all selected, and the extension is put back after.
    fn begin_rename(&mut self, now: f64, tab: &mut Tab, index: usize, text: Option<&str>, hide_ext: bool) -> Effect {
        let Some(e) = tab.folder.entries.get(index) else { return Effect::None };
        // Not drives, and not the Recycle Bin's deleted items. (This PC's search results carry their
        // full path.)
        if e.kind == EntryKind::Drive || (tab.folder.path.is_empty() && e.target.is_none()) || folder::is_place(&tab.folder.path) || self.hwnd.is_invalid() {
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
            col(HEAD_DIMENSIONS, "Picture size", "dimensions"),
            col(HEAD_LENGTH, "Length", "length"),
            MenuItem::separator(),
            MenuItem::new(HEAD_FOLDERS_FIRST, "Folders first").checked(s.folders_first),
            MenuItem::new(HEAD_EXTENSIONS, "File extensions").checked(!s.hide_known_extensions),
            MenuItem::new(HEAD_FOLDER_SIZES, "Folder sizes").checked(s.folder_sizes),
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
            HEAD_MODIFIED | HEAD_CREATED | HEAD_TYPE | HEAD_SIZE | HEAD_DIMENSIONS | HEAD_LENGTH => {
                let key = OPTIONAL[(item - HEAD_MODIFIED) as usize].key();
                let on = !app.settings.shows_column(key);
                app.set_column(key, on);
                // Turned on where it can't show: say where it does.
                let here = app.tab().media_here;
                match item {
                    HEAD_DIMENSIONS if on && !here.dims => app.say("Picture size shows in folders with pictures or videos"),
                    HEAD_LENGTH if on && !here.length => app.say("Length shows in folders with videos or songs"),
                    _ => {}
                }
            }
            HEAD_FOLDERS_FIRST => {
                let on = !app.settings.folders_first;
                app.set_folders_first(on);
            }
            HEAD_EXTENSIONS => {
                let hide = !app.settings.hide_known_extensions;
                app.settings.hide_known_extensions = hide;
            }
            HEAD_FOLDER_SIZES => {
                let on = !app.settings.folder_sizes;
                app.set_folder_sizes(on);
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

    // ------------------------------------------------------------ Picture size and Length (media.rs)

    /// What media.rs knows about the rows on screen for the Picture size and Length columns, by
    /// row; what it doesn't is asked for. The rows on screen first, then, while it has little of it
    /// left, more of the rest of the listing (on the PC's own drives, or anywhere while the tab is
    /// sorted by one of the columns). Nothing is asked while neither column shows.
    fn media_rows(&mut self, tab: &Tab, geo: &Geometry, scroll: f32) -> Vec<(usize, media::Facts)> {
        let cols = media::Columns { dims: self.cols.iter().any(|p| p.col == Col::Dimensions), length: self.cols.iter().any(|p| p.col == Col::Length) };
        if !cols.any() {
            return Vec::new();
        }
        self.media_ask(tab, cols, geo.visible(scroll))
    }

    /// A view without columns (List, the icons) of a tab sorted by Picture size or Length: the
    /// listing is still read (the rest, as no value shows), so the sort gets them all.
    fn media_for_sort(&mut self, tab: &Tab, app: &AppState) {
        let cols = media::Columns {
            dims: tab.sort == SortKey::Dimensions && app.settings.shows_column("dimensions"),
            length: tab.sort == SortKey::Length && app.settings.shows_column("length"),
        };
        if cols.any() {
            self.media_ask(tab, cols, std::iter::empty());
        }
    }

    /// Asks media.rs for what `cols` need of the tab's listing (`media_rows`): these rows on screen,
    /// and more of the rest. Returns what is known about those rows.
    fn media_ask(&mut self, tab: &Tab, cols: media::Columns, on_screen: impl Iterator<Item = usize>) -> Vec<(usize, media::Facts)> {
        let listing = &tab.folder.path;
        let m = &mut self.media;
        if m.tab != tab.id || m.listing != *listing || m.entries != tab.folder.entries.len() || m.cols != cols {
            // Another listing (or items came or went, or another column shows): the rest from the top.
            m.readable = !listing.is_empty() && crate::archive::split(listing).is_none();
            m.local = m.readable && fileops::has_recycle_bin(listing);
            (m.tab, m.listing, m.entries, m.cols) = (tab.id, listing.clone(), tab.folder.entries.len(), cols);
            (m.next, m.waiting, m.restart) = (0, 0, true);
        }
        if !m.readable {
            return Vec::new();
        }
        let rows: Vec<(usize, &Entry)> = on_screen.filter_map(|row| tab.entry_at(row).map(|e| (row, e))).filter(|(_, e)| media::worth_asking(e, cols)).collect();
        let mut found = Vec::with_capacity(rows.len());
        let mut screen = Vec::new();
        for ((row, e), facts) in rows.iter().zip(media::known(listing, rows.iter().map(|(_, e)| *e))) {
            match facts {
                Some(f) => found.push((*row, f)),
                None => screen.push(media::Ask::of(listing, e)),
            }
        }
        // The rest: the items shown, in the listing's own order (which sorting doesn't change).
        let sorted = matches!(tab.sort, SortKey::Dimensions | SortKey::Length);
        let mut rest = Vec::new();
        if (m.local || sorted) && m.waiting < media::REST_BATCH / 4 {
            let entries = &tab.folder.entries;
            while m.next < entries.len() && rest.len() < media::REST_BATCH {
                let end = (m.next + media::REST_BATCH).min(entries.len());
                let batch: Vec<&Entry> = (m.next..end).filter(|&i| tab.row_of(i).is_some()).map(|i| &entries[i]).filter(|e| media::worth_asking(e, cols)).collect();
                for (e, facts) in batch.iter().zip(media::known(listing, batch.iter().copied())) {
                    if facts.is_none() {
                        rest.push(media::Ask::of(listing, e));
                    }
                }
                m.next = end;
            }
        }
        m.waiting = media::want(self.media_id, listing, screen, rest, std::mem::take(&mut m.restart), self.hwnd);
        (m.asked, m.active) = (true, true);
        found
    }

    /// Every frame, before painting: media.rs is told what this list no longer needs and which
    /// folders' values to keep, and a tab sorted by Picture size or Length sorts again as values
    /// come in (at most every RESORT_EVERY; as folder sizes do in app.rs). True while a sort is due.
    fn tick_media(&mut self, now: f64, app: &mut AppState) -> bool {
        // Both columns off: nothing is read, and what was is forgotten.
        if !app.settings.shows_column("dimensions") && !app.settings.shows_column("length") {
            media::clear();
        }
        let m = &mut self.media;
        // Nothing asked during the last frame (another view, a place without these columns, split
        // view closed): what this list asked for goes.
        if !std::mem::take(&mut m.asked) && m.active {
            media::stop(self.media_id);
            (m.active, m.tab) = (false, 0);
        }
        // The values of folders no tab shows any more are forgotten.
        let mut open = std::collections::hash_map::DefaultHasher::new();
        for t in &app.tabs {
            t.folder.path.hash(&mut open);
        }
        let open = open.finish();
        if open != m.kept {
            m.kept = open;
            media::keep(app.tabs.iter().map(|t| t.folder.path.as_str()));
        }
        let t = app.tab();
        let arrived = media::arrived();
        if arrived == m.sorted || m.tab != t.id || !matches!(t.sort, SortKey::Dimensions | SortKey::Length) || t.custom_here().is_some() {
            return false;
        }
        if now - m.sorted_at < RESORT_EVERY {
            return true; // (a moment later)
        }
        (m.sorted, m.sorted_at) = (arrived, now);
        app.tab_mut().rebuild_view();
        false
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
        // Home and Duplicates keep an order of their own until a column is clicked: till then no
        // column is the sorted one.
        let own_order = !tab.column_now && (crate::recent::is_recent(&tab.folder.path) || tab.search.as_ref().is_some_and(|s| s.is_dupes()));
        let end = self.rect.right() - SCROLL_GUTTER;
        if theme.era.old() {
            // An older Windows' column titles (retrolist.rs).
            crate::retrolist::header_band(r, theme, header);
            let end = end.min(header.right());
            for (i, p) in self.cols.clone().iter().enumerate() {
                let x = p.x.max(header.x);
                let cell = Rect::new(x, header.y, (p.x + p.w).min(end) - x, header.h);
                if cell.w <= 0.0 {
                    continue;
                }
                let label = p.col.title(this_pc, bin, crate::recent::is_recent(&tab.folder.path));
                let right = p.col.right_aligned();
                let text_rect = if i == 0 { Rect::new(cell.x + ROW_PAD + 4.0, cell.y, cell.w - ROW_PAD - 16.0, cell.h) } else { cell.inset(10.0, 0.0) };
                let sorted = p.col.sort_key() == Some(tab.sort) && tab.custom_here().is_none() && !own_order && !(this_pc && p.col == Col::Free && tab.sort != SortKey::Date);
                let hot = hover_col == Some(p.col) && p.col.sort_key().is_some();
                crate::retrolist::header_item(r, theme, cell, text_rect, label, right, sorted.then_some(tab.descending), hot);
            }
            return;
        }
        r.fill_paint(header, 0.0, &theme.list_header);
        for (i, p) in self.cols.clone().iter().enumerate() {
            let cell = Rect::new(p.x, header.y, (p.x + p.w).min(end) - p.x, header.h);
            if cell.w <= 0.0 {
                continue;
            }
            if hover_col == Some(p.col) && p.col.sort_key().is_some() {
                r.fill_paint(cell, 0.0, &theme.hover_paint);
            }
            if theme.bevels {
                crate::pathbar::bevel(r, theme, cell, true, theme.list_header.base());
            }
            if i > 0 {
                // The line between two column titles (none in most skins).
                r.fill_rect(Rect::new(cell.x, cell.y, 1.0, cell.h), theme.column_line);
            }
            let label = p.col.title(this_pc, bin, crate::recent::is_recent(&tab.folder.path));
            let right = p.col.right_aligned();
            let text_rect = if i == 0 { Rect::new(cell.x + ROW_PAD + 4.0, cell.y, cell.w - ROW_PAD - 16.0, cell.h) } else { cell.inset(10.0, 0.0) };
            let style = TextStyle::new(theme.list_header_text).align(if right { Align::Right } else { Align::Left });
            // With liquid buttons the titles are bold, as the sidebar's panel headers are.
            let style = if theme.buttons == ButtonStyle::Liquid { style.bold() } else { style };
            r.text(label, text_rect, &style);
            // (None while the custom sort order decides, or Recent keeps Windows' newest-first order.)
            if p.col.sort_key() == Some(tab.sort) && tab.custom_here().is_none() && !own_order && !(this_pc && p.col == Col::Free && tab.sort != SortKey::Date) {
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

    /// The text of one cell (not Name's). `bin`: what the Recycle Bin says about the item, in the bin;
    /// `media`: its picture size and length, once media.rs has read them.
    fn cell_text(col: Col, e: &Entry, root: &str, app: &AppState, bin: Option<&BinItem>, media: Option<&media::Facts>) -> String {
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
            // A folder's size, once Glint has added it up (foldersize.rs; an empty one shows none).
            Col::Size if e.size > 0 && s.folder_sizes => folder::format_size_as(e.size, s.size_format),
            Col::Size => String::new(),
            Col::Dimensions => media::size_text(media),
            Col::Length => media::length_text(media),
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
            // (Rows ask media.rs for their Picture size and Length as they're drawn.)
            if self.flow != Flow::Rows {
                self.media_for_sort(tab, app);
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
            Some(if recycle::is_bin(&tab.path) {
                "The Recycle Bin is empty."
            } else if crate::recent::is_recent(&tab.path) {
                "Files you open show up here."
            } else {
                "This folder is empty."
            }.to_string())
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
                Drag::Bar => ThumbState::Held(self.repeat.hold),
                _ if self.mouse.is_some_and(|p| self.near_track(p)) => ThumbState::Hover,
                _ => ThumbState::Rest,
            };
            let room = crate::retroctl::Room::of(tab.scroll, self.max_scroll(tab));
            paint_scrollbar(r, theme, self.lane(), thumb, state, room);
        }
    }

    /// Everything the paint lights up because of where the mouse is (see `painted_hover`).
    fn hover_key(&self, tab: &Tab) -> HoverKey {
        let track = self.thumb(tab).is_some() && self.mouse.is_some_and(|p| self.near_track(p));
        if self.home_on {
            // (As home::paint finds it, in the area `paint` gives it.)
            let area = Rect::new(self.rect.x, self.rect.y, self.rect.w - SCROLL_GUTTER.min(12.0), self.rect.h);
            let scroll = tab.scroll.clamp(0.0, (self.home.layout.height - self.rect.h).max(0.0));
            let spot = self
                .mouse
                .filter(|p| self.drag == Drag::None && area.contains(*p))
                .and_then(|p| self.home.layout.spot_at(Point::new(p.x - area.x, p.y - area.y + scroll)));
            return HoverKey { spot, track, ..Default::default() };
        }
        let column = match self.drag {
            Drag::None if self.flow == Flow::Rows => self.mouse.and_then(|p| if self.edge_at(p).is_some() { None } else { self.column_at(p) }),
            _ => None,
        };
        HoverKey { item: self.hovered(tab), column, track, spot: None }
    }

    /// The item under the mouse, to draw highlighted (not while dragging something in the list).
    fn hovered(&self, tab: &Tab) -> Option<usize> {
        if self.drag != Drag::None || self.band.as_ref().is_some_and(|b| b.active) {
            return None;
        }
        self.mouse.and_then(|p| self.row_at(tab, p))
    }

    /// Whether an item is cut to the clipboard (drawn faded until it's pasted, as in Explorer).
    /// Items of folders open in the tree view aren't marked (names repeat across folders), nor
    /// search results or duplicates from other folders that share a cut item's name.
    fn is_cut(tab: &Tab, cut: Option<&std::collections::HashSet<String>>, e: &Entry) -> bool {
        cut.is_some_and(|c| c.contains(&e.lower))
            && (e.target.is_none() || (!tab.tree.is_open() && crate::fileops::same_path(folder::result_folder(e), &tab.folder.path)))
    }

    /// 98 / 2000 mark a selected item by its name only, its icon blended half way into the
    /// selection's colour, and in grey (the button face, black text) while the keyboard is
    /// elsewhere. Whether a selected item shows in the selection's colours (`lit`), and the
    /// paint behind its name.
    fn classic_label(&self, theme: &Theme) -> (bool, Paint) {
        if self.has_focus {
            (true, theme.accent_tint_paint)
        } else {
            (false, Paint::Flat(theme.toolbar))
        }
    }

    /// Draws an item's icon blended half way into the selection's colour (98 / 2000's selected
    /// icon) when `tint`.
    fn tint_icon(r: &mut Renderer, theme: &Theme, tint: bool) {
        r.set_tint(tint.then(|| (Color { a: 1.0, ..theme.accent_tint_paint.base() }, 0.5)));
    }

    /// The colour of an item's name.
    fn name_color(theme: &Theme, e: &Entry, selected: bool, is_cut: bool) -> crate::theme::Color {
        // (An older Windows' selection is dark: a cut item on it keeps the selection's colour.)
        if selected && (!is_cut || theme.era.old()) {
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
        // Picture size and Length: what media.rs knows about the rows on screen (the rest is asked for).
        let media = self.media_rows(tab, geo, scroll);
        // Duplicates: a line above each new set, so the sets stand apart (not once a column sorts
        // them: a set's files are then apart anyway).
        let dupes = tab.search.as_ref().is_some_and(|s| s.is_dupes()) && !tab.column_now;
        // Colour tags: a dot after the name (tags.rs).
        let mut tags = app.tags.lookup(&tab.folder.path);
        for row in geo.visible(scroll) {
            let Some(index) = tab.index_at(row) else { continue };
            let e = &tab.folder.entries[index];
            let line = geo.cell(row, scroll);
            let y = line.y;
            if dupes && row > 0 && tab.index_at(row - 1).is_some_and(|p| tab.folder.entries[p].free != e.free) {
                r.fill_rect(Rect::new(line.x, y, line.w, 1.0), theme.line);
            }
            let selected = tab.is_selected(index);
            // The folder something dragged over the list would go into: outlined in red.
            let dropping = self.drop_row.as_deref().is_some_and(|d| e.is_container() && e.path_in(&tab.folder.path).to_lowercase() == d);
            // 98 / 2000 mark only the name (below), as their Explorer did.
            let label_only = theme.era.classic();
            let (lit, label_paint) = self.classic_label(theme);
            if selected {
                if !label_only {
                    crate::pathbar::fill_row(r, theme, line, &theme.accent_tint_paint, false);
                    r.fill_rect(Rect::new(line.x, line.y, size::SELECT_EDGE, line.h), theme.select_edge);
                }
            } else if hover == Some(row) || dropping {
                crate::pathbar::fill_row(r, theme, line, &theme.hover_paint, true);
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
                } else if selected && theme.era.old() {
                    // On an older Windows' selection (navy, blue): its own text colour.
                    theme.selection_text
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
            let icon_rect = Rect::new(line.x + ROW_PAD + offset, y + (row_h - self.icon) / 2.0, self.icon, self.icon);
            Self::tint_icon(r, theme, label_only && selected && lit);
            if !crate::eraicons::draw_entry(r, theme.icons, &tab.folder.path, e, icon_rect) {
                let icon = if large { app.icons.for_entry_sized(&tab.folder.path, e, true) } else { app.icons.for_entry(&tab.folder.path, e) };
                r.icon(&app.icons, icon, icon_rect);
            }
            Self::tint_icon(r, theme, false);
            let is_cut = Self::is_cut(tab, cut, e);
            if is_cut {
                // The row's own background over the icon, half see-through, so it fades into its row.
                let under = if hover == Some(row) && !selected { theme.hover } else { theme.panel };
                r.fill_rect(icon_rect, under.with_alpha(0.55));
                if selected {
                    r.fill_rect(icon_rect, theme.accent_tint.with_alpha(theme.accent_tint.a * 0.55));
                }
            }
            let name_color = Self::name_color(theme, e, selected && (lit || !label_only), is_cut);
            let name_x = line.x + ROW_PAD + offset + self.icon + 8.0;
            let name_right = self.name_right();
            let name_style = TextStyle::new(name_color).size(self.font);
            let tag = tags.of(e);
            if label_only && selected {
                // The selection behind the name only, and the keyboard's dotted box round it.
                let w = r.measure(shown_name(e, hide_ext), &name_style).min((name_right - name_x - 8.0 - crate::tags::room(tag)).max(0.0)).ceil();
                let label = Rect::new(r.snap(name_x - 2.0), r.snap(y + 1.0), w + 4.0, (row_h - 2.0).round());
                r.fill_paint(label, 0.0, &label_paint);
                if tab.focus == Some(index) && lit {
                    crate::retroctl::dotted(r, label, theme.selection_text);
                }
            }
            let name_box = Rect::new(name_x, y, name_right - name_x - 8.0 - crate::tags::room(tag), row_h);
            r.text(shown_name(e, hide_ext), name_box, &name_style);
            crate::tags::after_name(r, theme, tag, shown_name(e, hide_ext), &name_style, name_box, selected);
            let detail = TextStyle::new(if selected && !label_only { theme.selection_details } else { theme.muted }).size(self.font);
            let facts = media.iter().find(|(at, _)| *at == row).map(|(_, f)| f);
            for p in cols.iter().skip(1) {
                let w = (p.x + p.w).min(end) - p.x - 20.0;
                if w <= 0.0 {
                    continue;
                }
                let text = Self::cell_text(p.col, e, &tab.folder.path, app, bin.and_then(|b| b.items.get(index)), facts);
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
        let mut tags = app.tags.lookup(&tab.folder.path);
        for row in geo.visible(scroll) {
            let Some(index) = tab.index_at(row) else { continue };
            let e = &tab.folder.entries[index];
            let cell = geo.cell(row, scroll);
            if cell.right() < area.x || cell.x > area.right() {
                continue;
            }
            let selected = tab.is_selected(index);
            let is_cut = Self::is_cut(tab, cut, e);
            // 98 / 2000: the name only (see `classic_label`).
            let label_only = theme.era.classic();
            let (lit, label_paint) = self.classic_label(theme);
            let style = TextStyle::new(Self::name_color(theme, e, selected && (lit || !label_only), is_cut)).size(self.font);
            let name = shown_name(e, hide_ext);
            let name_x = cell.x + ROW_PAD + self.icon + 8.0;
            // (A colour tag's dot goes after the name, in the item: tags.rs.)
            let tag = tags.of(e);
            let room = (cell.right() - name_x - 10.0 - crate::tags::room(tag)).max(0.0);
            let w = r.measure(name, &style).min(room);
            let item = Rect::new(cell.x + 4.0, cell.y + 1.0, name_x + w + 6.0 + crate::tags::room(tag) - (cell.x + 4.0), cell.h - 2.0);
            self.list_hit.push((row, item.right() - cell.x));
            let dropping = self.drop_row.as_deref().is_some_and(|d| e.is_container() && e.path_in(&tab.folder.path).to_lowercase() == d);
            if selected && label_only {
                let label = Rect::new(r.snap(name_x - 2.0), r.snap(cell.y + 1.0), w.ceil() + 4.0, (cell.h - 2.0).round());
                r.fill_paint(label, 0.0, &label_paint);
                if tab.focus == Some(index) && lit {
                    crate::retroctl::dotted(r, label, theme.selection_text);
                }
            } else if selected {
                r.fill_paint(item, 0.0, &theme.accent_tint_paint);
                r.fill_rect(Rect::new(item.x, item.y, size::SELECT_EDGE, item.h), theme.select_edge);
            } else if hover == Some(row) || dropping {
                r.fill_paint(item, 0.0, &theme.hover_paint);
            }
            if dropping {
                r.stroke_rect(item, 1.0, theme.accent);
            }
            let icon_rect = Rect::new(cell.x + ROW_PAD, cell.y + (cell.h - self.icon) / 2.0, self.icon, self.icon);
            Self::tint_icon(r, theme, label_only && selected && lit);
            if !crate::eraicons::draw_entry(r, theme.icons, &tab.folder.path, e, icon_rect) {
                let icon = if large { app.icons.for_entry_sized(&tab.folder.path, e, true) } else { app.icons.for_entry(&tab.folder.path, e) };
                r.icon(&app.icons, icon, icon_rect);
            }
            Self::tint_icon(r, theme, false);
            if is_cut {
                r.fill_rect(icon_rect, theme.panel.with_alpha(0.55));
            }
            r.text(name, Rect::new(name_x, cell.y, room, cell.h), &style);
            crate::tags::after_name(r, theme, tag, name, &style, Rect::new(name_x, cell.y, room, cell.h), selected);
        }
    }

    /// Medium, Large and Extra large icons: a tile per item, its picture above its name.
    fn paint_tiles(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, tab: &Tab, geo: &Geometry, scroll: f32) {
        let hover = self.hovered(tab);
        let cut = app.cut_names(&tab.folder.path);
        let hide_ext = app.settings.hide_known_extensions;
        let side = self.tile.picture;
        let mut tags = app.tags.lookup(&tab.folder.path);
        for row in geo.visible(scroll) {
            let Some(index) = tab.index_at(row) else { continue };
            let e = &tab.folder.entries[index];
            let cell = geo.cell(row, scroll);
            let tile = Geometry::tile(cell);
            let selected = tab.is_selected(index);
            let dropping = self.drop_row.as_deref().is_some_and(|d| e.is_container() && e.path_in(&tab.folder.path).to_lowercase() == d);
            // 98 / 2000: the name only (below, see `classic_label`).
            let label_only = theme.era.classic();
            let (lit, label_paint) = self.classic_label(theme);
            // In the skin's shape (round corners only in a rounded skin), with the skin's selection
            // edge along the bottom.
            if selected {
                if !label_only {
                    fill_box_paint(r, theme, tile, 0.0, &theme.accent_tint_paint);
                    r.fill_rect(Rect::new(tile.x + 8.0, tile.bottom() - 2.0, tile.w - 16.0, 2.0), theme.select_edge);
                }
            } else if hover == Some(row) || dropping {
                fill_box_paint(r, theme, tile, 0.0, &theme.hover_paint);
            }
            if dropping {
                outline_box(r, theme, tile.inset(0.5, 0.5), 0.0, 1.0, theme.accent);
            }
            let pic = Rect::new(cell.x + (cell.w - side) / 2.0, cell.y + TILE_PAD, side, side);
            Self::tint_icon(r, theme, label_only && selected && lit);
            let drawn = self.paint_picture(r, theme, app, &tab.folder.path, e, pic);
            Self::tint_icon(r, theme, false);
            // A colour tag's dot, by the picture (tags.rs).
            crate::tags::on_tile(r, theme, tags.of(e), drawn, tile, selected);
            let is_cut = Self::is_cut(tab, cut, e);
            if is_cut {
                let under = if selected { theme.panel } else if hover == Some(row) { theme.hover } else { theme.panel };
                r.fill_rect(drawn, under.with_alpha(0.55));
            }
            let name = Rect::new(tile.x + 4.0, pic.bottom() + TILE_TEXT_GAP, tile.w - 8.0, self.tile.lines as f32 * NAME_LINE_H + 1.0);
            let style = TextStyle::new(Self::name_color(theme, e, selected && (lit || !label_only), is_cut)).align(Align::Center);
            if selected && label_only {
                // Behind the name's lines: as wide as the name (or the box, when it wraps).
                let full = r.measure(shown_name(e, hide_ext), &style).ceil();
                let (w, lines) = if full <= name.w { (full, 1.0) } else { (name.w, self.tile.lines as f32) };
                let label = Rect::new(r.snap(name.x + (name.w - w) / 2.0 - 2.0), r.snap(name.y), w + 4.0, (lines * NAME_LINE_H + 1.0).round());
                r.fill_paint(label, 0.0, &label_paint);
                if tab.focus == Some(index) && lit {
                    crate::retroctl::dotted(r, label, theme.selection_text);
                }
            }
            r.text_wrapped(shown_name(e, hide_ext), name, &style);
        }
    }

    /// An item's picture, in the box `b`: its thumbnail (pictures, videos, PDFs: filling the box,
    /// with a thin frame) or Windows' big icon (at its own size at most) - made in the background
    /// (thumbs.rs), with the list's own icon meanwhile. Returns where it was drawn.
    /// The skin's wallpaper (filling the list unless the skin says otherwise, see
    /// [`wallpaper_layout`]), faint enough for the names to read over it. It's read in the
    /// background (thumbs.rs): none until it's there.
    fn paint_wallpaper(&self, r: &mut Renderer, theme: &Theme) {
        if theme.wallpaper.is_empty() || theme.wallpaper_opacity <= 0.0 {
            // (The last skin's picture, if there was one, needn't stay in memory.)
            thumbs::forget_wallpaper();
            return;
        }
        // In split view the picture lies across both halves, as one (each shows its part), and
        // so does its fade.
        let b = self.wall_area.unwrap_or(self.rect);
        // It's read no bigger than it's drawn.
        let fit = wallpaper_fit(theme, (b.w * self.scale, b.h * self.scale));
        let Some(pic) = thumbs::wallpaper(&theme.wallpaper, theme.wallpaper_blur, theme.wallpaper_fit == WallpaperFit::Tile, fit) else { return };
        let Some(how) = wallpaper_layout(b, (pic.width() as f32, pic.height() as f32), theme, 1.0 / self.scale.max(0.1)) else { return };
        r.wallpaper(&pic, how, b, theme.wallpaper_opacity, theme.wallpaper_fade);
    }

    /// Split view: the wallpaper is laid out over `area` (both halves) instead of this list alone.
    pub fn set_wall_area(&mut self, area: Option<Rect>) {
        self.wall_area = area;
    }

    fn paint_picture(&self, r: &mut Renderer, theme: &Theme, app: &AppState, dir: &str, e: &Entry, b: Rect) -> Rect {
        picture(r, theme, app, dir, e, b, self.scale)
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
                self.item_menu_at(tab, r)
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

    /// Where the keyboard's menu for the item in row `r` opens: below it (Home: below the middle
    /// of its tile or line on the page).
    fn item_menu_at(&self, tab: &Tab, r: usize) -> Point {
        let area = self.rows_area();
        // (max: a clamp whose ends are the wrong way round panics.)
        let inside = |x: f32, y: f32| Point::new(x.clamp(area.x, area.right().max(area.x)), y.clamp(area.y, area.bottom().max(area.y)));
        if self.home_on {
            if let Some(rc) = tab.index_at(r).and_then(|i| self.home.layout.rect_of(i)) {
                let scroll = tab.scroll.clamp(0.0, self.max_scroll(tab));
                return inside(self.rect.x + rc.x + rc.w / 2.0, self.rect.y + rc.y - scroll + rc.h);
            }
        }
        let left = area.x + ROW_PAD + self.icon + 8.0;
        let cell = self.geo(tab).cell(r, tab.scroll);
        let x = if self.flow == Flow::Rows { left + 16.0 } else { cell.x + cell.w / 2.0 };
        inside(x, cell.bottom())
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
    /// An older Windows' arrow button or track held down.
    Held(crate::retroctl::Hold),
}

/// An item's picture in the box `b` (see `ListView::paint_picture`); `scale` is the screen's.
pub(crate) fn picture(r: &mut Renderer, theme: &Theme, app: &AppState, dir: &str, e: &Entry, b: Rect, scale: f32) -> Rect {
    let scale = scale.max(0.5);
    // Pictures are asked for in screen pixels, so they're sharp at any scaling.
    let px = (b.w * scale).round().max(16.0) as u32;
    let path = e.path_in(dir);
    // The same version the details panel and the Space preview ask with (details.rs): pictures are
    // kept by path, so two versions of one would be made again and again.
    let mut version = thumbs::version_of(e.modified, e.size);
    if e.kind == EntryKind::Drive {
        version ^= e.free; // a drive's free space read again
    }
    // With older Windows' icons, folders and drives show theirs, and so does any file without a
    // real picture of its own (Windows' icon for it would come back as its picture).
    let era = theme.icons != crate::theme::IconSet::Windows;
    let wanted = !(era && e.is_container());
    if let Some(pic) = wanted.then(|| thumbs::request_versioned(&path, version, px)).flatten().filter(|pic| !(era && pic.is_icon())) {
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
    let s = size::ICON_LARGE.min(b.w);
    let at = Rect::new(b.x + (b.w - s) / 2.0, b.y + (b.h - s) / 2.0, s, s);
    if !crate::eraicons::draw_entry(r, theme.icons, dir, e, at) {
        let icon = app.icons.for_entry_sized(dir, e, true);
        r.icon(&app.icons, icon, at);
    }
    at
}

/// Draws a scrollbar (the file list's, the sidebar's): the skin's track along `lane` if it paints
/// one (with a line on the side facing the items), and the thumb in `thumb`: a bar with round ends
/// in the skin's `scrollbar` paint (a gradient shades it across, like a rod), its edge and three
/// grip lines across its middle if the skin has them. Being dragged, a flat thumb takes the accent
/// colour and a shaded one gets a little darker.
pub fn paint_scrollbar(r: &mut Renderer, theme: &Theme, lane: Rect, thumb: Rect, state: ThumbState, room: crate::retroctl::Room) {
    if theme.era.old() {
        let hold = if let ThumbState::Held(h) = state { h } else { crate::retroctl::Hold::None };
        let hot = matches!(state, ThumbState::Hover | ThumbState::Held(_));
        let look = crate::retroctl::BarLook { hot, dragged: state == ThumbState::Dragged, hold, room };
        return crate::retroctl::scrollbar(r, theme, lane, thumb, look);
    }
    let upright = thumb.h >= thumb.w;
    if !theme.scrollbar_track.is_clear() {
        r.fill_paint(lane, 0.0, &theme.scrollbar_track);
        let edge = if upright { Rect::new(lane.x, lane.y, 1.0, lane.h) } else { Rect::new(lane.x, lane.y, lane.w, 1.0) };
        r.fill_rect(edge, theme.line);
    }
    let radius = if theme.bevels { 0.0 } else { thumb.w.min(thumb.h) / 2.0 };
    let shaded = theme.scroll_thumb_paint.gradient().is_some();
    let paint = match state {
        ThumbState::Dragged if shaded => theme.scroll_thumb_hover_paint.darker(0.1),
        ThumbState::Dragged => Paint::Flat(theme.accent),
        ThumbState::Hover => theme.scroll_thumb_hover_paint,
        ThumbState::Rest | ThumbState::Held(_) => theme.scroll_thumb_paint,
    };
    r.fill_paint(thumb, radius, &paint);
    if theme.bevels {
        crate::pathbar::bevel(r, theme, thumb, true, paint.base());
    } else if theme.scrollbar_border.a > 0.0 {
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
pub(crate) fn shown_name(e: &Entry, hide_ext: bool) -> &str {
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
    /// screen (not This PC's drives, not the Recycle Bin, not while another folder is on its way). Search
    /// results are found by their full path; they must all be in one folder (Windows' data object
    /// is made for items of one folder), otherwise the status bar says so and nothing is dragged.
    fn drag_out(&self, p: &Press, app: &mut AppState) -> Option<DragOut> {
        let tab = app.tab();
        let real = tab.id == p.tab_id && !tab.waiting_long() && (!tab.folder.path.is_empty() || tab.is_searching()) && tab.folder.path == tab.path && !recycle::is_bin(&tab.path);
        if !real {
            return None;
        }
        let entries: Vec<&Entry> = tab.selected_entries().into_iter().map(|i| &tab.folder.entries[i]).filter(|e| e.kind != EntryKind::Drive).collect();
        let paths: Vec<String> = entries.iter().map(|e| e.path_in(&tab.folder.path)).collect();
        let folder = paths.first().and_then(|p| folder::parent(p))?;
        if !paths.iter().all(|p| folder::parent(p).is_some_and(|f| fileops::same_path(&f, &folder))) {
            app.say("Drag items from one folder at a time.");
            return None;
        }
        let names: Vec<String> = paths.iter().map(|p| folder::file_name(p).to_string()).collect();
        // What the listing says about them goes along, so Windows needn't look each one up on disk
        // again (not Home's: its dates are when files were opened).
        if !crate::recent::is_recent(&tab.folder.path) {
            let listed = entries
                .iter()
                .zip(&names)
                .map(|(e, name)| crate::dragdrop::Listed { name: name.clone(), attributes: e.attributes, size: e.size, modified: e.modified, created: e.created })
                .collect();
            crate::dragdrop::about_to_drag(&folder, listed);
        }
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
        if self.home_on {
            // Home: among the tiles and rows as they're laid out.
            let l = &self.home.layout;
            let to = match key {
                VK_UP => home::step(l, tab.focus, 0, -1),
                VK_DOWN => home::step(l, tab.focus, 0, 1),
                VK_LEFT => home::step(l, tab.focus, -1, 0),
                VK_RIGHT => home::step(l, tab.focus, 1, 0),
                VK_HOME | VK_PRIOR => l.items().next().map(|(i, _)| i),
                VK_END | VK_NEXT => l.items().last().map(|(i, _)| i),
                _ => return None,
            };
            return to.and_then(|i| tab.row_of(i)).or(tab.focus_row());
        }
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

impl ListView {
    /// Ctrl + wheel ("Ctrl + scroll wheel changes view"), as in File Explorer: up for bigger,
    /// down for smaller, through Details, List, Medium, Large and Extra large icons. The folder
    /// keeps the view, as a VIEW pick does.
    fn zoom(&mut self, notches: f32, app: &mut AppState) -> Effect {
        const STEPS: [ViewMode; 5] = [ViewMode::Details, ViewMode::List, ViewMode::Medium, ViewMode::Large, ViewMode::ExtraLarge];
        self.zoom_wheel += notches;
        if self.zoom_wheel.abs() < 1.0 {
            return Effect::None;
        }
        let up = self.zoom_wheel > 0.0;
        self.zoom_wheel = 0.0;
        let now = STEPS.iter().position(|v| *v == app.tab().view_mode).unwrap_or(0);
        let next = if up { (now + 1).min(STEPS.len() - 1) } else { now.saturating_sub(1) };
        if next == now {
            return Effect::None;
        }
        Effect::Command(Command::SetView(STEPS[next]))
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

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        // SAFETY: a plain focus query.
        self.has_focus = ctx.active && unsafe { GetFocus() } == self.hwnd;
        let tab = app.tab();
        self.take_size(app);
        self.place_columns(app);
        // (In split view a shaded background also lies across both halves, as one.)
        r.push_clip(self.rect);
        r.fill_paint(self.wall_area.unwrap_or(self.rect), 0.0, &theme.panel_paint);
        self.paint_wallpaper(r, theme);
        if self.home_on {
            let max = (self.home.layout.height - self.rect.h).max(0.0);
            let area = Rect::new(self.rect.x, self.rect.y, self.rect.w - SCROLL_GUTTER.min(12.0), self.rect.h);
            let mouse = if self.drag == Drag::None { self.mouse } else { None };
            home::paint(r, theme, app, tab, &mut self.home, area, tab.scroll.clamp(0.0, max), mouse, self.scale);
            if let Some(thumb) = self.thumb(tab) {
                let state = match self.drag {
                    Drag::Thumb { .. } => ThumbState::Dragged,
                    Drag::Bar => ThumbState::Held(self.repeat.hold),
                    _ if self.mouse.is_some_and(|p| self.near_track(p)) => ThumbState::Hover,
                    _ => ThumbState::Rest,
                };
                let room = crate::retroctl::Room::of(tab.scroll, self.max_scroll(tab));
                paint_scrollbar(r, theme, self.lane(), thumb, state, room);
            }
            r.pop_clip();
            self.painted_hover = self.hover_key(tab);
            return;
        }
        if self.flow == Flow::Rows {
            self.paint_header(r, theme, tab);
        }
        self.paint_rows(r, theme, app, tab);
        r.pop_clip();
        self.painted_hover = self.hover_key(tab);
        if theme.bevels {
            crate::pathbar::bevel(r, theme, self.rect, false, theme.window);
        }
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
        if let Some(p) = self.press.as_ref().filter(|p| p.on_item && !p.refused && self.drag == Drag::None) {
            let (dx, dy) = drag_distance(self.scale);
            if (e.pos.x - p.at.x).abs() > dx || (e.pos.y - p.at.y).abs() > dy {
                let p = self.press.take();
                self.cancel_slow_click();
                self.click_open = None; // a drag, not a click: nothing opens
                if let Some(d) = p.as_ref().and_then(|p| self.drag_out(p, app)) {
                    return Effect::DragOut(d);
                }
                // No drag after all: the button coming up still finishes the click (or opens the
                // right-click menu).
                self.press = p.map(|p| Press { refused: true, ..p });
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
                // Home has its own height (`max_scroll`) and always scrolls downwards.
                let (track, max) = (self.track(), self.max_scroll(tab));
                if let Some(thumb) = self.thumb(tab) {
                    let (pos, start, span) = if geo.sideways() && !self.home_on { (e.pos.x, track.x, track.w - thumb.w) } else { (e.pos.y, track.y, track.h - thumb.h) };
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
            // Only hovering: a new frame only when what is lit up changes (a frame for every
            // pixel moved would redraw the whole window at the screen's rate).
            Drag::None if self.press.is_none() && self.band.is_none() => {
                return if self.hover_key(tab) != self.painted_hover { Effect::Redraw } else { Effect::None };
            }
            Drag::Bar | Drag::None => {}
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
        // Home: its chevrons fold a section; its pills pick Recent or Favorites.
        if self.home_on && e.button == MouseButton::Left && !self.near_track(e.pos) {
            let at = self.home_point(app.tab(), e.pos);
            let h = &mut self.home;
            let changed = match h.layout.spot_at(at) {
                Some(home::Spot::Quick) => {
                    h.quick_open = !h.quick_open;
                    true
                }
                Some(home::Spot::Lists) => {
                    h.lists_open = !h.lists_open;
                    true
                }
                Some(home::Spot::Recent) if h.favorites => {
                    h.favorites = false;
                    true
                }
                Some(home::Spot::Favorites) if !h.favorites => {
                    h.favorites = true;
                    true
                }
                Some(home::Spot::Recent | home::Spot::Favorites) => return Effect::None,
                _ => false,
            };
            if changed {
                // What was selected may be out of sight now.
                app.tab_mut().clear_selection();
                return Effect::Redraw;
            }
        }
        let geo = self.geo(app.tab());
        let tab = app.tab_mut();
        if e.button == MouseButton::Left {
            // Scrollbar: grab the thumb, or page towards the click.
            if self.near_track(e.pos) {
                if let Some(thumb) = self.thumb(tab) {
                    // An older Windows' arrow buttons: a line up or down, again and again while held.
                    if let Some(held) = crate::retroctl::arrow_at(self.lane(), self.arrows, e.pos) {
                        return self.press_bar(tab, crate::retroctl::Hold::Arrow(held), ctx.now);
                    }
                    let (pos, start, end) = if geo.sideways() && !self.home_on { (e.pos.x, thumb.x, thumb.right()) } else { (e.pos.y, thumb.y, thumb.bottom()) };
                    if pos >= start && pos < end {
                        self.drag = Drag::Thumb { grab: pos - start };
                        return Effect::Redraw;
                    }
                    // Its track pages on while held, until the thumb reaches the mouse.
                    if self.arrows > 0.0 {
                        let way = if pos < start { crate::retroctl::Held::Start } else { crate::retroctl::Held::End };
                        return self.press_bar(tab, crate::retroctl::Hold::Page(way), ctx.now);
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
        let press = |on_item, then| Press { at: e.pos, button: e.button, tab_id, on_item, then, refused: false };
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
                } else if let Some(index) = index.filter(|_| again && self.was_active && self.on_name(tab, row, e.pos) && (!tab.folder.path.is_empty() || tab.is_searching())) {
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
                // Dragging from here draws a rectangle that selects what it touches (not on Home).
                if !tab.waiting_long() && !self.home_on {
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
            if self.repeat.release() {
                crate::retroctl::repeat_timer(self.hwnd, BAR_REPEAT_TIMER, None);
            }
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
            None if app.settings.double_click_up && !tab.waiting_long() && !self.home_on && !e.mods.ctrl && !e.mods.shift => Effect::Command(Command::Up),
            None => Effect::None,
        }
    }

    fn wheel(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.cancel_slow_click();
        // Scrolling while renaming keeps what was typed, like clicking elsewhere.
        if self.rename.is_some() {
            return self.end_rename(ctx.now, true, true).map_or(Effect::Redraw, Effect::Command);
        }
        if e.mods.ctrl && app.settings.ctrl_wheel_view {
            return self.zoom(e.wheel, app);
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
            // Duplicates: Esc goes back to the folder (after the selection is cleared).
            VK_ESCAPE if !tab.selected.iter().any(|&s| s) && tab.search.as_ref().is_some_and(|s| s.is_dupes()) => Effect::Command(Command::EndDuplicates),
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
            // Undo and redo the last file actions (undo.rs).
            VK_Z if mods.ctrl && !mods.shift => Effect::Command(Command::Undo),
            VK_Y if mods.ctrl && !mods.shift => Effect::Command(Command::Redo),
            VK_Z if mods.ctrl && mods.shift => Effect::Command(Command::Redo),
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
        if crate::recent::is_recent(&tab.path) {
            return None;
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
        // A list with no room (split view's second half while it's closed) leaves the tab alone:
        // its zero height would scroll it back to the top.
        if self.rect.w <= 0.0 || self.rect.h <= 0.0 {
            return false;
        }
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
        let max = if self.home_on { (self.home.layout.height - self.rect.h).max(0.0) } else { geo.max_scroll() };
        tab.scroll = tab.scroll.clamp(0.0, max);
        // How a step of scrolling goes in this view, for a re-read that keeps the top item in place.
        tab.lines = Some((geo.line(), geo.per_line));
        if tab.reveal_focus {
            tab.reveal_focus = false;
            if let Some(row) = tab.focus_row().filter(|_| !self.home_on) {
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
        // Picture size and Length (media.rs): sorted again as values come in.
        let resort = self.tick_media(now, app);
        // The name box follows its item; a new folder's box opens once the folder is read again.
        self.keep_name_box(now, app);
        self.take_rename_request(now, app);
        self.anim.running() || now < self.repaint_until || banding || resort
    }
}

/// How big the skin's wallpaper is drawn at most, for reading it no bigger (thumbs.rs
/// `WallFit`; `area`: the list's, in screen pixels). None when it's drawn at its own size (tiled,
/// or `actual`, with no `wallpaper-size`): then how big it is decides how big it shows.
pub(crate) fn wallpaper_fit(theme: &Theme, area: (f32, f32)) -> Option<thumbs::WallFit> {
    if theme.wallpaper_size.is_none() && matches!(theme.wallpaper_fit, WallpaperFit::Tile | WallpaperFit::Actual) {
        return None;
    }
    // Filling, whole or stretched, it covers at most the area (in its longer direction, whatever
    // the area's shape) times the zoom; a size is a part of the area's width.
    let part = theme.wallpaper_size.unwrap_or(1.0).max(1.0);
    Some(thumbs::WallFit { area, zoom: theme.wallpaper_zoom * part })
}

/// Where a skin's wallpaper `pic` (its size in pixels) goes in `area` (the list, or both halves in
/// split view), as the skin says (theme.rs `wallpaper_*`): None when none of it would show.
/// `dip_per_px` is how big one of the screen's pixels is (`tile` and `actual` show the picture at
/// its own size in those).
///
/// - Its size: `fill` covers the area (cut to fit), `whole` fits all of it in, `tile` and `actual`
///   are its own size; `wallpaper-size` (a part of the area's width) decides instead when set, the
///   shape kept. Then the zoom. `stretch` is the area's size times the zoom, whatever its shape.
/// - Its place: `wallpaper_at` across and `wallpaper_at_y` down (0 the left or top, 1 the right
///   or bottom: that side of the picture against that side of the area), then the offset. Up and
///   down is the top unless said, for `whole` and `actual` the bottom.
/// - `tile` repeats it all over the area from there; the others show it once, cut to the area.
pub(crate) fn wallpaper_layout(area: Rect, pic: (f32, f32), theme: &Theme, dip_per_px: f32) -> Option<WallDraw> {
    let (w, h) = pic;
    if !(w > 0.0 && h > 0.0 && area.w > 0.0 && area.h > 0.0) {
        return None;
    }
    let fit = theme.wallpaper_fit;
    let zoom = theme.wallpaper_zoom;
    let (dw, dh) = if fit == WallpaperFit::Stretch {
        (area.w * zoom, area.h * zoom)
    } else {
        let k = match (theme.wallpaper_size, fit) {
            (Some(part), _) => area.w * part / w,
            (None, WallpaperFit::Whole) => (area.w / w).min(area.h / h),
            (None, WallpaperFit::Tile | WallpaperFit::Actual) => dip_per_px,
            (None, _) => (area.w / w).max(area.h / h),
        };
        (w * k * zoom, h * k * zoom)
    };
    if !(dw.is_finite() && dh.is_finite() && dw > 0.0 && dh > 0.0) {
        return None;
    }
    let at_y = theme.wallpaper_at_y.unwrap_or(if matches!(fit, WallpaperFit::Whole | WallpaperFit::Actual) { 1.0 } else { 0.0 });
    let (dx, dy) = theme.wallpaper_offset;
    let x = area.x + (area.w - dw) * theme.wallpaper_at + dx;
    let y = area.y + (area.h - dh) * at_y + dy;
    if fit == WallpaperFit::Tile {
        return Some(WallDraw::Tiled { fill: area, origin: Point::new(x, y), tile: (dw, dh) });
    }
    let (x0, y0, x1, y1) = (x.max(area.x), y.max(area.y), (x + dw).min(area.right()), (y + dh).min(area.bottom()));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    // The part of the picture that shows, in its pixels.
    let (kx, ky) = (dw / w, dh / h);
    let src = Rect::new((x0 - x) / kx, (y0 - y) / ky, (x1 - x0) / kx, (y1 - y0) / ky);
    Some(WallDraw::Once { dest: Rect::new(x0, y0, x1 - x0, y1 - y0), src })
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

    /// Letters typed together jump to a name starting with them; when none does, the last letter
    /// starts again on its own instead of doing nothing (File Explorer stops there).
    #[test]
    fn a_letter_that_matches_nothing_together_starts_again() {
        let entry = |name: &str| Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: EntryKind::File,
            size: 0,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: String::new(),
            type_name: Arc::from("File"),
            target: None,
            free: 0,
        };
        let mut tab = Tab::new(1, "C:\\x".into());
        let folder = crate::folder::Folder { path: "C:\\x".into(), entries: ["apple.txt", "banana.txt", "berry.txt", "cherry.txt"].map(entry).to_vec(), ..Default::default() };
        crate::app::apply_listing(&mut tab, folder);
        let mut list = ListView::new();
        let name = |t: &Tab| t.focus.map(|i| t.folder.entries[i].name.clone());
        assert!(list.jump_to_typed(&mut tab, 'b', 0.0));
        assert!(list.jump_to_typed(&mut tab, 'e', 0.2));
        assert_eq!(name(&tab).as_deref(), Some("berry.txt"), "\"be\" together");
        assert!(list.jump_to_typed(&mut tab, 'c', 0.4));
        assert_eq!(name(&tab).as_deref(), Some("cherry.txt"), "no \"bec\": c on its own");
        assert!(list.jump_to_typed(&mut tab, 'h', 0.6));
        assert_eq!(name(&tab).as_deref(), Some("cherry.txt"), "and on from there: \"ch\"");
    }

    /// A cut file fades only itself: search results and duplicates of the same name elsewhere don't.
    #[test]
    fn only_the_cut_file_fades_not_others_of_its_name() {
        let mut tab = Tab::new(1, "C:\\Work".into());
        tab.folder.path = "C:\\Work".into();
        let cut: std::collections::HashSet<String> = ["notes.txt".to_string(), "\u{1F600} é.txt".to_string()].into();
        let at = |name: &str, path: Option<&str>| Entry { target: path.map(str::to_string), ..file(name) };
        let is = |e: &Entry| ListView::is_cut(&tab, Some(&cut), e);
        assert!(is(&at("notes.txt", None)), "the folder's own item");
        assert!(is(&at("Notes.TXT", None)), "by its lower-case name");
        assert!(is(&at("notes.txt", Some("C:\\Work\\notes.txt"))), "the result that is the cut file");
        assert!(is(&at("notes.txt", Some("c:\\work\\notes.txt"))), "capitals don't matter");
        assert!(!is(&at("notes.txt", Some("C:\\Work\\a\\notes.txt"))), "same name, another folder");
        assert!(!is(&at("notes.txt", Some("C:\\Workshop\\notes.txt"))), "a folder that only starts the same");
        assert!(!is(&at("notes.txt", Some("D:\\notes.txt"))));
        assert!(is(&at("\u{1F600} é.txt", Some("C:\\Work\\\u{1F600} é.txt"))));
        assert!(!is(&at("\u{1F600} é.txt", Some("C:\\Work\\b\\\u{1F600} é.txt"))));
        assert!(!is(&at("other.txt", None)));
        assert!(!ListView::is_cut(&tab, None, &at("notes.txt", None)), "nothing cut");
    }

    /// Home's page scrolls with the wheel (its height is the page's, not the rows').
    #[test]
    fn home_scrolls() {
        let _turn = media::test_turn(); // (ticking forgets the media values another test reads)
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        let names: Vec<String> = (0..65).map(|i| format!("f{i}.txt")).collect();
        let entries = names.iter().map(|n| Entry { target: Some(format!("C:\\x\\{n}")), ..file(n) }).collect();
        let parts = crate::recent::Parts { pinned: 6, recent: 6, favorites: 65 };
        let folder = crate::folder::Folder { path: crate::recent::LOCATION.into(), entries, home: Some(Box::new(parts)), ..Default::default() };
        crate::app::apply_listing(&mut app.tabs[0], folder);
        let mut list = ListView::new();
        let ctx = Ctx { hwnd: HWND::default(), scale: 1.0, now: 0.0, maximized: false, active: true };
        list.layout(Rect::new(0.0, 0.0, 600.0, 300.0), &ctx, &app);
        list.tick(0.0, &mut app);
        assert!(list.home_on, "Home's page shows");
        let tab = &app.tabs[0];
        list.home.layout = home::layout(592.0, parts, tab.folder.entries.len(), &list.home, |i| tab.row_of(i).is_some());
        assert!(list.home.layout.height > 600.0, "the page is taller than the list: {}", list.home.layout.height);
        let e = MouseEvent { pos: Point::new(100.0, 100.0), button: MouseButton::Middle, mods: crate::ui::Mods::default(), wheel: -3.0 };
        let _ = list.wheel(&Ctx { now: 1.0, ..ctx }, &e, &mut app);
        list.tick(2.0, &mut app);
        let scrolled = app.tabs[0].scroll;
        assert!(scrolled > 0.0, "scrolled down");
        // A list that was never given room (split view's hidden half) doesn't scroll it back.
        let mut hidden = ListView::new();
        hidden.tick(3.0, &mut app);
        list.tick(3.0, &mut app);
        assert_eq!(app.tabs[0].scroll, scrolled, "still scrolled");
    }

    /// Moving the mouse over the list asks for a new frame only when what is lit up changes:
    /// another row, a column title, the scroll bar, Home's tiles; drags and bands always do.
    #[test]
    fn hovering_redraws_only_when_the_highlight_changes() {
        let _turn = media::test_turn();
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        let names: Vec<String> = (0..200).map(|i| format!("f{i} \u{1F600}.txt")).collect();
        let folder = crate::folder::Folder { path: "C:\\x".into(), entries: names.iter().map(|n| file(n)).collect(), ..Default::default() };
        crate::app::apply_listing(&mut app.tabs[0], folder);
        let mut list = ListView::new();
        let ctx = Ctx { hwnd: HWND::default(), scale: 1.0, now: 0.0, maximized: false, active: true };
        list.layout(Rect::new(0.0, 0.0, 600.0, 400.0), &ctx, &app);
        list.tick(0.0, &mut app);
        list.take_size(&app);
        list.place_columns(&app);
        assert_eq!(list.flow, Flow::Rows);
        let ev = |x: f32, y: f32| MouseEvent { pos: Point::new(x, y), button: MouseButton::Left, mods: crate::ui::Mods::default(), wheel: 0.0 };
        let paint = |list: &mut ListView, app: &AppState| list.painted_hover = list.hover_key(app.tab());
        let redraws = |list: &mut ListView, app: &mut AppState, x: f32, y: f32| matches!(list.mouse_move(&ctx, &ev(x, y), app), Effect::Redraw);
        let area = list.rows_area();
        let row_h = list.geo(app.tab()).cell(1, 0.0).y - list.geo(app.tab()).cell(0, 0.0).y;
        let y0 = list.geo(app.tab()).cell(0, 0.0).y + 2.0;
        // Onto the first row: lit up.
        assert!(redraws(&mut list, &mut app, 100.0, y0));
        paint(&mut list, &app);
        assert_eq!(list.painted_hover.item, Some(0));
        // Along the same row: nothing new to draw.
        for dx in 1..30 {
            assert!(!redraws(&mut list, &mut app, 100.0 + dx as f32, y0 + (dx % 3) as f32), "moved {dx} along row 0");
        }
        // To the next row: drawn again, once.
        assert!(redraws(&mut list, &mut app, 100.0, y0 + row_h));
        paint(&mut list, &app);
        assert!(!redraws(&mut list, &mut app, 120.0, y0 + row_h + 1.0));
        // Not painted yet since a change: still asks (the frame shows the old one).
        assert!(redraws(&mut list, &mut app, 100.0, y0 + 2.0 * row_h));
        assert!(redraws(&mut list, &mut app, 101.0, y0 + 2.0 * row_h));
        paint(&mut list, &app);
        // Up to the column titles, from one to another.
        let header = list.header();
        let cols: Vec<(f32, f32)> = list.cols.iter().map(|c| (c.x, c.w)).collect();
        assert!(redraws(&mut list, &mut app, cols[0].0 + 10.0, header.y + header.h / 2.0));
        paint(&mut list, &app);
        assert!(list.painted_hover.column.is_some());
        assert!(!redraws(&mut list, &mut app, cols[0].0 + 12.0, header.y + header.h / 2.0));
        if cols.len() > 1 {
            assert!(redraws(&mut list, &mut app, cols[1].0 + cols[1].1 / 2.0, header.y + header.h / 2.0));
            paint(&mut list, &app);
        }
        // Near the scroll bar: its thumb lights up.
        let near = area.right() - 2.0;
        if list.thumb(app.tab()).is_some() {
            assert!(redraws(&mut list, &mut app, near, area.y + 50.0));
            paint(&mut list, &app);
            assert!(list.painted_hover.track);
            assert!(!redraws(&mut list, &mut app, near, area.y + 51.0));
        }
        // Scrolled under a still pointer: the next frame paints the new row, and moving on that
        // row after it needs nothing more.
        assert!(redraws(&mut list, &mut app, 100.0, y0));
        paint(&mut list, &app);
        app.tabs[0].scroll = 3.0 * row_h;
        assert_ne!(list.hover_key(app.tab()), list.painted_hover, "another row under it now");
        paint(&mut list, &app);
        assert!(!redraws(&mut list, &mut app, 101.0, y0));
        // A press that becomes a band, and a column edge dragged: every move draws.
        list.band = Some(Band { tab_id: app.tabs[0].id, start: Point::new(500.0, y0), now: Point::new(500.0, y0), base: vec![false; 200], active: false, at: 0.0 });
        assert!(!matches!(list.mouse_move(&ctx, &ev(501.0, y0), &mut app), Effect::None));
        app.tabs[0].scroll = 0.0;
        list.band = Some(Band { tab_id: app.tabs[0].id, start: Point::new(500.0, y0), now: Point::new(500.0, y0), base: vec![false; 200], active: false, at: 0.0 });
        assert!(redraws(&mut list, &mut app, 501.0, y0), "a press not yet a band");
        assert!(matches!(list.mouse_move(&ctx, &ev(560.0, y0 + 60.0), &mut app), Effect::Animate), "a band");
        list.band = None;
        list.drag = Drag::Bar;
        assert!(redraws(&mut list, &mut app, 102.0, y0));
        list.drag = Drag::None;
        // Leaving: the highlight goes.
        assert!(matches!(list.mouse_leave(&ctx, &mut app), Effect::Redraw));
        assert_eq!(list.hover_key(app.tab()).item, None);
    }

    /// Home: tiles, lines and the chevrons light up one by one, not on every pixel.
    #[test]
    fn hovering_home_redraws_only_when_the_spot_changes() {
        let _turn = media::test_turn();
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        let names: Vec<String> = (0..30).map(|i| format!("f{i}.txt")).collect();
        let entries = names.iter().map(|n| Entry { target: Some(format!("C:\\x\\{n}")), ..file(n) }).collect();
        let parts = crate::recent::Parts { pinned: 6, recent: 6, favorites: 30 };
        let folder = crate::folder::Folder { path: crate::recent::LOCATION.into(), entries, home: Some(Box::new(parts)), ..Default::default() };
        crate::app::apply_listing(&mut app.tabs[0], folder);
        let mut list = ListView::new();
        let ctx = Ctx { hwnd: HWND::default(), scale: 1.0, now: 0.0, maximized: false, active: true };
        list.layout(Rect::new(0.0, 0.0, 600.0, 300.0), &ctx, &app);
        list.tick(0.0, &mut app);
        assert!(list.home_on);
        let tab = &app.tabs[0];
        list.home.layout = home::layout(600.0 - SCROLL_GUTTER.min(12.0), parts, tab.folder.entries.len(), &list.home, |i| tab.row_of(i).is_some());
        let ev = |p: Point| MouseEvent { pos: p, button: MouseButton::Left, mods: crate::ui::Mods::default(), wheel: 0.0 };
        let spots: Vec<(home::Spot, Rect)> = list.home.layout.spots.clone();
        let mut lit = 0;
        for (spot, rc) in spots.iter().filter(|(_, rc)| rc.bottom() < 290.0 && rc.w > 6.0 && rc.h > 6.0).take(12) {
            let inside = Point::new(rc.x + rc.w / 2.0, rc.y + rc.h / 2.0);
            let moved = matches!(list.mouse_move(&ctx, &ev(inside), &mut app), Effect::Redraw);
            if moved {
                lit += 1;
            }
            list.painted_hover = list.hover_key(app.tab());
            assert_eq!(list.painted_hover.spot, Some(*spot));
            assert!(matches!(list.mouse_move(&ctx, &ev(Point::new(inside.x + 1.0, inside.y + 1.0)), &mut app), Effect::None), "{spot:?} again");
        }
        assert!(lit >= 2, "spots lit up: {lit}");
    }

    /// The Apps key on Home opens the menu at the chosen tile or line, scrolled or not.
    #[test]
    fn home_keyboard_menu_opens_at_the_item() {
        let _turn = media::test_turn();
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        let names: Vec<String> = (0..65).map(|i| format!("f{i} \u{1F600}.txt")).collect();
        let entries = names.iter().map(|n| Entry { target: Some(format!("C:\\x\\{n}")), ..file(n) }).collect();
        let parts = crate::recent::Parts { pinned: 6, recent: 6, favorites: 65 };
        let folder = crate::folder::Folder { path: crate::recent::LOCATION.into(), entries, home: Some(Box::new(parts)), ..Default::default() };
        crate::app::apply_listing(&mut app.tabs[0], folder);
        let mut list = ListView::new();
        let ctx = Ctx { hwnd: HWND::default(), scale: 1.0, now: 0.0, maximized: false, active: true };
        list.layout(Rect::new(10.0, 40.0, 600.0, 300.0), &ctx, &app);
        list.tick(0.0, &mut app);
        assert!(list.home_on);
        let tab = &app.tabs[0];
        list.home.layout = home::layout(592.0, parts, tab.folder.entries.len(), &list.home, |i| tab.row_of(i).is_some());
        for (index, scroll) in [(0usize, 0.0f32), (3, 0.0), (8, 0.0), (40, 0.0), (60, 0.0)] {
            let tab = &mut app.tabs[0];
            tab.scroll = scroll;
            let row = tab.row_of(index).expect("shown");
            list.reveal(tab, row);
            let at = list.item_menu_at(tab, row);
            let rc = list.home.layout.rect_of(index).expect("on the page");
            let s = tab.scroll;
            // Just above the menu's corner is the item itself, on screen.
            let probe = Point::new(at.x, at.y - 2.0);
            assert!(list.rect.contains(probe), "item {index}: {at:?} in {:?}", list.rect);
            assert_eq!(list.home.layout.spot_at(list.home_point(tab, probe)), Some(home::Spot::Item(index)), "item {index} at {rc:?}, scroll {s}: menu {at:?}");
        }
    }

    #[test]
    fn a_name_typed_before_loses_the_hidden_extension() {
        assert_eq!(strip_extension("draft.PDF", ".pdf"), "draft");
        assert_eq!(strip_extension("draft", ".pdf"), "draft");
        assert_eq!(strip_extension("é.pdf", ".pdf"), "é");
        assert_eq!(strip_extension("x", ""), "x");
    }

    /// A theme with the wallpaper settings given (the rest standard).
    fn wall_theme(fit: WallpaperFit, at: (f32, Option<f32>), zoom: f32, size: Option<f32>, offset: (f32, f32)) -> Theme {
        let mut t = Theme::dark();
        (t.wallpaper_fit, t.wallpaper_at, t.wallpaper_at_y, t.wallpaper_zoom, t.wallpaper_size, t.wallpaper_offset) = (fit, at.0, at.1, zoom, size, offset);
        t
    }

    fn once(d: Option<WallDraw>) -> (Rect, Rect) {
        match d {
            Some(WallDraw::Once { dest, src }) => (dest, src),
            other => panic!("not once: {other:?}"),
        }
    }

    #[test]
    fn wallpapers_are_placed_as_the_skin_says() {
        let area = Rect::new(0.0, 0.0, 400.0, 400.0);
        let pic = (200.0, 100.0);
        let standard = (1.0, None);
        // Filling: as big as covers it (4 times), its right side and top in view.
        let (dest, src) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Fill, standard, 1.0, None, (0.0, 0.0)), 1.0));
        assert_eq!((dest, src), (area, Rect::new(100.0, 0.0, 100.0, 100.0)));
        // Whole: all of it (twice as big), at the bottom.
        let (dest, src) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Whole, standard, 1.0, None, (0.0, 0.0)), 1.0));
        assert_eq!((dest, src), (Rect::new(0.0, 200.0, 400.0, 200.0), Rect::new(0.0, 0.0, 200.0, 100.0)));
        // Stretch: all of it, to the area's shape; a size doesn't change that.
        let (dest, src) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Stretch, standard, 1.0, Some(0.25), (0.0, 0.0)), 1.0));
        assert_eq!((dest, src), (area, Rect::new(0.0, 0.0, 200.0, 100.0)));
        // Actual: its own size in the screen's pixels (at 200%: half as many DIPs), bottom right.
        let (dest, _) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Actual, standard, 1.0, None, (0.0, 0.0)), 0.5));
        assert_eq!(dest, Rect::new(300.0, 350.0, 100.0, 50.0));
        // A size: half the area's width, its shape kept; the zoom on top; then the offset.
        let (dest, _) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Actual, (0.0, Some(0.0)), 1.0, Some(0.5), (0.0, 0.0)), 1.0));
        assert_eq!(dest, Rect::new(0.0, 0.0, 200.0, 100.0));
        let (dest, _) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Actual, (0.0, Some(0.0)), 1.5, Some(0.5), (20.0, -10.0)), 1.0));
        assert_eq!(dest, Rect::new(20.0, 0.0, 300.0, 140.0), "moved 20 across and 10 up, its top cut off");
        // Percentages: 25% across, 75% down.
        let (dest, _) = once(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Actual, (0.25, Some(0.75)), 1.0, None, (0.0, 0.0)), 1.0));
        assert_eq!(dest, Rect::new(50.0, 225.0, 200.0, 100.0));
        // Moved all the way off: nothing to draw.
        assert_eq!(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Actual, standard, 1.0, None, (2000.0, 0.0)), 1.0), None);
        assert_eq!(wallpaper_layout(area, pic, &wall_theme(WallpaperFit::Fill, standard, 1.0, None, (0.0, -2000.0)), 1.0), None);
    }

    #[test]
    fn awkward_wallpapers_still_place_sensibly() {
        let standard = (1.0, None);
        let plain = |fit| wall_theme(fit, standard, 1.0, None, (0.0, 0.0));
        // A 1 x 1 picture filling a huge area.
        let huge = Rect::new(0.0, 0.0, 100_000.0, 60_000.0);
        let (dest, src) = once(wallpaper_layout(huge, (1.0, 1.0), &plain(WallpaperFit::Fill), 1.0));
        assert_eq!(dest, huge);
        assert!(src.w > 0.0 && src.w <= 1.0 && src.h > 0.0 && src.h <= 1.0, "{src:?}");
        // An area smaller than the picture, at its own size: the bottom right corner of it shows.
        let small = Rect::new(50.0, 50.0, 10.0, 10.0);
        let (dest, src) = once(wallpaper_layout(small, (4000.0, 3000.0), &plain(WallpaperFit::Actual), 1.0));
        assert_eq!((dest, src), (small, Rect::new(3990.0, 2990.0, 10.0, 10.0)));
        // Tiles: from where the picture would sit, as big as zoomed (a sixth, at -500).
        let area = Rect::new(10.0, 20.0, 300.0, 200.0);
        match wallpaper_layout(area, (1.0, 1.0), &wall_theme(WallpaperFit::Tile, (0.0, Some(0.0)), 1.0 / 6.0, None, (0.0, 0.0)), 1.0) {
            Some(WallDraw::Tiled { fill, origin, tile }) => {
                assert_eq!((fill, origin), (area, Point::new(10.0, 20.0)));
                assert!((tile.0 - 1.0 / 6.0).abs() < 1e-6 && (tile.1 - 1.0 / 6.0).abs() < 1e-6, "{tile:?}");
            }
            other => panic!("{other:?}"),
        }
        // Tiles moved by the offset, and placed from the right side.
        match wallpaper_layout(area, (64.0, 32.0), &wall_theme(WallpaperFit::Tile, standard, 1.0, None, (5.0, -5.0)), 1.0) {
            Some(WallDraw::Tiled { origin, tile, .. }) => assert_eq!((origin, tile), (Point::new(10.0 + 300.0 - 64.0 + 5.0, 15.0), (64.0, 32.0))),
            other => panic!("{other:?}"),
        }
        // Nothing to place: no picture, or no room.
        assert_eq!(wallpaper_layout(area, (0.0, 10.0), &plain(WallpaperFit::Fill), 1.0), None);
        assert_eq!(wallpaper_layout(Rect::new(0.0, 0.0, 0.0, 100.0), (10.0, 10.0), &plain(WallpaperFit::Fill), 1.0), None);
        assert_eq!(wallpaper_layout(area, (10.0, 10.0), &plain(WallpaperFit::Tile), 0.0), None, "no size at all");
        // Split view: both halves are given the same area, so they place it the same.
        let both = Rect::new(0.0, 0.0, 800.0, 400.0);
        let theme = plain(WallpaperFit::Whole);
        assert_eq!(wallpaper_layout(both, (300.0, 200.0), &theme, 1.0), wallpaper_layout(both, (300.0, 200.0), &theme, 1.0));
    }

    #[test]
    fn a_wallpapers_fade_runs_in_from_each_edge() {
        use crate::render::fade_stops;
        assert_eq!(fade_stops(0.1), [(0.0, 0.0), (0.1, 1.0), (0.9, 1.0), (1.0, 0.0)]);
        assert_eq!(fade_stops(0.5), [(0.0, 0.0), (0.5, 1.0), (0.5, 1.0), (1.0, 0.0)], "the most: fades from the middle");
        assert_eq!(fade_stops(0.9), fade_stops(0.5));
        assert_eq!(fade_stops(-1.0), fade_stops(0.0));
        assert!(fade_stops(0.3).windows(2).all(|p| p[0].0 <= p[1].0), "stops in order");
    }
}
