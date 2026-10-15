//! The Settings window (Ctrl+, or the SETTINGS button): a window of its own, owned by the main
//! window and drawn by Glint like everything else. The pages are listed on the left (General,
//! Appearance, Files and folders, Mouse and keyboard, About), with cards of settings on the right:
//! each row has a short title (a note under it only where needed) and its control. A change applies
//! at once and is saved like any other choice. Settings that aren't in this version yet are shown
//! greyed, with "Coming later", so the pages already look the way they will.
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
//! Keyboard: Tab / Shift+Tab move between the page list and the controls; Up and Down change the
//! page in the page list and move between controls on a page; Space or Enter flips a switch, opens
//! a drop-down (also Alt+Down or F4) or presses a button; Ctrl+Tab and Ctrl+PgDn / PgUp change the
//! page; PgUp, PgDn, Home and End scroll; Esc closes. Arrows never change a setting by themselves.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Map, Value};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, GetMonitorInfoW, InvalidateRect, MonitorFromRect, MonitorFromWindow, RedrawWindow, ScreenToClient,
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
    IDC_ARROW, MINMAXINFO, SIZE_MAXIMIZED, SIZE_MINIMIZED, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_RESTORE, SW_SHOW, WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT, WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW,
    WS_CAPTION, WS_CLIPCHILDREN, WS_SYSMENU, WS_THICKFRAME,
};

use crate::app::AppState;
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
use crate::ui::{Command, Menu, MenuItem, Point, Rect};

mod pages;
pub use pages::ABOUT;
use pages::{Act, Ctl, Facts, Item, Row, PAGES};

/// The window's title (what Alt+Tab and the taskbar's thumbnails would call it).
pub const TITLE: &str = "Glint settings";
/// The page list's width.
const NAV_W: f32 = 210.0;
/// Space above the first page in the list.
const NAV_TOP: f32 = 14.0;
/// The page's margins round the cards.
const PAD_X: f32 = 28.0;
const PAD_TOP: f32 = 18.0;
const PAD_BOTTOM: f32 = 28.0;
/// Room kept free on the right for the scrollbar.
const GUTTER: f32 = 10.0;
const MAX_CARD_W: f32 = 780.0;
const PAGE_TITLE_H: f32 = 34.0;
const HEADING_H: f32 = 22.0;
/// Inside a row.
const ROW_PAD_X: f32 = 16.0;
const ROW_PAD_Y: f32 = 12.0;
const ROW_MIN_H: f32 = 46.0;
const TITLE_LINE: f32 = 19.0;
const NOTE_LINE: f32 = 16.0;
/// How far a row that belongs to the one above it is pulled in.
const INDENT: f32 = 22.0;
/// Between a row's text and its control.
const CONTROL_GAP: f32 = 24.0;
/// The shortcuts' first column.
const KEYS_W: f32 = 168.0;
const KEYS_ROW_H: f32 = 36.0;
/// Drop-downs are as wide as their longest choice, within these.
const DROPDOWN_MIN: f32 = 180.0;
const DROPDOWN_MAX: f32 = 320.0;
/// The window's size when nothing is remembered, and the smallest it gets (DIPs).
const DEFAULT_W: f32 = 900.0;
const DEFAULT_H: f32 = 660.0;
const MIN_W: f32 = 360.0;
const MIN_H: f32 = 420.0;
/// Docked beside the main window: this wide at first.
const DOCK_W: f32 = 480.0;
/// "Small, in the middle".
const SMALL_W: f32 = 560.0;
const SMALL_H: f32 = 540.0;
/// Narrower than this, the pages are buttons along the top instead of a list down the left.
const NARROW_W: f32 = 620.0;
/// The page buttons along the top: their margins.
const PILL_PAD: f32 = 8.0;

thread_local! {
    /// Where Settings opens ("Settings opens"), for the window's own messages (`hit_test`, `follow`).
    static OPENS: Cell<SettingsOpens> = const { Cell::new(SettingsOpens::Docked) };
}
/// One notch of the mouse wheel scrolls this far.
const WHEEL_STEP: f32 = 96.0;
/// The text box's id (textbox.rs: 400-499 belong to the Settings window).
const FOLDER_BOX: u16 = 400;
/// Under a greyed row.
const LATER: &str = "Coming later";
/// The text of the title line.
const TITLE_FONT: f32 = 12.0;

/// Results of background work for this window (see `post`).
const WM_APP_BG: u32 = WM_APP + 50;
/// Tab pressed in the text box (its own subclass tells the window; see `tab_proc`).
const WM_APP_TAB: u32 = WM_APP + 51;
/// CLEAR must be pressed a second time within this many seconds.
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
    /// A drop-down's list: a native menu owned by this window, at `menu.at` (its client DIPs).
    Menu(Menu),
    /// Windows' folder picker, starting in this folder.
    PickFolder(String),
    /// Put the window here (after a DPI change).
    Place(RECT),
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
    /// The page list.
    Nav,
    /// A control of row `.0`.
    Ctl(usize, Part),
}

/// Which of a row's controls: the main one, or the second (BROWSE next to the text box, the
/// switch next to EDIT…).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Main,
    Second,
}

/// What is under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hot {
    Close,
    Nav(usize),
    Ctl(usize, Part),
    Thumb,
}

/// Drawn parts of a page other than rows, in page coordinates (y from the top of the scrolling page).
#[derive(Clone, Debug)]
enum Block {
    Title(Rect, &'static str),
    Heading(Rect, &'static str),
    Card(Rect),
    Line(Rect),
}

/// A row as laid out.
#[derive(Clone, Debug)]
struct Placed {
    row: Row,
    /// The whole row, in page coordinates.
    rect: Rect,
    title: Vec<String>,
    note: Vec<String>,
    /// Where the text starts (across, and down from the row's top), and how wide it may be.
    text_x: f32,
    text_y: f32,
    text_w: f32,
    /// The controls (page coordinates).
    main: Option<Rect>,
    second: Option<Rect>,
    /// It can be used (it works in this version, and the switch it depends on is on).
    enabled: bool,
}

impl Placed {
    fn rect_of(&self, part: Part) -> Option<Rect> {
        match part {
            Part::Main => self.main,
            Part::Second => self.second,
        }
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
    /// The drop-down whose list is open: its setting and the values of its choices.
    menu_for: Option<(&'static str, Vec<pages::Val>)>,
    terminals: Option<[bool; 6]>,
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
    /// The text box had the keyboard when the window was switched away from (it gets it back).
    box_had_focus: bool,
    /// Space or Enter on a drop-down: its list opens when that key's character arrives (it would
    /// otherwise reach the menu and close it again).
    act_on_char: Option<(usize, Part)>,
    /// The skins for the Skin drop-down (id, name, its line about itself), as of the library's
    /// generation (skin.rs): a skin added or renamed in the customiser shows at once.
    skins: Vec<(String, String, String)>,
    skins_generation: u64,
    /// Where the window opens and stays ("Settings opens").
    opens: SettingsOpens,
    /// The page buttons along the top, when the window is narrow (`narrow`); empty otherwise.
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
/// `owner`, where `opens` says: beside it, small in its middle, over its files, or where it was
/// left (`bounds`, screen pixels; else at the size it had last time, `remembered`, DIPs, in the
/// middle). None if Windows can't make it.
pub fn create(owner: HWND, theme: &Theme, remembered: Option<[f32; 2]>, bounds: Option<[i32; 4]>, opens: SettingsOpens, page: usize) -> Option<SettingsWindow> {
    OPENS.with(|o| o.set(opens));
    register_class();
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: window creation and plain queries on our own windows; all structs are valid.
    unsafe {
        let dpi = GetDpiForWindow(owner).max(96);
        let scale = dpi as f32 / 96.0;
        let px = |v: f32| (v * scale) as i32;
        let (x, y, w, h) = match (opens, bounds) {
            (SettingsOpens::Docked, _) => beside(owner, px(DOCK_W)),
            (SettingsOpens::Small, _) => centred_on(owner, px(SMALL_W), px(SMALL_H)),
            (SettingsOpens::Tab, _) => over_files(owner, scale),
            (SettingsOpens::Free, Some([x, y, w, h])) if on_a_screen([x, y, w, h]) => (x, y, w, h),
            // None yet, or its screen is gone since (a second monitor unplugged): over the main window.
            (SettingsOpens::Free, _) => {
                let [w, h] = remembered.unwrap_or([DEFAULT_W, DEFAULT_H]);
                centred_on(owner, px(w.max(MIN_W)), px(h.max(MIN_H)))
            }
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
        // Tab in the text box moves on to the next control (an EDIT outside a dialog ignores it).
        let _ = SetWindowSubclass(folder_box.hwnd(), Some(tab_proc), 2, 0);
        let mut window = SettingsWindow {
            hwnd,
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            renderer,
            dpi,
            size: (0.0, 0.0),
            classic: false,
            started: Instant::now(),
            page: page.min(PAGES.len() - 1),
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
            menu_for: None,
            terminals: None,
            cache_size: None,
            cache_said: None,
            clear_asked_at: None,
            clearing: false,
            portable_said: None,
            portable_busy: false,
            repaint_until: 0.0,
            focus_box_soon: false,
            box_had_focus: false,
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
pub fn run_job(hwnd: HWND, scale: f32, job: Job, show_menu: fn(HWND, &Menu, f32) -> u32) -> JobDone {
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
            Job::Menu(menu) => return JobDone::Picked(show_menu(hwnd, &menu, scale)),
            Job::PickFolder(start) => return pick_folder(hwnd, &start).map_or(JobDone::Nothing, JobDone::Folder),
            Job::Follow => follow(hwnd, true),
            Job::Place(r) => {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
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

/// WM_NCHITTEST: the edges resize (frame.rs), the empty title bar moves the window, the rest is ours.
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

/// Docked: `w` wide beside `owner`, on its right if there's room, else on its left, else at the
/// screen's right edge; as tall as the main window (the visible parts, screen pixels).
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

/// As a tab: over the main window's files, below its tabs and toolbar.
fn over_files(owner: HWND, scale: f32) -> (i32, i32, i32, i32) {
    let main = visible_rect(owner);
    let top = main.top + ((size::TITLE_H + size::TOOLBAR_H) * scale) as i32;
    (main.left, top, main.right - main.left, (main.bottom - top).max(100))
}

/// Back in its place by the main window (it moved or changed size, or Settings did); `fresh`:
/// "Settings opens" just changed, so at that choice's first size too. Where you left it stays put.
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
    let (w, h) = (now.right - now.left, now.bottom - now.top);
    let px = |v: f32| (v * scale) as i32;
    let to = match OPENS.with(Cell::get) {
        SettingsOpens::Docked => beside(owner, if fresh { px(DOCK_W) } else { w }),
        SettingsOpens::Small if fresh => centred_on(owner, px(SMALL_W), px(SMALL_H)),
        SettingsOpens::Small => centred_on(owner, w, h),
        SettingsOpens::Tab => over_files(owner, scale),
        SettingsOpens::Free => return,
    };
    place_visible(hwnd, to);
}

/// The title bar isn't a caption: Settings stays where Glint put it, in the middle of the main
/// window (it follows that window, `follow_owner`). Its edges still resize it.
fn hit_test(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let opens = OPENS.with(Cell::get);
    // As a tab it's part of the main window: no edges to drag.
    if opens != SettingsOpens::Tab {
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
    // Where you left it: the title bar moves it (all but the ×).
    if opens == SettingsOpens::Free && y < size::TITLE_H && x < w - ksize::CLOSE_W {
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

    /// Too narrow for the page list down the left: the pages are buttons along the top.
    fn narrow(&self) -> bool {
        self.size.0 < NARROW_W
    }

    /// The title bar's height (none as a tab: the tab is its title).
    fn title_h(&self) -> f32 {
        if self.opens == SettingsOpens::Tab {
            0.0
        } else {
            size::TITLE_H
        }
    }

    /// The page list's width (none when the pages are along the top).
    fn nav_w(&self) -> f32 {
        if self.narrow() {
            0.0
        } else {
            NAV_W
        }
    }

    /// Where the page buttons along the top end.
    fn pills_bottom(&self) -> f32 {
        self.pills.iter().map(|p| p.bottom()).fold(self.title_h(), f32::max) + if self.pills.is_empty() { 0.0 } else { PILL_PAD - 2.0 }
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
        self.renderer.set_font(&theme.font, 1.0);
        self.renderer.set_title_font(&theme.title_font);
        self.layout_dirty = true;
        self.bar = kit::Bar::of(theme);
        self.box_at = None; // the text box is placed again, with the new font
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
        self.measure_cache();
    }

    fn measure_cache(&self) {
        let to = self.poster();
        shell::background("settings: cache size", move || post(to, Bg::CacheSize(settings::cache_size())));
    }

    // ------------------------------------------------------------ layout

    /// The scrolling part on the right.
    fn content_rect(&self) -> Rect {
        let (x, y) = (self.nav_w(), self.pills_bottom());
        Rect::new(x, y, (self.size.0 - x).max(0.0), (self.size.1 - y).max(0.0))
    }

    fn close_rect(&self) -> Rect {
        if self.opens == SettingsOpens::Tab {
            return Rect::default();
        }
        Rect::new(self.size.0 - ksize::CLOSE_W, 0.0, ksize::CLOSE_W, size::TITLE_H)
    }

    fn nav_rect(&self, i: usize) -> Rect {
        if self.narrow() {
            return self.pills.get(i).copied().unwrap_or_default();
        }
        Rect::new(0.0, self.title_h() + NAV_TOP + i as f32 * ksize::NAV_ITEM_H, NAV_W, ksize::NAV_ITEM_H)
    }

    fn track(&self) -> Rect {
        self.bar.track(self.content_rect())
    }

    /// From page coordinates to the window's (DIPs).
    fn on_screen(&self, r: Rect) -> Rect {
        Rect::new(r.x, r.y + self.content_rect().y - self.scroller.offset, r.w, r.h)
    }

    /// Lays the page out: where each heading, card, row and control goes, with the text wrapped.
    fn layout(&mut self) {
        self.layout_dirty = false;
        // Narrow: the pages along the top, as many to a line as fit.
        self.pills.clear();
        if self.narrow() {
            let (mut x, mut y) = (PILL_PAD, self.title_h() + PILL_PAD);
            for name in PAGES {
                let w = self.renderer.measure(name, &TextStyle::new(Color::hex(0)).size(ksize::NAV_FONT).semibold()).ceil() + 36.0;
                if x > PILL_PAD && x + w > self.size.0 - PILL_PAD {
                    (x, y) = (PILL_PAD, y + ksize::NAV_ITEM_H);
                }
                self.pills.push(Rect::new(x, y, w, ksize::NAV_ITEM_H));
                x += w;
            }
        }
        let content = self.content_rect();
        let pad_x = if self.narrow() { PAD_X / 2.0 } else { PAD_X };
        let card_w = (content.w - 2.0 * pad_x - GUTTER).clamp(260.0, MAX_CARD_W);
        let x0 = content.x + pad_x;
        let facts = Facts {
            values: &self.values,
            terminals: self.terminals,
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
        let items = pages::page(self.page, &facts);
        let r = &mut self.renderer;
        let mut blocks = vec![Block::Title(Rect::new(x0, PAD_TOP, card_w, PAGE_TITLE_H), PAGES[self.page])];
        let mut rows = Vec::new();
        let mut y = PAD_TOP + PAGE_TITLE_H + 10.0;
        for (n, item) in items.into_iter().enumerate() {
            match item {
                Item::Heading(text) => {
                    y += if n == 0 { 0.0 } else { 16.0 };
                    blocks.push(Block::Heading(Rect::new(x0 + 1.0, y, card_w, HEADING_H), text));
                    y += HEADING_H + 6.0;
                }
                Item::Card(card_rows) => {
                    let top = y;
                    let at = blocks.len();
                    let mut any = false;
                    for row in card_rows {
                        if row.only_if.is_some_and(|(k, v)| self.values.get(k) != Some(&v.json())) {
                            continue;
                        }
                        if any {
                            blocks.push(Block::Line(Rect::new(x0 + 1.0, y, card_w - 2.0, 1.0)));
                        }
                        any = true;
                        let enabled = usable(&self.values, &row, self.portable_busy) && !(row.key == "Theme" && self.no_twin);
                        let placed = place_row(r, row, x0, y, card_w, enabled);
                        y += placed.rect.h;
                        rows.push(placed);
                    }
                    if any {
                        blocks.insert(at, Block::Card(Rect::new(x0, top, card_w, y - top)));
                        y += 2.0;
                    }
                }
            }
        }
        self.blocks = blocks;
        self.rows = rows;
        self.scroller.set_extent(y + PAD_BOTTOM, content.h);
        // The keyboard can't stay on a control that's gone or greyed now.
        if let Spot::Ctl(i, part) = self.focus {
            if !self.rows.get(i).is_some_and(|p| p.enabled && p.rect_of(part).is_some()) {
                self.focus = Spot::Nav;
            }
        }
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
        let skin = self.values.get("Skin").and_then(Value::as_str).unwrap_or_default();
        let dark = self.values.get("Theme").and_then(Value::as_str) != Some("light");
        let no_twin = app.skins.twin(skin, !dark) == app.skins.twin(skin, dark);
        if no_twin != self.no_twin {
            self.no_twin = no_twin;
            self.layout_dirty = true;
        }
        if self.layout_dirty {
            self.layout();
        }
        let now = self.now();
        let mut more = self.scroller.tick(now) | self.flips.tick(now) || now < self.repaint_until;
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
        // The page list on the sidebar's colour (a gradient running down the whole window).
        let top = self.title_h();
        let below = Rect::new(0.0, top, self.size.0, (self.size.1 - top).max(0.0));
        self.renderer.fill_paint(below, 0.0, &theme.sidebar_paint);
        if !self.narrow() {
            kit::page_list_well(&mut self.renderer, theme, Rect::new(0.0, top, NAV_W, below.h));
        }
        if top > 0.0 {
            self.paint_caption(theme);
        }
        self.paint_nav(theme);
        self.paint_page(theme);
        if b > 0.0 {
            self.renderer.set_origin(0.0, 0.0);
            frame::paint_classic_border(&mut self.renderer, theme, Rect::new(0.0, 0.0, self.size.0 + 2.0 * b, self.size.1 + 2.0 * b));
        }
        if self.renderer.end() == FrameResult::DeviceLost {
            more = true;
        }
        self.place_box();
        if more {
            self.invalidate();
        }
    }

    fn paint_caption(&mut self, theme: &Theme) {
        let close = self.close_rect();
        let r = &mut self.renderer;
        let bar = Rect::new(0.0, 0.0, self.size.0, size::TITLE_H);
        kit::title_bar(r, theme, bar, self.active);
        let x = kit::logo(r, theme, bar);
        crate::retro::title(r, theme, Rect::new(x + 10.0, 0.0, (self.size.0 - x - 10.0 - ksize::CLOSE_W).max(0.0), size::TITLE_H), "Settings", self.active);
        let hot = self.hover == Some(Hot::Close);
        kit::close_button(r, theme, close, hot, hot && self.pressed == Some(Hot::Close), self.active);
    }

    fn paint_nav(&mut self, theme: &Theme) {
        for (i, name) in PAGES.iter().enumerate() {
            let rect = self.nav_rect(i);
            let look = Look { hover: self.hover == Some(Hot::Nav(i)), focused: self.cues && self.focus == Spot::Nav && i == self.page, ..Look::enabled() };
            kit::nav_item(&mut self.renderer, theme, rect, name, i == self.page, look);
        }
    }

    fn paint_page(&mut self, theme: &Theme) {
        let content = self.content_rect();
        self.renderer.fill_paint(content, 0.0, &theme.toolbar_paint);
        self.renderer.push_clip(content);
        let dy = content.y - self.scroller.offset;
        let shift = |r: Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
        // 98 / 2000: a card's heading is drawn with it, on its top line.
        let classic = theme.era.classic();
        for (k, block) in self.blocks.iter().enumerate() {
            let r = &mut self.renderer;
            match block {
                Block::Title(rect, text) => kit::page_title(r, theme, shift(*rect), text),
                Block::Heading(..) if classic && matches!(self.blocks.get(k + 1), Some(Block::Card(_))) => {}
                Block::Heading(rect, text) => kit::heading(r, theme, shift(*rect), text),
                Block::Card(rect) => match k.checked_sub(1).and_then(|j| self.blocks.get(j)) {
                    Some(Block::Heading(h, text)) if classic => kit::titled_group(r, theme, shift(*rect), shift(*h), text),
                    _ => kit::card(r, theme, shift(*rect)),
                },
                Block::Line(rect) => kit::separator(r, theme, rect.x, rect.y + dy, rect.w),
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

    fn look_of(&self, i: usize, part: Part) -> Look {
        let me = Hot::Ctl(i, part);
        Look {
            hover: self.hover == Some(me),
            pressed: self.pressed == Some(me) && self.hover == Some(me),
            focused: self.cues && self.focus == Spot::Ctl(i, part),
            enabled: self.rows[i].enabled,
        }
    }

    fn paint_row(&mut self, theme: &Theme, i: usize, dy: f32) {
        let p = &self.rows[i];
        let (main_look, second_look) = (self.look_of(i, Part::Main), self.look_of(i, Part::Second));
        let shift = |r: Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
        let rect = shift(p.rect);
        let live = p.row.live();
        let title_color = if p.enabled { theme.text } else { theme.muted };
        let r = &mut self.renderer;
        if let Ctl::Text(what) = &p.row.ctl {
            // A shortcut: the keys, then what they do (and, greyed, that it comes later).
            let line = if live { rect } else { Rect::new(rect.x, rect.y + 7.0, rect.w, TITLE_LINE) };
            let keys_color = if live { theme.text } else { theme.muted };
            r.text(&p.row.title, Rect::new(p.text_x, line.y, KEYS_W - 12.0, line.h), &TextStyle::new(keys_color).size(TITLE_FONT).semibold());
            let what_x = p.text_x + KEYS_W;
            let what_w = (rect.right() - ROW_PAD_X - what_x).max(0.0);
            r.text(what, Rect::new(what_x, line.y, what_w, line.h), &TextStyle::new(if live { theme.muted } else { theme.muted.with_alpha(0.7) }).size(TITLE_FONT));
            if !live {
                r.text(LATER, Rect::new(what_x, line.bottom(), what_w, NOTE_LINE), &TextStyle::new(theme.muted).size(ksize::NOTE_FONT).semibold());
            }
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
        // Where a switch is drawn: at its value, or on its way there just after a flip.
        let flips = &mut self.flips;
        let mut switch = |part: Part| {
            let on = toggle_on(&p.row.ctl, &self.values);
            flip_id(&p.row.ctl, part).map_or(if on { 1.0 } else { 0.0 }, |id| flips.at(&id, on, now))
        };
        match &p.row.ctl {
            Ctl::Toggle(_) | Ctl::ToggleNot(_) | Ctl::ToggleIn(..) => {
                if let Some(m) = main {
                    kit::toggle(r, theme, m, switch(Part::Main), main_look);
                }
            }
            Ctl::Choice(key, choices) => {
                if let Some(m) = main {
                    let current = self.values.get(*key);
                    let label = choices.iter().find(|(v, _)| Some(&v.json()) == current).map(|(_, l)| l.clone()).unwrap_or_else(|| match current {
                        Some(Value::String(s)) => s.clone(),
                        _ => String::new(),
                    });
                    kit::dropdown(r, theme, m, &label, main_look);
                }
            }
            Ctl::Button(label, _) => {
                if let Some(m) = main {
                    kit::button(r, theme, m, label, main_look);
                }
            }
            Ctl::ButtonToggle(label, _, _) => {
                if let Some(m) = main {
                    kit::button(r, theme, m, label, main_look);
                }
                if let Some(s) = second {
                    kit::toggle(r, theme, s, switch(Part::Second), second_look);
                }
            }
            Ctl::Folder => {
                if let Some(m) = main {
                    // The real text box sits on top while the whole of it is in view; otherwise the
                    // frame shows the text itself.
                    let shown = self.box_at.is_some();
                    let text = self.values.get("StartupFolder").and_then(Value::as_str).unwrap_or("");
                    kit::field(r, theme, m, (!shown).then_some(text), shown && self.folder_box.has_focus(), main_look);
                }
                if let Some(s) = second {
                    kit::button(r, theme, s, "BROWSE…", second_look);
                }
            }
            Ctl::None | Ctl::Text(_) => {}
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
                if frame::is_wine() {
                    // Wine shows the hidden box until a later frame replaces it.
                    self.repaint_until = self.now() + 0.3;
                    self.invalidate();
                }
            }
        }
        self.box_at = want;
        if self.focus_box_soon && self.box_at.is_some() {
            self.focus_box_soon = false;
            self.folder_box.focus();
            self.folder_box.select_all();
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
        for (i, placed) in self.rows.iter().enumerate() {
            for part in [Part::Main, Part::Second] {
                let Some(r) = placed.rect_of(part).map(|r| self.on_screen(r)) else { continue };
                let hit = match (&placed.row.ctl, part) {
                    (Ctl::Toggle(_) | Ctl::ToggleNot(_) | Ctl::ToggleIn(..), _) | (Ctl::ButtonToggle(..), Part::Second) => kit::toggle_contains(r, p),
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
        let mut spots = vec![Spot::Nav];
        for (i, p) in self.rows.iter().enumerate() {
            if !p.enabled {
                continue;
            }
            for part in [Part::Main, Part::Second] {
                if p.rect_of(part).is_some() {
                    spots.push(Spot::Ctl(i, part));
                }
            }
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
        if let Spot::Ctl(i, part) = spot {
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
                self.scroller.reveal(row.y, row.bottom(), 16.0, self.now());
            }
        } else if self.folder_box.has_focus() {
            self.folder_box.give_focus_back();
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
        }
        self.focus = Spot::Nav;
        self.cues = keyboard;
        if self.folder_box.has_focus() {
            self.folder_box.give_focus_back();
        }
        self.invalidate();
    }

    /// A key, when the window (not the text box) has the keyboard. Returns whether it was used.
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
        match () {
            _ if is(VK_ESCAPE) || (ctrl && vk == 'W' as u16) => asks.push(Ask::Job(Job::Close)),
            _ if (ctrl && is(VK_TAB) && !shift) || (ctrl && is(VK_NEXT)) => self.go_to_page((self.page + 1) % n, true),
            _ if (ctrl && is(VK_TAB) && shift) || (ctrl && is(VK_PRIOR)) => self.go_to_page((self.page + n - 1) % n, true),
            _ if is(VK_TAB) => self.move_focus(if shift { -1 } else { 1 }, true),
            // Light or dark, as in the main window.
            _ if ctrl && shift && vk == 'L' as u16 => asks.push(Ask::Command(Command::ToggleTheme)),
            _ if ctrl && is(VK_OEM_COMMA) => {}
            _ if self.focus == Spot::Nav && (is(VK_UP) || is(VK_DOWN)) => {
                let page = if is(VK_UP) { self.page.saturating_sub(1) } else { (self.page + 1).min(n - 1) };
                self.go_to_page(page, true);
            }
            _ if self.focus == Spot::Nav && (is(VK_RIGHT) || is(VK_RETURN)) => self.move_focus(1, false),
            // (Not Alt+Down: that opens a drop-down, below.)
            _ if !alt && (is(VK_UP) || is(VK_DOWN)) => self.move_focus(if is(VK_UP) { -1 } else { 1 }, false),
            _ if is(VK_LEFT) && self.focus != Spot::Nav => self.set_focus(Spot::Nav),
            _ if is(VK_SPACE) || is(VK_RETURN) || is(VK_F4) || (alt && is(VK_DOWN)) => {
                self.cues = true;
                if let Spot::Ctl(i, part) = self.focus {
                    let dropdown = matches!(self.rows.get(i).map(|p| &p.row.ctl), Some(Ctl::Choice(..)));
                    if dropdown && (is(VK_SPACE) || is(VK_RETURN)) {
                        // The character Windows makes from Space or Enter is already on its way; in
                        // the drop-down's menu it would close the menu again. Open it when the
                        // character has come (WM_CHAR).
                        self.act_on_char = Some((i, part));
                    } else if dropdown || !(is(VK_F4) || alt) {
                        asks.extend(self.activate(i, part, app));
                    }
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
        if app.settings.settings_opens != self.opens {
            // "Settings opens" changed: to its new place now (a job: moving sends messages).
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
        // A switch flipped by the person glides to its new place (if its value really changes; one
        // that changes later, Portable's, just shows its new place then).
        if let Some(id) = flip_id(&ctl, part) {
            let now = self.now();
            self.flips.start(&id, !toggle_on(&ctl, &self.values), now, kit::SWITCH_TIME);
        }
        match (ctl, part) {
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
                let items = choices.iter().enumerate().map(|(n, (v, label))| MenuItem::new(n as u32 + 1, label).checked(Some(v.json()) == current)).collect();
                self.menu_for = Some((k, choices.iter().map(|(v, _)| *v).collect()));
                let b = self.border();
                vec![Ask::Job(Job::Menu(Menu { tag: 1, at: Point::new(at.x + b, at.bottom() + 2.0 + b), items, right_aligned: false }))]
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
            (Ctl::None | Ctl::Text(_), _) => Vec::new(),
        }
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
            // (Greyed until there's a Discord: the row isn't in LIVE.)
            Act::Discord => {}
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

    /// What a job brought back (a drop-down's pick, a folder from the picker).
    pub fn job_done(&mut self, done: JobDone, app: &mut AppState) -> Vec<Ask> {
        match done {
            JobDone::Nothing => Vec::new(),
            JobDone::Picked(id) => {
                let Some((key, values)) = self.menu_for.take() else { return Vec::new() };
                let Some(v) = id.checked_sub(1).and_then(|n| values.get(n as usize)) else { return Vec::new() };
                let asks = self.set_value(app, key, v.json());
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
            // Resized from an edge: back to the middle of the main window.
            WM_EXITSIZEMOVE => {
                self.follow_owner();
                // Where you left it: remembered for next time.
                let mut r = RECT::default();
                // SAFETY: plain queries on our own window.
                if self.opens == SettingsOpens::Free && unsafe { !IsZoomed(self.hwnd).as_bool() && GetWindowRect(self.hwnd, &mut r).is_ok() } {
                    app.settings.settings_bounds = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
                }
                Some(LRESULT(0))
            }
            WM_SIZE => {
                let (w, h) = (lo_word(lparam.0 as usize), hi_word(lparam.0 as usize));
                self.renderer.resize(w, h);
                if w > 0 && h > 0 {
                    let s = self.scale();
                    self.size = self.inner_size(w, h);
                    self.layout_dirty = true;
                    // Remembered for next time (the window's outer size, in DIPs).
                    let mut r = RECT::default();
                    // SAFETY: plain queries on our own window.
                    if self.opens == SettingsOpens::Free && wparam.0 != SIZE_MAXIMIZED as usize && unsafe { !IsZoomed(self.hwnd).as_bool() && GetWindowRect(self.hwnd, &mut r).is_ok() } {
                        let size = [((r.right - r.left) as f32 / s).round(), ((r.bottom - r.top) as f32 / s).round()];
                        if app.settings.settings_size != Some(size) {
                            app.settings.settings_size = Some(size);
                            self.values = values_of(&app.settings);
                        }
                    }
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
                if !self.active {
                    self.box_had_focus = self.folder_box.has_focus();
                    None
                } else if std::mem::take(&mut self.box_had_focus) && self.box_at.is_some() {
                    // Typing goes on where it was, instead of Windows giving the keyboard to the window.
                    self.folder_box.focus();
                    Some(LRESULT(0))
                } else {
                    None
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
                Some(LRESULT(0))
            }
            WM_APP_TAB => {
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

/// Lays out one row at `y` (page coordinates) in a card `card_w` wide starting at `x0`.
fn place_row(r: &mut Renderer, row: Row, x0: f32, y: f32, card_w: f32, enabled: bool) -> Placed {
    let left = x0 + ROW_PAD_X + if row.indent { INDENT } else { 0.0 };
    let right = x0 + card_w - ROW_PAD_X;
    let title_style = TextStyle::new(Color::hex(0)).size(TITLE_FONT);
    let note_style = TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT);
    let full = (right - left).max(40.0);
    let mut placed = Placed { rect: Rect::new(x0, y, card_w, ROW_MIN_H), title: Vec::new(), note: Vec::new(), text_x: left, text_y: ROW_PAD_Y, text_w: full, main: None, second: None, enabled, row };
    match &placed.row.ctl {
        Ctl::Text(_) => {
            placed.rect.h = if placed.row.live() { KEYS_ROW_H } else { 7.0 + TITLE_LINE + NOTE_LINE + 8.0 };
            return placed;
        }
        Ctl::Folder => {
            let pad = 8.0;
            let browse = kit::button_width(r, "BROWSE…");
            let box_x = x0 + ROW_PAD_X + INDENT;
            placed.main = Some(Rect::new(box_x, y + pad, (right - browse - 12.0 - box_x).max(80.0), ksize::FIELD_H));
            placed.second = Some(Rect::new(right - browse, y + pad + (ksize::FIELD_H - ksize::BUTTON_H) / 2.0, browse, ksize::BUTTON_H));
            placed.rect.h = ksize::FIELD_H + 2.0 * pad;
            return placed;
        }
        _ => {}
    }
    // The controls' sizes: (main width, main height, second width).
    let (w, h, second_w) = match &placed.row.ctl {
        Ctl::Toggle(_) | Ctl::ToggleNot(_) | Ctl::ToggleIn(..) => (ksize::TOGGLE_W, ksize::TOGGLE_H, 0.0),
        Ctl::Choice(_, choices) => (kit::dropdown_width(r, choices.iter().map(|(_, l)| l.as_str()), DROPDOWN_MIN, DROPDOWN_MAX), ksize::FIELD_H, 0.0),
        Ctl::Button(label, _) => (kit::button_width(r, label), ksize::BUTTON_H, 0.0),
        Ctl::ButtonToggle(label, ..) => (kit::button_width(r, label), ksize::BUTTON_H, ksize::TOGGLE_W),
        _ => (0.0, 0.0, 0.0),
    };
    let controls = if w > 0.0 { w + if second_w > 0.0 { 14.0 + second_w } else { 0.0 } } else { 0.0 };
    let text_w = (full - if controls > 0.0 { controls + CONTROL_GAP } else { 0.0 }).max(80.0);
    placed.text_w = text_w;
    placed.title = if placed.row.title.is_empty() { Vec::new() } else { kit::wrap(r, &placed.row.title, &title_style, text_w) };
    if let Some(note) = &placed.row.note {
        placed.note = kit::wrap(r, note, &note_style, text_w);
    }
    let later = if placed.row.live() { 0.0 } else { NOTE_LINE };
    let text_h = placed.title.len() as f32 * TITLE_LINE + placed.note.len() as f32 * NOTE_LINE + later;
    let row_h = (text_h.max(h) + 2.0 * ROW_PAD_Y).max(ROW_MIN_H).ceil();
    placed.rect.h = row_h;
    // The text in the middle of the row, beside a taller control too.
    placed.text_y = ((row_h - text_h) / 2.0).round();
    if w > 0.0 {
        let x = right - controls;
        placed.main = Some(Rect::new(x, y + ((row_h - h) / 2.0).round(), w, h));
        if second_w > 0.0 {
            placed.second = Some(Rect::new(right - second_w, y + ((row_h - ksize::TOGGLE_H) / 2.0).round(), second_w, ksize::TOGGLE_H));
        }
    }
    placed
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
    fn rows_need_their_switch() {
        let mut values = values_of(&Settings::default());
        let facts = Facts {
            values: &values,
            terminals: None,
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
