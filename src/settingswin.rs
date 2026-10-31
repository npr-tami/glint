//! The Settings window (Ctrl+, or the SETTINGS button): a small window of its own, owned by the
//! main window and drawn by Glint like everything else. Its four pages (General, Look, Files, Keys)
//! are tabs along the top or a list down the left, with a search box above them and a line at the
//! bottom (Glint's version, Report a bug, RESET PAGE). A change applies at once and is saved like
//! any other choice; the window stays open while Glint is used, so every change shows behind it.
//!
//! How it looks is the person's choice too (Appearance > Settings window, `settings.rs`):
//! - **Opens**: floating where it was left (the middle of Glint at first), glued to Glint's side, or
//!   over Glint's right edge where the details panel goes.
//! - **Choices look like**: ticks and round buttons in group boxes, like File Explorer's Folder
//!   Options (a short choice as round buttons, a long one as a drop-down); or sentences, every
//!   choice spelled out with round buttons under it and no boxes.
//! - **Show**: everything, or the everyday settings with "More settings" for the rest.
//! - **Skins as**: a drop-down, or little tiles in each skin's colours.
//! - The search box, a dot by each setting that isn't at its default, RESET PAGE, and reopening on
//!   the page used last can each be switched off.
//!
//! Settings that aren't in this version yet are shown greyed, with "Coming later".
//!
//! How it fits in:
//! - **One state, one guard.** The window's state (`SettingsWindow`) lives in the main window's
//!   state, and its messages go through the main window's re-entrancy guard
//!   (`window::settings_message`). So it changes `app.settings` directly, and the main window
//!   notices and saves, as for any other change - and the two windows can never hold their state
//!   twice at once.
//! - **Nothing modal inside a message.** A drop-down's list is a native popup menu and BROWSE shows
//!   Windows' folder picker; both run their own message loop. They come back to the main window as
//!   [`Job`]s, which it runs after the message, outside its guard ([`run_job`]), and the result
//!   comes back through `job_done`.
//! - **Never waiting for the disk.** Which terminals are installed, how big the cache is, clearing
//!   it, switching Portable and making a folder before it's opened are done on background threads;
//!   their results are posted back (WM_APP_BG).
//! - What is on the pages, what works yet and what a change does is in settingswin/pages.rs. The
//!   controls are drawn with kit.rs.
//!
//! Keyboard: Tab / Shift+Tab move between the search box, the pages and the controls; on the pages,
//! Left and Right (tabs) or Up and Down (list) change the page; Up and Down move between controls
//! on a page; Space or Enter flips a tick, picks a round button, opens a drop-down (also Alt+Down or
//! F4) or presses a button; Ctrl+F goes to the search box; Ctrl+Tab and Ctrl+PgDn / PgUp change the
//! page; PgUp, PgDn, Home and End scroll; Esc closes. Arrows never change a setting by themselves.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Map, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateSolidBrush, DeleteObject, GetMonitorInfoW, InvalidateRect, MonitorFromRect, MonitorFromWindow, RedrawWindow, ScreenToClient,
    SetBkColor, SetTextColor, ValidateRect, COLOR_WINDOW, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL,
    RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_CONTROL, VK_DOWN,
    VK_END, VK_ESCAPE, VK_F4, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_OEM_COMMA, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT,
    VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName, SetWindowSubclass, FOS_FORCEFILESYSTEM,
    FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetParent, GetWindow, GetWindowRect, IsIconic, IsZoomed,
    KillTimer, LoadCursorW,
    LoadIconW, PostMessageW, RegisterClassExW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, GW_OWNER, HTCLIENT,
    IDC_ARROW, MINMAXINFO, SIZE_MINIMIZED, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_RESTORE, SW_SHOW, WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW,
    WS_CAPTION, WS_CLIPCHILDREN, WS_SYSMENU, WS_THICKFRAME,
};

use crate::app::AppState;
use crate::registry;
use crate::droplist;
use crate::frame;
use crate::winutil::{hi_word, lo_word, place_visible, visible_rect};
use crate::kit::{self, size as ksize, Look, Scroller};
use crate::log;
// Settings > About > Updates (update.rs).
use crate::{life, update};
use crate::render::{FrameResult, Renderer, TextStyle};
use crate::settings::{self, Settings, SettingsOpens, Terminal};
use crate::shell;
use crate::textbox::{self, TextBox, TextEvent, TextEventKind, WM_APP_TEXT};
use crate::theme::{size, Color, Theme};
use crate::ui::{Command, MenuItem, MenuPicture, Point, Rect};

mod flat;
mod pages;
pub use pages::{findable, ABOUT, ABOUT_ROW, PAGES};
use pages::{Act, Ctl, Facts, Item, Row};

/// The window's title (what Alt+Tab and the taskbar's thumbnails would call it).
pub const TITLE: &str = "Glint settings";
/// The page list's width, and its rows.
const NAV_W: f32 = 130.0;
const NAV_ITEM_H: f32 = 30.0;
/// Space above the first page in the list.
const NAV_TOP: f32 = 8.0;
/// The tabs along the top: their height and margins.
const TAB_H: f32 = 30.0;
const PILL_PAD: f32 = 6.0;
/// Narrower than this, the pages are tabs even when they should be a list.
const LIST_MIN_W: f32 = 460.0;
/// Floating with the pages as a list, the window is at least this wide (the list takes room).
const FLOAT_LIST_W: f32 = 540.0;
/// The search box, and the space round it.
const SEARCH_H: f32 = 26.0;
const SEARCH_PAD: f32 = 8.0;
/// The line at the bottom (version, Report a bug, RESET PAGE).
const FOOT_H: f32 = 36.0;
/// The page's margins round the groups.
const PAD_X: f32 = 14.0;
const PAD_TOP: f32 = 14.0;
const PAD_BOTTOM: f32 = 16.0;
/// Room kept free on the right for the scrollbar.
const GUTTER: f32 = 8.0;
const MAX_CARD_W: f32 = 640.0;
/// A group: its heading, the space inside its box, and between groups.
const HEADING_H: f32 = 18.0;
const GROUP_PAD: f32 = 10.0;
const GROUP_GAP: f32 = 10.0;
/// Inside a row.
const ROW_H: f32 = 26.0;
const OPTION_H: f32 = 22.0;
const TITLE_LINE: f32 = 17.0;
const NOTE_LINE: f32 = 15.0;
/// How far a row that belongs to the one above it, or a choice's round buttons, are pulled in.
const INDENT: f32 = 20.0;
/// Between a row's text and its control.
const CONTROL_GAP: f32 = 14.0;
/// Ticks and round buttons, and the space after them.
const CHECK: f32 = 13.0;
const CHECK_GAP: f32 = 8.0;
/// Drop-downs and buttons, a little smaller than in the other windows.
const CTL_H: f32 = 22.0;
/// The shortcuts: the keys' column, and a line.
const KEYS_W: f32 = 128.0;
const KEYS_ROW_H: f32 = 22.0;
/// Two columns of shortcuts from this width.
const KEYS_TWO_W: f32 = 520.0;
/// Drop-downs are as wide as their longest choice, within these.
const DROPDOWN_MIN: f32 = 120.0;
const DROPDOWN_MAX: f32 = 240.0;
/// Skin tiles: at least this wide, this tall (the colours, then the name).
const TILE_MIN_W: f32 = 104.0;
const TILE_H: f32 = 48.0;
const TILE_GAP: f32 = 6.0;
/// The dot by a setting that isn't at its default.
const DOT: f32 = 5.0;
/// The window's sizes (DIPs): floating at first, glued to the side, in the details panel's place;
/// and the smallest it gets.
const FLOAT_W: f32 = 440.0;
const FLOAT_H: f32 = 520.0;
const SIDE_W: f32 = 340.0;
const PANEL_W: f32 = 320.0;
const MIN_W: f32 = 300.0;
const MIN_H: f32 = 300.0;

thread_local! {
    /// Where Settings opens ("Opens"), and whether its pages are a list, for the window's own
    /// messages (`hit_test`, `follow`).
    static OPENS: Cell<SettingsOpens> = const { Cell::new(SettingsOpens::Floating) };
    static LIST: Cell<bool> = const { Cell::new(false) };
    /// Drawn as the mockup (flat.rs) in this skin: also how high the title bar is (`hit_test`).
    static FLAT: Cell<bool> = const { Cell::new(false) };
}

fn flat_on() -> bool {
    FLAT.with(Cell::get)
}

/// The title bar's height in the skin in use.
fn title_height() -> f32 {
    if flat_on() {
        flat::TITLE_H
    } else {
        size::TITLE_H
    }
}

// The controls, drawn as the mockup or in the skin's own shapes.

fn button_w(r: &mut Renderer, label: &str) -> f32 {
    if flat_on() {
        flat::button_width(r, label)
    } else {
        kit::button_width(r, label)
    }
}

fn drop_width<'a>(r: &mut Renderer, labels: impl IntoIterator<Item = &'a str>) -> f32 {
    if flat_on() {
        flat::dropdown_width(r, labels, 90.0, DROPDOWN_MAX)
    } else {
        kit::dropdown_width(r, labels, DROPDOWN_MIN, DROPDOWN_MAX)
    }
}

fn draw_button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look) {
    if flat_on() {
        flat::button(r, theme, rect, label, look)
    } else {
        kit::button(r, theme, rect, label, look)
    }
}

fn draw_check(r: &mut Renderer, theme: &Theme, rect: Rect, on: f32, look: Look) {
    if flat_on() {
        flat::check(r, theme, rect, on, look)
    } else {
        kit::checkbox(r, theme, rect, on, look)
    }
}

fn draw_radio(r: &mut Renderer, theme: &Theme, rect: Rect, on: f32, look: Look) {
    if flat_on() {
        flat::radio(r, theme, rect, on, look)
    } else {
        kit::radio(r, theme, rect, on, look)
    }
}

fn draw_dropdown(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, look: Look) {
    if flat_on() {
        flat::dropdown(r, theme, rect, text, look)
    } else {
        kit::dropdown(r, theme, rect, text, look)
    }
}
/// One notch of the mouse wheel scrolls this far.
const WHEEL_STEP: f32 = 96.0;
/// The text boxes' ids (textbox.rs: 400-499 belong to the Settings window).
const FOLDER_BOX: u16 = 400;
const SEARCH_BOX: u16 = 401;
/// Under a greyed row.
const LATER: &str = "Coming later";
/// The text of the title line.
const TITLE_FONT: f32 = 12.0;

/// Results of background work for this window (see `post`).
const WM_APP_BG: u32 = WM_APP + 50;
/// Tab pressed in the text box (its own subclass tells the window; see `tab_proc`).
const WM_APP_TAB: u32 = WM_APP + 51;
/// CLEAR and RESET PAGE must be pressed a second time within this many seconds.
const CLEAR_CONFIRM_SECONDS: f64 = 8.0;
/// Trying to draw again after the graphics card refused a frame.
const RETRY_TIMER: usize = 1;

thread_local! {
    /// The text box's colours (background COLORREF, text COLORREF, brush), for WM_CTLCOLOREDIT.
    static EDIT_COLORS: Cell<(u32, u32, isize)> = const { Cell::new((0, 0, 0)) };
}

/// Something the main window does for this window after the message (see the module comment).
#[derive(Clone, Debug)]
pub enum Job {
    /// Show this window (just made): cloaked until its first frame, like the main window.
    Show,
    /// Bring it to the front (Ctrl+, while it's open).
    Activate,
    /// A drop-down's list, opened under its box (`.1`, client DIPs; `.0.under` is filled in then).
    Drop(droplist::DropList, Rect),
    /// Windows' folder picker, starting in this folder.
    PickFolder(String),
    /// Put the window here (after a DPI change).
    Place(RECT),
    /// Floating: at least this wide (DIPs), where it is (the pages just became a list).
    Widen(f32),
    /// To its place for "Settings opens", at that choice's first size (it just changed).
    Follow,
    Close,
}

/// What a job brought back.
#[derive(Clone, Debug, PartialEq)]
pub enum JobDone {
    Nothing,
    /// The menu item picked (0: none).
    Picked(u32),
    /// The folder picked in Windows' folder picker.
    Folder(String),
}

/// What the main window should do after this window handled something.
#[derive(Clone, Debug)]
pub enum Ask {
    /// An app or window command (light or dark, open a folder in a tab).
    Command(Command),
    Job(Job),
    /// Paint the main window again (a setting it shows changed).
    RedrawMain,
}

/// Results of background work.
#[derive(Debug)]
enum Bg {
    Terminals([bool; 6]),
    /// What the Zip icon row says (registry::zip_icon_said).
    ZipSaid(String),
    CacheSize(u64),
    /// CLEAR is done: the bytes left (files in use).
    Cleared(u64),
    Portable(Result<String, String>),
    /// The folder exists now (it was made if needed): open it in a tab.
    Open(String),
}

/// Background results waiting for their window: (the window's number, the result).
static RESULTS: Mutex<Vec<(u64, Bg)>> = Mutex::new(Vec::new());
/// Each Settings window gets a number of its own, so a result meant for one that was closed meanwhile
/// never reaches the next one (which may even get the same window handle).
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Where a background job sends its result: the window's handle (to wake it) and its number.
#[derive(Clone, Copy)]
struct Poster {
    hwnd: isize,
    serial: u64,
}

/// Hands a background result to the window and wakes it.
fn post(to: Poster, bg: Bg) {
    RESULTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((to.serial, bg));
    // SAFETY: posting to a window by handle; if it's gone the message is simply dropped.
    unsafe {
        let _ = PostMessageW(Some(HWND(to.hwnd as *mut _)), WM_APP_BG, WPARAM(0), LPARAM(0));
    }
}

/// A place in the window that can have the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    /// The search box (a real Windows EDIT, which then has the keyboard itself).
    Search,
    /// The pages (tabs or list).
    Nav,
    /// A control of row `.0`.
    Ctl(usize, Part),
    /// "More settings" / "Fewer settings" at the end of a page.
    More,
    /// RESET PAGE at the bottom.
    Reset,
}

/// Which of a row's controls: the main one, the second (BROWSE next to the text box, the tick next
/// to EDIT…), or one of a choice's round buttons or skin tiles (by its number in the choices).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Main,
    Second,
    Option(usize),
}

/// What is under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hot {
    Close,
    Nav(usize),
    Ctl(usize, Part),
    Thumb,
    More,
    Reset,
    ReportBug,
}

/// How a row's control is shown (it depends on the window's own choices, `Style`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// A tick with the title beside it (an on/off setting; a button beside it for EDIT…).
    Tick,
    /// The title, then a round button for each choice under it.
    Radios,
    /// The title, then a tile for each skin.
    Tiles,
    /// The title with its control on the right (a drop-down, a button), or nothing.
    Plain,
    /// The start folder's text box with BROWSE.
    Folder,
    /// A shortcut: the keys, then what they do.
    Keys,
}

/// Drawn parts of a page other than rows, in page coordinates (y from the top of the scrolling page).
#[derive(Clone, Debug)]
enum Block {
    /// A group box with its title on its top line (ticks), or a heading above the rows (sentences).
    Group(Rect, String),
    Heading(Rect, String),
    /// A line of text on its own ("Nothing found").
    Note(Rect, String),
    /// "More settings" / "Fewer settings".
    More(Rect, String),
}

/// The window's own choices (Appearance > Settings window), as read from the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Style {
    /// The pages as a list down the left (else tabs along the top).
    list: bool,
    /// Every choice spelled out (else ticks and round buttons in group boxes).
    sentences: bool,
    /// Only the everyday settings, with "More settings" for the rest.
    basic: bool,
    tiles: bool,
    search: bool,
    dots: bool,
    reset: bool,
}

impl Style {
    fn of(values: &Map<String, Value>) -> Style {
        let is = |key: &str, word: &str| values.get(key).and_then(Value::as_str) == Some(word);
        let on = |key: &str| values.get(key).and_then(Value::as_bool).unwrap_or(true);
        Style {
            list: is("SettingsPages", "list"),
            sentences: is("SettingsStyle", "sentences"),
            basic: is("SettingsShow", "basic"),
            tiles: is("SettingsSkins", "tiles"),
            search: on("SettingsSearch"),
            dots: on("SettingsDots"),
            reset: on("SettingsReset"),
        }
    }
}

/// A row as laid out.
#[derive(Clone, Debug)]
struct Placed {
    row: Row,
    shape: Shape,
    /// The whole row, in page coordinates.
    rect: Rect,
    title: Vec<String>,
    note: Vec<String>,
    /// Where the text starts (across, and down from the row's top), and how wide it may be.
    text_x: f32,
    text_y: f32,
    text_w: f32,
    /// The controls (page coordinates): a drop-down, button or text box, and the second one; for a
    /// tick its box (`main`), and what a click takes (`hit`: the box and its title).
    main: Option<Rect>,
    second: Option<Rect>,
    hit: Option<Rect>,
    /// A choice's round buttons or skin tiles, in the order of its choices, and what a click on
    /// each takes (the button and its words).
    options: Vec<Rect>,
    option_hits: Vec<Rect>,
    /// It can be used (it works in this version, and the switch it depends on is on).
    enabled: bool,
    /// Where the dot goes when the setting isn't at its default.
    dot: Point,
}

impl Placed {
    fn rect_of(&self, part: Part) -> Option<Rect> {
        match part {
            Part::Main => self.main,
            Part::Second => self.second,
            Part::Option(n) => self.options.get(n).copied(),
        }
    }

    /// What a click on `part` takes.
    fn hit_of(&self, part: Part) -> Option<Rect> {
        match part {
            Part::Option(n) => self.option_hits.get(n).copied(),
            p if self.shape == Shape::Tick && p == self.tick_part() => self.hit,
            _ => self.rect_of(part),
        }
    }

    /// Which part of a ticked row is its tick (beside EDIT… it's the second).
    fn tick_part(&self) -> Part {
        if matches!(self.row.ctl, Ctl::ButtonToggle(..)) {
            Part::Second
        } else {
            Part::Main
        }
    }

    /// Every part that can have the keyboard, in Tab order.
    fn parts(&self) -> Vec<Part> {
        let mut parts: Vec<Part> = [Part::Main, Part::Second].into_iter().filter(|p| self.rect_of(*p).is_some()).collect();
        parts.extend((0..self.options.len()).map(Part::Option));
        parts
    }
}

pub struct SettingsWindow {
    hwnd: HWND,
    /// This window's number (see NEXT_SERIAL).
    serial: u64,
    renderer: Renderer,
    dpi: u32,
    /// Client size in DIPs (inside 98 / 2000's window frame, when the skin draws one).
    size: (f32, f32),
    /// The skin is 98 / 2000's: the window has their raised frame (frame.rs).
    classic: bool,
    started: Instant,
    page: usize,
    /// The window's own choices, as of the last layout.
    style: Style,
    /// Every setting at its default (for the dots, and RESET PAGE).
    defaults: Map<String, Value>,
    blocks: Vec<Block>,
    rows: Vec<Placed>,
    scroller: Scroller,
    layout_dirty: bool,
    /// The settings as last drawn, as written in settings.json.
    values: Map<String, Value>,
    focus: Spot,
    /// Show the keyboard's outline (after a key was used, as in Windows; a click hides it).
    cues: bool,
    hover: Option<Hot>,
    pressed: Option<Hot>,
    /// Dragging the scrollbar's thumb: where in the thumb it was grabbed.
    drag: Option<f32>,
    tracking: bool,
    active: bool,
    /// The start folder's text box (a real Windows EDIT), and where it was last put.
    folder_box: TextBox,
    box_at: Option<Rect>,
    /// What the text box was last given, to notice a change from elsewhere (BROWSE).
    box_text: String,
    /// The search box, and where it was last put; what's typed in it.
    search_box: TextBox,
    search_at: Option<Rect>,
    query: String,
    /// "More settings" was pressed: the whole of every page shows, while Settings shows the basics.
    show_more: bool,
    /// RESET PAGE was pressed once at this time and waits for a second press.
    reset_asked_at: Option<f64>,
    /// The line at the bottom: Report a bug, and RESET PAGE (window coordinates).
    bug_at: Option<Rect>,
    reset_at: Option<Rect>,
    /// Each skin's colours for its tile (background, panel, accent, text), by id, as of the
    /// library's generation.
    swatches: Vec<(String, [Color; 4])>,
    swatches_generation: u64,
    /// The drop-down whose list is open: its setting and the values of its choices.
    menu_for: Option<(&'static str, Vec<pages::Val>)>,
    terminals: Option<[bool; 6]>,
    /// What the Zip icon row says, once looked up.
    zip_said: Option<String>,
    /// The row to put the keyboard on once the page is laid out (found with the command box).
    point_at: Option<String>,
    cache_size: Option<u64>,
    /// What clearing the cache said last; CLEAR was pressed once at this time and waits for a
    /// second press; the cache is being emptied.
    cache_said: Option<String>,
    clear_asked_at: Option<f64>,
    clearing: bool,
    portable_said: Option<String>,
    portable_busy: bool,
    /// Under Wine a hidden text box stays on screen until a later frame: paint until then.
    repaint_until: f64,
    /// Put the keyboard in the text box once it's laid out ("A specific folder" was just picked).
    focus_box_soon: bool,
    /// The text box that had the keyboard when the window was switched away from (it gets it back).
    box_had_focus: Option<u16>,
    /// Space or Enter on a drop-down: its list opens when that key's character arrives (it would
    /// otherwise reach the menu and close it again).
    act_on_char: Option<(usize, Part)>,
    /// The skins for the Skin drop-down (id, name, its line about itself), as of the library's
    /// generation (skin.rs): a skin added or renamed in the customiser shows at once.
    skins: Vec<(String, String, String)>,
    skins_generation: u64,
    /// Where the window opens and stays ("Settings opens").
    opens: SettingsOpens,
    /// The page tabs along the top (`tabs`); empty when the pages are a list.
    pills: Vec<Rect>,
    /// The skin in use has no light or dark other half, so "Light or dark" is greyed.
    no_twin: bool,
    /// The switches the person just flipped, gliding to their new place (kit.rs).
    flips: kit::Flips,
    /// How thick the scrollbar is in the skin.
    bar: kit::Bar,
    /// An older Windows' scroll bar arrow or track held down, repeating.
    holding: kit::Holding,
}

/// The settings as written in settings.json.
fn values_of(s: &Settings) -> Map<String, Value> {
    match serde_json::to_value(s) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// Gives the setting called `key` (its name in settings.json) a new value, through the file's
/// form, so it works whatever Rust type holds it. False if the value doesn't fit the setting.
/// (Every field of Settings is written to the file, so the round trip keeps them all.)
fn set_in(s: &mut Settings, key: &str, value: Value) -> bool {
    let Ok(Value::Object(mut map)) = serde_json::to_value(&*s) else { return false };
    map.insert(key.to_string(), value);
    match serde_json::from_value::<Settings>(Value::Object(map)) {
        Ok(new) => {
            *s = new;
            true
        }
        Err(_) => false,
    }
}

/// A true/false value: a setting, or a switch kept outside settings.json ("@" names).
fn bool_of(values: &Map<String, Value>, key: &str) -> bool {
    match key {
        "@Portable" => settings::portable_now(),
        k if k.starts_with('@') => pages::outside(k).unwrap_or(false),
        k => values.get(k).and_then(Value::as_bool).unwrap_or(false),
    }
}

/// Whether a row's switch (`ctl`, its main one or the one beside a button) is on.
fn toggle_on(ctl: &Ctl, values: &Map<String, Value>) -> bool {
    match ctl {
        Ctl::Toggle(k) | Ctl::ButtonToggle(_, _, k) => bool_of(values, k),
        Ctl::ToggleNot(k) => !bool_of(values, k),
        Ctl::ToggleIn(k, item) => values.get(*k).and_then(Value::as_array).is_some_and(|a| a.iter().any(|v| v.as_str() == Some(item))),
        _ => false,
    }
}

/// The name a switch is known by while it glides (kit::Flips): its setting's, and for a column the
/// column's too. None for a part that isn't a switch.
fn flip_id(ctl: &Ctl, part: Part) -> Option<String> {
    match (ctl, part) {
        (Ctl::Toggle(k) | Ctl::ToggleNot(k), _) | (Ctl::ButtonToggle(_, _, k), Part::Second) => Some(k.to_string()),
        (Ctl::ToggleIn(k, item), _) => Some(format!("{k}/{item}")),
        _ => None,
    }
}

/// Whether a row can be used now.
fn usable(values: &Map<String, Value>, row: &Row, portable_busy: bool) -> bool {
    let needs = row.needs.is_none_or(|n| match n.strip_prefix('!') {
        Some(k) => !bool_of(values, k),
        None => bool_of(values, n),
    });
    let portable = row.key != "@Portable" || !(settings::test_folders() || portable_busy);
    row.live() && needs && portable
}

/// What a start folder typed or picked is saved as: no spaces or quotes round it; None if empty.
fn clean_folder(text: &str) -> Value {
    let t = text.trim().trim_matches('"').trim();
    if t.is_empty() {
        Value::Null
    } else {
        Value::String(t.to_string())
    }
}

/// Registers the window class (once; registering again just fails harmlessly).
fn register_class() {
    // SAFETY: plain class registration with a static name and our own window procedure.
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("GlintSettingsWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

/// Makes the Settings window (hidden: show it with [`Job::Show`] once its state is kept), owned by
/// `owner`, where `opens` says: floating where it was left (`place`, screen pixels; at first in the
/// middle of the main window), glued to its side, or over its right edge. None if Windows can't
/// make it.
pub fn create(owner: HWND, theme: &Theme, place: Option<[i32; 4]>, opens: SettingsOpens, list: bool, page: usize) -> Option<SettingsWindow> {
    OPENS.with(|o| o.set(opens));
    LIST.with(|l| l.set(list));
    register_class();
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: window creation and plain queries on our own windows; all structs are valid.
    unsafe {
        let dpi = GetDpiForWindow(owner).max(96);
        let scale = dpi as f32 / 96.0;
        let (x, y, w, h) = match (opens, place) {
            (SettingsOpens::Floating, Some([x, y, w, h])) if on_a_screen([x, y, w, h]) => (x, y, w, h),
            // None yet, or its screen is gone since (a second monitor unplugged): over the main window.
            (opens, _) => first_place(owner, opens, scale),
        };
        let instance = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GlintSettingsWindow"),
            PCWSTR(title.as_ptr()),
            WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        let dpi = GetDpiForWindow(hwnd).max(96);
        let renderer = match Renderer::new(hwnd, dpi) {
            Ok(r) => r,
            Err(e) => {
                log::error(&format!("The Settings window couldn't draw: {e}"));
                let _ = DestroyWindow(hwnd);
                return None;
            }
        };
        frame::init(hwnd);
        // WM_NCCALCSIZE again now that it's handled: the caption goes, as in the main window.
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        let folder_box = TextBox::new(hwnd, FOLDER_BOX);
        folder_box.set_hint("Type a folder, or use BROWSE");
        let search_box = TextBox::new(hwnd, SEARCH_BOX);
        search_box.set_hint("Search settings");
        // Tab in a text box moves on to the next control (an EDIT outside a dialog ignores it).
        for b in [&folder_box, &search_box] {
            let _ = SetWindowSubclass(b.hwnd(), Some(tab_proc), 2, 0);
        }
        let mut window = SettingsWindow {
            hwnd,
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            renderer,
            dpi,
            size: (0.0, 0.0),
            classic: false,
            started: Instant::now(),
            page: page.min(PAGES.len() - 1),
            style: Style::of(&Map::new()),
            defaults: values_of(&Settings::default()),
            blocks: Vec::new(),
            rows: Vec::new(),
            scroller: Scroller::default(),
            layout_dirty: true,
            values: Map::new(),
            focus: Spot::Nav,
            cues: false,
            hover: None,
            pressed: None,
            drag: None,
            tracking: false,
            active: true,
            folder_box,
            box_at: None,
            box_text: String::new(),
            search_box,
            search_at: None,
            query: String::new(),
            show_more: false,
            reset_asked_at: None,
            bug_at: None,
            reset_at: None,
            swatches: Vec::new(),
            swatches_generation: u64::MAX,
            menu_for: None,
            terminals: None,
            zip_said: None,
            point_at: None,
            cache_size: None,
            cache_said: None,
            clear_asked_at: None,
            clearing: false,
            portable_said: None,
            portable_busy: false,
            repaint_until: 0.0,
            focus_box_soon: false,
            box_had_focus: None,
            act_on_char: None,
            skins: Vec::new(),
            skins_generation: 0,
            no_twin: false,
            opens,
            pills: Vec::new(),
            flips: kit::Flips::default(),
            bar: kit::Bar::default(),
            holding: kit::Holding::default(),
        };
        window.apply_theme(theme);
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_ok() {
            window.size = ((rc.right - rc.left) as f32 / window.scale(), (rc.bottom - rc.top) as f32 / window.scale());
        }
        window.look_things_up();
        Some(window)
    }
}

/// Runs a job for the window `hwnd` (no state is touched: the main window calls this outside its
/// guard, since menus and the folder picker run their own message loop). `show_menu` shows a
/// native menu at client DIPs of a window and returns the picked id.
pub fn run_job(hwnd: HWND, scale: f32, job: Job) -> JobDone {
    // SAFETY: window operations on our own window, with no borrow of any state held.
    unsafe {
        match job {
            Job::Show => {
                frame::cloak(hwnd, true);
                let _ = ShowWindow(hwnd, SW_SHOW);
                // Flush against the main window now that Windows knows its borders.
                follow(hwnd, false);
                let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN);
                frame::cloak(hwnd, false);
                let _ = SetForegroundWindow(hwnd);
            }
            Job::Activate => {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            Job::Drop(mut list, at) => {
                let mut corners = [POINT { x: (at.x * scale).round() as i32, y: (at.y * scale).round() as i32 }, POINT { x: (at.right() * scale).round() as i32, y: (at.bottom() * scale).round() as i32 }];
                for p in &mut corners {
                    let _ = ClientToScreen(hwnd, p);
                }
                list.under = RECT { left: corners[0].x, top: corners[0].y, right: corners[1].x, bottom: corners[1].y };
                return JobDone::Picked(droplist::show(hwnd, &list));
            }
            Job::PickFolder(start) => return pick_folder(hwnd, &start).map_or(JobDone::Nothing, JobDone::Folder),
            Job::Follow => follow(hwnd, true),
            Job::Place(r) => {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            Job::Widen(w) => {
                let mut r = RECT::default();
                let w = (w * scale) as i32;
                if OPENS.with(Cell::get) == SettingsOpens::Floating && !IsZoomed(hwnd).as_bool() && GetWindowRect(hwnd, &mut r).is_ok() && r.right - r.left < w {
                    // Wider to the left when the screen ends on the right.
                    let work = work_area(hwnd, r);
                    let w = w.min(work.right - work.left);
                    let x = r.left.min(work.right - w).max(work.left);
                    let _ = SetWindowPos(hwnd, None, x, r.top, w, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
            Job::Close => {
                // The main window gets the focus back (not whatever Windows would pick).
                if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                    let _ = SetForegroundWindow(owner);
                }
                let _ = DestroyWindow(hwnd);
            }
        }
    }
    JobDone::Nothing
}

/// Windows' folder picker, owned by `owner`, starting at `start` when that's a folder on a local
/// disk (a network folder could keep it waiting). None if cancelled.
fn pick_folder(owner: HWND, start: &str) -> Option<String> {
    // SAFETY: COM is set up on this (the UI) thread (main.rs); every interface is released when
    // dropped and the returned string is freed with CoTaskMemFree.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        let _ = dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST);
        let _ = dialog.SetTitle(w!("Folder to open when Glint starts"));
        let start = start.trim();
        let drive: String = start.chars().take(3).collect();
        let local = drive.len() == 3 && drive.ends_with(":\\") && {
            let root: Vec<u16> = drive.encode_utf16().chain(std::iter::once(0)).collect();
            matches!(GetDriveTypeW(PCWSTR(root.as_ptr())), 2 | 3) // removable, fixed
        };
        if local {
            let path: Vec<u16> = start.encode_utf16().chain(std::iter::once(0)).collect();
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(path.as_ptr()), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(Some(owner)).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text.filter(|t| !t.is_empty())
    }
}

/// The text box's extra subclass: Tab and Shift+Tab move on (the window does it).
unsafe extern "system" fn tab_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == WM_KEYDOWN && wparam.0 == VK_TAB.0 as usize {
        // SAFETY: a key-state query and a post to our own parent window.
        unsafe {
            let shift = GetKeyState(VK_SHIFT.0 as i32) < 0;
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(Some(parent), WM_APP_TAB, WPARAM(shift as usize), LPARAM(0));
            }
        }
        return LRESULT(0);
    }
    if msg == WM_CHAR && wparam.0 == 9 {
        return LRESULT(0); // no beep
    }
    // SAFETY: everything else goes on to the next handler (textbox.rs's, then the EDIT's own).
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// The window procedure. Messages that need no state are answered here; the rest go to the state,
/// which the main window keeps, through its guard (window::settings_message).
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // The skin's own menus (oldmenu.rs).
    if let Some(r) = crate::oldmenu::handle(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        // SAFETY: lparam is the NCCALCSIZE_PARAMS Windows passed with this message.
        WM_NCCALCSIZE if wparam.0 != 0 => return unsafe { frame::nc_calc_size(hwnd, wparam, lparam) },
        WM_ERASEBKGND => return LRESULT(1),
        WM_CTLCOLOREDIT => return edit_colors(HDC(wparam.0 as *mut _)),
        WM_GETMINMAXINFO => {
            // SAFETY: lparam points to the MINMAXINFO Windows passed.
            unsafe {
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                info.ptMinTrackSize = POINT { x: (MIN_W * scale) as i32, y: (MIN_H * scale) as i32 };
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => return hit_test(hwnd, wparam, lparam),
        _ => {}
    }
    // SAFETY: plain query; the owner is the main window (set when this window was made).
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
    if let Some(r) = crate::window::settings_message(owner, hwnd, msg, wparam, lparam) {
        return r;
    }
    if msg == WM_PAINT {
        // The state is busy (a message inside another): paint on the next turn of the loop instead.
        // SAFETY: plain invalidate of our own window.
        unsafe {
            let _ = ValidateRect(Some(hwnd), None);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        return LRESULT(0);
    }
    if msg == WM_CLOSE {
        // Closing goes through the state (it must be dropped with the window); if the state is
        // busy, the close is simply not done now.
        return LRESULT(0);
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Where a window `w` x `h` (pixels) goes centred over `owner`, inside its screen's work area (the
/// size shrinks to fit the screen).
fn centred_on(owner: HWND, w: i32, h: i32) -> (i32, i32, i32, i32) {
    let mut main = RECT::default();
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: plain queries with valid out-structs.
    let work = unsafe {
        let _ = GetWindowRect(owner, &mut main);
        if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() { info.rcWork } else { main }
    };
    let w = w.min((work.right - work.left).max(200));
    let h = h.min((work.bottom - work.top).max(150));
    let x = ((main.left + main.right) / 2 - w / 2).clamp(work.left, (work.right - w).max(work.left));
    let y = ((main.top + main.bottom) / 2 - h / 2).clamp(work.top, (work.bottom - h).max(work.top));
    (x, y, w, h)
}

/// Whether a window at `x, y, w, h` (screen pixels) would be (at least partly) on a screen there is.
fn on_a_screen([x, y, w, h]: [i32; 4]) -> bool {
    let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
    // SAFETY: a plain monitor query with a valid in-struct.
    !unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// The screen's work area round `owner` (or `fallback`).
fn work_area(owner: HWND, fallback: RECT) -> RECT {
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a plain query with a valid out-struct.
    unsafe {
        if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            info.rcWork
        } else {
            fallback
        }
    }
}

/// Glued to the side: `w` wide beside `owner`, on its right if there's room, else on its left, else
/// at the screen's right edge (over Glint, when it fills the screen); as tall as the main window
/// (the visible parts, screen pixels).
fn beside(owner: HWND, w: i32) -> (i32, i32, i32, i32) {
    let main = visible_rect(owner);
    let work = work_area(owner, main);
    let w = w.min((work.right - work.left).max(200));
    let x = if main.right + w <= work.right {
        main.right
    } else if main.left - w >= work.left {
        main.left - w
    } else {
        (work.right - w).max(work.left)
    };
    let top = main.top.max(work.top);
    let bottom = main.bottom.min(work.bottom).max(top + 300);
    (x, top, w, bottom - top)
}

/// Over the main window's right edge, where the details panel goes: below its tabs and toolbar,
/// above its status bar, `w` wide.
fn in_panel(owner: HWND, scale: f32, w: i32) -> (i32, i32, i32, i32) {
    let main = visible_rect(owner);
    let top = main.top + ((size::TITLE_H + size::TOOLBAR_H) * scale) as i32;
    let bottom = (main.bottom - (size::STATUS_H * scale) as i32).max(top + 100);
    let w = w.min(((main.right - main.left) / 2).max(200));
    (main.right - w, top, w, bottom - top)
}

/// Where Settings first goes for `opens` (screen pixels): floating in the middle of the main window,
/// glued to its side, or in its details panel's place.
fn first_place(owner: HWND, opens: SettingsOpens, scale: f32) -> (i32, i32, i32, i32) {
    let px = |v: f32| (v * scale) as i32;
    match opens {
        SettingsOpens::Floating => centred_on(owner, px(if LIST.with(Cell::get) { FLOAT_LIST_W } else { FLOAT_W }), px(FLOAT_H)),
        SettingsOpens::Side => beside(owner, px(SIDE_W)),
        SettingsOpens::Panel => in_panel(owner, scale, px(PANEL_W)),
    }
}

/// Back in its place by the main window (it moved or changed size, or Settings did); `fresh`:
/// "Opens" just changed, so at that choice's first size and place too. Floating, it stays put.
fn follow(hwnd: HWND, fresh: bool) {
    // SAFETY: plain queries on our own windows.
    let owner = unsafe {
        let Ok(owner) = GetWindow(hwnd, GW_OWNER) else { return };
        if IsIconic(owner).as_bool() || IsZoomed(hwnd).as_bool() {
            return;
        }
        owner
    };
    // SAFETY: a plain query on our own window.
    let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;
    let now = visible_rect(hwnd);
    let w = now.right - now.left;
    let opens = OPENS.with(Cell::get);
    let to = match opens {
        _ if fresh => first_place(owner, opens, scale),
        SettingsOpens::Floating => return,
        SettingsOpens::Side => beside(owner, w),
        SettingsOpens::Panel => in_panel(owner, scale, w),
    };
    place_visible(hwnd, to);
}

/// WM_NCHITTEST: floating, the title bar moves the window (all but the ×); glued to the side its
/// edges still resize it; in the details panel's place it's part of the main window.
fn hit_test(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let opens = OPENS.with(Cell::get);
    if opens != SettingsOpens::Panel {
        if let Some(edge) = frame::resize_edge(hwnd, wparam, lparam) {
            return LRESULT(edge as isize);
        }
    }
    let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
    let mut rc = RECT::default();
    // SAFETY: plain queries on our own window with valid out-structs.
    let scale = unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
        let _ = GetClientRect(hwnd, &mut rc);
        GetDpiForWindow(hwnd).max(96) as f32 / 96.0
    };
    let (x, y, w) = (pt.x as f32 / scale, pt.y as f32 / scale, rc.right as f32 / scale);
    if opens == SettingsOpens::Floating && y < title_height() && x < w - ksize::CLOSE_W {
        return LRESULT(windows::Win32::UI::WindowsAndMessaging::HTCAPTION as isize);
    }
    LRESULT(HTCLIENT as isize)
}

/// WM_CTLCOLOREDIT: the text box in the theme's colours.
fn edit_colors(hdc: HDC) -> LRESULT {
    let (bg, fg, brush) = EDIT_COLORS.with(|c| c.get());
    // SAFETY: `hdc` is the control's DC for this paint; the brush lives until the theme changes.
    unsafe {
        SetTextColor(hdc, COLORREF(fg));
        SetBkColor(hdc, COLORREF(bg));
    }
    if brush == 0 {
        return LRESULT(COLOR_WINDOW.0 as isize + 1);
    }
    LRESULT(brush)
}

impl SettingsWindow {
    /// Back in its place by the main window (it moved or changed size, or Settings did).
    pub fn follow_owner(&mut self) {
        follow(self.hwnd, false);
        // Moved onto a screen of another scale: that WM_DPICHANGED came while busy too. The new
        // scale is taken here, at the size WM_DPICHANGED would give (the same to the eye), and
        // Settings goes back in its place at that size.
        // SAFETY: plain queries and a move of our own window; a valid out-struct.
        unsafe {
            let dpi = GetDpiForWindow(self.hwnd).max(96);
            let mut r = RECT::default();
            if dpi != self.dpi && GetWindowRect(self.hwnd, &mut r).is_ok() {
                let grow = |v: i32| (v as i64 * dpi as i64 / self.dpi as i64) as i32;
                let _ = SetWindowPos(self.hwnd, None, 0, 0, grow(r.right - r.left), grow(r.bottom - r.top), SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
                self.dpi = dpi;
                self.renderer.set_dpi(dpi);
                self.box_at = None;
                follow(self.hwnd, false);
            }
        }
        // The move's WM_SIZE comes while the app's state is busy and never reaches us: match the
        // drawing to the new size here, or the old picture shows stretched.
        let mut rc = RECT::default();
        // SAFETY: our own window and a valid out-struct.
        if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left && rc.bottom > rc.top {
            let (w, h) = ((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
            self.renderer.resize(w, h);
            self.size = self.inner_size(w, h);
            self.layout_dirty = true;
            self.invalidate();
        }
    }

    /// The pages are tabs along the top: chosen so, or too narrow for the list down the left.
    fn tabs(&self) -> bool {
        !self.style.list || self.size.0 < LIST_MIN_W
    }

    /// The title bar's height.
    fn title_h(&self) -> f32 {
        title_height()
    }

    /// The search box's frame (None when it's switched off).
    fn search_rect(&self) -> Option<Rect> {
        self.style.search.then(|| Rect::new(PAD_X, self.title_h() + SEARCH_PAD, (self.size.0 - 2.0 * PAD_X).max(40.0), SEARCH_H))
    }

    /// Where the search box's space ends (the title bar's bottom without one).
    fn search_bottom(&self) -> f32 {
        self.search_rect().map_or(self.title_h(), |r| r.bottom() + if self.tabs() { 2.0 } else { SEARCH_PAD })
    }

    /// The page list's width (none when the pages are tabs).
    fn nav_w(&self) -> f32 {
        if self.tabs() {
            0.0
        } else {
            NAV_W
        }
    }

    /// Where the tabs along the top end (the search box's space, without them).
    fn pills_bottom(&self) -> f32 {
        self.pills.iter().map(|p| p.bottom()).fold(self.search_bottom(), f32::max)
    }

    /// The line at the bottom.
    fn foot_rect(&self) -> Rect {
        Rect::new(0.0, (self.size.1 - FOOT_H).max(0.0), self.size.0, FOOT_H.min(self.size.1))
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// DIPs to pixels.
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// How wide 98 / 2000's window frame is round everything (0 for other skins or maximised).
    fn border(&self) -> f32 {
        if self.classic && !frame::zoomed(self.hwnd) {
            4.0
        } else {
            0.0
        }
    }

    /// The client area's size in DIPs, less the frame (from `w` x `h` pixels).
    fn inner_size(&self, w: u32, h: u32) -> (f32, f32) {
        let (s, b) = (self.scale(), self.border());
        ((w as f32 / s - 2.0 * b).max(0.0), (h as f32 / s - 2.0 * b).max(0.0))
    }

    fn poster(&self) -> Poster {
        Poster { hwnd: self.hwnd.0 as isize, serial: self.serial }
    }

    fn invalidate(&self) {
        // SAFETY: plain invalidate of our own window.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// The skin changed (or the window was just made): its colours and its font (at the standard
    /// size: the pages are laid out for it).
    pub fn apply_theme(&mut self, theme: &Theme) {
        FLAT.with(|f| f.set(flat::on(theme)));
        self.renderer.set_font(&theme.font, 1.0);
        self.renderer.set_title_font(&theme.title_font);
        self.layout_dirty = true;
        self.bar = kit::Bar::of(theme);
        // The text boxes are placed again, with the new font.
        self.box_at = None;
        self.search_at = None;
        if self.classic != theme.era.classic() {
            self.classic = theme.era.classic();
            let mut rc = RECT::default();
            // SAFETY: our own window and a valid out-struct.
            if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
                self.size = self.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
            }
        }
        let (bg, fg) = (theme.input.to_colorref(), theme.text.to_colorref());
        // SAFETY: a new brush replaces the old one, which is deleted (no control paints in between:
        // this is the UI thread).
        unsafe {
            let brush = CreateSolidBrush(COLORREF(bg));
            let old = EDIT_COLORS.with(|c| c.replace((bg, fg, brush.0 as isize))).2;
            if old != 0 {
                let _ = DeleteObject(HGDIOBJ(old as *mut _));
            }
            frame::apply_theme(self.hwnd, theme);
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    /// Something changed the settings from outside this window (Ctrl+H in the main window, say):
    /// show the new values.
    pub fn watch(&mut self, s: &Settings) {
        if values_of(s) != self.values {
            self.invalidate();
        }
    }

    /// Something it shows changed outside the settings (a check for updates started or ended):
    /// lay the page out again.
    pub fn refresh(&mut self) {
        self.layout_dirty = true;
        self.invalidate();
    }

    /// Opened again (Ctrl+, or SETTINGS while it's open), maybe for a particular page.
    /// Puts the keyboard on the row titled `title` (on the page shown), in view - once the page is
    /// laid out.
    pub fn point_at(&mut self, title: &str) {
        self.point_at = Some(title.to_string());
        self.layout_dirty = true;
        self.invalidate();
    }

    pub fn reopen(&mut self, page: Option<usize>) -> Vec<Ask> {
        if let Some(p) = page {
            self.go_to_page(p, false);
        }
        self.invalidate();
        vec![Ask::Job(Job::Activate)]
    }

    /// Asks for what's slow to find out: which terminals are installed, how big the cache is.
    fn look_things_up(&self) {
        let to = self.poster();
        shell::background("settings: terminals", move || {
            let mut found = [false; 6];
            for (i, (t, _)) in Terminal::ALL.iter().enumerate() {
                found[i] = *t == Terminal::Auto || shell::terminal_installed(*t);
            }
            post(to, Bg::Terminals(found));
        });
        self.look_up_zip();
        self.measure_cache();
    }

    /// Whether zip and 7z files open in Glint (the Zip icon row says so).
    fn look_up_zip(&self) {
        let to = self.poster();
        shell::background("settings: zip files", move || {
            let said = registry::this_exe().map(|exe| registry::zip_icon_said(&registry::Places::current(), &exe)).unwrap_or_default();
            post(to, Bg::ZipSaid(said));
        });
    }

    fn measure_cache(&self) {
        let to = self.poster();
        shell::background("settings: cache size", move || post(to, Bg::CacheSize(settings::cache_size())));
    }

    // ------------------------------------------------------------ layout

    /// The scrolling part, between the pages and the line at the bottom.
    fn content_rect(&self) -> Rect {
        let (x, y) = (self.nav_w(), self.pills_bottom());
        Rect::new(x, y, (self.size.0 - x).max(0.0), (self.foot_rect().y - y).max(0.0))
    }

    fn close_rect(&self) -> Rect {
        Rect::new(self.size.0 - ksize::CLOSE_W, 0.0, ksize::CLOSE_W, self.title_h())
    }

    fn nav_rect(&self, i: usize) -> Rect {
        if self.tabs() {
            return self.pills.get(i).copied().unwrap_or_default();
        }
        Rect::new(0.0, self.search_bottom() + NAV_TOP + i as f32 * NAV_ITEM_H, NAV_W, NAV_ITEM_H)
    }

    fn track(&self) -> Rect {
        self.bar.track(self.content_rect())
    }

    /// From page coordinates to the window's (DIPs).
    fn on_screen(&self, r: Rect) -> Rect {
        Rect::new(r.x, r.y + self.content_rect().y - self.scroller.offset, r.w, r.h)
    }

    /// The headings and rows of page `page`, with what the window knows now.
    fn page_items(&self, page: usize) -> Vec<Item> {
        let facts = Facts {
            values: &self.values,
            terminals: self.terminals,
            zip_said: self.zip_said.as_deref(),
            config: settings::config_now().display().to_string(),
            logs: settings::folders().logs.display().to_string(),
            cache: settings::folders().cache.display().to_string(),
            cache_size: self.cache_size,
            cache_said: self.cache_said.clone(),
            portable_said: self.portable_said.clone(),
            portable_locked: settings::test_folders(),
            skins: &self.skins,
            update_said: update::about(life::is_main(), self.values.get("CheckForUpdates").and_then(Value::as_bool).unwrap_or(true)).0,
            update_busy: !update::about(life::is_main(), true).1 && life::is_main(),
        };
        pages::page(page, &facts)
    }

    /// What shows: the page's groups (heading, rows) - or, while searching, what was found on every
    /// page, under each page's name.
    fn groups(&self, query: &str) -> Vec<(String, Vec<Row>)> {
        let mut groups = Vec::new();
        let pages: Vec<usize> = if query.is_empty() { vec![self.page] } else { (0..PAGES.len()).collect() };
        for page in pages {
            let mut heading = "";
            let mut found = Vec::new();
            for item in self.page_items(page) {
                match item {
                    Item::Heading(h) => heading = h,
                    Item::Card(rows) if query.is_empty() => groups.push((heading.to_string(), rows)),
                    // A row that is only round buttons takes its group's title, which asks the question.
                    Item::Card(rows) => found.extend(rows.into_iter().filter(|r| row_matches(r, heading, query)).map(|mut r| {
                        if r.title.is_empty() && r.ctl != Ctl::Folder {
                            r.title = heading.to_string();
                        }
                        r
                    })),
                }
            }
            if !found.is_empty() {
                groups.push((PAGES[page].to_string(), found));
            }
        }
        groups
    }

    /// Lays the page out: where each group, row and control goes, with the text wrapped.
    fn layout(&mut self) {
        self.layout_dirty = false;
        self.style = Style::of(&self.values);
        // The pages as tabs along the top, as many to a line as fit.
        self.pills.clear();
        if self.tabs() && flat_on() {
            // The mockup's: names along the left, a little room round each.
            let y = self.search_bottom() + 2.0;
            let mut x = 8.0;
            for name in PAGES {
                let w = flat::tab_width(&mut self.renderer, name);
                if x > 8.0 && x + w > self.size.0 - 8.0 {
                    break;
                }
                self.pills.push(Rect::new(x, y, w, flat::TAB_H));
                x += w + flat::TAB_GAP;
            }
            // (A name that doesn't fit goes on a second line.)
            let mut x = 8.0;
            let y2 = y + flat::TAB_H;
            for name in &PAGES[self.pills.len()..] {
                let w = flat::tab_width(&mut self.renderer, name);
                self.pills.push(Rect::new(x, y2, w, flat::TAB_H));
                x += w + flat::TAB_GAP;
            }
        } else if self.tabs() {
            let (mut x, mut y) = (PILL_PAD, self.search_bottom() + 2.0);
            let style = TextStyle::new(Color::hex(0)).size(ksize::NAV_FONT).semibold();
            let texts: Vec<f32> = PAGES.iter().map(|name| self.renderer.measure(name, &style).ceil()).collect();
            // Less room around each name before one goes to a second line (a skin with bigger text).
            let room = self.size.0 - 2.0 * PILL_PAD - texts.iter().sum::<f32>();
            let around = (room / PAGES.len() as f32).clamp(16.0, 36.0);
            // On one line they sit in the middle, with the same space left and right.
            let left = room - around * PAGES.len() as f32;
            if left >= 0.0 {
                x += left / 2.0;
            }
            for text in texts {
                let w = text + around;
                if x > PILL_PAD && x + w > self.size.0 - PILL_PAD + 0.5 {
                    (x, y) = (PILL_PAD, y + TAB_H);
                }
                self.pills.push(Rect::new(x, y, w, TAB_H));
                x += w;
            }
        }
        let content = self.content_rect();
        let card_w = (content.w - 2.0 * PAD_X - GUTTER).clamp(160.0, MAX_CARD_W);
        let x0 = content.x + PAD_X;
        let query = self.query.trim().to_lowercase();
        let groups = self.groups(&query);
        let style = self.style;
        let boxes = !style.sentences;
        // While only the basics show, the rest wait behind "More settings".
        let basic = style.basic && query.is_empty();
        let mut more = 0;
        let mut blocks = Vec::new();
        let mut rows = Vec::new();
        // (A group's title sits on its box's top line, half above it.)
        let mut y = PAD_TOP + if boxes { HEADING_H / 2.0 } else { 0.0 };
        for (title, group) in groups {
            let shown: Vec<Row> = group
                .into_iter()
                .filter(|row| row.only_if.is_none_or(|(k, v)| self.values.get(k) == Some(&v.json())))
                .filter(|row| {
                    let hidden = basic && row.more;
                    more += hidden as usize;
                    !hidden || self.show_more
                })
                .collect();
            if shown.is_empty() {
                continue;
            }
            let top = y;
            // A group without a title (the first rows of General) has no box either.
            let boxed = boxes && !title.is_empty();
            if boxes && !boxed {
                y -= HEADING_H / 2.0;
            }
            let (left, w) = if boxed {
                y += GROUP_PAD + 2.0;
                (x0 + GROUP_PAD, card_w - 2.0 * GROUP_PAD)
            } else if boxes {
                (x0, card_w)
            } else if title.is_empty() {
                (x0, card_w)
            } else {
                blocks.push(Block::Heading(Rect::new(x0, y, card_w, HEADING_H), title.clone()));
                y += HEADING_H + 4.0;
                (x0, card_w)
            };
            let r = &mut self.renderer;
            if shown.iter().all(|row| matches!(row.ctl, Ctl::Text(_))) {
                y = place_keys(r, shown, left, y, w, &mut rows);
            } else {
                // A group's drop-downs are all as wide as its widest one, so they line up.
                let drop_w = shown
                    .iter()
                    .filter(|row| shape_of(row, style) == Shape::Plain)
                    .filter_map(|row| match &row.ctl {
                        Ctl::Choice(_, choices) => Some(drop_width(r, choices.iter().map(|(_, l)| l.as_str()))),
                        _ => None,
                    })
                    .fold(0.0, f32::max);
                for row in shown {
                    let enabled = usable(&self.values, &row, self.portable_busy) && !(row.key == "Theme" && self.no_twin);
                    let shape = shape_of(&row, style);
                    let placed = place_row(r, row, shape, left, y, w, drop_w, enabled);
                    y += placed.rect.h;
                    rows.push(placed);
                }
            }
            if boxed {
                y += GROUP_PAD - 4.0;
                blocks.push(Block::Group(Rect::new(x0, top, card_w, y - top), title));
                y += GROUP_GAP + HEADING_H / 2.0;
            } else if boxes {
                y += GROUP_GAP - 4.0 + HEADING_H / 2.0;
            } else {
                y += GROUP_GAP;
            }
        }
        if !query.is_empty() && rows.is_empty() {
            blocks.push(Block::Note(Rect::new(x0, y, card_w, TITLE_LINE), format!("Nothing found for \u{201c}{}\u{201d}", self.query.trim())));
            y += TITLE_LINE + GROUP_GAP;
        }
        if basic && more > 0 {
            let text = if self.show_more { "Fewer settings".to_string() } else { format!("More settings ({more})") };
            let w = self.renderer.measure(&text, &TextStyle::new(Color::hex(0)).size(TITLE_FONT).semibold()).ceil() + 8.0;
            blocks.push(Block::More(Rect::new(x0 - 4.0, y - 4.0, w, ROW_H), text));
            y += ROW_H;
        }
        self.blocks = blocks;
        self.rows = rows;
        self.scroller.set_extent(y + PAD_BOTTOM, content.h);
        self.layout_foot();
        // The keyboard can't stay on something that's gone or greyed now.
        let gone = match self.focus {
            Spot::Ctl(i, part) => !self.rows.get(i).is_some_and(|p| p.enabled && p.rect_of(part).is_some()),
            Spot::More => !self.blocks.iter().any(|b| matches!(b, Block::More(..))),
            Spot::Reset => self.reset_at.is_none(),
            Spot::Search => !style.search,
            Spot::Nav => false,
        };
        if gone {
            self.focus = Spot::Nav;
        }
        // A setting found with the command box: the keyboard goes to it, and it scrolls into view.
        if content.h > 0.0 {
            if let Some(title) = self.point_at.take() {
                if let Some(i) = self.rows.iter().position(|p| p.row.title.trim() == title) {
                    let p = &self.rows[i];
                    let part = if p.row.ctl == Ctl::Folder || p.rect_of(Part::Main).is_none() { Part::Second } else { Part::Main };
                    let part = if p.shape == Shape::Tick { p.tick_part() } else { part };
                    if p.enabled && p.rect_of(part).is_some() {
                        self.set_focus(Spot::Ctl(i, part));
                    } else {
                        self.scroller.reveal(p.rect.y, p.rect.bottom(), 16.0, self.now());
                    }
                }
            }
        }
    }

    /// The line at the bottom: Report a bug after the version, RESET PAGE on the right (not while
    /// searching: there's no one page then).
    fn layout_foot(&mut self) {
        let foot = self.foot_rect();
        let now = self.now();
        let asked = self.reset_asked_at.is_some_and(|t| now - t < CLEAR_CONFIRM_SECONDS);
        self.reset_at = (self.style.reset && self.query.trim().is_empty()).then(|| {
            let w = button_w(&mut self.renderer, if asked { "PRESS AGAIN" } else { "RESET PAGE" });
            Rect::new(self.size.0 - PAD_X - w, foot.y + (FOOT_H - CTL_H) / 2.0, w, CTL_H)
        });
        let plain = TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT);
        let said = self.renderer.measure(LIVE_NOTE, &plain).ceil();
        let bug = self.renderer.measure(REPORT_BUG, &plain.semibold()).ceil() + 4.0;
        let x = PAD_X + said + 12.0;
        let end = self.reset_at.map_or(self.size.0 - PAD_X, |r| r.x - 10.0);
        self.bug_at = (x + bug <= end).then(|| Rect::new(x - 2.0, foot.y + (FOOT_H - 20.0) / 2.0, bug, 20.0));
    }

    // ------------------------------------------------------------ painting

    fn paint(&mut self, app: &AppState) {
        let fresh = values_of(&app.settings);
        if fresh != self.values {
            self.values = fresh;
            self.layout_dirty = true;
        }
        if app.skins.generation != self.skins_generation {
            self.skins_generation = app.skins.generation;
            self.skins = app.skins.entries().iter().map(|e| (e.id.clone(), e.name.clone(), e.description.clone())).collect();
            self.layout_dirty = true;
        }
        // The skin tiles' colours: each skin read once (again when the skins change).
        if self.style.tiles && self.swatches_generation != app.skins.generation {
            self.swatches_generation = app.skins.generation;
            self.swatches = app
                .skins
                .entries()
                .iter()
                .map(|e| {
                    let t = crate::skin::parse(&e.text, &e.name).theme;
                    (e.id.clone(), [t.window, t.panel, t.accent, t.text])
                })
                .collect();
        }
        let skin = self.values.get("Skin").and_then(Value::as_str).unwrap_or_default();
        let dark = self.values.get("Theme").and_then(Value::as_str) != Some("light");
        let no_twin = app.skins.twin(skin, !dark) == app.skins.twin(skin, dark);
        if no_twin != self.no_twin {
            self.no_twin = no_twin;
            self.layout_dirty = true;
        }
        // RESET PAGE goes back to its own words once the time to press it again is up.
        if self.reset_asked_at.is_some_and(|t| self.now() - t >= CLEAR_CONFIRM_SECONDS) {
            self.reset_asked_at = None;
            self.layout_dirty = true;
        }
        if self.layout_dirty {
            self.layout();
        }
        let now = self.now();
        let mut more = self.scroller.tick(now) | self.flips.tick(now) || now < self.repaint_until || self.reset_asked_at.is_some();
        let theme = &app.theme;
        if !self.renderer.begin() {
            // No surface to draw on right now (a graphics driver being reset): try again soon.
            // SAFETY: a timer on our own window.
            unsafe {
                SetTimer(Some(self.hwnd), RETRY_TIMER, 500, None);
            }
            return;
        }
        self.renderer.clear(theme.window);
        // 98 / 2000: everything inside the window's frame.
        let b = self.border();
        self.renderer.set_origin(b, b);
        // The search box, the pages and the line at the bottom on the sidebar's colour (a gradient
        // running down the whole window).
        let top = self.title_h();
        let below = Rect::new(0.0, top, self.size.0, (self.size.1 - top).max(0.0));
        if flat_on() {
            // One colour all over, as the mockup.
            self.renderer.fill_rect(below, theme.window);
        } else {
            self.renderer.fill_paint(below, 0.0, &theme.sidebar_paint);
        }
        if !self.tabs() {
            let list_top = self.search_bottom();
            let list = Rect::new(0.0, list_top, NAV_W, (self.foot_rect().y - list_top).max(0.0));
            kit::page_list_well(&mut self.renderer, theme, list);
        }
        self.paint_caption(theme);
        self.paint_search(theme);
        self.paint_nav(theme);
        self.paint_page(theme);
        self.paint_foot(theme);
        if b > 0.0 {
            self.renderer.set_origin(0.0, 0.0);
            frame::paint_classic_border(&mut self.renderer, theme, Rect::new(0.0, 0.0, self.size.0 + 2.0 * b, self.size.1 + 2.0 * b));
        }
        if self.renderer.end() == FrameResult::DeviceLost {
            more = true;
        }
        self.place_box();
        self.place_search();
        if more {
            self.invalidate();
        }
    }

    fn paint_caption(&mut self, theme: &Theme) {
        let close = self.close_rect();
        let r = &mut self.renderer;
        let bar = Rect::new(0.0, 0.0, self.size.0, title_height());
        if flat_on() {
            flat::title(r, theme, bar, self.active);
        } else {
            kit::title_bar(r, theme, bar, self.active);
            let x = kit::logo(r, theme, bar);
            crate::retro::title(r, theme, Rect::new(x + 10.0, 0.0, (self.size.0 - x - 10.0 - ksize::CLOSE_W).max(0.0), size::TITLE_H), "Settings", self.active);
        }
        let hot = self.hover == Some(Hot::Close);
        kit::close_button(r, theme, close, hot, hot && self.pressed == Some(Hot::Close), self.active);
    }

    /// The search box's frame (the real text box sits in it, `place_search`).
    fn paint_search(&mut self, theme: &Theme) {
        let Some(rect) = self.search_rect() else { return };
        let shown = self.search_at.is_some();
        let look = Look { focused: false, ..Look::enabled() };
        kit::field(&mut self.renderer, theme, rect, (!shown).then_some(self.query.as_str()), shown && self.search_box.has_focus(), look);
    }

    fn paint_nav(&mut self, theme: &Theme) {
        if self.tabs() && flat_on() {
            let y = self.pills_bottom();
            flat::tab_line(&mut self.renderer, theme, y, self.size.0);
        }
        let searching = !self.query.trim().is_empty();
        for (i, name) in PAGES.iter().enumerate() {
            let rect = self.nav_rect(i);
            let look = Look { hover: self.hover == Some(Hot::Nav(i)), focused: self.cues && self.focus == Spot::Nav && i == self.page, ..Look::enabled() };
            let draw = match (self.tabs(), flat_on()) {
                (true, true) => flat::tab,
                (true, false) => kit::nav_tab,
                (false, _) => kit::nav_item,
            };
            draw(&mut self.renderer, theme, rect, name, i == self.page && !searching, look);
        }
    }

    fn paint_page(&mut self, theme: &Theme) {
        let content = self.content_rect();
        if !flat_on() {
            self.renderer.fill_paint(content, 0.0, &theme.toolbar_paint);
        }
        self.renderer.push_clip(content);
        let dy = content.y - self.scroller.offset;
        let shift = |r: Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
        for block in &self.blocks {
            let r = &mut self.renderer;
            match block {
                Block::Group(rect, text) if flat_on() => flat::group(r, theme, shift(*rect), text, theme.window),
                Block::Group(rect, text) => kit::group(r, theme, shift(*rect), text),
                Block::Heading(rect, text) => kit::heading(r, theme, shift(*rect), text),
                Block::Note(rect, text) => r.text(text, shift(*rect), &TextStyle::new(theme.muted).size(TITLE_FONT)),
                Block::More(rect, text) => {
                    let rect = shift(*rect);
                    let hot = self.hover == Some(Hot::More);
                    let color = if hot { theme.text } else { theme.accent };
                    r.text(text, Rect::new(rect.x + 4.0, rect.y, rect.w - 4.0, rect.h), &TextStyle::new(color).size(TITLE_FONT).semibold());
                    if self.cues && self.focus == Spot::More {
                        kit::focus_ring(r, theme, rect, 0.0);
                    }
                }
            }
        }
        for i in 0..self.rows.len() {
            let rect = shift(self.rows[i].rect);
            if rect.bottom() < content.y || rect.y > content.bottom() {
                continue;
            }
            self.paint_row(theme, i, dy);
        }
        self.renderer.pop_clip();
        let hover = matches!(self.hover, Some(Hot::Thumb)) || self.drag.is_some();
        let track = self.track();
        kit::scrollbar(&mut self.renderer, theme, track, &self.scroller, hover, self.holding.hold_of(0));
    }

    /// The line at the bottom: the version, Report a bug, Discord (coming soon) where there's room,
    /// and RESET PAGE.
    fn paint_foot(&mut self, theme: &Theme) {
        let foot = self.foot_rect();
        let r = &mut self.renderer;
        if flat_on() {
            r.fill_rect(Rect::new(0.0, foot.y, self.size.0, 1.0), theme.line);
        } else {
            kit::separator(r, theme, 0.0, foot.y, self.size.0);
        }
        let plain = TextStyle::new(theme.muted).size(ksize::NOTE_FONT);
        let reset_x = self.reset_at.map_or(self.size.0 - PAD_X, |r| r.x - 10.0);
        // "Changes apply right away" goes first when there's no room for both.
        if self.bug_at.is_some() || r.measure(LIVE_NOTE, &plain).ceil() <= reset_x - PAD_X {
            r.text(LIVE_NOTE, Rect::new(PAD_X, foot.y, (reset_x - PAD_X).max(0.0), foot.h), &plain);
        }
        if let Some(bug) = self.bug_at {
            let hot = self.hover == Some(Hot::ReportBug);
            r.text(REPORT_BUG, Rect::new(bug.x + 2.0, foot.y, bug.w, foot.h), &TextStyle::new(if hot { theme.text } else { theme.accent }).size(ksize::NOTE_FONT).semibold());
        }
        if let Some(rect) = self.reset_at {
            let asked = self.reset_asked_at.is_some();
            let me = Hot::Reset;
            let look = Look { hover: self.hover == Some(me), pressed: self.pressed == Some(me) && self.hover == Some(me), focused: self.cues && self.focus == Spot::Reset, enabled: true };
            draw_button(r, theme, rect, if asked { "PRESS AGAIN" } else { "RESET PAGE" }, look);
        }
    }

    fn look_of(&self, i: usize, part: Part) -> Look {
        let me = Hot::Ctl(i, part);
        Look {
            hover: self.hover == Some(me),
            pressed: self.pressed == Some(me) && self.hover == Some(me),
            focused: self.cues && self.focus == Spot::Ctl(i, part),
            enabled: self.rows[i].enabled,
        }
    }

    /// Whether the row's setting isn't at its default (it gets a dot). Not for buttons, the
    /// shortcuts, what Windows keeps ("@" names), the skin, or light or dark.
    fn changed(&self, row: &Row) -> bool {
        // Picking a skin, or light or dark, is choosing a look, not changing a setting away from
        // how it should be: no dot.
        if matches!(row.key, "Skin" | "Theme") {
            return false;
        }
        let listed = |values: &Map<String, Value>, k: &str, item: &str| values.get(k).and_then(Value::as_array).is_some_and(|a| a.iter().any(|v| v.as_str() == Some(item)));
        match &row.ctl {
            Ctl::ToggleIn(k, item) => listed(&self.values, k, item) != listed(&self.defaults, k, item),
            Ctl::Toggle(k) | Ctl::ToggleNot(k) | Ctl::ButtonToggle(_, _, k) | Ctl::Choice(k, _) if !k.starts_with('@') => self.values.get(*k) != self.defaults.get(*k),
            _ => false,
        }
    }

    fn paint_row(&mut self, theme: &Theme, i: usize, dy: f32) {
        let dot = self.style.dots && self.changed(&self.rows[i].row);
        let looks: Vec<Look> = self.rows[i].parts().into_iter().map(|part| self.look_of(i, part)).collect();
        let look_at = |part: Part, parts: &[Part]| parts.iter().position(|p| *p == part).map_or(Look::enabled(), |n| looks[n]);
        let p = &self.rows[i];
        let parts = p.parts();
        let shift = |r: Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
        let rect = shift(p.rect);
        let live = p.row.live();
        let title_color = if p.enabled { theme.text } else { theme.muted };
        let r = &mut self.renderer;
        if dot {
            r.fill_ellipse(Point::new(p.dot.x, p.dot.y + dy), DOT / 2.0, DOT / 2.0, theme.accent);
        }
        if p.shape == Shape::Keys {
            // A shortcut: the keys, then what they do (greyed while it doesn't work yet).
            let (keys, what) = if live { (theme.text, theme.muted) } else { (theme.muted.with_alpha(0.7), theme.muted.with_alpha(0.7)) };
            r.text(&p.row.title, Rect::new(p.text_x, rect.y, KEYS_W - 8.0, rect.h), &TextStyle::new(keys).size(TITLE_FONT).semibold());
            let what_text = if let Ctl::Text(w) = &p.row.ctl { *w } else { "" };
            r.text(what_text, Rect::new(p.text_x + KEYS_W, rect.y, (p.text_w - KEYS_W).max(0.0), rect.h), &TextStyle::new(what).size(TITLE_FONT));
            return;
        }
        let mut y = rect.y + p.text_y;
        for line in &p.title {
            r.text(line, Rect::new(p.text_x, y, p.text_w, TITLE_LINE), &TextStyle::new(title_color).size(TITLE_FONT));
            y += TITLE_LINE;
        }
        let note_color = if p.enabled || live { theme.muted } else { theme.muted.with_alpha(0.7) };
        for line in &p.note {
            r.text(line, Rect::new(p.text_x, y, p.text_w, NOTE_LINE), &TextStyle::new(note_color).size(ksize::NOTE_FONT));
            y += NOTE_LINE;
        }
        if !live {
            r.text(LATER, Rect::new(p.text_x, y, p.text_w, NOTE_LINE), &TextStyle::new(theme.muted).size(ksize::NOTE_FONT).semibold());
        }
        let main = p.main.map(shift);
        let second = p.second.map(shift);
        let now = self.started.elapsed().as_secs_f64();
        // Where a tick is drawn: at its value, or on its way there just after a flip.
        let flips = &mut self.flips;
        let mut tick = |part: Part| {
            let on = toggle_on(&p.row.ctl, &self.values);
            flip_id(&p.row.ctl, part).map_or(if on { 1.0 } else { 0.0 }, |id| flips.at(&id, on, now))
        };
        match (&p.row.ctl, p.shape) {
            (Ctl::Toggle(_) | Ctl::ToggleNot(_) | Ctl::ToggleIn(..), _) => {
                if let Some(m) = main {
                    draw_check(r, theme, m, tick(Part::Main), look_at(Part::Main, &parts));
                }
            }
            (Ctl::ButtonToggle(label, _, _), _) => {
                if let Some(m) = main {
                    draw_button(r, theme, m, label, look_at(Part::Main, &parts));
                }
                if let Some(s) = second {
                    draw_check(r, theme, s, tick(Part::Second), look_at(Part::Second, &parts));
                }
            }
            (Ctl::Choice(key, choices), Shape::Radios) => {
                let current = self.values.get(*key);
                for (n, (v, label)) in choices.iter().enumerate() {
                    let Some(o) = p.options.get(n).map(|o| shift(*o)) else { break };
                    let look = look_at(Part::Option(n), &parts);
                    draw_radio(r, theme, o, if Some(&v.json()) == current { 1.0 } else { 0.0 }, look);
                    let x = o.right() + CHECK_GAP;
                    let color = if look.enabled { theme.text } else { theme.muted };
                    r.text(label, Rect::new(x, o.y + (CHECK - OPTION_H) / 2.0, (rect.right() - x).max(0.0), OPTION_H), &TextStyle::new(color).size(TITLE_FONT));
                }
            }
            (Ctl::Choice(key, choices), Shape::Tiles) => {
                let current = self.values.get(*key);
                for (n, (v, label)) in choices.iter().enumerate() {
                    let Some(o) = p.options.get(n).map(|o| shift(*o)) else { break };
                    let colours = match v {
                        pages::Val::S(id) => self.swatches.iter().find(|(s, _)| s == id).map(|(_, c)| *c),
                        pages::Val::B(_) => None,
                    };
                    skin_tile(r, theme, o, label, colours, Some(&v.json()) == current, look_at(Part::Option(n), &parts));
                }
            }
            (Ctl::Choice(key, choices), _) => {
                if let Some(m) = main {
                    let current = self.values.get(*key);
                    let label = choices.iter().find(|(v, _)| Some(&v.json()) == current).map(|(_, l)| l.clone()).unwrap_or_else(|| match current {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    });
                    draw_dropdown(r, theme, m, &label, look_at(Part::Main, &parts));
                }
            }
            (Ctl::Button(label, _), _) => {
                if let Some(m) = main {
                    draw_button(r, theme, m, label, look_at(Part::Main, &parts));
                }
            }
            (Ctl::Folder, _) => {
                if let Some(m) = main {
                    // The real text box sits on top while the whole of it is in view; otherwise the
                    // frame shows the text itself.
                    let shown = self.box_at.is_some();
                    let text = self.values.get("StartupFolder").and_then(Value::as_str).unwrap_or("");
                    kit::field(r, theme, m, (!shown).then_some(text), shown && self.folder_box.has_focus(), look_at(Part::Main, &parts));
                }
                if let Some(s) = second {
                    draw_button(r, theme, s, "BROWSE…", look_at(Part::Second, &parts));
                }
            }
            (Ctl::Text(_) | Ctl::None, _) => {}
        }
    }

    /// Puts the start folder's text box over its frame - when the frame is wholly in view - or
    /// hides it.
    fn place_box(&mut self) {
        let content = self.content_rect();
        let want = self.rows.iter().find(|p| p.row.ctl == Ctl::Folder).and_then(|p| p.main).map(|m| self.on_screen(m)).filter(|m| m.y >= content.y && m.bottom() <= content.bottom());
        // The box shows the saved folder - unless someone is typing in it.
        let text = self.values.get("StartupFolder").and_then(Value::as_str).unwrap_or("").to_string();
        if want.is_some() && !self.folder_box.has_focus() && text != self.box_text {
            self.folder_box.set_text(&text);
            self.box_text = text;
        }
        if want == self.box_at {
            return;
        }
        match want {
            Some(rect) => {
                let b = self.border();
                self.folder_box.place(Rect::new(rect.x + b, rect.y + b, rect.w, rect.h).inset(1.0, 1.0), 7.0, self.scale());
                if self.box_at.is_none() {
                    self.folder_box.show();
                }
            }
            None => {
                if self.folder_box.has_focus() {
                    self.folder_box.give_focus_back();
                }
                self.folder_box.hide();
                self.wine_repaint();
            }
        }
        self.box_at = want;
        if self.focus_box_soon && self.box_at.is_some() {
            self.focus_box_soon = false;
            self.folder_box.focus();
            self.folder_box.select_all();
        }
    }

    /// Puts the search box in its frame, or hides it when it's switched off.
    fn place_search(&mut self) {
        let want = self.search_rect();
        if want == self.search_at {
            return;
        }
        match want {
            Some(rect) => {
                let b = self.border();
                self.search_box.place(Rect::new(rect.x + b, rect.y + b, rect.w, rect.h).inset(1.0, 1.0), 7.0, self.scale());
                if self.search_at.is_none() {
                    self.search_box.show();
                }
            }
            None => {
                if self.search_box.has_focus() {
                    self.search_box.give_focus_back();
                }
                self.search_box.hide();
                self.wine_repaint();
            }
        }
        self.search_at = want;
    }

    /// Under Wine a hidden text box stays on screen until a later frame replaces it.
    fn wine_repaint(&mut self) {
        if frame::is_wine() {
            self.repaint_until = self.now() + 0.3;
            self.invalidate();
        }
    }

    // ------------------------------------------------------------ the mouse

    fn target_at(&self, p: Point) -> Option<Hot> {
        if p.y < self.title_h() {
            return self.close_rect().contains(p).then_some(Hot::Close);
        }
        if let Some(i) = (0..PAGES.len()).find(|&i| self.nav_rect(i).contains(p)) {
            return Some(Hot::Nav(i));
        }
        if p.y >= self.foot_rect().y {
            if self.reset_at.is_some_and(|r| kit::button_contains(r, p)) {
                return Some(Hot::Reset);
            }
            return self.bug_at.is_some_and(|r| r.contains(p)).then_some(Hot::ReportBug);
        }
        if p.x < self.nav_w() || p.y < self.content_rect().y {
            return None;
        }
        if let Some(thumb) = self.scroller.thumb(self.track()) {
            if thumb.inset(-4.0, 0.0).contains(p) {
                return Some(Hot::Thumb);
            }
        }
        let content = self.content_rect();
        if !content.contains(p) {
            return None;
        }
        if self.blocks.iter().any(|b| matches!(b, Block::More(r, _) if self.on_screen(*r).contains(p))) {
            return Some(Hot::More);
        }
        for (i, placed) in self.rows.iter().enumerate() {
            for part in placed.parts() {
                let Some(r) = placed.hit_of(part).map(|r| self.on_screen(r)) else { continue };
                let hit = match (&placed.row.ctl, part) {
                    (Ctl::Button(..) | Ctl::ButtonToggle(..), Part::Main) | (Ctl::Folder, Part::Second) => kit::button_contains(r, p),
                    _ => r.contains(p),
                };
                if hit {
                    return Some(Hot::Ctl(i, part));
                }
            }
        }
        None
    }

    fn client_point(&self, lparam: LPARAM) -> Point {
        let (s, b) = (self.scale(), self.border());
        Point::new((lparam.0 & 0xffff) as i16 as f32 / s - b, ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s - b)
    }

    fn mouse_move(&mut self, p: Point) {
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            // SAFETY: a valid struct for our own window.
            self.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        if self.holding.active() {
            self.holding.at = p;
            return;
        }
        if let Some(grab) = self.drag {
            let track = self.track();
            if let Some(thumb) = self.scroller.thumb(track) {
                let to = self.scroller.offset_for_thumb(track, thumb.h, p.y - grab);
                self.scroller.jump_to(to);
                self.invalidate();
            }
            return;
        }
        let hover = self.target_at(p);
        if hover != self.hover {
            self.hover = hover;
            self.invalidate();
        }
    }

    fn mouse_down(&mut self, p: Point) -> Vec<Ask> {
        self.cues = false;
        self.act_on_char = None;
        // An older Windows' scroll bar: an arrow button moves the page a line, the track a page,
        // again and again while the button is held.
        let (place, now) = (self.bar.place(self.content_rect()), self.now());
        if self.holding.press(self.hwnd, place, &mut self.scroller, 0, p, now) {
            self.invalidate();
            return Vec::new();
        }
        let target = self.target_at(p);
        // SAFETY: focus and capture on our own window.
        unsafe {
            if GetFocus() != self.hwnd {
                let _ = SetFocus(Some(self.hwnd));
            }
            SetCapture(self.hwnd);
        }
        self.pressed = target;
        match target {
            Some(Hot::Nav(i)) => {
                // A page picked while searching: the search is over.
                self.clear_search();
                self.go_to_page(i, false);
                self.focus = Spot::Nav;
            }
            Some(Hot::Thumb) => {
                if let Some(thumb) = self.scroller.thumb(self.track()) {
                    self.drag = Some(p.y - thumb.y);
                }
            }
            Some(Hot::Ctl(i, part)) if self.rows[i].enabled => {
                if self.rows[i].row.ctl == Ctl::Folder && part == Part::Main {
                    self.folder_box.focus();
                }
                self.focus = Spot::Ctl(i, part);
            }
            Some(Hot::More) => self.focus = Spot::More,
            Some(Hot::Reset) => self.focus = Spot::Reset,
            _ => {}
        }
        self.invalidate();
        Vec::new()
    }

    fn mouse_up(&mut self, p: Point, app: &mut AppState) -> Vec<Ask> {
        if self.holding.release(self.hwnd) {
            // SAFETY: releasing our own capture.
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
            return Vec::new();
        }
        let was = self.pressed.take();
        self.drag = None;
        // SAFETY: releasing our own capture.
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        if was.is_none() || was != self.target_at(p) {
            return Vec::new();
        }
        match was {
            Some(Hot::Close) => vec![Ask::Job(Job::Close)],
            Some(Hot::Ctl(i, part)) => self.activate(i, part, app),
            Some(Hot::More) => self.flip_more(),
            Some(Hot::Reset) => self.reset_page(app),
            Some(Hot::ReportBug) => self.act(Act::ReportBug),
            _ => Vec::new(),
        }
    }

    fn wheel(&mut self, notches: f32) {
        if self.scroller.scroll_by(-notches * WHEEL_STEP, self.now()) {
            self.invalidate();
        }
    }

    // ------------------------------------------------------------ the keyboard

    /// The places that can have the keyboard, in Tab order.
    fn spots(&self) -> Vec<Spot> {
        let mut spots = Vec::new();
        if self.style.search {
            spots.push(Spot::Search);
        }
        spots.push(Spot::Nav);
        for (i, p) in self.rows.iter().enumerate() {
            if p.enabled {
                spots.extend(p.parts().into_iter().map(|part| Spot::Ctl(i, part)));
            }
        }
        if self.blocks.iter().any(|b| matches!(b, Block::More(..))) {
            spots.push(Spot::More);
        }
        if self.reset_at.is_some() {
            spots.push(Spot::Reset);
        }
        spots
    }

    /// Moves the keyboard `step` places on (Tab, Shift+Tab, the arrows), round the ends when `wrap`.
    fn move_focus(&mut self, step: i32, wrap: bool) {
        let spots = self.spots();
        let at = spots.iter().position(|s| *s == self.focus).unwrap_or(0) as i32;
        let n = spots.len() as i32;
        let next = if wrap { (at + step).rem_euclid(n) } else { (at + step).clamp(0, n - 1) };
        self.set_focus(spots[next as usize]);
    }

    /// Gives the keyboard to `spot` and scrolls it into view.
    fn set_focus(&mut self, spot: Spot) {
        self.focus = spot;
        self.cues = true;
        if spot != Spot::Search && self.search_box.has_focus() {
            self.search_box.give_focus_back();
        }
        match spot {
            Spot::Search => {
                self.search_box.focus();
                self.search_box.select_all();
            }
            Spot::Ctl(i, part) => {
                let row = self.rows[i].rect;
                if self.rows[i].row.ctl == Ctl::Folder && part == Part::Main {
                    // The text box takes the keyboard itself: bring it into view at once.
                    if row.y < self.scroller.offset || row.bottom() > self.scroller.offset + self.content_rect().h {
                        self.scroller.jump_to(row.y - 40.0);
                    }
                    self.focus_box_soon = true;
                    self.box_at = None; // placed again (and focused) on the next frame
                } else {
                    if self.folder_box.has_focus() {
                        self.folder_box.give_focus_back();
                    }
                    let shown = self.rows[i].rect_of(part).unwrap_or(row);
                    self.scroller.reveal(shown.y.min(row.y), shown.bottom(), 16.0, self.now());
                }
            }
            Spot::More => {
                if let Some(Block::More(r, _)) = self.blocks.iter().find(|b| matches!(b, Block::More(..))) {
                    self.scroller.reveal(r.y, r.bottom(), 16.0, self.now());
                }
            }
            _ => {
                if self.folder_box.has_focus() {
                    self.folder_box.give_focus_back();
                }
            }
        }
        self.invalidate();
    }

    fn go_to_page(&mut self, page: usize, keyboard: bool) {
        let page = page.min(PAGES.len() - 1);
        if page != self.page {
            self.page = page;
            self.layout_dirty = true;
            self.scroller.jump_to(0.0);
            self.hover = None;
            self.reset_asked_at = None;
        }
        self.focus = Spot::Nav;
        self.cues = keyboard;
        if self.folder_box.has_focus() {
            self.folder_box.give_focus_back();
        }
        self.invalidate();
    }

    /// Empties the search box (the page shows again).
    fn clear_search(&mut self) {
        if !self.query.is_empty() {
            self.query.clear();
            self.search_box.set_text("");
            self.layout_dirty = true;
            self.scroller.jump_to(0.0);
            self.invalidate();
        }
    }

    /// A key, when the window (not a text box) has the keyboard. Returns whether it was used.
    fn key(&mut self, vk: u16, app: &mut AppState, asks: &mut Vec<Ask>) -> bool {
        // SAFETY: plain key-state queries.
        let (ctrl, shift, alt) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0) };
        let is = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| vk == k.0;
        let n = PAGES.len();
        self.act_on_char = None; // a new key: an earlier one's character won't come any more
        if alt && !is(VK_DOWN) {
            return false; // Alt+F4, Alt+Space: Windows
        }
        let view = self.content_rect().h;
        let now = self.now();
        // On the pages, the arrows along them change the page; the other way they go into the page.
        let (along, into) = if self.tabs() { (is(VK_LEFT) || is(VK_RIGHT), is(VK_DOWN)) } else { (is(VK_UP) || is(VK_DOWN), is(VK_RIGHT)) };
        let back = if self.tabs() { is(VK_LEFT) || is(VK_UP) } else { is(VK_UP) };
        match () {
            _ if is(VK_ESCAPE) || (ctrl && vk == 'W' as u16) => asks.push(Ask::Job(Job::Close)),
            _ if (ctrl && is(VK_TAB) && !shift) || (ctrl && is(VK_NEXT)) => self.go_to_page((self.page + 1) % n, true),
            _ if (ctrl && is(VK_TAB) && shift) || (ctrl && is(VK_PRIOR)) => self.go_to_page((self.page + n - 1) % n, true),
            _ if ctrl && vk == 'F' as u16 && self.style.search => self.set_focus(Spot::Search),
            _ if is(VK_TAB) => self.move_focus(if shift { -1 } else { 1 }, true),
            // Light or dark, as in the main window.
            _ if ctrl && shift && vk == 'L' as u16 => asks.push(Ask::Command(Command::ToggleTheme)),
            _ if ctrl && is(VK_OEM_COMMA) => {}
            _ if self.focus == Spot::Nav && along => {
                self.clear_search();
                let page = if back { self.page.saturating_sub(1) } else { (self.page + 1).min(n - 1) };
                self.go_to_page(page, true);
            }
            _ if self.focus == Spot::Nav && (into || is(VK_RETURN)) => self.move_focus(1, false),
            // (Not Alt+Down: that opens a drop-down, below.)
            _ if !alt && (is(VK_UP) || is(VK_DOWN)) => self.move_focus(if is(VK_UP) { -1 } else { 1 }, false),
            _ if is(VK_LEFT) && self.focus != Spot::Nav => self.set_focus(Spot::Nav),
            _ if is(VK_SPACE) || is(VK_RETURN) || is(VK_F4) || (alt && is(VK_DOWN)) => {
                self.cues = true;
                let press = is(VK_SPACE) || is(VK_RETURN);
                match self.focus {
                    Spot::Ctl(i, part) => {
                        let dropdown = matches!(self.rows.get(i).map(|p| (&p.row.ctl, part)), Some((Ctl::Choice(..), Part::Main)));
                        if dropdown && press {
                            // The character Windows makes from Space or Enter is already on its way;
                            // in the drop-down's menu it would close the menu again. Open it when the
                            // character has come (WM_CHAR).
                            self.act_on_char = Some((i, part));
                        } else if dropdown || !(is(VK_F4) || alt) {
                            asks.extend(self.activate(i, part, app));
                        }
                    }
                    Spot::More if press => asks.extend(self.flip_more()),
                    Spot::Reset if press => asks.extend(self.reset_page(app)),
                    _ => {}
                }
                self.invalidate();
            }
            _ if is(VK_NEXT) || is(VK_PRIOR) => {
                let step = (view - 40.0).max(40.0);
                if self.scroller.scroll_by(if is(VK_NEXT) { step } else { -step }, now) {
                    self.invalidate();
                }
            }
            _ if is(VK_HOME) || is(VK_END) => {
                let to = if is(VK_HOME) { 0.0 } else { self.scroller.max() };
                if self.scroller.scroll_to(to, now) {
                    self.invalidate();
                }
            }
            _ => return false,
        }
        true
    }

    // ------------------------------------------------------------ changing things

    /// Gives `key` a new value in `app.settings`, then does what that setting does right away.
    fn set_value(&mut self, app: &mut AppState, key: &'static str, value: Value) -> Vec<Ask> {
        let before = app.settings.clone();
        if !set_in(&mut app.settings, key, value.clone()) {
            log::error(&format!("The Settings window couldn't give {key} the value {value}"));
            return Vec::new();
        }
        self.values = values_of(&app.settings);
        self.layout_dirty = true;
        self.invalidate();
        let mut asks = vec![Ask::RedrawMain];
        asks.extend(pages::after_change(key, &before, app).map(Ask::Command));
        if key == "SettingsSearch" && !app.settings.settings_search {
            self.clear_search();
        }
        let list = app.settings.settings_pages == settings::SettingsPages::List;
        if list != LIST.with(Cell::get) {
            LIST.with(|l| l.set(list));
            if list {
                // Room for the list beside the page.
                asks.push(Ask::Job(Job::Widen(FLOAT_LIST_W)));
            }
        }
        if app.settings.settings_opens != self.opens {
            // "Opens" changed: to its new place now (a job: moving sends messages).
            self.opens = app.settings.settings_opens;
            OPENS.with(|o| o.set(self.opens));
            asks.push(Ask::Job(Job::Follow));
        }
        asks
    }

    /// A control was clicked, or Space / Enter was pressed on it.
    fn activate(&mut self, i: usize, part: Part, app: &mut AppState) -> Vec<Ask> {
        let Some(p) = self.rows.get(i) else { return Vec::new() };
        if !p.enabled {
            return Vec::new();
        }
        let ctl = p.row.ctl.clone();
        let anchor = p.main.map(|m| self.on_screen(m));
        // A tick flipped by the person fades to its new look (if its value really changes; one
        // that changes later, Portable's, just shows its new look then).
        if let Some(id) = flip_id(&ctl, part) {
            let now = self.now();
            self.flips.start(&id, !toggle_on(&ctl, &self.values), now, kit::CHECK_TIME);
        }
        match (ctl, part) {
            // A round button or a skin tile: that choice.
            (Ctl::Choice(k, choices), Part::Option(n)) => {
                let Some((v, _)) = choices.get(n) else { return Vec::new() };
                if self.values.get(k) == Some(&v.json()) {
                    return Vec::new();
                }
                let asks = self.set_value(app, k, v.json());
                if k == "Skin" {
                    // The window takes the new skin at once, so its tiles show what's picked.
                    self.apply_theme(&app.theme);
                }
                asks
            }
            (Ctl::Toggle("@Portable"), _) => self.switch_portable(),
            (Ctl::Toggle(k), _) if k.starts_with('@') => {
                if let Err(e) = pages::set_outside(k, !bool_of(&self.values, k)) {
                    app.say(e);
                }
                self.invalidate();
                Vec::new()
            }
            (Ctl::Toggle(k), _) | (Ctl::ButtonToggle(_, _, k), Part::Second) => {
                let on = bool_of(&self.values, k);
                self.set_value(app, k, Value::Bool(!on))
            }
            (Ctl::ToggleNot(k), _) => {
                let stored = bool_of(&self.values, k);
                self.set_value(app, k, Value::Bool(!stored))
            }
            (Ctl::ToggleIn(k, item), _) => {
                let mut list: Vec<String> = self.values.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
                if let Some(at) = list.iter().position(|c| c == item) {
                    list.remove(at);
                } else {
                    list.push(item.to_string());
                }
                // Always in the order the columns are shown.
                let ordered: Vec<Value> = settings::COLUMN_NAMES.iter().filter(|c| list.iter().any(|l| l == *c)).map(|c| Value::String(c.to_string())).collect();
                self.set_value(app, k, Value::Array(ordered))
            }
            (Ctl::Choice(k, choices), _) => {
                let Some(at) = anchor else { return Vec::new() };
                let current = self.values.get(k).cloned();
                let mut items: Vec<MenuItem> = choices.iter().enumerate().map(|(n, (v, label))| MenuItem::new(n as u32 + 1, label).checked(Some(v.json()) == current)).collect();
                for (item, (v, _)) in items.iter_mut().zip(&choices) {
                    if let Some(picture) = choice_picture(k, *v, &app.theme) {
                        *item = std::mem::take(item).picture(picture);
                    }
                }
                if k == "Skin" {
                    // The skin in use stands out: ticked and in bold.
                    for item in &mut items {
                        item.bold = item.checked;
                    }
                    items = skin_groups(&choices, items);
                }
                self.menu_for = Some((k, choices.iter().map(|(v, _)| *v).collect()));
                let b = self.border();
                let list = droplist::DropList { items, under: RECT::default(), look: droplist::DropLook::of(&app.theme), menu: false, right: false };
                vec![Ask::Job(Job::Drop(list, Rect::new(at.x + b, at.y + b, at.w, at.h)))]
            }
            (Ctl::Button(_, act), _) | (Ctl::ButtonToggle(_, act, _), Part::Main) => self.act(act),
            (Ctl::Folder, Part::Main) => {
                self.set_focus(Spot::Ctl(i, Part::Main));
                Vec::new()
            }
            (Ctl::Folder, Part::Second) => {
                let start = self.values.get("StartupFolder").and_then(Value::as_str).unwrap_or("").to_string();
                vec![Ask::Job(Job::PickFolder(start))]
            }
            (Ctl::Text(_) | Ctl::None, _) | (_, Part::Option(_)) => Vec::new(),
        }
    }

    /// "More settings" or "Fewer settings": the rest of every page shows, or goes again.
    fn flip_more(&mut self) -> Vec<Ask> {
        self.show_more = !self.show_more;
        self.layout_dirty = true;
        self.focus = Spot::More;
        self.invalidate();
        Vec::new()
    }

    /// RESET PAGE: every setting on the page back to its default - once it's pressed a second
    /// time (the first press asks for that). What Windows keeps ("@" names) stays as it is.
    fn reset_page(&mut self, app: &mut AppState) -> Vec<Ask> {
        let now = self.now();
        if !self.reset_asked_at.is_some_and(|t| now - t < CLEAR_CONFIRM_SECONDS) {
            self.reset_asked_at = Some(now);
            self.layout_dirty = true;
            self.invalidate();
            return Vec::new();
        }
        self.reset_asked_at = None;
        let mut keys: Vec<&'static str> = Vec::new();
        for item in self.page_items(self.page) {
            let Item::Card(rows) = item else { continue };
            for row in rows {
                let key = match row.ctl {
                    Ctl::Toggle(k) | Ctl::ToggleNot(k) | Ctl::ToggleIn(k, _) | Ctl::ButtonToggle(_, _, k) | Ctl::Choice(k, _) => k,
                    Ctl::Folder => "StartupFolder",
                    _ => continue,
                };
                if !key.starts_with('@') && !keys.contains(&key) {
                    keys.push(key);
                }
            }
        }
        let mut asks = Vec::new();
        for key in keys {
            let Some(default) = self.defaults.get(key).cloned() else { continue };
            if self.values.get(key) != Some(&default) {
                asks.extend(self.set_value(app, key, default));
            }
        }
        self.apply_theme(&app.theme);
        app.say(&format!("{} is back to its defaults", PAGES[self.page]));
        self.layout_dirty = true;
        self.invalidate();
        asks
    }

    /// A button's job.
    fn act(&mut self, act: Act) -> Vec<Ask> {
        let to = self.poster();
        match act {
            Act::OpenSettingsFolder | Act::OpenLogsFolder => {
                let dir = if act == Act::OpenSettingsFolder { settings::config_now() } else { settings::folders().logs.clone() };
                // Made first if it isn't there yet (nothing was saved or logged so far), away from
                // this thread: it may be on a slow or redirected drive.
                shell::background("settings: open folder", move || {
                    let _ = std::fs::create_dir_all(&dir);
                    post(to, Bg::Open(dir.display().to_string()));
                });
            }
            Act::ReportBug => shell::open_link(&format!("https://github.com/{}/issues", settings::UPDATE_REPO)),
            Act::ClearCache => {
                // Deleting files asks twice: the cache folder is next to glint.exe when portable,
                // where a folder called "cache" might not be only Glint's.
                let now = self.now();
                if self.clearing {
                    return Vec::new(); // already under way
                }
                if self.cache_size == Some(0) {
                    self.cache_said = Some("The cache is already empty.".into());
                } else if self.clear_asked_at.is_some_and(|t| now - t < CLEAR_CONFIRM_SECONDS) {
                    self.clear_asked_at = None;
                    self.clearing = true;
                    self.cache_said = Some("Clearing…".into());
                    self.cache_size = None;
                    let started = shell::background("settings: clear cache", move || post(to, Bg::Cleared(settings::clear_cache())));
                    if !started {
                        self.clearing = false;
                        self.cache_said = Some("Windows couldn't do that right now. Try again.".into());
                    }
                } else {
                    self.clear_asked_at = Some(now);
                    self.cache_said = Some("Press CLEAR again to confirm.".into());
                }
                self.layout_dirty = true;
            }
            // The skin customiser (skinwin.rs), a window of its own.
            Act::SkinEditor => return vec![Ask::Command(Command::OpenSkinEditor)],
            // Look for a new version now (the main window does it; the row says how it went).
            Act::Update => {
                self.layout_dirty = true;
                self.invalidate();
                return vec![Ask::Command(Command::CheckForUpdates)];
            }
            // Not in this version (its row is greyed).
            Act::SortEditor => return vec![Ask::Command(Command::OpenSortEditor)],
            // (Greyed until there's a Discord: the row isn't in LIVE.)
            Act::Discord => {}
        }
        self.invalidate();
        Vec::new()
    }

    /// Settings > About > Portable: moves the settings next to glint.exe or back, on a background
    /// thread (copying files may take a moment); the row says what happened.
    fn switch_portable(&mut self) -> Vec<Ask> {
        if self.portable_busy {
            return Vec::new();
        }
        let on = !settings::portable_now();
        self.portable_busy = true;
        self.portable_said = Some("Moving the settings…".into());
        self.layout_dirty = true;
        self.invalidate();
        let to = self.poster();
        let started = shell::background("settings: portable", move || post(to, Bg::Portable(settings::set_portable(on))));
        if !started {
            self.portable_busy = false;
            self.portable_said = Some("Windows couldn't do that right now. Try again.".into());
        }
        Vec::new()
    }

    /// Takes in what background work found.
    fn take_results(&mut self) -> Vec<Ask> {
        // Only one Settings window is ever open: results for another number are for one that was
        // closed, and go.
        let mine = self.serial;
        let results: Vec<Bg> = std::mem::take(&mut *RESULTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            .into_iter()
            .filter(|(serial, _)| *serial == mine)
            .map(|(_, b)| b)
            .collect();
        let mut asks = Vec::new();
        for bg in results {
            match bg {
                Bg::Terminals(found) => self.terminals = Some(found),
                Bg::ZipSaid(said) => self.zip_said = Some(said),
                // (A size measured before CLEAR was pressed is out of date while clearing.)
                Bg::CacheSize(bytes) => {
                    if !self.clearing {
                        self.cache_size = Some(bytes);
                    }
                }
                Bg::Cleared(bytes) => {
                    self.clearing = false;
                    self.cache_size = Some(bytes);
                    self.cache_said = Some(if bytes == 0 {
                        "Cleared.".to_string()
                    } else {
                        format!("Cleared. {} is in use and was left.", crate::folder::format_size(bytes))
                    });
                }
                Bg::Portable(result) => {
                    self.portable_busy = false;
                    self.portable_said = Some(match result {
                        Ok(said) => said,
                        Err(why) => format!("Couldn't switch: {why}"),
                    });
                }
                Bg::Open(dir) => {
                    // The folder opens in Glint, and this window closes so it shows.
                    asks.push(Ask::Command(Command::NewTabAt(dir)));
                    asks.push(Ask::Job(Job::Close));
                }
            }
        }
        self.layout_dirty = true;
        self.invalidate();
        if frame::is_wine() {
            // Wine shows a frame painted for news from another thread only once a later one is.
            self.repaint_until = self.now() + 0.1;
        }
        asks
    }

    /// The start folder's text box: what's typed is saved as it goes (a moment later, like every
    /// choice); Enter or Esc gives the keyboard back to the window.
    fn text_event(&mut self, ev: TextEvent, app: &mut AppState) -> Vec<Ask> {
        if ev.id == SEARCH_BOX {
            self.search_event(ev.kind);
            return Vec::new();
        }
        if ev.id != FOLDER_BOX {
            return Vec::new();
        }
        match ev.kind {
            TextEventKind::Changed => {
                let text = self.folder_box.text();
                self.box_text = text.clone();
                self.set_value(app, "StartupFolder", clean_folder(&text))
            }
            TextEventKind::Enter | TextEventKind::Escape => {
                self.folder_box.give_focus_back();
                self.cues = true;
                self.invalidate();
                Vec::new()
            }
            TextEventKind::LostFocus => {
                self.invalidate();
                Vec::new()
            }
            // (The down arrow just moves the caret here; Windows' suggestions use it too.)
            TextEventKind::Down | TextEventKind::Tab => Vec::new(),
        }
    }

    /// The search box: what's typed shows what matches on every page at once; Enter or Down go to
    /// the first thing found; Esc empties it, or (empty) gives the keyboard back to the window.
    fn search_event(&mut self, kind: TextEventKind) {
        match kind {
            TextEventKind::Changed => {
                let text = self.search_box.text();
                if text != self.query {
                    self.query = text;
                    self.layout_dirty = true;
                    self.scroller.jump_to(0.0);
                    self.hover = None;
                }
            }
            TextEventKind::Enter | TextEventKind::Down => {
                if self.layout_dirty {
                    self.layout();
                }
                let first = self.spots().into_iter().find(|s| matches!(s, Spot::Ctl(..)));
                if let Some(spot) = first {
                    self.set_focus(spot);
                }
            }
            TextEventKind::Escape if !self.query.is_empty() => self.clear_search(),
            TextEventKind::Escape => {
                self.search_box.give_focus_back();
                self.set_focus(Spot::Nav);
            }
            TextEventKind::LostFocus | TextEventKind::Tab => {}
        }
        self.invalidate();
    }

    /// What a job brought back (a drop-down's pick, a folder from the picker).
    pub fn job_done(&mut self, done: JobDone, app: &mut AppState) -> Vec<Ask> {
        match done {
            JobDone::Nothing => Vec::new(),
            JobDone::Picked(id) => {
                let Some((key, values)) = self.menu_for.take() else { return Vec::new() };
                let Some(v) = id.checked_sub(1).and_then(|n| values.get(n as usize)) else { return Vec::new() };
                let asks = self.set_value(app, key, v.json());
                if key == "Skin" {
                    // The window takes the new skin at once.
                    self.apply_theme(&app.theme);
                }
                // "A specific folder" with no folder yet: the keyboard goes to the folder box.
                if key == "Startup" && *v == pages::Val::S("folder") && app.settings.startup_folder.is_none() {
                    self.layout();
                    if let Some(i) = self.rows.iter().position(|p| p.row.ctl == Ctl::Folder) {
                        self.set_focus(Spot::Ctl(i, Part::Main));
                    }
                }
                asks
            }
            JobDone::Folder(path) => {
                self.box_text.clear(); // the box shows the new folder on the next frame
                self.set_value(app, "StartupFolder", clean_folder(&path))
            }
        }
    }

    // ------------------------------------------------------------ messages

    /// Handles one message of this window, with the app's state. None: Windows' default.
    pub fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM, app: &mut AppState) -> (Option<LRESULT>, Vec<Ask>) {
        let mut asks = Vec::new();
        let result = match msg {
            WM_PAINT => {
                // SAFETY: marks our own window as painted (anything invalidated while painting
                // then asks for one more frame).
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                self.paint(app);
                Some(LRESULT(0))
            }
            WM_SIZE if wparam.0 == SIZE_MINIMIZED as usize => Some(LRESULT(0)),
            // Moved or resized: back in its place by the main window - or, floating, remembered
            // where it was left.
            WM_EXITSIZEMOVE => {
                self.follow_owner();
                let mut r = RECT::default();
                // SAFETY: plain queries on our own window.
                if self.opens == SettingsOpens::Floating && unsafe { !IsZoomed(self.hwnd).as_bool() && GetWindowRect(self.hwnd, &mut r).is_ok() } {
                    app.settings.settings_place = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
                }
                Some(LRESULT(0))
            }
            WM_SIZE => {
                let (w, h) = (lo_word(lparam.0 as usize), hi_word(lparam.0 as usize));
                self.renderer.resize(w, h);
                if w > 0 && h > 0 {
                    self.size = self.inner_size(w, h);
                    self.layout_dirty = true;
                    self.search_at = None;
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = lo_word(wparam.0).max(96);
                self.renderer.set_dpi(self.dpi);
                let mut rc = RECT::default();
                // SAFETY: our own window and a valid out-struct.
                if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
                    self.size = self.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
                }
                self.layout_dirty = true;
                self.box_at = None;
                self.invalidate();
                // SAFETY: lparam points to the suggested window rectangle.
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                asks.push(Ask::Job(Job::Place(suggested)));
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let p = self.client_point(lparam);
                self.mouse_move(p);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.drag.is_none() && self.hover.take().is_some() {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_down(p));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_up(p, app));
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.pressed = None;
                self.drag = None;
                self.holding.release(self.hwnd);
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                self.wheel(hi_word(wparam.0) as u16 as i16 as f32 / 120.0);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => self.key(wparam.0 as u16, app, &mut asks).then_some(LRESULT(0)),
            // Space or Enter's character, after the key opened a drop-down (see `key`). Other
            // letters typed at the window do nothing (and don't beep), nor Alt+letter (there is
            // no menu bar).
            WM_CHAR => {
                if let Some((i, part)) = self.act_on_char.take() {
                    asks.extend(self.activate(i, part, app));
                }
                Some(LRESULT(0))
            }
            WM_SYSCHAR if wparam.0 != ' ' as usize => Some(LRESULT(0)),
            WM_ACTIVATE => {
                self.active = lo_word(wparam.0) != WA_INACTIVE;
                self.invalidate();
                // Back from Windows' "Open with": the Zip icon row says what's true now.
                if self.active {
                    self.look_up_zip();
                }
                if !self.active {
                    self.box_had_focus = if self.folder_box.has_focus() {
                        Some(FOLDER_BOX)
                    } else if self.search_box.has_focus() {
                        Some(SEARCH_BOX)
                    } else {
                        None
                    };
                    None
                } else {
                    // Typing goes on where it was, instead of Windows giving the keyboard to the window.
                    match self.box_had_focus.take() {
                        Some(FOLDER_BOX) if self.box_at.is_some() => {
                            self.folder_box.focus();
                            Some(LRESULT(0))
                        }
                        Some(SEARCH_BOX) if self.search_at.is_some() => {
                            self.search_box.focus();
                            Some(LRESULT(0))
                        }
                        _ => None,
                    }
                }
            }
            WM_CLOSE => {
                // Destroyed after this message, outside the guard, so the state goes with it.
                asks.push(Ask::Job(Job::Close));
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == RETRY_TIMER => {
                // SAFETY: stops our own one-off timer.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), RETRY_TIMER);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == kit::REPEAT_TIMER => {
                let (place, now) = (self.bar.place(self.content_rect()), self.now());
                if self.holding.fire(self.hwnd, place, &mut self.scroller, now) {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_APP_TEXT => {
                let ev = TextEvent { id: wparam.0 as u16, kind: TextEventKind::from_number(lparam.0) };
                asks.extend(self.text_event(ev, app));
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                // EN_CHANGE (0x0300) from the text box, unless the text was set here.
                if hi_word(wparam.0) == 0x0300 && !textbox::setting_text() {
                    asks.extend(self.text_event(TextEvent { id: lo_word(wparam.0) as u16, kind: TextEventKind::Changed }, app));
                }
                // EN_SETFOCUS (0x0100): clicked into, so Tab goes on from there.
                if hi_word(wparam.0) == 0x0100 && lo_word(wparam.0) == FOLDER_BOX as u32 {
                    if let Some(i) = self.rows.iter().position(|p| p.row.ctl == Ctl::Folder) {
                        self.focus = Spot::Ctl(i, Part::Main);
                    }
                    self.invalidate();
                }
                if hi_word(wparam.0) == 0x0100 && lo_word(wparam.0) == SEARCH_BOX as u32 {
                    self.focus = Spot::Search;
                    if self.folder_box.has_focus() {
                        self.folder_box.give_focus_back();
                    }
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_APP_TAB => {
                if self.search_box.has_focus() {
                    self.focus = Spot::Search;
                    self.search_box.give_focus_back();
                }
                self.folder_box.give_focus_back();
                self.move_focus(if wparam.0 != 0 { -1 } else { 1 }, true);
                Some(LRESULT(0))
            }
            WM_APP_BG => {
                asks.extend(self.take_results());
                Some(LRESULT(0))
            }
            _ => None,
        };
        // The page shown, for reopening there next time.
        if app.settings.settings_page != self.page {
            app.settings.settings_page = self.page;
        }
        (result, asks)
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        let brush = EDIT_COLORS.with(|c| c.replace((0, 0, 0))).2;
        if brush != 0 {
            // SAFETY: the brush is ours, and the text box that used it is gone with the window.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(brush as *mut _));
            }
        }
    }
}

/// The words of the link at the bottom that opens GitHub's issues page.
const REPORT_BUG: &str = "Report a bug";
/// At the bottom: there's no OK or Apply.
const LIVE_NOTE: &str = "Changes apply right away";

/// Whether a row is something searched for (`query`, in small letters): its title, its note, its
/// choices, what a shortcut does, or its group's heading.
fn row_matches(row: &Row, heading: &str, query: &str) -> bool {
    let has = |text: &str| text.to_lowercase().contains(query);
    has(&row.title)
        || row.note.as_deref().is_some_and(has)
        || has(heading)
        || match &row.ctl {
            Ctl::Choice(_, choices) => choices.iter().any(|(_, label)| has(label)),
            Ctl::Text(what) => has(what),
            _ => false,
        }
}

/// How a row's control is shown, with the window's own choices.
fn shape_of(row: &Row, style: Style) -> Shape {
    match &row.ctl {
        Ctl::Toggle(_) | Ctl::ToggleNot(_) | Ctl::ToggleIn(..) | Ctl::ButtonToggle(..) => Shape::Tick,
        Ctl::Choice("Skin", _) if style.tiles => Shape::Tiles,
        Ctl::Choice(..) if !row.long && (row.radios || style.sentences) => Shape::Radios,
        Ctl::Folder => Shape::Folder,
        Ctl::Text(_) => Shape::Keys,
        _ => Shape::Plain,
    }
}

/// The widest of `lines` in `style`.
fn widest(r: &mut Renderer, lines: &[String], style: &TextStyle) -> f32 {
    lines.iter().map(|l| r.measure(l, style).ceil()).fold(0.0, f32::max)
}

/// Lays out one row at `y` (page coordinates), `w` wide from `left` (inside its group's box); a
/// drop-down is `drop_w` wide (its group's widest).
#[allow(clippy::too_many_arguments)]
fn place_row(r: &mut Renderer, row: Row, shape: Shape, left: f32, y: f32, w: f32, drop_w: f32, enabled: bool) -> Placed {
    let lx = left + if row.indent { INDENT } else { 0.0 };
    let right = left + w;
    let title_style = TextStyle::new(Color::hex(0)).size(TITLE_FONT);
    let note_style = TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT);
    let later = if row.live() { 0.0 } else { NOTE_LINE };
    // The text's first line sits in the middle of a row's height.
    let text_y = ((ROW_H - TITLE_LINE) / 2.0).round();
    let mut placed = Placed {
        shape,
        rect: Rect::new(left, y, w, ROW_H),
        title: Vec::new(),
        note: Vec::new(),
        text_x: lx,
        text_y,
        text_w: (right - lx).max(40.0),
        main: None,
        second: None,
        hit: None,
        options: Vec::new(),
        option_hits: Vec::new(),
        enabled,
        dot: Point::new(lx - 7.0, y + text_y + TITLE_LINE / 2.0),
        row,
    };
    let wrap_text = |r: &mut Renderer, placed: &mut Placed, text_w: f32| {
        placed.text_w = text_w.max(40.0);
        placed.title = if placed.row.title.is_empty() { Vec::new() } else { kit::wrap(r, &placed.row.title, &title_style, placed.text_w) };
        if let Some(note) = &placed.row.note {
            placed.note = kit::wrap(r, note, &note_style, placed.text_w);
        }
        placed.title.len() as f32 * TITLE_LINE + placed.note.len() as f32 * NOTE_LINE + later
    };
    match shape {
        Shape::Tick => {
            // The tick, its title beside it (and EDIT… on the right when it has one).
            let button = match &placed.row.ctl {
                Ctl::ButtonToggle(label, ..) => button_w(r, label),
                _ => 0.0,
            };
            let text_x = lx + CHECK + CHECK_GAP;
            let text_w = right - text_x - if button > 0.0 { button + CONTROL_GAP } else { 0.0 };
            placed.text_x = text_x;
            let text_h = wrap_text(r, &mut placed, text_w);
            let h = (text_h + 2.0 * text_y).max(ROW_H).ceil();
            placed.rect.h = h;
            let tick = Rect::new(lx, y + text_y + ((TITLE_LINE - CHECK) / 2.0).round(), CHECK, CHECK);
            let words = widest(r, &placed.title, &title_style).max(widest(r, &placed.note, &note_style));
            placed.hit = Some(Rect::new(lx - 3.0, y + 1.0, CHECK + CHECK_GAP + words + 6.0, h - 2.0));
            placed.dot.x = lx - 7.0;
            if button > 0.0 {
                placed.main = Some(Rect::new(right - button, y + ((ROW_H - CTL_H) / 2.0).round(), button, CTL_H));
                placed.second = Some(tick);
            } else {
                placed.main = Some(tick);
            }
        }
        Shape::Radios | Shape::Tiles => {
            // The title, then a round button (or a tile) for each choice under it.
            let text_h = wrap_text(r, &mut placed, right - lx);
            let top = y + if text_h > 0.0 { text_y + text_h + 2.0 } else { 2.0 };
            let Ctl::Choice(_, choices) = &placed.row.ctl else { return placed };
            let labels: Vec<String> = choices.iter().map(|(_, l)| l.clone()).collect();
            // (Round buttons with no question over them - their group's title asks it - aren't pulled in.)
            let x = lx + if placed.title.is_empty() { 0.0 } else { INDENT };
            let bottom = if shape == Shape::Radios {
                for (n, label) in labels.iter().enumerate() {
                    let oy = top + n as f32 * OPTION_H;
                    let button = Rect::new(x, oy + ((OPTION_H - CHECK) / 2.0).round(), CHECK, CHECK);
                    let words = r.measure(label, &title_style).ceil().min((right - x - CHECK - CHECK_GAP).max(0.0));
                    placed.options.push(button);
                    placed.option_hits.push(Rect::new(x - 3.0, oy, CHECK + CHECK_GAP + words + 6.0, OPTION_H));
                }
                top + labels.len() as f32 * OPTION_H
            } else {
                let room = (right - x).max(TILE_MIN_W);
                let cols = (((room + TILE_GAP) / (TILE_MIN_W + TILE_GAP)).floor() as usize).max(1);
                let tile_w = ((room - (cols - 1) as f32 * TILE_GAP) / cols as f32).floor();
                for n in 0..labels.len() {
                    let (col, line) = (n % cols, n / cols);
                    let tile = Rect::new(x + col as f32 * (tile_w + TILE_GAP), top + 2.0 + line as f32 * (TILE_H + TILE_GAP), tile_w, TILE_H);
                    placed.options.push(tile);
                    placed.option_hits.push(tile);
                }
                top + 2.0 + labels.len().div_ceil(cols) as f32 * (TILE_H + TILE_GAP)
            };
            placed.rect.h = (bottom + 4.0 - y).ceil();
        }
        Shape::Folder => {
            let browse = button_w(r, "BROWSE…");
            let box_x = left + INDENT;
            placed.main = Some(Rect::new(box_x, y + 2.0, (right - browse - 8.0 - box_x).max(80.0), CTL_H));
            placed.second = Some(Rect::new(right - browse, y + 2.0, browse, CTL_H));
            placed.rect.h = CTL_H + 8.0;
        }
        Shape::Keys => {}
        Shape::Plain => {
            // The title on the left, its drop-down or button on the right - or, when that leaves
            // too little room for the words, under them.
            let control = match &placed.row.ctl {
                Ctl::Choice(_, choices) => drop_width(r, choices.iter().map(|(_, l)| l.as_str())).max(drop_w).min(w),
                Ctl::Button(label, _) => button_w(r, label),
                _ => 0.0,
            };
            let beside = right - lx - if control > 0.0 { control + CONTROL_GAP } else { 0.0 };
            if control > 0.0 && beside < 110.0 {
                let text_h = wrap_text(r, &mut placed, right - lx);
                let top = y + text_y + text_h + 4.0;
                placed.main = Some(Rect::new(lx, top, control.min(right - lx), CTL_H));
                placed.rect.h = (top + CTL_H + 6.0 - y).ceil();
            } else {
                let text_h = wrap_text(r, &mut placed, beside);
                let h = (text_h + 2.0 * text_y).max(ROW_H).ceil();
                placed.rect.h = h;
                if control > 0.0 {
                    // Beside the first line of the title.
                    let cy = y + ((ROW_H - CTL_H) / 2.0).round();
                    placed.main = Some(Rect::new(right - control, cy, control, CTL_H));
                }
            }
        }
    }
    placed
}

/// Lays out the shortcuts as a cheat sheet: the keys, then what they do, a line each - in two
/// columns when there's room. Returns where they end.
fn place_keys(r: &mut Renderer, rows: Vec<Row>, left: f32, y: f32, w: f32, out: &mut Vec<Placed>) -> f32 {
    let _ = r;
    let cols = if w >= KEYS_TWO_W { 2 } else { 1 };
    let gap = 16.0;
    let col_w = ((w - (cols - 1) as f32 * gap) / cols as f32).floor();
    let per_col = rows.len().div_ceil(cols).max(1);
    for (n, row) in rows.into_iter().enumerate() {
        let (col, line) = (n / per_col, n % per_col);
        let x = left + col as f32 * (col_w + gap);
        let rect = Rect::new(x, y + line as f32 * KEYS_ROW_H, col_w, KEYS_ROW_H);
        let enabled = row.live();
        out.push(Placed {
            shape: Shape::Keys,
            rect,
            title: Vec::new(),
            note: Vec::new(),
            text_x: x,
            text_y: 0.0,
            text_w: col_w,
            main: None,
            second: None,
            hit: None,
            options: Vec::new(),
            option_hits: Vec::new(),
            enabled,
            dot: Point::new(x, rect.y),
            row,
        });
    }
    y + per_col as f32 * KEYS_ROW_H
}

/// A skin's tile: its colours (background, a panel, the accent as a bar, a line of its text) over
/// its name; the skin in use framed in the accent.
fn skin_tile(r: &mut Renderer, theme: &Theme, rect: Rect, name: &str, colours: Option<[Color; 4]>, picked: bool, look: Look) {
    let swatch = Rect::new(rect.x + 3.0, rect.y + 3.0, rect.w - 6.0, 24.0);
    match colours {
        Some([background, panel, accent, text]) => {
            r.fill_rect(swatch, background);
            r.fill_rect(Rect::new(swatch.x + swatch.w * 0.45, swatch.y + 4.0, swatch.w * 0.55 - 4.0, swatch.h - 8.0), panel);
            r.fill_rect(Rect::new(swatch.x + 4.0, swatch.y + 5.0, 3.0, swatch.h - 10.0), accent);
            r.fill_rect(Rect::new(swatch.x + 10.0, swatch.y + 10.0, (swatch.w * 0.3).max(4.0), 3.0), text);
        }
        None => r.fill_checker(swatch, 4.0, theme.panel, theme.line),
    }
    let edge = if picked {
        theme.accent
    } else if look.hover {
        theme.text.with_alpha(0.5)
    } else {
        theme.line
    };
    r.stroke_rect(rect.inset(0.5, 0.5), if picked { 2.0 } else { 1.0 }, edge);
    let color = if picked { theme.text } else { theme.muted };
    let style = TextStyle::new(color).size(ksize::NOTE_FONT);
    r.text(name, Rect::new(rect.x + 5.0, swatch.bottom() + 1.0, rect.w - 10.0, rect.bottom() - swatch.bottom() - 2.0), &if picked { style.semibold() } else { style });
    if look.focused {
        kit::focus_ring(r, theme, rect.inset(-2.0, -2.0), 0.0);
    }
}

/// The little picture beside a choice in a drop-down: the Logo list shows each logo, the Zip icon
/// list each icon (glint.exe's own, by their numbers in res/glint.rc).
fn choice_picture(key: &str, value: pages::Val, theme: &crate::theme::Theme) -> Option<MenuPicture> {
    let pages::Val::S(v) = value else { return None };
    match (key, v) {
        ("Logo", "box") => Some(MenuPicture::Box(theme.logo_paint.base())),
        ("Logo", "chrome") => Some(MenuPicture::Wordmark { caps: false, dark: theme.dark }),
        ("Logo", "caps") => Some(MenuPicture::Wordmark { caps: true, dark: theme.dark }),
        ("ZipIcon", "label") => Some(MenuPicture::Icon(2)),
        ("ZipIcon", "folder") => Some(MenuPicture::Icon(4)),
        ("ZipIcon", "page") => Some(MenuPicture::Icon(5)),
        ("ZipIcon", "glint") => Some(MenuPicture::Icon(1)),
        _ => None,
    }
}

/// The skin list in two groups under greyed headings: Glint's own skins under "Default", the
/// person's skin files under "Your skins" (a heading only shows when its group has skins).
fn skin_groups(choices: &[(pages::Val, String)], items: Vec<MenuItem>) -> Vec<MenuItem> {
    let theirs = |v: &pages::Val| matches!(v, pages::Val::S(id) if id.starts_with(crate::skin::FILE_PREFIX));
    let (mine, yours): (Vec<_>, Vec<_>) = choices.iter().zip(items).partition(|((v, _), _)| !theirs(v));
    let mut out = Vec::new();
    for (title, group) in [("Default", mine), ("Your skins", yours)] {
        if group.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(MenuItem::separator());
        }
        out.push(MenuItem::new(0, title).disabled(true));
        out.extend(group.into_iter().map(|(_, item)| item));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_changes_by_its_name() {
        let mut s = Settings::default();
        assert!(set_in(&mut s, "ShowHidden", Value::Bool(true)));
        assert!(s.show_hidden);
        assert!(set_in(&mut s, "ConfirmDelete", Value::String("never".into())));
        assert_eq!(s.confirm_delete, settings::ConfirmDelete::Never);
        assert!(set_in(&mut s, "StartupFolder", Value::String("D:\\Work".into())));
        assert_eq!(s.startup_folder.as_deref(), Some("D:\\Work"));
        // A value that doesn't fit changes nothing.
        assert!(!set_in(&mut s, "ConfirmDelete", Value::String("sometimes".into())));
        assert_eq!(s.confirm_delete, settings::ConfirmDelete::Never);
        // Everything else stays as it was, the remembered state too.
        let mut before = Settings { bounds: Some([1, 2, 3, 4]), column_width_map: [("size".to_string(), 99.5)].into(), sort_column: 2, ..Default::default() };
        before.tabs = vec![settings::SavedTab { path: "C:\\x".into(), sort_column: 1, sort_descending: true, view: None, pinned: false }];
        let mut after = before.clone();
        assert!(set_in(&mut after, "Terminal", Value::String("cmd".into())));
        after.terminal = before.terminal;
        assert_eq!(after, before);
    }

    #[test]
    fn start_folders_are_cleaned() {
        assert_eq!(clean_folder("  \"D:\\My Work\"  "), Value::String("D:\\My Work".into()));
        assert_eq!(clean_folder("   "), Value::Null);
    }

    #[test]
    fn logo_and_zip_icon_choices_have_their_pictures() {
        use pages::Val::{B, S};
        let theme = crate::theme::Theme::dark();
        for v in ["box", "chrome", "caps"] {
            assert!(choice_picture("Logo", S(v), &theme).is_some(), "{v}");
        }
        assert_eq!(choice_picture("Logo", S("none"), &theme), None);
        assert_eq!(choice_picture("ZipIcon", S("label"), &theme), Some(MenuPicture::Icon(2)));
        assert_eq!(choice_picture("ZipIcon", S("glint"), &theme), Some(MenuPicture::Icon(1)));
        assert_eq!(choice_picture("Skin", S("nvidia"), &theme), None);
        assert_eq!(choice_picture("ZipIcon", B(true), &theme), None);
    }

    #[test]
    fn skins_are_grouped_under_default_and_your_skins() {
        use pages::Val::S;
        let choices = vec![(S("nvidia"), "NVIDIA".to_string()), (S("file:Pink"), "Pink".to_string()), (S("amd"), "AMD Radeon".to_string()), (S("file:Blue"), "Blue".to_string())];
        let items: Vec<MenuItem> = choices.iter().enumerate().map(|(n, (_, l))| MenuItem::new(n as u32 + 1, l).checked(n == 1)).collect();
        let out = skin_groups(&choices, items);
        let labels: Vec<&str> = out.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["Default", "NVIDIA", "AMD Radeon", "", "Your skins", "Pink", "Blue"]);
        // Headings can't be picked; each skin keeps its own number (the pick finds its choice).
        assert!(out[0].disabled && out[4].disabled);
        assert_eq!(out.iter().filter(|m| !m.label.is_empty() && !m.disabled).map(|m| m.id).collect::<Vec<_>>(), [1, 3, 2, 4]);
        assert!(out[5].checked);
        // Only Glint's own skins: no "Your skins" heading, no line.
        let only = vec![(S("nvidia"), "NVIDIA".to_string())];
        let out = skin_groups(&only, vec![MenuItem::new(1, "NVIDIA")]);
        assert_eq!(out.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(), ["Default", "NVIDIA"]);
    }

    fn row_of(page: usize, key: &str) -> Row {
        let values = values_of(&Settings::default());
        let facts = Facts {
            values: &values,
            terminals: None,
            zip_said: None,
            config: String::new(),
            logs: String::new(),
            cache: String::new(),
            cache_size: None,
            cache_said: None,
            portable_said: None,
            portable_locked: false,
            skins: &[],
            update_said: String::new(),
            update_busy: false,
        };
        pages::page(page, &facts)
            .into_iter()
            .flat_map(|i| match i {
                Item::Card(rows) => rows,
                Item::Heading(_) => Vec::new(),
            })
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("{key} isn't on page {page}"))
    }

    /// Ticks: short choices as round buttons, long ones as drop-downs. Sentences: every choice
    /// spelled out but the long ones. Skins as tiles only when asked for.
    #[test]
    fn each_row_has_its_shape() {
        let ticks = Style::of(&values_of(&Settings::default()));
        assert!(!ticks.sentences && !ticks.list && !ticks.basic && !ticks.tiles && ticks.search && ticks.dots && ticks.reset);
        let s = Settings { settings_style: settings::SettingsStyle::Sentences, settings_skins: settings::SettingsSkins::Tiles, settings_pages: settings::SettingsPages::List, settings_show: settings::SettingsShow::Basic, settings_search: false, ..Default::default() };
        let sentences = Style::of(&values_of(&s));
        assert!(sentences.sentences && sentences.list && sentences.basic && sentences.tiles && !sentences.search);
        let new_tab = row_of(0, "NewTab");
        let home = row_of(0, "HomeOpens");
        let startup = row_of(0, "Startup");
        let skin = row_of(1, "Skin");
        assert_eq!((shape_of(&new_tab, ticks), shape_of(&home, ticks), shape_of(&startup, ticks)), (Shape::Radios, Shape::Plain, Shape::Plain));
        assert_eq!((shape_of(&new_tab, sentences), shape_of(&home, sentences), shape_of(&startup, sentences)), (Shape::Radios, Shape::Plain, Shape::Plain));
        assert_eq!((shape_of(&skin, ticks), shape_of(&skin, sentences)), (Shape::Plain, Shape::Tiles));
        assert_eq!(shape_of(&row_of(2, "ShowHidden"), sentences), Shape::Tick);
        assert_eq!(shape_of(&row_of(2, "CustomSort"), ticks), Shape::Tick, "a tick with EDIT… beside it");
        assert_eq!(shape_of(&row_of(0, "StartupFolder"), ticks), Shape::Folder);
    }

    /// The search finds a setting by its title, its note, one of its choices or its group, in any
    /// case; a shortcut by what it does.
    #[test]
    fn the_search_finds_settings() {
        let hidden = row_of(2, "ShowHidden");
        assert!(row_matches(&hidden, "Show", "hidden"));
        assert!(row_matches(&hidden, "Show", "show"), "its group");
        assert!(!row_matches(&hidden, "Show", "tray"));
        let new_tab = row_of(0, "NewTab");
        assert!(row_matches(&new_tab, "Startup", "this pc"), "a choice");
        let system = row_of(2, "ShowSystem");
        assert!(row_matches(&system, "Show", "best left"), "its note");
        let keys = Row { title: "Ctrl+H".into(), note: None, ctl: Ctl::Text("Show or hide hidden files"), key: "@Keys", indent: false, needs: None, only_if: None, more: false, radios: false, long: false };
        assert!(row_matches(&keys, "Shortcuts", "hidden") && row_matches(&keys, "Shortcuts", "ctrl+h"));
    }

    #[test]
    fn rows_need_their_switch() {
        let mut values = values_of(&Settings::default());
        let facts = Facts {
            values: &values,
            terminals: None,
            zip_said: None,
            config: String::new(),
            logs: String::new(),
            cache: String::new(),
            cache_size: None,
            cache_said: None,
            portable_said: None,
            portable_locked: false,
            skins: &[],
            update_said: String::new(),
            update_busy: false,
        };
        let tray = pages::page(0, &facts)
            .into_iter()
            .flat_map(|i| match i {
                Item::Card(rows) => rows,
                Item::Heading(_) => Vec::new(),
            })
            .find(|r| r.key == "TrayIcon")
            .unwrap();
        assert_eq!(tray.needs, Some("StayRunning"));
        // "Also show protected files" works only while hidden files are shown.
        let system = Row { key: "ShowSystem", needs: Some("ShowHidden"), ..tray.clone() };
        assert!(!usable(&values, &system, false));
        values.insert("ShowHidden".into(), Value::Bool(true));
        assert!(usable(&values, &system, false));
        // "!Name": while that switch is off.
        let folders = Row { key: "ShowSystem", needs: Some("!ShowHidden"), ..tray };
        assert!(!usable(&values, &folders, false));
    }
}
