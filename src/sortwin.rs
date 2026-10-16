//! The custom sort order's editor (SORT > Edit custom order…, Settings > Files and folders > EDIT…):
//! a window of Glint's own, drawn in the Settings window's style (kit.rs), docked beside the main
//! window and moving and resizing with it (window/sorting.rs keeps it there). DETACH in its title
//! bar lets it go: then it moves and resizes on its own, like Settings, and is remembered that way
//! ("SortDetached", "SortBounds"); DOCK puts it back.
//!
//! - **The rules box**, drawn by Glint with a monospaced font (so it can show more than an EDIT
//!   could): line numbers down its left side (a red dot by a line with a mistake, a triangle by one
//!   with a tip), order rules in the accent colour, notes muted, a red line under a word Glint doesn't
//!   know, and suggestions like Minecraft's command box (complete.rs) - the first one as grey text
//!   after the cursor, up to eight in a list under the line. Tab (or Enter while the list is open)
//!   takes one, Up / Down pick, Esc closes the list. The folder's rules the cursor is in are tinted
//!   (with their brackets marked when the cursor is on either), faint words after the cursor's line
//!   say what it does, and Enter after an "in … (" whose ")" never came adds it. Ctrl+Z takes back
//!   typing a word at a time (Ctrl+Y puts it back; kept while Glint runs), Ctrl+/ makes lines notes
//!   and back, Alt+Up / Alt+Down move them, and right-clicking gives a menu of all that.
//! - **On screen** above it: which rules sort the folder in the main window (a bar beside them in
//!   the rules box), or that custom sort is off or a column sorts it for now, with a link to change
//!   that.
//! - **Problems** under it, every one, wrapped so they can be read whole (scrolling when there are
//!   many): the line's mistake first; one with a fix ("did you mean -newest?") is a link that fixes
//!   it, and clicking the words goes to the line. A folder written in the rules that isn't there is
//!   a tip, with the closest folder beside it as the fix.
//! - **The switch** (custom sort on or off), **START FROM…** (ready-made orders: it asks before
//!   replacing rules of your own) and **GUIDE**, which shows a short guide at the bottom (hidden at
//!   first; "SortGuideShown"). The rules box takes the rest of the window: the main window beside
//!   it shows the result.
//!
//! Every change is used a moment after typing stops (APPLY_TIMER): every open tab, and
//! settings.json ("SortRecipe"). Like the skin customiser, its state lives in the main window's
//! state and its messages go through the main window's guard; menus, showing and closing come back
//! as a [`Job`].

use std::cell::Cell;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromRect, MonitorFromWindow, ScreenToClient, ValidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MONITOR_DEFAULTTONULL,
};
use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DIVIDE, VK_DOWN,
    VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_OEM_2, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_V, VK_X, VK_Y, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindow, GetWindowLongPtrW, GetWindowRect, IsIconic, KillTimer,
    LoadCursorW, LoadIconW, RegisterClassExW, SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, CS_DBLCLKS, GWL_STYLE,
    GW_OWNER, HTCAPTION, HTCLIENT, IDC_ARROW, IDC_HAND, IDC_IBEAM, MINMAXINFO, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, SW_SHOWNOACTIVATE, WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_APP,
    WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST,
    WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN, WS_POPUP, WS_SYSMENU,
    WS_THICKFRAME,
};

use crate::app::AppState;
use crate::complete::{self, Completion, Lister, Place};
use crate::folder::{self, EntryKind};
use crate::frame;
use crate::kit::{self, size as ksize, Bar, Look};
use crate::log;
use crate::render::{Align, FrameResult, Renderer, TextStyle};
use crate::shell;
use crate::sortrecipe::{self, Message, Recipe, Source, Written};
use crate::theme::{size, Color, Theme};
use crate::ui::{Menu, MenuItem, Point, Rect};
use crate::codebox::{at_col, clipboard_text, col_of, key_down, line_end, line_of, next_char, prev_char, swap_lines, whole_lines, Track};
use crate::winutil::{hi_word, lo_word};

pub const TITLE: &str = "Sort order and views";

/// The window's width beside the main window, in DIPs.
const WIDTH: f32 = 470.0;
const PAD: f32 = 16.0;
/// The rules box's text: size and line height.
const MONO: f32 = 12.5;
const LINE_H: f32 = 19.0;
const BOX_PAD: f32 = 8.0;
/// A problem's first line, and each line it wraps onto.
const PROBLEM_H: f32 = 18.0;
const PROBLEM_LINE: f32 = 15.0;
/// At most this many lines of one problem (the last ends with "…" when there's more).
const PROBLEM_MAX_LINES: usize = 4;
/// The most of the window's height the problems take (they scroll when there are more).
const PROBLEMS_MAX: f32 = 0.34;
/// Room at the problems' right for their scrollbar.
const PROBLEM_BAR: f32 = 10.0;
/// The line saying which rules sort the folder on screen.
const HERE_H: f32 = 18.0;
/// The DETACH / DOCK button in the title bar (left of ×), in DIPs.
const DETACH_W: f32 = 76.0;
/// The smallest the window gets when it's on its own, in DIPs.
const MIN_W: f32 = 360.0;
const MIN_H: f32 = 320.0;
/// The line numbers' column: room for a problem's mark, and the gap after the numbers.
const GUTTER_MARK: f32 = 12.0;
const GUTTER_GAP: f32 = 10.0;
const GUIDE_LINE: f32 = 16.0;
/// The guide's example lines: the rules box's font, a little smaller.
const GUIDE_MONO: f32 = 11.5;
const POPUP_ROW: f32 = 22.0;
const POPUP_MAX: usize = 8;
const POPUP_W: f32 = 330.0;
/// The red of a word Glint doesn't know.
const PROBLEM_RED: Color = Color::hex(0xe5484d);

thread_local! {
    /// The skin is 98 / 2000's: the window has their frame round it (for `hit_test`, which runs
    /// without the window's state).
    static CLASSIC: Cell<bool> = const { Cell::new(false) };
}

/// A folder's subfolders were listed for the suggestions (complete::Lister).
const WM_APP_LISTED: u32 = WM_APP + 86;
/// Typing stopped: use the rules (tabs, settings).
const APPLY_TIMER: usize = 1;
const APPLY_DELAY_MS: u32 = 250;
/// Looks for a new folder on screen in the main window (its subfolders are suggested after "in").
const WATCH_TIMER: usize = 2;
const WATCH_MS: u32 = 400;
const MENU_START_FROM: u32 = 1;
/// The rules box's right-click menu, and its items (START FROM…'s are 1 and up, one per order).
const MENU_EDIT: u32 = 2;
const EDIT_UNDO: u32 = 101;
const EDIT_REDO: u32 = 102;
const EDIT_CUT: u32 = 103;
const EDIT_COPY: u32 = 104;
const EDIT_PASTE: u32 = 105;
const EDIT_ALL: u32 = 106;
const EDIT_NOTE: u32 = 107;
const EDIT_UP: u32 = 108;
const EDIT_DOWN: u32 = 109;
const EDIT_SUGGEST: u32 = 110;
/// Typing that goes on within this long (seconds) is taken back by one Ctrl+Z.
const GROUP_SECS: f64 = 2.0;

/// Something the main window does for this window after the message.
#[derive(Clone, Debug)]
pub enum Job {
    /// Show this window (just made) beside the main window.
    Show,
    /// Bring it to the front (opened again while it's open).
    Activate,
    /// A menu at `menu.at` (this window's client DIPs).
    Menu(Menu),
    /// Put the window here (beside the main window).
    Place(RECT),
    /// Close it and open it again (docked or on its own, as the settings now say): its frame is
    /// set when it's made.
    Reopen,
    Close,
}

/// What a job brought back.
#[derive(Clone, Debug, PartialEq)]
pub enum JobDone {
    Nothing,
    /// The menu item picked (0: none).
    Picked(u32),
}

/// What the main window should do after this window handled something.
#[derive(Clone, Debug)]
pub enum Ask {
    Job(Job),
    /// Paint the main window again (its tabs were sorted again).
    RedrawMain,
}

/// The things that react to the mouse.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hot {
    Close,
    Detach,
    Switch,
    StartFrom,
    Guide,
    Fix(usize),
    /// A problem's words: clicking goes to its line.
    Problem(usize),
    /// The words saying which rules sort the folder on screen (clicking goes to them), and its link.
    Here,
    HereLink,
    /// START FROM…'s question: replace the rules, or keep them.
    Replace,
    Keep,
    /// The rules box's scrollbars (down, across) and the problems'.
    VBar,
    HBar,
    PBar,
    Editor,
    Suggestion(usize),
}

/// Which rules sort the folder on screen, as the line above the rules box says it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Here {
    text: String,
    link: Option<HereLink>,
    /// The line its words go to (from 1).
    line: Option<usize>,
    /// The blocks that sort it (first and last lines, from 1): marked down the rules box's side.
    marked: Vec<(usize, usize)>,
}

/// What the line's link does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum HereLink {
    /// Custom sort is off: turn it on.
    TurnOn,
    /// A column sorts the folder for now: back to the rules.
    UseRules,
}

impl HereLink {
    fn label(self) -> &'static str {
        match self {
            HereLink::TurnOn => "Turn it on",
            HereLink::UseRules => "Use the rules",
        }
    }
}

/// A kind of change, for Ctrl+Z to take back typing a word at a time.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Edit {
    Typing,
    Deleting,
    /// Anything else (pasting, a suggestion, Enter): taken back on its own.
    Other,
}

/// Where everything is, worked out before each frame.
#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    close: Rect,
    detach: Rect,
    switch: Rect,
    start_from: Rect,
    guide_button: Rect,
    here: Rect,
    /// START FROM…'s question (empty when not asking), and its buttons.
    confirm: Rect,
    replace: Rect,
    keep: Rect,
    editor: Rect,
    problems: Rect,
    guide: Rect,
}

pub struct SortWindow {
    hwnd: HWND,
    renderer: Renderer,
    dpi: u32,
    size: (f32, f32),
    started: Instant,
    active: bool,
    tracking: bool,
    hover: Option<Hot>,
    pressed: Option<Hot>,
    /// Dragging in the rules box to select.
    selecting: bool,
    lay: Layout,
    /// The rules as typed, the cursor and the other end of the selection (byte offsets).
    text: String,
    caret: usize,
    anchor: usize,
    /// Where the cursor wants to be across lines (Up / Down keep the column).
    want_col: Option<usize>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    /// The last change: what it was, where the cursor was after it, and when (typing that goes on
    /// joins it, for Ctrl+Z).
    last_edit: Option<(Edit, usize, Instant)>,
    /// The first half of a character typed in two (an emoji, say), waiting for the second.
    half_char: Option<u16>,
    /// The rules box's scroll, in DIPs (across, down).
    scroll: (f32, f32),
    /// The problems' scroll and full height, and the cursor's line they were last sorted for.
    problem_scroll: f32,
    problem_content: f32,
    problems_for: usize,
    /// The scrollbars as last drawn (when there's something to scroll), the skin's bar size, and
    /// the one being dragged with where its thumb is held.
    vtrack: Option<Track>,
    htrack: Option<Track>,
    ptrack: Option<Track>,
    bar: Bar,
    drag: Option<(Hot, f32)>,
    /// Which rules sort the folder on screen, and the words and link as last drawn.
    here: Here,
    /// Its words, wrapped to fit (at most two lines).
    here_lines: Vec<String>,
    here_words: Rect,
    here_link: Rect,
    /// A ready-made order picked in START FROM… that would replace rules of your own: asked first.
    confirm: Option<usize>,
    confirm_lines: Vec<String>,
    char_w: f32,
    recipe: Recipe,
    completion: Completion,
    pick: usize,
    /// Esc closed the list: it stays closed until the next change.
    list_closed: bool,
    /// Ctrl+Space opened the list: it shows what fits even where nothing's typed, until the next
    /// change (or the list closes).
    asked: bool,
    places: Vec<Place>,
    lister: Lister,
    /// The text last used (in the tabs and settings.json).
    applied: String,
    apply_due: bool,
    /// The folder on screen when the suggestions' places were made (they follow it).
    seen: Option<Seen>,
    /// On its own (DETACH), not beside the main window.
    detached: bool,
    /// The problems shown, wrapped to fit: (message number, its lines).
    problem_lines: Vec<(usize, Vec<String>)>,
    /// The problems' links, as last drawn: (where, message number).
    fixes: Vec<(Rect, usize)>,
    /// The problems' words, as last drawn: (where, message number).
    problem_rects: Vec<(Rect, usize)>,
    /// Suggestions' rows, as last drawn.
    suggestion_rects: Vec<Rect>,
    flips: kit::Flips,
}

/// The guide: an example first (lines starting with "|", shown as typed), then the words.
const GUIDE: &[&str] = &[
    "For example:",
    "|in Downloads (",
    "|newest_first      # newest at the top",
    "|folder_last       # folders at the bottom",
    "|)",
    "|in Pictures (",
    "|large_icons       # opens with large icons",
    "|newest_first",
    "|)",
    "A folder's rules go between \"in <folder> (\" and \")\". Rules outside them are for every other folder.",
    "Several folders, the same rules: write \"in = (\", then each folder's path on a line of its own, then the rules, then \")\".",
    "Add and_subfolders to a folder's rules to give them to every folder inside it too.",
    "Sort:  newest, oldest, biggest, smallest, a_to_z, by_type",
    "Kinds:  folder, file, image, video, music, document, archive, program, code, font, shortcut, hidden, other (everything else)",
    "When:  today, yesterday, updated_3_days, added_2_hours, older_than_1_year",
    "Size:  bigger_than_100mb, smaller_than_1mb, empty (files with nothing in them)",
    "Name:  name_has_invoice, name_starts_with_img, name_ends_with_final",
    "One file type:  png, mp4, exe\u{2026} (any other type with a dot:  .blend)",
    "A sort on its own line is for everything. After a kind, on the same line, it's just for that kind:  folder a_to_z",
    "Add _first or _last to put a kind at the very top or bottom:  folder_first",
    "View:  details, list, tree, medium_icons, large_icons, extra_large_icons",
    "A folder with only a view, like large_icons, keeps the sort every other folder uses.",
    "More:  updated_first, newest_by_day",
    "The - in front is up to you:  -newest and newest are the same.",
    "A line starting with # is a note.",
    "Ctrl+Space shows the words that fit where the cursor is.",
    "Ctrl+/ turns a line into a note and back. Alt+Up and Alt+Down move a line.",
];

/// The GUIDE button's words: it shows the guide, or hides it.
fn guide_label(app: &AppState) -> &'static str {
    if app.settings.sort_guide_shown {
        "HIDE GUIDE"
    } else {
        "GUIDE"
    }
}

/// Registers the window class (once; registering again just fails harmlessly).
fn register_class() {
    // SAFETY: plain class registration with a static name and our own window procedure.
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("GlintSortWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

/// Where the editor goes beside `owner` (screen pixels): on its right if there's room, else on its
/// left, else at the screen's right edge; as tall as the main window.
pub fn dock_rect(owner: HWND) -> RECT {
    // SAFETY: plain queries on our own windows with valid out-structs.
    unsafe {
        let mut main = RECT::default();
        // The visible frame (Windows 10 / 11 add invisible borders to the window's rectangle).
        if DwmGetWindowAttribute(owner, DWMWA_EXTENDED_FRAME_BOUNDS, &mut main as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).is_err() || main.right <= main.left {
            let _ = GetWindowRect(owner, &mut main);
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let work = if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() { info.rcWork } else { main };
        let scale = GetDpiForWindow(owner).max(96) as f32 / 96.0;
        let w = ((WIDTH * scale) as i32).min((work.right - work.left).max(200));
        let x = if main.right + w <= work.right {
            main.right
        } else if main.left - w >= work.left {
            main.left - w
        } else {
            (work.right - w).max(work.left)
        };
        let top = main.top.max(work.top);
        let bottom = main.bottom.min(work.bottom).max(top + 200);
        RECT { left: x, top, right: x + w, bottom }
    }
}

/// The window's styles: docked, a plain popup beside the main window; on its own, with a frame that
/// moves and resizes (drawn like Settings' through frame.rs).
fn styles(detached: bool) -> WINDOW_STYLE {
    if detached {
        WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN
    } else {
        WS_POPUP | WS_CLIPCHILDREN
    }
}

/// Whether the window is on its own (it has a frame then).
fn is_detached(hwnd: HWND) -> bool {
    // SAFETY: a plain query on our own window.
    unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_THICKFRAME.0 != 0 }
}

/// Where the editor goes when it's let go (screen pixels): where it was last on its own, if that's
/// still on a screen; else a little off where it is (`now`), so it's seen to come away.
fn detached_rect(remembered: Option<[i32; 4]>, now: RECT, scale: f32) -> RECT {
    if let Some([x, y, w, h]) = remembered {
        let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
        // SAFETY: a plain query.
        if w > 0 && h > 0 && !unsafe { MonitorFromRect(&r, MONITOR_DEFAULTTONULL) }.is_invalid() {
            return r;
        }
    }
    let step = (24.0 * scale) as i32;
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: plain queries with a valid out-struct.
    let work = unsafe { if GetMonitorInfoW(MonitorFromRect(&now, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() { info.rcWork } else { now } };
    let (w, h) = (now.right - now.left, (now.bottom - now.top - 2 * step).max((MIN_H * scale) as i32));
    let x = (now.left - step).clamp(work.left, (work.right - w).max(work.left));
    let y = (now.top + step).clamp(work.top, (work.bottom - h).max(work.top));
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

/// Makes the editor (hidden: show it with [`Job::Show`] once its state is kept), owned by `owner`
/// and docked beside it (or on its own, where it was, if it was let go last time). None if Windows
/// can't make it.
pub fn create(owner: HWND, app: &mut AppState) -> Option<SortWindow> {
    register_class();
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    let detached = app.settings.sort_detached;
    let docked = dock_rect(owner);
    // SAFETY: a plain query.
    let scale = unsafe { GetDpiForWindow(owner) }.max(96) as f32 / 96.0;
    let r = if detached { detached_rect(app.settings.sort_bounds, docked, scale) } else { docked };
    // SAFETY: window creation and plain queries on our own windows.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GlintSortWindow"),
            PCWSTR(title.as_ptr()),
            styles(detached),
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
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
                log::error(&format!("The sort editor couldn't draw: {e}"));
                let _ = DestroyWindow(hwnd);
                return None;
            }
        };
        frame::init(hwnd);
        let text = app.settings.sort_recipe.clone().unwrap_or_else(|| sortrecipe::STARTER.to_string());
        let mut window = SortWindow {
            hwnd,
            renderer,
            dpi,
            size: (0.0, 0.0),
            started: Instant::now(),
            active: true,
            tracking: false,
            hover: None,
            pressed: None,
            selecting: false,
            lay: Layout::default(),
            recipe: Recipe::parse(&text),
            applied: text.clone(),
            caret: 0,
            anchor: 0,
            text,
            want_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            half_char: None,
            scroll: (0.0, 0.0),
            problem_scroll: 0.0,
            problem_content: 0.0,
            problems_for: 0,
            vtrack: None,
            htrack: None,
            ptrack: None,
            bar: Bar::default(),
            drag: None,
            here: Here::default(),
            here_lines: Vec::new(),
            here_words: Rect::default(),
            here_link: Rect::default(),
            confirm: None,
            confirm_lines: Vec::new(),
            char_w: 0.0,
            completion: Completion::default(),
            pick: 0,
            list_closed: true,
            asked: false,
            places: Vec::new(),
            lister: Lister::default(),
            apply_due: false,
            seen: None,
            detached,
            problem_lines: Vec::new(),
            fixes: Vec::new(),
            problem_rects: Vec::new(),
            suggestion_rects: Vec::new(),
            flips: kit::Flips::default(),
        };
        window.apply_theme(&app.theme);
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_ok() {
            window.size = window.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
        }
        window.renderer.resize(rc.right as u32, rc.bottom as u32);
        window.places = places(app);
        window.seen = Some(seen(app.tab()));
        // Made again for DETACH / DOCK: the cursor and the scroll where they were.
        if let Some((caret, scroll)) = app.sort_caret.take() {
            if caret <= window.text.len() && window.text.is_char_boundary(caret) {
                (window.caret, window.anchor) = (caret, caret);
            }
            window.scroll.1 = scroll.max(0.0);
        }
        // Ctrl+Z still takes back what was typed before it was closed (or made again), while the
        // rules are as it left them.
        if let Some((text, undo, redo)) = app.sort_history.take() {
            if text == window.text {
                (window.undo, window.redo) = (undo, redo);
            }
        }
        window.check_places();
        window.here = here(app, &window.recipe);
        let _ = SetTimer(Some(hwnd), WATCH_TIMER, WATCH_MS, None);
        Some(window)
    }
}

/// Runs a job for the window `hwnd` (no state is touched: menus run their own message loop).
pub fn run_job(hwnd: HWND, scale: f32, job: Job, show_menu: fn(HWND, &Menu, f32) -> u32) -> JobDone {
    // SAFETY: window operations on our own window, with no borrow of any state held.
    unsafe {
        match job {
            Job::Show => {
                frame::cloak(hwnd, true);
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                let _ = InvalidateRect(Some(hwnd), None, false);
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
            Job::Place(r) => {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            // The main window does this one (window/sorting.rs): it makes the new window.
            Job::Reopen => {}
            Job::Close => {
                // The keyboard goes back to the main window first, so it doesn't land elsewhere.
                if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                    let _ = SetForegroundWindow(owner);
                }
                let _ = DestroyWindow(hwnd);
            }
        }
    }
    JobDone::Nothing
}

/// The window procedure: the rest goes to the state, which the main window keeps, through its
/// guard (window::sorting::sort_message).
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // The skin's own menus (oldmenu.rs).
    if let Some(r) = crate::oldmenu::handle(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        // On its own: Glint's own title bar inside Windows' frame, as in Settings.
        // SAFETY: lparam is the NCCALCSIZE_PARAMS Windows passed with this message.
        WM_NCCALCSIZE if wparam.0 != 0 && is_detached(hwnd) => return unsafe { frame::nc_calc_size(hwnd, wparam, lparam) },
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCHITTEST => return hit_test(hwnd, wparam, lparam),
        WM_GETMINMAXINFO if is_detached(hwnd) => {
            // SAFETY: lparam points to the MINMAXINFO Windows passed.
            unsafe {
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                info.ptMinTrackSize = POINT { x: (MIN_W * scale) as i32, y: (MIN_H * scale) as i32 };
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: plain query; the owner is the main window (set when this window was made).
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
    if let Some(r) = crate::window::sorting::sort_message(owner, hwnd, msg, wparam, lparam) {
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
        return LRESULT(0); // closing goes through the state
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// WM_NCHITTEST. Docked, it sits beside the main window and goes where it goes: nothing to drag or
/// resize. On its own, the edges resize it (frame.rs) and the empty title bar moves it.
fn hit_test(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if !is_detached(hwnd) {
        return LRESULT(HTCLIENT as isize);
    }
    if let Some(edge) = frame::resize_edge(hwnd, wparam, lparam) {
        return LRESULT(edge as isize);
    }
    let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
    let mut rc = RECT::default();
    // SAFETY: plain queries on our own window with valid out-structs.
    let scale = unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
        let _ = GetClientRect(hwnd, &mut rc);
        GetDpiForWindow(hwnd).max(96) as f32 / 96.0
    };
    // 98 / 2000: the title bar is inside the window's frame.
    let b = if CLASSIC.with(Cell::get) && !frame::zoomed(hwnd) { frame::CLASSIC_BORDER } else { 0.0 };
    let (x, y, w) = (pt.x as f32 / scale - b, pt.y as f32 / scale - b, rc.right as f32 / scale - 2.0 * b);
    let buttons = w - ksize::CLOSE_W - DETACH_W - 8.0;
    let code = if (0.0..size::TITLE_H).contains(&y) && x < buttons { HTCAPTION } else { HTCLIENT };
    LRESULT(code as isize)
}

/// The folder on screen: which tab, where, how many items, when it was read.
type Seen = (u64, String, usize, u64);

fn seen(tab: &crate::app::Tab) -> Seen {
    (tab.id, tab.folder.path.clone(), tab.folder.entries.len(), tab.load_ms.map_or(0, f64::to_bits))
}

/// The folders "in" can be followed by: the user's places, pinned folders, the open tabs' folders,
/// the subfolders of the folder on screen, and the drives.
fn places(app: &AppState) -> Vec<Place> {
    let mut out: Vec<Place> = shell::known_folders().into_iter().map(|k| Place { name: k.name.to_string(), path: k.path, hint: "Quick access".into() }).collect();
    let name = |p: &str| folder::file_name(p).to_string();
    for p in &app.settings.pinned {
        out.push(Place { name: name(p), path: p.clone(), hint: "Pinned".into() });
    }
    for t in &app.tabs {
        if !t.path.is_empty() && !crate::folder::is_place(&t.path) {
            out.push(Place { name: name(&t.path), path: t.path.clone(), hint: "Open tab".into() });
        }
    }
    let tab = app.tab();
    if !tab.folder.path.is_empty() && !crate::recycle::is_bin(&tab.folder.path) && tab.search.is_none() {
        for e in tab.folder.entries.iter().filter(|e| e.kind == EntryKind::Folder && !e.is_hidden()).take(300) {
            out.push(Place { name: e.name.clone(), path: folder::join(&tab.folder.path, &e.name), hint: "Subfolder".into() });
        }
    }
    for d in &app.drives {
        out.push(Place { name: d.name.clone(), path: d.root.clone(), hint: "Drive".into() });
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.'
}

/// Ctrl+Left: the start of this word (or the one before).
fn word_left(text: &str, pos: usize) -> usize {
    let mut p = pos;
    while p > 0 && !is_word(text[..p].chars().next_back().unwrap_or(' ')) {
        p = prev_char(text, p);
    }
    while p > 0 && is_word(text[..p].chars().next_back().unwrap_or(' ')) {
        p = prev_char(text, p);
    }
    p
}

/// Ctrl+Right: past this word and the spaces after it.
fn word_right(text: &str, pos: usize) -> usize {
    // At a line's end: on to the start of the next.
    if text[pos..].starts_with('\n') {
        return pos + 1;
    }
    let mut p = pos;
    while p < text.len() && is_word(text[p..].chars().next().unwrap_or(' ')) {
        p = next_char(text, p);
    }
    while p < text.len() && !is_word(text[p..].chars().next().unwrap_or('a')) && !text[p..].starts_with('\n') {
        p = next_char(text, p);
    }
    p
}

/// The words saying which rules sort the folder on screen, and their link.
fn here_style(color: Color) -> TextStyle {
    TextStyle::new(color).size(ksize::NOTE_FONT + 0.5)
}

fn here_link_style(color: Color) -> TextStyle {
    TextStyle::new(color).semibold()
}

/// A problem as it reads under the rules box: its line first.
fn problem_text(m: &sortrecipe::Message) -> String {
    let text = if m.line > 0 { format!("Line {}: {}", m.line, m.text) } else { m.text.clone() };
    if m.fix.is_some() {
        text.replace(" Did you mean", "  Did you mean")
    } else {
        text
    }
}

/// A problem's fix link.
fn fix_text(m: &sortrecipe::Message, fix: &str) -> String {
    format!("Fix: {}", m.fix_label.as_deref().unwrap_or(fix))
}

/// Which rules sort the folder on screen in the main window, in words (with `recipe`, the rules as
/// typed, so the lines it names are the ones on screen).
fn here(app: &AppState, recipe: &Recipe) -> Here {
    if !app.settings.custom_sort {
        return Here { text: "Custom sort is off, so these rules don't sort anything yet.".into(), link: Some(HereLink::TurnOn), ..Here::default() };
    }
    let tab = app.tab();
    let path = &tab.folder.path;
    let name = app.display_name(path);
    if path.is_empty() {
        return Here { text: format!("On screen: {name}, which these rules don't sort."), ..Here::default() };
    }
    if tab.column_now {
        return Here { text: format!("On screen: {name}, sorted by a column for now."), link: Some(HereLink::UseRules), ..Here::default() };
    }
    match recipe.source(path) {
        Source::Folder { label, starts, own } => {
            let line = starts.first().copied();
            let at = line.map_or(String::new(), |n| format!(" (line {n})"));
            let text = if own {
                format!("On screen: {name}, sorted by its own rules{at}.")
            } else {
                format!("On screen: {name}, sorted by the rules for {label} and the folders inside it{at}.")
            };
            let marked = recipe.blocks.iter().filter(|b| starts.iter().any(|&s| b.start <= s && s <= b.end)).map(|b| (b.start, b.end)).collect();
            Here { text, link: None, line, marked }
        }
        Source::Everywhere => Here { text: format!("On screen: {name}, sorted by the rules for every folder."), ..Here::default() },
        Source::Nothing => Here { text: format!("On screen: {name}. No rules here sort it, so its column does."), ..Here::default() },
    }
}

/// The tip for a folder written in the rules that isn't there (`text` is the rules): with the
/// closest of `names`, the folders beside where it should be, as the fix.
fn missing_tip(text: &str, w: &Written, names: Option<&[String]>) -> Option<Message> {
    let written = text.get(w.span.0..w.span.1)?;
    let trimmed = written.trim_end_matches(['\\', '/']);
    let leaf_at = trimmed.rfind(['\\', '/']).map_or(0, |i| i + 1);
    let leaf = &trimmed[leaf_at..];
    let mut said = format!("Glint can't find {written} right now.");
    let near = names.and_then(|n| sortrecipe::closest(leaf, n.iter().map(String::as_str))).filter(|n| !n.eq_ignore_ascii_case(leaf));
    if let Some(name) = near {
        said.push_str(&format!(" Did you mean {name}?"));
    }
    let span = (w.span.0 + leaf_at, w.span.0 + leaf_at + leaf.len());
    Some(Message { line: w.line, text: said, error: false, span: Some(span), fix: near.map(str::to_string), fix_label: None })
}

/// The lines with a problem (numbered from 0), each with whether it's a mistake, mistakes before
/// tips: the gutter's mark for a line is its first one here, so a line with both gets the red dot.
fn line_marks(messages: &[Message]) -> Vec<(usize, bool)> {
    let mut marks: Vec<(usize, bool)> = messages.iter().filter(|m| m.line > 0).map(|m| (m.line - 1, m.error)).collect();
    marks.sort_by_key(|&(line, error)| (line, !error));
    marks
}

/// The faint words after the cursor's line (`line` from 1), and whether they're about a mistake:
/// its first mistake when it has one, in the same words as in the problems under the box (which
/// list it first too), else what the line does.
fn cursor_hint(recipe: &Recipe, line: usize) -> Option<(String, bool)> {
    if let Some(m) = recipe.messages.iter().find(|m| m.error && m.line == line) {
        let text = problem_text(m);
        let said = text.strip_prefix(&format!("Line {line}: ")).unwrap_or(&text).to_string();
        return Some((said, true));
    }
    recipe.explained.iter().find(|(l, _)| *l == line).map(|(_, s)| (s.clone(), false))
}

/// The colour of the kinds of things (folder, image, .png, bigger_than_1mb…) in the rules box:
/// the strong text colour, or, in a skin where that's the same as the plain one, the plain one a
/// little toward the accent, so a word Glint knows never looks like one it doesn't.
fn group_color(theme: &Theme) -> Color {
    let (a, b) = (theme.text_strong, theme.text);
    let apart = (a.r - b.r).abs() + (a.g - b.g).abs() + (a.b - b.b).abs();
    if apart >= 0.12 {
        a
    } else {
        b.mix(theme.accent.with_alpha(b.a), 0.45)
    }
}

/// The colour of a word in the rules box: notes muted, "in … (" lines strong, order rules, views
/// and and_subfolders in the accent, kinds of things in [`group_color`], and a word Glint doesn't
/// know in the plain text colour (with a red line under it).
fn word_color(theme: &Theme, word: &str, in_note: bool, in_line: bool) -> Color {
    if in_note {
        theme.muted
    } else if in_line {
        theme.text_strong
    } else if sortrecipe::is_order_word(word) || sortrecipe::is_view_word(word) || sortrecipe::is_folder_word(word) {
        theme.accent
    } else if sortrecipe::is_group_word(word) {
        group_color(theme)
    } else {
        theme.text
    }
}

/// Enter at the end of a line opening a folder's rules ("in Downloads (") whose ")" never came:
/// what goes in (a line for the rules, then the ")"), and how far into it the cursor goes.
fn closing(text: &str, caret: usize, unclosed: &[usize]) -> Option<(String, usize)> {
    let (start, line) = line_of(text, caret);
    let end = line_end(text, caret);
    let row = &text[start..end];
    if !text[caret..end].trim().is_empty() || !unclosed.contains(&(line + 1)) || !sortrecipe::strip_note(row).trim_end().ends_with('(') {
        return None;
    }
    let indent = &row[..row.len() - row.trim_start().len()];
    Some((format!("\n{indent}\n{indent})"), 1 + indent.len()))
}

/// Alt+Up / Alt+Down with the cursor at `caret` and the selection's other end at `anchor`: the
/// part of the text that changes, what it becomes, and where the cursor and the other end go;
/// None at the top or the bottom.
fn move_lines(text: &str, caret: usize, anchor: usize, up: bool) -> Option<((usize, usize), String, usize, usize)> {
    let (first, last) = whole_lines(text, caret.min(anchor), caret.max(anchor));
    let (part, new, shift) = swap_lines(text, first, last, up)?;
    // (A selection ending at the start of the line below ends past the text when that was the
    // last line: it ends at the end instead.)
    let moved = |pos: usize| ((pos as isize + shift) as usize).min(text.len());
    Some((part, new, moved(caret), moved(anchor)))
}

/// Whether the lines (blank ones aside) are all notes.
fn all_notes(lines: &str) -> bool {
    let mut any = false;
    for l in lines.split('\n').map(str::trim_start).filter(|l| !l.is_empty()) {
        if !l.starts_with('#') {
            return false;
        }
        any = true;
    }
    any
}

/// Ctrl+/: the lines made notes ("# " after their indent), or rules again when they all are notes.
/// Blank lines stay as they are.
fn toggle_notes(lines: &str) -> String {
    let notes = all_notes(lines);
    let line = |l: &str| {
        let body = l.trim_start();
        let indent = &l[..l.len() - body.len()];
        if body.is_empty() {
            l.to_string()
        } else if notes {
            let rest = body.strip_prefix('#').unwrap_or(body);
            format!("{indent}{}", rest.strip_prefix(' ').unwrap_or(rest))
        } else {
            format!("{indent}# {body}")
        }
    };
    lines.split('\n').map(line).collect::<Vec<_>>().join("\n")
}

/// Where a folder's rules open on its first line (the last bracket before any note), and close on
/// its last (the first), in characters.
fn open_bracket(row: &str) -> Option<usize> {
    let code = sortrecipe::strip_note(row);
    code.char_indices().rfind(|&(_, c)| matches!(c, '(' | '{' | '[')).map(|(i, _)| code[..i].chars().count())
}

fn close_bracket(row: &str) -> Option<usize> {
    let code = sortrecipe::strip_note(row);
    code.char_indices().find(|&(_, c)| matches!(c, ')' | '}' | ']')).map(|(i, _)| code[..i].chars().count())
}

impl SortWindow {
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

    fn invalidate(&self) {
        // SAFETY: plain invalidate of our own window.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// The skin changed (or the window was just made).
    pub fn apply_theme(&mut self, theme: &Theme) {
        self.renderer.set_font(&theme.font, 1.0);
        self.renderer.set_title_font(&theme.title_font);
        self.char_w = 0.0;
        if CLASSIC.with(|c| c.replace(theme.era.classic())) != theme.era.classic() {
            // 98 / 2000's frame comes or goes: what's inside it changes size.
            let mut rc = RECT::default();
            // SAFETY: our own window and a valid out-struct.
            if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
                self.size = self.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
            }
        }
        frame::apply_theme(self.hwnd, theme);
        self.invalidate();
    }

    /// How wide 98 / 2000's window frame is round everything (0 for other skins or maximised).
    fn border(&self) -> f32 {
        if CLASSIC.with(Cell::get) && !frame::zoomed(self.hwnd) {
            frame::CLASSIC_BORDER
        } else {
            0.0
        }
    }

    /// The client area's size in DIPs, less the frame (from `w` x `h` pixels).
    fn inner_size(&self, w: u32, h: u32) -> (f32, f32) {
        let (s, b) = (self.scale(), self.border());
        ((w as f32 / s - 2.0 * b).max(0.0), (h as f32 / s - 2.0 * b).max(0.0))
    }

    /// A menu's place `at` in the window (inside 98 / 2000's frame) as the window's client point.
    fn menu_at(&self, at: Point) -> Point {
        let b = self.border();
        Point::new(at.x + b, at.y + b)
    }

    /// Opened again while it's open.
    pub fn reopen(&mut self) -> Vec<Ask> {
        self.invalidate();
        vec![Ask::Job(Job::Activate)]
    }

    /// The main window moved or changed size: follow it.
    pub fn dock(&self, owner: HWND) -> Option<Job> {
        // SAFETY: plain queries on our own windows.
        if self.detached || unsafe { IsIconic(owner).as_bool() } {
            return None;
        }
        let r = dock_rect(owner);
        let mut now = RECT::default();
        // SAFETY: as above.
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut now);
        }
        (now != r).then_some(Job::Place(r))
    }

    /// DETACH / DOCK: lets the window go (it moves and resizes on its own) or puts it back beside
    /// the main window. The window is made again for that (with the rules as typed, used first):
    /// its frame is set when it's made, which every system draws right.
    fn toggle_detached(&mut self, app: &mut AppState) -> Vec<Ask> {
        let mut asks = self.apply_now(app);
        let detached = !self.detached;
        if detached {
            let mut now = RECT::default();
            // SAFETY: a plain query on our own window.
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut now);
            }
            let r = detached_rect(app.settings.sort_bounds, now, self.scale());
            app.settings.sort_bounds = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
        } else {
            self.remember_place(app);
        }
        app.settings.sort_detached = detached;
        app.sort_caret = Some((self.caret, self.scroll.1));
        asks.push(Ask::Job(Job::Reopen));
        asks
    }

    /// On its own, it was moved or resized: remembered for next time.
    fn remember_place(&mut self, app: &mut AppState) {
        let mut r = RECT::default();
        // SAFETY: plain queries on our own window.
        if self.detached && unsafe { !IsIconic(self.hwnd).as_bool() && GetWindowRect(self.hwnd, &mut r).is_ok() } && r.right > r.left && r.bottom > r.top {
            app.settings.sort_bounds = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
        }
    }

    // ------------------------------------------------------------ the text

    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// Replaces `a..b` with `with`, the cursor after it; remembered for Ctrl+Z.
    fn replace(&mut self, a: usize, b: usize, with: &str, app: &AppState) {
        self.edit(a, b, with, Edit::Other, app);
    }

    /// Replaces `a..b` with `with` (a change of the `kind` given), the cursor after it. Typing (or
    /// deleting) that goes on from where the last left off, a moment later, is taken back with it
    /// by one Ctrl+Z; a space starts a new word.
    fn edit(&mut self, a: usize, b: usize, with: &str, kind: Edit, app: &AppState) {
        let goes_on = kind != Edit::Other
            && self.caret == self.anchor
            && !(kind == Edit::Typing && with.starts_with(char::is_whitespace))
            && self.last_edit.is_some_and(|(k, at, when)| k == kind && at == self.caret && when.elapsed().as_secs_f64() < GROUP_SECS);
        if !goes_on {
            self.undo.push((self.text.clone(), self.caret));
            if self.undo.len() > 300 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.text.replace_range(a..b, with);
        self.caret = a + with.len();
        self.anchor = self.caret;
        self.last_edit = Some((kind, self.caret, Instant::now()));
        self.changed(app);
    }

    fn insert(&mut self, s: &str, app: &AppState) {
        let (a, b) = self.selection();
        self.replace(a, b, s, app);
    }

    /// Ctrl+Z (or with `redo`, Ctrl+Y): the rules as they were before the last change (or after).
    fn undo_redo(&mut self, redo: bool, app: &AppState) {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some((text, caret)) = from.pop() else { return };
        to.push((std::mem::replace(&mut self.text, text), self.caret));
        self.caret = caret.min(self.text.len());
        while !self.text.is_char_boundary(self.caret) {
            self.caret -= 1;
        }
        self.anchor = self.caret;
        self.last_edit = None;
        let undo = std::mem::take(&mut self.undo);
        let redo = std::mem::take(&mut self.redo);
        self.changed(app);
        (self.undo, self.redo) = (undo, redo);
        self.list_closed = true;
    }

    /// Ctrl+C, or with `cut` Ctrl+X.
    fn copy(&mut self, cut: bool, app: &AppState) {
        let (a, b) = self.selection();
        if a != b {
            // Cut takes the rules away only once they're on the clipboard (another program may
            // have that open).
            if shell::copy_text(self.hwnd, &self.text[a..b].replace('\n', "\r\n")) && cut {
                self.replace(a, b, "", app);
            }
        }
    }

    fn paste(&mut self, app: &AppState) {
        if let Some(t) = clipboard_text(self.hwnd) {
            let t = t.replace("\r\n", "\n").replace('\r', "\n").replace('\t', " ");
            self.insert(&t, app);
            self.list_closed = true;
        }
    }

    /// Ctrl+/: the cursor's lines become notes, or rules again.
    fn toggle_note(&mut self, app: &AppState) {
        let (a, b) = self.selection();
        let (first, last) = whole_lines(&self.text, a, b);
        let old = self.text[first..last].to_string();
        let new = toggle_notes(&old);
        if new == old {
            return;
        }
        let caret = self.caret;
        self.replace(first, last, &new, app);
        if a == b {
            // The cursor stays by the same word.
            let indent = first + old.len() - old.trim_start().len();
            let moved = caret as isize + new.len() as isize - old.len() as isize;
            let pos = if caret >= indent { moved.max(indent as isize) as usize } else { caret };
            (self.caret, self.anchor) = (pos, pos);
        } else {
            (self.anchor, self.caret) = (first, first + new.len());
        }
        self.list_closed = true;
        self.reveal_caret();
    }

    /// Alt+Up / Alt+Down: the cursor's lines go up or down one.
    fn move_line(&mut self, up: bool, app: &AppState) {
        let Some(((from, to), new, caret, anchor)) = move_lines(&self.text, self.caret, self.anchor, up) else { return };
        self.replace(from, to, &new, app);
        (self.caret, self.anchor) = (caret, anchor);
        self.list_closed = true;
        self.reveal_caret();
    }

    /// Whether START FROM… would replace rules of your own (not empty, the starter or another
    /// ready-made order): it asks first then.
    fn would_lose_rules(&self) -> bool {
        let t = self.text.trim();
        !t.is_empty() && t != sortrecipe::STARTER.trim() && !sortrecipe::PRESETS.iter().any(|(_, p)| p.trim() == t)
    }

    /// Puts the ready-made order `i` in place of the rules (Ctrl+Z brings them back).
    fn start_from(&mut self, i: usize, app: &mut AppState) -> Vec<Ask> {
        self.confirm = None;
        let Some((_, text)) = sortrecipe::PRESETS.get(i) else { return Vec::new() };
        self.set_text(text, app);
        self.apply_now(app)
    }

    /// The right-click menu, at `at`.
    fn edit_menu(&self, at: Point) -> Menu {
        let (a, b) = self.selection();
        let none = a == b;
        // SAFETY: a plain clipboard query.
        let paste = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() };
        let (first, last) = whole_lines(&self.text, a, b);
        let note = if all_notes(&self.text[first..last]) { "Make it a rule again\tCtrl+/" } else { "Make it a note\tCtrl+/" };
        let items = vec![
            MenuItem::new(EDIT_UNDO, "Undo\tCtrl+Z").disabled(self.undo.is_empty()),
            MenuItem::new(EDIT_REDO, "Redo\tCtrl+Y").disabled(self.redo.is_empty()),
            MenuItem::separator(),
            MenuItem::new(EDIT_CUT, "Cut\tCtrl+X").disabled(none),
            MenuItem::new(EDIT_COPY, "Copy\tCtrl+C").disabled(none),
            MenuItem::new(EDIT_PASTE, "Paste\tCtrl+V").disabled(!paste),
            MenuItem::new(EDIT_ALL, "Select all\tCtrl+A"),
            MenuItem::separator(),
            MenuItem::new(EDIT_NOTE, note),
            MenuItem::new(EDIT_UP, "Move line up\tAlt+Up"),
            MenuItem::new(EDIT_DOWN, "Move line down\tAlt+Down"),
            MenuItem::separator(),
            MenuItem::new(EDIT_SUGGEST, "Suggest words\tCtrl+Space"),
        ];
        Menu { tag: MENU_EDIT, at: self.menu_at(at), items, right_aligned: false }
    }

    /// What was picked in the right-click menu.
    fn edit_pick(&mut self, id: u32, app: &AppState) {
        match id {
            EDIT_UNDO | EDIT_REDO => self.undo_redo(id == EDIT_REDO, app),
            EDIT_CUT | EDIT_COPY => self.copy(id == EDIT_CUT, app),
            EDIT_PASTE => self.paste(app),
            EDIT_ALL => (self.anchor, self.caret) = (0, self.text.len()),
            EDIT_NOTE => self.toggle_note(app),
            EDIT_UP | EDIT_DOWN => self.move_line(id == EDIT_UP, app),
            EDIT_SUGGEST => self.suggest_here(),
            _ => {}
        }
        self.invalidate();
    }

    /// The folders written in the rules that aren't there (a drive not plugged in, a name typed
    /// wrong): a tip each. Each folder is looked for once, in the background (WM_APP_LISTED
    /// comes back), and so are the folders beside it, for "did you mean".
    fn check_places(&mut self) {
        let mut tips = Vec::new();
        for w in self.recipe.places.iter().filter(|w| !w.named) {
            match self.lister.exists(&w.path) {
                Some(false) => {}
                Some(true) => continue,
                None => {
                    self.lister.look(&w.path, self.hwnd, WM_APP_LISTED);
                    continue;
                }
            }
            let parent = folder::parent(&w.path).filter(|p| !p.is_empty());
            let names = parent.as_deref().and_then(|p| self.lister.get(p));
            if let (Some(p), None) = (&parent, &names) {
                self.lister.ask(p, self.hwnd, WM_APP_LISTED);
            }
            tips.extend(missing_tip(&self.text, w, names.as_deref().map(Vec::as_slice)));
        }
        self.recipe.messages.extend(tips);
    }

    /// The text changed: read it again, suggest again, and use it a moment after typing stops.
    fn changed(&mut self, app: &AppState) {
        self.recipe = Recipe::parse(&self.text);
        self.check_places();
        self.here = here(app, &self.recipe);
        self.list_closed = false;
        self.asked = false;
        self.want_col = None;
        self.suggest(app);
        self.apply_due = true;
        // SAFETY: a timer on our own window (started again by each change).
        unsafe {
            SetTimer(Some(self.hwnd), APPLY_TIMER, APPLY_DELAY_MS, None);
        }
        self.reveal_caret();
        self.invalidate();
    }

    /// Works out the suggestions for the cursor's place (asking for a folder's listing if needed).
    fn suggest(&mut self, _app: &AppState) {
        self.asked &= !self.list_closed;
        self.suggest_with(self.asked);
    }

    /// Ctrl+Space: the list at the cursor, even before anything's typed there.
    fn suggest_here(&mut self) {
        self.anchor = self.caret;
        self.list_closed = false;
        self.asked = true;
        self.suggest_with(true);
        self.reveal_caret();
    }

    /// The suggestions for the cursor's place; `asked`: with Ctrl+Space ([`complete::complete_asked`]).
    fn suggest_with(&mut self, asked: bool) {
        let lister = &self.lister;
        let listed = |dir: &str| lister.get(dir);
        let c = if asked { complete::complete_asked(&self.text, self.caret, &self.places, &listed) } else { complete::complete(&self.text, self.caret, &self.places, &listed) };
        if let Some(dir) = &c.wants {
            self.lister.ask(dir, self.hwnd, WM_APP_LISTED);
        }
        if c.items.len() != self.completion.items.len() || c.start != self.completion.start {
            self.pick = 0;
        }
        self.pick = self.pick.min(c.items.len().saturating_sub(1));
        self.completion = c;
    }

    /// Whether the suggestions' list shows.
    fn list_open(&self) -> bool {
        !self.list_closed && !self.completion.items.is_empty() && self.caret == self.anchor
    }

    /// Takes the picked suggestion.
    fn accept(&mut self, i: usize, app: &AppState) {
        let Some(s) = self.completion.items.get(i).cloned() else { return };
        let (a, b) = (self.completion.start, self.completion.end.min(self.text.len()));
        self.anchor = a;
        self.caret = b;
        self.insert(&s.insert, app);
        // A finished word (not "in " or a folder to go into) closes the list.
        if !s.insert.ends_with(['\\', ' ', '_']) {
            self.list_closed = true;
        }
    }

    /// Clicking a problem's fix: the word becomes what was meant.
    fn fix(&mut self, i: usize, app: &AppState) {
        let Some(m) = self.recipe.messages.get(i).cloned() else { return };
        if let (Some((a, b)), Some(with)) = (m.span, m.fix) {
            if b <= self.text.len() && self.text.is_char_boundary(a) && self.text.is_char_boundary(b) {
                self.anchor = a;
                self.caret = b;
                self.insert(&with, app);
                self.list_closed = true;
            }
        }
    }

    fn set_text(&mut self, text: &str, app: &AppState) {
        let len = self.text.len();
        self.anchor = 0;
        self.caret = len;
        self.insert(text, app);
        self.caret = 0;
        self.anchor = 0;
        self.list_closed = true;
        self.scroll = (0.0, 0.0);
    }

    /// Moves the cursor (with Shift, the selection grows).
    fn move_to(&mut self, pos: usize, keep_col: bool) {
        self.caret = pos.min(self.text.len());
        self.last_edit = None;
        if !key_down(VK_SHIFT.0) {
            self.anchor = self.caret;
        }
        if !keep_col {
            self.want_col = None;
        }
        self.reveal_caret();
    }

    fn line_move(&mut self, lines: i32) {
        let col = *self.want_col.get_or_insert(col_of(&self.text, self.caret));
        let (start, _) = line_of(&self.text, self.caret);
        let mut target = start;
        if lines < 0 {
            for _ in 0..-lines {
                if target == 0 {
                    break;
                }
                target = line_of(&self.text, target - 1).0;
            }
        } else {
            for _ in 0..lines {
                let end = line_end(&self.text, target);
                if end >= self.text.len() {
                    break;
                }
                target = end + 1;
            }
        }
        if target == start && (lines < 0 && start == 0 || lines > 0 && line_end(&self.text, start) == self.text.len()) {
            let to = if lines < 0 { 0 } else { self.text.len() };
            self.move_to(to, true);
            return;
        }
        let pos = at_col(&self.text, target, col);
        self.move_to(pos, true);
    }

    /// The line numbers' column at the left of the rules box (DIPs): a mark for a line with a
    /// problem, then room for the biggest number.
    fn gutter_w(&self) -> f32 {
        let digits = (self.text.matches('\n').count() + 1).to_string().len().max(2);
        GUTTER_MARK + digits as f32 * self.char_w + GUTTER_GAP
    }

    /// Scrolls the rules box so the cursor shows.
    fn reveal_caret(&mut self) {
        let e = self.lay.editor;
        if e.w <= 0.0 || self.char_w <= 0.0 {
            return;
        }
        let (_, line) = line_of(&self.text, self.caret);
        let (vh, vw) = (e.h - 2.0 * BOX_PAD, e.w - 2.0 * BOX_PAD - self.gutter_w());
        let y = line as f32 * LINE_H;
        if y < self.scroll.1 {
            self.scroll.1 = y;
        } else if y + LINE_H > self.scroll.1 + vh {
            self.scroll.1 = y + LINE_H - vh;
        }
        let x = col_of(&self.text, self.caret) as f32 * self.char_w;
        if x < self.scroll.0 {
            self.scroll.0 = (x - 40.0).max(0.0);
        } else if x + 2.0 > self.scroll.0 + vw {
            self.scroll.0 = x + 40.0 - vw;
        }
    }

    /// The text position under a point in the rules box.
    fn pos_at(&self, p: Point) -> usize {
        let e = self.lay.editor;
        let line = ((p.y - e.y - BOX_PAD + self.scroll.1) / LINE_H).floor().max(0.0) as usize;
        let col = ((p.x - e.x - BOX_PAD - self.gutter_w() + self.scroll.0) / self.char_w.max(1.0)).round().max(0.0) as usize;
        let mut start = 0;
        for _ in 0..line {
            let end = line_end(&self.text, start);
            if end >= self.text.len() {
                return self.text.len();
            }
            start = end + 1;
        }
        at_col(&self.text, start, col)
    }

    /// The cursor's place on screen (the top of its line).
    fn caret_point(&self, pos: usize) -> Point {
        let e = self.lay.editor;
        let (_, line) = line_of(&self.text, pos);
        Point::new(e.x + BOX_PAD + self.gutter_w() + col_of(&self.text, pos) as f32 * self.char_w - self.scroll.0, e.y + BOX_PAD + line as f32 * LINE_H - self.scroll.1)
    }

    // ------------------------------------------------------------ using the rules

    /// Uses the rules now: in every tab, and saved (the main window saves what changed).
    fn apply_now(&mut self, app: &mut AppState) -> Vec<Ask> {
        self.apply_due = false;
        // SAFETY: stops our own timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), APPLY_TIMER);
        }
        if self.text == self.applied {
            return Vec::new();
        }
        self.applied = self.text.clone();
        let on = app.settings.custom_sort;
        app.set_custom_sort(on, Some(self.text.clone()));
        vec![Ask::RedrawMain]
    }

    /// WATCH_TIMER: another folder on screen (or it changed): its subfolders are suggested, and
    /// the line above the rules box says which rules sort it.
    fn watch(&mut self, app: &AppState) {
        let now = seen(app.tab());
        if self.seen.as_ref() != Some(&now) {
            self.places = places(app);
            self.seen = Some(now);
        }
        let here = here(app, &self.recipe);
        if here != self.here {
            self.here = here;
            self.invalidate();
        }
    }

    /// The link on the line above the rules box: custom sort on, with the rules as typed (which
    /// also puts a folder sorted by a column for now back to them).
    fn use_rules(&mut self, app: &mut AppState) -> Vec<Ask> {
        self.flips.start("on", true, self.now(), kit::SWITCH_TIME);
        app.set_custom_sort(true, Some(self.text.clone()));
        self.applied = self.text.clone();
        self.apply_due = false;
        self.here = here(app, &self.recipe);
        vec![Ask::RedrawMain]
    }

    // ------------------------------------------------------------ layout and painting

    fn layout(&mut self, app: &AppState) {
        let (w, h) = self.size;
        let mut l = Layout { close: Rect::new(w - ksize::CLOSE_W, 0.0, ksize::CLOSE_W, size::TITLE_H), ..Layout::default() };
        l.detach = Rect::new(l.close.x - 8.0 - DETACH_W, (size::TITLE_H - ksize::BUTTON_H) / 2.0, DETACH_W, ksize::BUTTON_H);
        let mut y = size::TITLE_H + 10.0;
        l.switch = Rect::new(PAD, y + 4.0, ksize::TOGGLE_W, ksize::TOGGLE_H);
        let bw = kit::button_width(&mut self.renderer, "START FROM\u{2026}");
        l.start_from = Rect::new(w - PAD - bw, y, bw, ksize::BUTTON_H);
        let gw = kit::button_width(&mut self.renderer, guide_label(app));
        l.guide_button = Rect::new(l.start_from.x - 8.0 - gw, y, gw, ksize::BUTTON_H);
        y += ksize::BUTTON_H + 8.0;
        self.here = here(app, &self.recipe);
        let link_w = self.here.link.map_or(0.0, |k| self.renderer.measure(k.label(), &here_link_style(Color::hex(0))) + 12.0);
        let bar_w = if self.here.marked.is_empty() { 0.0 } else { 9.0 };
        let mut lines = kit::wrap(&mut self.renderer, &self.here.text, &here_style(Color::hex(0)), (w - 2.0 * PAD - bar_w - link_w).max(60.0));
        if lines.len() > 2 {
            let rest = lines[1..].join(" ");
            lines.truncate(1);
            lines.push(rest); // drawn with "…" where it doesn't fit
        }
        self.here_lines = lines;
        l.here = Rect::new(PAD, y, w - 2.0 * PAD, HERE_H * self.here_lines.len().max(1) as f32);
        y += l.here.h + 6.0;
        // START FROM…'s question, when it asks.
        if let Some(i) = self.confirm {
            let name = sortrecipe::PRESETS.get(i).map_or("", |p| p.0);
            let style = TextStyle::new(Color::hex(0));
            self.confirm_lines = kit::wrap(&mut self.renderer, &format!("Replace your rules with \u{201c}{name}\u{201d}?"), &style, w - 2.0 * PAD - 24.0);
            self.confirm_lines.truncate(2);
            let text_h = self.confirm_lines.len() as f32 * 18.0;
            l.confirm = Rect::new(PAD, y, w - 2.0 * PAD, 10.0 + text_h + 8.0 + ksize::BUTTON_H + 10.0);
            let (rw, kw) = (kit::button_width(&mut self.renderer, "REPLACE"), kit::button_width(&mut self.renderer, "KEEP MINE"));
            let by = l.confirm.bottom() - 10.0 - ksize::BUTTON_H;
            l.replace = Rect::new(l.confirm.right() - 12.0 - rw, by, rw, ksize::BUTTON_H);
            l.keep = Rect::new(l.replace.x - 8.0 - kw, by, kw, ksize::BUTTON_H);
            y += l.confirm.h + 8.0;
        }
        // From the bottom up: the guide (when shown, at most half the window), the problems (at
        // most a third of it: they scroll), and the rules box in all the rest.
        let mut bottom = h - PAD;
        if app.settings.sort_guide_shown {
            let lines = self.guide_lines(w - 2.0 * PAD).len() as f32;
            let gh = (22.0 + lines * GUIDE_LINE).min(((bottom - y) * 0.5).max(22.0 + 3.0 * GUIDE_LINE));
            bottom -= gh;
            l.guide = Rect::new(PAD, bottom, w - 2.0 * PAD, gh);
            bottom -= 10.0;
        }
        self.problem_lines = self.wrap_problems(w - 2.0 * PAD - PROBLEM_BAR);
        let ph: f32 = self.problem_lines.iter().map(|(_, lines)| PROBLEM_H + (lines.len().max(1) - 1) as f32 * PROBLEM_LINE).sum();
        let shown = ph.min((h * PROBLEMS_MAX).max(2.0 * PROBLEM_H));
        // The cursor's line's problem comes first: shown from the top when the cursor moves.
        let (_, caret_line) = line_of(&self.text, self.caret);
        if caret_line != self.problems_for {
            self.problems_for = caret_line;
            self.problem_scroll = 0.0;
        }
        self.problem_content = ph;
        self.problem_scroll = self.problem_scroll.clamp(0.0, (ph - shown).max(0.0));
        bottom -= shown;
        l.problems = Rect::new(PAD, bottom, w - 2.0 * PAD, shown);
        bottom -= if shown > 0.0 { 6.0 } else { 0.0 };
        l.editor = Rect::new(PAD, y, w - 2.0 * PAD, (bottom - y).max(4.0 * LINE_H + 2.0 * BOX_PAD));
        self.lay = l;
    }

    /// The problems shown, each wrapped to `width` (less its fix link), at most
    /// [`PROBLEM_MAX_LINES`] lines.
    fn wrap_problems(&mut self, width: f32) -> Vec<(usize, Vec<String>)> {
        let style = TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT);
        let link = TextStyle::new(Color::hex(0)).semibold();
        let mut out = Vec::new();
        for i in self.shown_problems() {
            let m = &self.recipe.messages[i];
            let text = problem_text(m);
            let fix_w = m.fix.as_ref().map_or(0.0, |fix| self.renderer.measure(&fix_text(m, fix), &link) + 12.0);
            let mut lines = kit::wrap(&mut self.renderer, &text, &style, (width - 16.0 - fix_w).max(60.0));
            if lines.len() > PROBLEM_MAX_LINES {
                lines.truncate(PROBLEM_MAX_LINES);
                if let Some(last) = lines.last_mut() {
                    last.push('\u{2026}');
                }
            }
            out.push((i, lines));
        }
        out
    }

    /// The problems, as message numbers: the cursor line's first, then mistakes, then tips.
    fn shown_problems(&self) -> Vec<usize> {
        let (_, line) = line_of(&self.text, self.caret);
        let mut v: Vec<usize> = (0..self.recipe.messages.len()).collect();
        v.sort_by_key(|&i| {
            let m = &self.recipe.messages[i];
            (m.line != line + 1, !m.error, m.line)
        });
        v
    }

    /// The guide, broken to fit: (line, whether it's an example line, in the rules box's font).
    fn guide_lines(&mut self, width: f32) -> Vec<(String, bool)> {
        let style = TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT);
        let mut out = Vec::new();
        for g in GUIDE {
            match g.strip_prefix('|') {
                Some(code) => out.push((code.to_string(), true)),
                None => out.extend(kit::wrap(&mut self.renderer, g, &style, width).into_iter().map(|l| (l, false))),
            }
        }
        out
    }

    fn paint(&mut self, app: &mut AppState) {
        if self.char_w <= 0.0 {
            self.char_w = self.renderer.mono_char_width(MONO);
        }
        self.layout(app);
        let now = self.now();
        let mut more = self.flips.tick(now);
        if !self.renderer.begin() {
            return;
        }
        let theme = app.theme.clone();
        let l = self.lay;
        self.renderer.clear(theme.window);
        // 98 / 2000: everything inside the window's frame.
        let b = self.border();
        self.renderer.set_origin(b, b);
        kit::backdrop(&mut self.renderer, &theme, Rect::new(0.0, 0.0, self.size.0, self.size.1));
        // The title bar: the logo, the name, ×.
        {
            let r = &mut self.renderer;
            let bar = Rect::new(0.0, 0.0, self.size.0, size::TITLE_H);
            kit::title_bar(r, &theme, bar, self.active);
            let x = kit::logo(r, &theme, bar);
            crate::retro::title(r, &theme, Rect::new(x + 10.0, 0.0, (l.detach.x - x - 10.0).max(0.0), size::TITLE_H), "Sort order and views", self.active);
            let detach = Look { hover: self.hover == Some(Hot::Detach), pressed: self.pressed == Some(Hot::Detach), focused: false, enabled: true };
            kit::button(r, &theme, l.detach, if self.detached { "DOCK" } else { "DETACH" }, detach);
            let hot = self.hover == Some(Hot::Close);
            kit::close_button(r, &theme, l.close, hot, hot && self.pressed == Some(Hot::Close), self.active);
            if !self.detached && b <= 0.0 {
                // A hairline down the side it shares with the main window.
                r.fill_rect(Rect::new(0.0, 0.0, 1.0, self.size.1), theme.line);
            }
        }
        // The switch, START FROM…, GUIDE.
        let on = app.settings.custom_sort;
        let look = |hot: Hot, s: &Self| Look { hover: s.hover == Some(hot), pressed: s.pressed == Some(hot), focused: false, enabled: true };
        let (switch_look, start_look, guide_look) = (look(Hot::Switch, self), look(Hot::StartFrom, self), look(Hot::Guide, self));
        let flip = self.flips.at("on", on, now);
        {
            let r = &mut self.renderer;
            kit::toggle(r, &theme, l.switch, flip, switch_look);
            let label = if on { "Custom sort is on" } else { "Off: the columns decide" };
            r.text(label, Rect::new(l.switch.right() + 10.0, l.switch.y - 4.0, 200.0, l.switch.h + 8.0), &TextStyle::new(theme.text));
            kit::button(r, &theme, l.start_from, "START FROM\u{2026}", start_look);
            kit::button(r, &theme, l.guide_button, guide_label(app), guide_look);
        }
        self.paint_here(&theme);
        self.paint_confirm(&theme);
        self.paint_editor(&theme);
        self.paint_problems(&theme);
        if l.guide.h > 0.0 {
            let lines = self.guide_lines(l.guide.w);
            let r = &mut self.renderer;
            r.push_clip(l.guide);
            r.text("Guide", Rect::new(l.guide.x, l.guide.y, 100.0, 18.0), &TextStyle::new(theme.text).semibold());
            let style = TextStyle::new(theme.muted).size(ksize::NOTE_FONT);
            let cw = r.mono_char_width(GUIDE_MONO);
            for (i, (line, example)) in lines.iter().enumerate() {
                let at = Rect::new(l.guide.x, l.guide.y + 22.0 + i as f32 * GUIDE_LINE, l.guide.w, GUIDE_LINE);
                if *example {
                    // As in the rules box: the rules in the text colour, the note after # muted.
                    let (code, note) = line.split_at(line.find('#').unwrap_or(line.len()));
                    r.mono_text(code, Rect::new(at.x + 8.0, at.y, at.w, at.h), GUIDE_MONO, theme.text);
                    if !note.is_empty() {
                        let x = at.x + 8.0 + code.chars().count() as f32 * cw;
                        r.mono_text(note, Rect::new(x, at.y, (at.right() - x).max(0.0), at.h), GUIDE_MONO, theme.muted);
                    }
                } else {
                    r.text(line, at, &style);
                }
            }
            r.pop_clip();
        }
        self.paint_suggestions(&theme);
        if b > 0.0 {
            self.renderer.set_origin(0.0, 0.0);
            frame::paint_classic_border(&mut self.renderer, &theme, Rect::new(0.0, 0.0, self.size.0 + 2.0 * b, self.size.1 + 2.0 * b));
        }
        if self.renderer.end() == FrameResult::DeviceLost {
            more = true;
        }
        if more {
            self.invalidate();
        }
    }

    /// The line saying which rules sort the folder on screen (an accent bar before it, as beside
    /// those rules in the rules box), with its link.
    fn paint_here(&mut self, theme: &Theme) {
        let l = self.lay.here;
        let r = &mut self.renderer;
        let link_style = here_link_style(theme.accent);
        let link = self.here.link.map(HereLink::label);
        let link_w = link.map_or(0.0, |s| r.measure(s, &link_style) + 4.0);
        let mut x = l.x;
        if !self.here.marked.is_empty() {
            r.fill_rect(Rect::new(x, l.y + 3.0, 2.5, l.h - 6.0), theme.accent);
            x += 9.0;
        }
        let hot = self.here.line.is_some() && self.hover == Some(Hot::Here);
        let style = here_style(if hot { theme.text_strong } else { theme.muted });
        let room = (l.right() - x - if link.is_some() { link_w + 8.0 } else { 0.0 }).max(0.0);
        let mut last_w = 0.0_f32;
        let mut widest = 0.0_f32;
        for (i, line) in self.here_lines.iter().enumerate() {
            last_w = (r.measure(line, &style) + 2.0).min(room);
            widest = widest.max(last_w);
            r.text(line, Rect::new(x, l.y + i as f32 * HERE_H, last_w, HERE_H), &style);
        }
        self.here_words = if self.here.line.is_some() { Rect::new(x, l.y, widest, l.h) } else { Rect::default() };
        self.here_link = Rect::default();
        if let Some(label) = link {
            let rect = Rect::new(x + last_w + 8.0, l.bottom() - HERE_H, link_w, HERE_H);
            let hot = self.hover == Some(Hot::HereLink);
            r.text(label, rect, &link_style.color(if hot { theme.text_strong } else { theme.accent }));
            if hot {
                r.fill_rect(Rect::new(rect.x, rect.bottom() - 3.0, rect.w - 4.0, 1.0), theme.text_strong);
            }
            self.here_link = rect;
        }
    }

    /// START FROM…'s question, when it asks: replace the rules, or keep them.
    fn paint_confirm(&mut self, theme: &Theme) {
        let l = self.lay;
        if self.confirm.is_none() || l.confirm.h <= 0.0 {
            return;
        }
        let look = |hot: Hot, s: &Self| Look { hover: s.hover == Some(hot), pressed: s.pressed == Some(hot), focused: false, enabled: true };
        let (replace, keep) = (look(Hot::Replace, self), look(Hot::Keep, self));
        let r = &mut self.renderer;
        kit::card(r, theme, l.confirm);
        for (i, line) in self.confirm_lines.iter().enumerate() {
            r.text(line, Rect::new(l.confirm.x + 12.0, l.confirm.y + 10.0 + i as f32 * 18.0, l.confirm.w - 24.0, 18.0), &TextStyle::new(theme.text));
        }
        kit::button(r, theme, l.keep, "KEEP MINE", keep);
        kit::primary_button(r, theme, l.replace, "REPLACE", replace);
    }

    fn paint_editor(&mut self, theme: &Theme) {
        let e = self.lay.editor;
        let typing = self.active;
        let cw = self.char_w;
        let caret_at = self.caret_point(self.caret);
        let list_open = self.list_open();
        let ghost = if list_open { self.completion.ghost(&self.text) } else { None };
        let gutter = self.gutter_w();
        let (caret_start, caret_line) = line_of(&self.text, self.caret);
        let caret_row_chars = self.text[caret_start..line_end(&self.text, self.caret)].chars().count();
        let (sel_a, sel_b) = self.selection();
        // The lines with a mistake, and with a tip (numbered from 0).
        let marks = line_marks(&self.recipe.messages);
        // The folder's rules the cursor is in (tinted), and their brackets when it's on either.
        let here_line = caret_line + 1;
        let block = self.recipe.blocks.iter().filter(|b| b.start <= here_line && here_line <= b.end).min_by_key(|b| b.end - b.start).cloned();
        let brackets = block.as_ref().filter(|b| b.closed && (b.start == here_line || b.end == here_line)).map(|b| (b.start, b.end));
        // What the cursor's line does (or its mistake), in faint words after it.
        let explained = if self.active && ghost.is_none() && sel_a == sel_b && !list_open { cursor_hint(&self.recipe, here_line) } else { None };
        // The scrollbars: down the right side, along the bottom.
        self.bar = Bar::of(theme);
        let (view_h, view_w) = (e.h - 2.0 * BOX_PAD, e.w - 2.0 * BOX_PAD - gutter);
        let lines = self.text.matches('\n').count() + 1;
        self.vtrack = Track::new(self.bar.track(e), lines as f32 * LINE_H, view_h, self.scroll.1, false);
        let across = Rect::new(e.x + BOX_PAD + gutter, e.bottom() - self.bar.w - self.bar.margin, (view_w - self.bar.w - self.bar.margin).max(0.0), self.bar.w);
        self.htrack = Track::new(across, self.widest(), view_w, self.scroll.0, true);
        let hot_bar = |h: Hot, s: &Self| s.hover == Some(h) || s.drag.is_some_and(|(d, _)| d == h);
        let (v_hot, h_hot) = (hot_bar(Hot::VBar, self), hot_bar(Hot::HBar, self));
        let r = &mut self.renderer;
        kit::field(r, theme, e, None, typing, Look::enabled());
        let inner = e.inset(BOX_PAD, BOX_PAD);
        // The rules that sort the folder on screen: an accent bar beside them.
        r.push_clip(Rect::new(e.x, inner.y, e.w, inner.h));
        for &(a, b) in &self.here.marked {
            let top = inner.y + (a.max(1) - 1) as f32 * LINE_H - self.scroll.1;
            let bottom = inner.y + b as f32 * LINE_H - self.scroll.1;
            r.fill_rect(Rect::new(e.x + 3.0, top + 2.0, 2.5, (bottom - top - 4.0).max(2.0)), theme.accent);
        }
        r.pop_clip();
        r.push_clip(inner);
        let number_w = gutter - GUTTER_MARK - GUTTER_GAP;
        let text_area = Rect::new(inner.x + gutter, inner.y, (inner.w - gutter).max(0.0), inner.h);
        let errors: Vec<(usize, usize)> = self.recipe.messages.iter().filter(|m| m.error).filter_map(|m| m.span).collect();
        let mut start = 0;
        let mut line = 0;
        loop {
            let end = line_end(&self.text, start);
            let y = inner.y + line as f32 * LINE_H - self.scroll.1;
            if y > inner.bottom() {
                break;
            }
            if y + LINE_H >= inner.y {
                if block.as_ref().is_some_and(|b| b.start <= line + 1 && line < b.end) {
                    r.fill_rect(Rect::new(text_area.x - 4.0, y, text_area.w + 4.0, LINE_H), theme.accent.with_alpha(0.07));
                }
                // The line's number (the cursor's in the text colour), with a mark for a problem.
                let number = (line + 1).to_string();
                let nx = inner.x + GUTTER_MARK + number_w - number.len() as f32 * cw;
                r.mono_text(&number, Rect::new(nx, y, number.len() as f32 * cw + cw, LINE_H), MONO, if line == caret_line { theme.text } else { theme.muted.with_alpha(0.7) });
                if let Some(&(_, error)) = marks.iter().find(|(l, _)| *l == line) {
                    let (mark, color) = if error { ("\u{25cf}", PROBLEM_RED) } else { ("\u{25b2}", theme.muted) };
                    r.text(mark, Rect::new(inner.x, y, GUTTER_MARK, LINE_H), &TextStyle::new(color).size(8.0));
                }
                r.push_clip(text_area);
                let row = &self.text[start..end];
                let x0 = text_area.x - self.scroll.0;
                let col = |pos: usize| self.text[start..pos.clamp(start, end)].chars().count() as f32;
                // The selection.
                if sel_a != sel_b && sel_a <= end && sel_b >= start {
                    let (a, b) = (col(sel_a.max(start)), col(sel_b.min(end)) + if sel_b > end { 0.5 } else { 0.0 });
                    // (An older Windows' selection is dark: only a wash of it, so the words stay readable.)
                    let wash = if theme.era.old() { theme.accent_tint.with_alpha(0.3) } else { theme.accent_tint };
                    r.fill_rect(Rect::new(x0 + a * cw, y, ((b - a) * cw).max(2.0), LINE_H), wash);
                }
                // The brackets of the cursor's folder rules, boxed.
                if let Some((open, close)) = brackets {
                    let at = if line + 1 == open { open_bracket(row) } else if line + 1 == close { close_bracket(row) } else { None };
                    if let Some(c) = at {
                        let cell = Rect::new(x0 + c as f32 * cw - 1.0, y + 1.5, cw + 2.0, LINE_H - 3.0);
                        r.fill_rect(cell, theme.accent.with_alpha(0.14));
                        r.stroke_rect(cell, 1.0, theme.accent.with_alpha(0.85));
                    }
                }
                // The words, coloured (word_color), split where the rules split them: at spaces,
                // commas and semicolons. "in … (", ")" and "(" are strong.
                let trimmed = row.trim_start();
                let code = sortrecipe::strip_note(row);
                let note = (code.len() < row.len()).then_some(code.len());
                let is_in = trimmed.len() > 3 && trimmed.get(..3).is_some_and(|head| head.eq_ignore_ascii_case("in ")) || matches!(code.trim(), ")" | "(");
                let sep = |c: char| c.is_whitespace() || c == ',' || c == ';';
                let mut i = 0;
                for piece in row.split_inclusive(sep) {
                    let at = start + i;
                    i += piece.len();
                    let in_note = note.is_some_and(|n| at - start >= n);
                    // A note, or a folder's path on an "in" line, is drawn as one (a comma in a
                    // folder's name is part of it).
                    let word = if in_note || is_in { piece.trim_end() } else { piece.trim_end_matches(sep) };
                    if word.is_empty() && piece.trim().is_empty() {
                        continue;
                    }
                    let x = x0 + col(at) * cw;
                    if !word.is_empty() {
                        let color = word_color(theme, word, in_note, is_in);
                        r.mono_text(word, Rect::new(x, y, word.chars().count() as f32 * cw + cw, LINE_H), MONO, color);
                    }
                    // The comma or semicolon after it.
                    let mark = piece[word.len()..].trim_end();
                    if !mark.is_empty() {
                        let mx = x + word.chars().count() as f32 * cw;
                        r.mono_text(mark, Rect::new(mx, y, mark.chars().count() as f32 * cw + cw, LINE_H), MONO, if in_note { theme.muted } else { theme.text });
                    }
                }
                // A red line under words Glint doesn't know.
                for &(a, b) in &errors {
                    if a < end.max(start + 1) && b > start && a >= start {
                        let (ca, cb) = (col(a), col(b.min(end)));
                        r.fill_rect(Rect::new(x0 + ca * cw, y + LINE_H - 3.0, ((cb - ca) * cw).max(cw), 1.5), PROBLEM_RED);
                    }
                }
                r.pop_clip();
            }
            if end >= self.text.len() {
                break;
            }
            start = end + 1;
            line += 1;
        }
        // The first suggestion as grey text after the cursor (or what the line does, fainter,
        // a little after it when it fits), and the cursor.
        r.push_clip(text_area);
        if self.active {
            let c = caret_at;
            if let Some(ghost) = ghost {
                let ghost: String = ghost.chars().take(80).collect();
                r.mono_text(&ghost, Rect::new(c.x, c.y, ghost.chars().count() as f32 * cw + cw, LINE_H), MONO, theme.muted.with_alpha(0.75));
            } else if let Some((said, mistake)) = explained {
                let x = text_area.x - self.scroll.0 + (caret_row_chars + 3) as f32 * cw;
                let room = text_area.right() - x - if self.vtrack.is_some() { self.bar.w + 2.0 * self.bar.margin } else { 0.0 };
                if room >= 90.0 {
                    let color = if mistake { PROBLEM_RED.with_alpha(0.85) } else { theme.muted.with_alpha(0.7) };
                    let style = TextStyle::new(color).size(ksize::NOTE_FONT);
                    r.text(&said, Rect::new(x, c.y, room, LINE_H), &style);
                }
            }
            r.fill_rect(Rect::new(c.x.round() - 0.5, c.y + 2.0, 1.5, LINE_H - 4.0), theme.text);
        }
        r.pop_clip();
        r.pop_clip();
        if let Some(t) = self.vtrack {
            t.paint(r, theme, v_hot);
        }
        if let Some(t) = self.htrack {
            t.paint(r, theme, h_hot);
        }
    }

    /// How far the rules box's text reaches across (its longest line, and a little room after it).
    fn widest(&self) -> f32 {
        let chars = self.text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0);
        chars as f32 * self.char_w + 48.0
    }

    /// Every problem, scrolled when there are more than fit.
    fn paint_problems(&mut self, theme: &Theme) {
        self.fixes.clear();
        self.problem_rects.clear();
        let p = self.lay.problems;
        self.ptrack = None;
        if p.h <= 0.0 {
            return;
        }
        let track = Rect::new(p.right() - self.bar.w, p.y + 2.0, self.bar.w, (p.h - 4.0).max(0.0));
        self.ptrack = Track::new(track, self.problem_content, p.h, self.problem_scroll, false);
        let bar_hot = self.hover == Some(Hot::PBar) || self.drag.is_some_and(|(d, _)| d == Hot::PBar);
        let right = p.right() - PROBLEM_BAR;
        let r = &mut self.renderer;
        r.push_clip(p);
        let mut y = p.y - self.problem_scroll;
        for (i, lines) in &self.problem_lines {
            let i = *i;
            let Some(m) = self.recipe.messages.get(i) else { continue };
            let h = PROBLEM_H + (lines.len().max(1) - 1) as f32 * PROBLEM_LINE;
            if y + h < p.y || y > p.bottom() {
                y += h;
                continue;
            }
            let (mark, color) = if m.error { ("\u{25cf}", PROBLEM_RED) } else { ("\u{25b2}", theme.muted) };
            r.text(mark, Rect::new(p.x, y, 14.0, PROBLEM_H), &TextStyle::new(color).size(9.0));
            let mut x_end = right;
            if let Some(fix) = &m.fix {
                let label = fix_text(m, fix);
                let style = TextStyle::new(theme.accent).semibold();
                let w = r.measure(&label, &style) + 4.0;
                let rect = Rect::new(right - w, y, w, PROBLEM_H);
                let hot = self.hover == Some(Hot::Fix(i));
                r.text(&label, rect, &style.color(if hot { theme.text_strong } else { theme.accent }));
                if hot {
                    r.fill_rect(Rect::new(rect.x, rect.bottom() - 3.0, rect.w - 4.0, 1.0), theme.text_strong);
                }
                self.fixes.push((rect, i));
                x_end = rect.x - 8.0;
            }
            // The words: clicking them goes to the line (they light up under the mouse).
            let hot = self.hover == Some(Hot::Problem(i));
            let color = if hot { theme.text_strong } else if m.error { theme.text } else { theme.muted };
            let style = TextStyle::new(color).size(ksize::NOTE_FONT);
            let words = Rect::new(p.x + 16.0, y, (x_end - p.x - 16.0).max(0.0), h);
            for (k, line) in lines.iter().enumerate() {
                let ly = if k == 0 { y } else { y + PROBLEM_H + (k - 1) as f32 * PROBLEM_LINE };
                r.text(line, Rect::new(words.x, ly, words.w, if k == 0 { PROBLEM_H } else { PROBLEM_LINE }), &style);
            }
            if m.line > 0 {
                self.problem_rects.push((words, i));
            }
            y += h;
        }
        r.pop_clip();
        if let Some(t) = self.ptrack {
            t.paint(r, theme, bar_hot);
        }
    }

    /// Clicking a problem's words: the cursor goes to its line (to the word it's about, if any).
    fn go_to_problem(&mut self, i: usize, app: &AppState) {
        let Some(m) = self.recipe.messages.get(i) else { return };
        match m.span {
            Some((a, _)) if a <= self.text.len() && self.text.is_char_boundary(a) => self.go_to(a, app),
            _ => self.go_to_line(m.line, app),
        }
    }

    /// The cursor to the start of line `n` (from 1).
    fn go_to_line(&mut self, n: usize, app: &AppState) {
        let mut start = 0;
        for _ in 1..n {
            match self.text[start..].find('\n') {
                Some(k) => start += k + 1,
                None => break,
            }
        }
        self.go_to(start, app);
    }

    fn go_to(&mut self, pos: usize, app: &AppState) {
        self.anchor = pos;
        self.caret = pos;
        self.want_col = None;
        self.last_edit = None;
        self.list_closed = true;
        self.reveal_caret();
        self.suggest(app);
    }

    /// The list under the cursor's line: up to eight suggestions, each with a few words.
    fn paint_suggestions(&mut self, theme: &Theme) {
        self.suggestion_rects.clear();
        if !self.list_open() || !self.active {
            return;
        }
        let at = self.caret_point(self.completion.start);
        let n = self.completion.items.len().min(POPUP_MAX);
        let first = self.pick.saturating_sub(POPUP_MAX - 1).min(self.completion.items.len().saturating_sub(n));
        let w = POPUP_W.min(self.size.0 - 2.0 * PAD);
        let x = at.x.clamp(PAD, (self.size.0 - PAD - w).max(PAD));
        let mut y = at.y + LINE_H + 2.0;
        let h = n as f32 * POPUP_ROW + 6.0;
        if y + h > self.size.1 - 4.0 {
            y = at.y - h - 2.0; // no room under the line: above it
        }
        let r = &mut self.renderer;
        let bg = Rect::new(x, y, w, h);
        // In a skin with sidebar panels: a rounded list in the sidebar panels' colour, the
        // suggestion picked in the selection's colour with rounded ends, like a menu.
        let rounded = kit::panels(theme);
        let radius = if rounded { theme.corner(h).min(8.0) } else { 0.0 };
        if rounded {
            r.fill_round_rect(Rect::new(bg.x + 1.0, bg.y + 3.0, bg.w, bg.h), radius, Color::hex(0).with_alpha(0.22));
            r.fill_paint(bg, radius, &theme.sidebar_panel);
            let edge = if theme.input_border.a > 0.0 { theme.input_border } else { theme.line };
            r.stroke_round_rect(bg.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, edge);
        } else {
            r.fill_rect(Rect::new(bg.x + 2.0, bg.y + 3.0, bg.w, bg.h), Color::hex(0).with_alpha(0.18));
            r.fill_rect(bg, theme.panel);
            r.stroke_rect(bg, 1.0, theme.line);
        }
        for (k, s) in self.completion.items.iter().enumerate().skip(first).take(n) {
            let row = Rect::new(x + 3.0, y + 3.0 + (k - first) as f32 * POPUP_ROW, w - 6.0, POPUP_ROW);
            let picked = k == self.pick;
            if rounded {
                if picked {
                    r.fill_paint(row, (radius - 2.0).max(0.0), &theme.accent_tint_paint);
                } else if self.hover == Some(Hot::Suggestion(k)) {
                    r.fill_paint(row, (radius - 2.0).max(0.0), &theme.hover_paint);
                }
            } else if picked {
                r.fill_rect(row, theme.accent_tint);
                r.fill_rect(Rect::new(row.x, row.y, 2.0, row.h), theme.select_edge);
            } else if self.hover == Some(Hot::Suggestion(k)) {
                r.fill_rect(row, theme.hover);
            }
            let label_w = (s.label.chars().count() as f32 * self.char_w + 12.0).min(row.w * 0.62);
            let (label_color, hint_color) = match (picked, rounded) {
                (true, true) => (theme.selection_text, theme.selection_details),
                (true, false) if theme.era.old() => (theme.selection_text, theme.selection_details),
                (true, false) => (theme.text_strong, theme.muted),
                _ => (theme.text, theme.muted),
            };
            r.mono_text(&s.label, Rect::new(row.x + 8.0, row.y, label_w, row.h), MONO, label_color);
            r.text(&s.hint, Rect::new(row.x + 14.0 + label_w, row.y, (row.w - label_w - 20.0).max(0.0), row.h), &TextStyle::new(hint_color).size(11.0));
            self.suggestion_rects.push(row);
        }
        if first > 0 || first + n < self.completion.items.len() {
            let more = format!("{} of {}", self.pick + 1, self.completion.items.len());
            r.text(&more, Rect::new(x, y + h - 14.0, w - 8.0, 12.0), &TextStyle::new(theme.muted).size(9.5).align(Align::Right));
        }
    }

    // ------------------------------------------------------------ the mouse and keys

    fn client_point(&self, lparam: LPARAM) -> Point {
        let (s, b) = (self.scale(), self.border());
        Point::new((lparam.0 & 0xffff) as i16 as f32 / s - b, ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s - b)
    }

    fn hot_at(&self, p: Point) -> Option<Hot> {
        if let Some(k) = self.suggestion_rects.iter().position(|r| r.contains(p)) {
            let first = self.pick.saturating_sub(POPUP_MAX - 1).min(self.completion.items.len().saturating_sub(self.suggestion_rects.len()));
            return Some(Hot::Suggestion(first + k));
        }
        let l = &self.lay;
        if l.close.contains(p) {
            return Some(Hot::Close);
        }
        if l.detach.contains(p) {
            return Some(Hot::Detach);
        }
        // The buttons first: in a narrow window the switch's words reach under them.
        if l.start_from.contains(p) {
            return Some(Hot::StartFrom);
        }
        if l.guide_button.contains(p) {
            return Some(Hot::Guide);
        }
        if kit::toggle_contains(l.switch, p) || Rect::new(l.switch.right(), l.switch.y - 4.0, 180.0, l.switch.h + 8.0).contains(p) {
            return Some(Hot::Switch);
        }
        if self.confirm.is_some() {
            if l.replace.contains(p) {
                return Some(Hot::Replace);
            }
            if l.keep.contains(p) {
                return Some(Hot::Keep);
            }
        }
        if self.here_link.contains(p) {
            return Some(Hot::HereLink);
        }
        if self.here_words.contains(p) {
            return Some(Hot::Here);
        }
        if l.problems.contains(p) {
            if self.ptrack.is_some_and(|t| t.hit(p)) {
                return Some(Hot::PBar);
            }
            if let Some((_, i)) = self.fixes.iter().find(|(r, _)| r.contains(p)) {
                return Some(Hot::Fix(*i));
            }
            if let Some((_, i)) = self.problem_rects.iter().find(|(r, _)| r.contains(p)) {
                return Some(Hot::Problem(*i));
            }
        }
        if self.vtrack.is_some_and(|t| t.hit(p)) {
            return Some(Hot::VBar);
        }
        if self.htrack.is_some_and(|t| t.hit(p)) {
            return Some(Hot::HBar);
        }
        l.editor.contains(p).then_some(Hot::Editor)
    }

    /// The scrollbar `h` as last drawn.
    fn track(&self, h: Hot) -> Option<Track> {
        match h {
            Hot::VBar => self.vtrack,
            Hot::HBar => self.htrack,
            Hot::PBar => self.ptrack,
            _ => None,
        }
    }

    /// Scrolls what the scrollbar `h` scrolls to `to`.
    fn set_scroll(&mut self, h: Hot, to: f32) {
        match h {
            Hot::VBar => self.scroll.1 = to,
            Hot::HBar => self.scroll.0 = to,
            Hot::PBar => self.problem_scroll = to,
            _ => {}
        }
        self.invalidate();
    }

    fn mouse_move(&mut self, p: Point, app: &AppState) {
        if !self.tracking {
            let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            // SAFETY: a valid struct for our own window.
            unsafe {
                let _ = TrackMouseEvent(&mut t);
            }
            self.tracking = true;
        }
        if let Some((h, grab)) = self.drag {
            if let Some(t) = self.track(h) {
                self.set_scroll(h, t.offset_at(p, grab));
            }
            return;
        }
        if self.selecting {
            let pos = self.pos_at(p);
            if pos != self.caret {
                self.caret = pos;
                self.reveal_caret();
                self.suggest(app);
                self.invalidate();
            }
            return;
        }
        let hot = self.hot_at(p);
        if hot != self.hover {
            self.hover = hot;
            self.invalidate();
        }
    }

    fn mouse_down(&mut self, p: Point, double: bool, app: &mut AppState) -> Vec<Ask> {
        let hot = self.hot_at(p);
        self.pressed = hot;
        let mut asks = Vec::new();
        match hot {
            Some(Hot::Suggestion(i)) => self.accept(i, app),
            Some(Hot::Switch) => {
                let on = !app.settings.custom_sort;
                self.flips.start("on", on, self.now(), kit::SWITCH_TIME);
                app.set_custom_sort(on, Some(self.text.clone()));
                self.applied = self.text.clone();
                asks.push(Ask::RedrawMain);
            }
            Some(h @ (Hot::VBar | Hot::HBar | Hot::PBar)) => {
                if let Some(t) = self.track(h) {
                    let grab = t.grab(p);
                    self.drag = Some((h, grab));
                    self.set_scroll(h, t.offset_at(p, grab));
                    // SAFETY: captures the mouse for our own window while dragging.
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
            }
            Some(Hot::Here) => {
                if let Some(n) = self.here.line {
                    self.go_to_line(n, app);
                }
            }
            Some(Hot::HereLink) => asks.extend(self.use_rules(app)),
            Some(Hot::Replace) => {
                if let Some(i) = self.confirm {
                    asks.extend(self.start_from(i, app));
                }
            }
            Some(Hot::Keep) => self.confirm = None,
            Some(Hot::StartFrom) => {
                let b = self.lay.start_from;
                let items = sortrecipe::PRESETS.iter().enumerate().map(|(i, (name, _))| MenuItem::new(i as u32 + 1, name)).collect();
                let at = self.menu_at(Point::new(b.right(), b.bottom() + 2.0));
                asks.push(Ask::Job(Job::Menu(Menu { tag: MENU_START_FROM, at, items, right_aligned: true })));
            }
            Some(Hot::Guide) => {
                app.settings.sort_guide_shown = !app.settings.sort_guide_shown;
            }
            Some(Hot::Fix(i)) => self.fix(i, app),
            Some(Hot::Problem(i)) => self.go_to_problem(i, app),
            Some(Hot::Detach) => asks.extend(self.toggle_detached(app)),
            Some(Hot::Editor) => {
                let pos = self.pos_at(p);
                if double {
                    // A word: from its start to its end.
                    let mut a = pos;
                    while a > 0 && is_word(self.text[..a].chars().next_back().unwrap_or(' ')) {
                        a = prev_char(&self.text, a);
                    }
                    let mut b = pos;
                    while b < self.text.len() && is_word(self.text[b..].chars().next().unwrap_or(' ')) {
                        b = next_char(&self.text, b);
                    }
                    self.anchor = a;
                    self.caret = b;
                } else {
                    self.move_to(pos, false);
                    self.selecting = true;
                    // SAFETY: captures the mouse for our own window while selecting.
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                self.list_closed = true;
                self.suggest(app);
            }
            Some(Hot::Close) | None => {}
        }
        self.invalidate();
        asks
    }

    fn mouse_up(&mut self, p: Point) -> Vec<Ask> {
        let was = self.pressed.take();
        if self.selecting || self.drag.is_some() {
            self.selecting = false;
            self.drag = None;
            // SAFETY: releases our own capture.
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        self.invalidate();
        if was == Some(Hot::Close) && self.hot_at(p) == Some(Hot::Close) {
            return vec![Ask::Job(Job::Close)];
        }
        Vec::new()
    }

    /// The wheel (`notches` up), or with `across` scrolling sideways (`notches` to the left): the
    /// rules box or the problems under the mouse.
    fn wheel(&mut self, p: Point, notches: f32, across: bool) {
        if self.lay.problems.contains(p) && !across {
            let max = (self.problem_content - self.lay.problems.h).max(0.0);
            self.problem_scroll = (self.problem_scroll - notches * 2.0 * PROBLEM_H).clamp(0.0, max);
        } else if self.lay.editor.contains(p) && across {
            let max = (self.widest() - (self.lay.editor.w - 2.0 * BOX_PAD - self.gutter_w())).max(0.0);
            self.scroll.0 = (self.scroll.0 - notches * 6.0 * self.char_w.max(4.0)).clamp(0.0, max);
        } else if self.lay.editor.contains(p) {
            let lines = self.text.matches('\n').count() as f32 + 1.0;
            let max = (lines * LINE_H - (self.lay.editor.h - 2.0 * BOX_PAD)).max(0.0);
            self.scroll.1 = (self.scroll.1 - notches * 3.0 * LINE_H).clamp(0.0, max);
        }
        self.invalidate();
    }

    /// A key in the rules box (the window's only place for the keyboard). True when it was used.
    fn key(&mut self, vk: u16, app: &mut AppState, asks: &mut Vec<Ask>) -> bool {
        let ctrl = key_down(VK_CONTROL.0);
        let alt = key_down(VK_MENU.0);
        let open = self.list_open();
        let text_len = self.text.len();
        match vk {
            // Esc closes the list, else lets go of the selection, else the question, else the window.
            _ if vk == VK_ESCAPE.0 => {
                if open {
                    self.list_closed = true;
                } else if self.caret != self.anchor {
                    self.anchor = self.caret;
                } else if self.confirm.is_some() {
                    self.confirm = None;
                } else {
                    asks.push(Ask::Job(Job::Close));
                }
            }
            _ if alt && !ctrl && (vk == VK_UP.0 || vk == VK_DOWN.0) => self.move_line(vk == VK_UP.0, app),
            // Other keys with Alt are Windows' (Alt+F4).
            _ if alt && !ctrl => return false,
            _ if vk == VK_TAB.0 => {
                if open {
                    self.accept(self.pick, app);
                }
            }
            _ if vk == VK_RETURN.0 => {
                if open {
                    self.accept(self.pick, app);
                } else {
                    let (a, b) = self.selection();
                    // After "in Downloads (" whose ")" never came: a line for the rules, and the ")".
                    match closing(&self.text, a, &self.recipe.unclosed).filter(|_| a == b) {
                        Some((with, at)) => {
                            self.replace(a, a, &with, app);
                            (self.caret, self.anchor) = (a + at, a + at);
                            self.reveal_caret();
                        }
                        None => self.insert("\n", app),
                    }
                    self.list_closed = true;
                }
            }
            _ if vk == VK_UP.0 || vk == VK_DOWN.0 => {
                let down = vk == VK_DOWN.0;
                if open {
                    let n = self.completion.items.len();
                    self.pick = if down { (self.pick + 1) % n } else { (self.pick + n - 1) % n };
                } else {
                    // Moving between lines never opens a list; only typing does.
                    self.line_move(if down { 1 } else { -1 });
                    self.list_closed = true;
                    self.suggest(app);
                }
            }
            _ if vk == VK_PRIOR.0 || vk == VK_NEXT.0 => {
                let lines = ((self.lay.editor.h - 2.0 * BOX_PAD) / LINE_H) as i32;
                self.line_move(if vk == VK_NEXT.0 { lines } else { -lines });
                self.list_closed = true;
            }
            _ if vk == VK_LEFT.0 || vk == VK_RIGHT.0 => {
                let right = vk == VK_RIGHT.0;
                let (a, b) = self.selection();
                let pos = if a != b && !key_down(VK_SHIFT.0) {
                    if right {
                        b
                    } else {
                        a
                    }
                } else if ctrl {
                    if right {
                        word_right(&self.text, self.caret)
                    } else {
                        word_left(&self.text, self.caret)
                    }
                } else if right {
                    next_char(&self.text, self.caret)
                } else {
                    prev_char(&self.text, self.caret)
                };
                self.move_to(pos, false);
                self.list_closed = true;
            }
            _ if vk == VK_HOME.0 || vk == VK_END.0 => {
                let pos = match (vk == VK_HOME.0, ctrl) {
                    (true, true) => 0,
                    (false, true) => text_len,
                    (true, false) => line_of(&self.text, self.caret).0,
                    (false, false) => line_end(&self.text, self.caret),
                };
                self.move_to(pos, false);
                self.list_closed = true;
            }
            _ if vk == VK_BACK.0 || vk == VK_DELETE.0 => {
                let (a, b) = self.selection();
                // One letter at a time is taken back together by Ctrl+Z; a word or a selection
                // on its own.
                let kind = if a == b && !ctrl { Edit::Deleting } else { Edit::Other };
                let (a, b) = if a != b {
                    (a, b)
                } else if vk == VK_BACK.0 {
                    (if ctrl { word_left(&self.text, a) } else { prev_char(&self.text, a) }, a)
                } else {
                    (a, if ctrl { word_right(&self.text, a) } else { next_char(&self.text, a) })
                };
                if a != b {
                    self.edit(a, b, "", kind, app);
                }
            }
            // (Not with Alt too: Ctrl+Alt is AltGr, which types letters like ą or @.)
            _ if ctrl && !alt && vk == VK_A.0 => {
                self.anchor = 0;
                self.caret = text_len;
            }
            _ if ctrl && !alt && (vk == VK_C.0 || vk == VK_X.0) => self.copy(vk == VK_X.0, app),
            _ if ctrl && !alt && vk == VK_V.0 => self.paste(app),
            _ if ctrl && !alt && vk == VK_Z.0 => self.undo_redo(key_down(VK_SHIFT.0), app),
            _ if ctrl && !alt && vk == VK_Y.0 => self.undo_redo(true, app),
            _ if ctrl && !alt && (vk == VK_OEM_2.0 || vk == VK_DIVIDE.0) => self.toggle_note(app),
            // Ctrl+Space: the words that fit where the cursor is.
            _ if ctrl && !alt && vk == VK_SPACE.0 => self.suggest_here(),
            _ => return false,
        }
        self.invalidate();
        true
    }

    /// What a job brought back: START FROM…'s pick (asked about first when it would replace rules
    /// of your own), or the right-click menu's.
    pub fn job_done(&mut self, done: JobDone, app: &mut AppState) -> Vec<Ask> {
        let JobDone::Picked(n) = done else { return Vec::new() };
        if n >= EDIT_UNDO {
            self.edit_pick(n, app);
            return Vec::new();
        }
        let Some(i) = n.checked_sub(1).map(|i| i as usize).filter(|&i| i < sortrecipe::PRESETS.len()) else { return Vec::new() };
        if self.would_lose_rules() {
            self.confirm = Some(i);
            self.invalidate();
            return Vec::new();
        }
        self.start_from(i, app)
    }

    pub fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM, app: &mut AppState) -> (Option<LRESULT>, Vec<Ask>) {
        let mut asks = Vec::new();
        let result = match msg {
            WM_PAINT => {
                // SAFETY: marks our own window as painted.
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                self.paint(app);
                Some(LRESULT(0))
            }
            WM_SIZE if wparam.0 == SIZE_MINIMIZED as usize => Some(LRESULT(0)),
            WM_SIZE => {
                let (w, h) = (lo_word(lparam.0 as usize), hi_word(lparam.0 as usize));
                self.renderer.resize(w, h);
                if w > 0 && h > 0 {
                    self.size = self.inner_size(w, h);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = lo_word(wparam.0).max(96);
                self.renderer.set_dpi(self.dpi);
                self.char_w = 0.0;
                if self.detached {
                    // On its own: where Windows suggests for the new scaling.
                    // SAFETY: with WM_DPICHANGED, lparam points to the suggested RECT.
                    let r = unsafe { *(lparam.0 as *const RECT) };
                    asks.push(Ask::Job(Job::Place(r)));
                } else if let Ok(owner) = unsafe { GetWindow(self.hwnd, GW_OWNER) } {
                    asks.extend(self.dock(owner).map(Ask::Job));
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_SETCURSOR if lo_word(lparam.0 as usize) == HTCLIENT => {
                let mut pt = POINT::default();
                // SAFETY: plain cursor queries and setting the cursor's shape.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                    let (s, b) = (self.scale(), self.border());
                    let p = Point::new(pt.x as f32 / s - b, pt.y as f32 / s - b);
                    let shape = match self.hot_at(p) {
                        Some(Hot::Editor) => IDC_IBEAM,
                        Some(Hot::Problem(_) | Hot::Fix(_) | Hot::Here | Hot::HereLink) => IDC_HAND,
                        _ => IDC_ARROW,
                    };
                    SetCursor(LoadCursorW(None, shape).ok());
                }
                Some(LRESULT(1))
            }
            WM_MOUSEMOVE => {
                let p = self.client_point(lparam);
                self.mouse_move(p, app);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover.take().is_some() {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_down(p, msg == WM_LBUTTONDBLCLK, app));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_up(p));
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.selecting = false;
                self.drag = None;
                Some(LRESULT(0))
            }
            // The right-click menu: in the rules box (the cursor goes there, unless it's on the
            // selection), or from the keyboard (the menu key, Shift+F10) at the cursor.
            WM_RBUTTONUP => {
                let p = self.client_point(lparam);
                if !self.lay.editor.contains(p) {
                    return (None, asks);
                }
                let pos = self.pos_at(p);
                let (a, b) = self.selection();
                if a == b || pos < a || pos > b {
                    self.go_to(pos, app);
                }
                self.list_closed = true;
                asks.push(Ask::Job(Job::Menu(self.edit_menu(p))));
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_CONTEXTMENU if lparam.0 == -1 || lparam.0 & 0xffff_ffff == 0xffff_ffff => {
                let c = self.caret_point(self.caret);
                self.list_closed = true;
                asks.push(Ask::Job(Job::Menu(self.edit_menu(Point::new(c.x, c.y + LINE_H)))));
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_EXITSIZEMOVE => {
                self.remember_place(app);
                None
            }
            // The wheel (with Shift, sideways), and a sideways wheel or swipe.
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                // SAFETY: converts in place for our own window.
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let (s, b) = (self.scale(), self.border());
                let notches = hi_word(wparam.0) as u16 as i16 as f32 / 120.0;
                let p = Point::new(pt.x as f32 / s - b, pt.y as f32 / s - b);
                if msg == WM_MOUSEHWHEEL {
                    self.wheel(p, -notches, true);
                } else {
                    self.wheel(p, notches, key_down(VK_SHIFT.0));
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => self.key(wparam.0 as u16, app, &mut asks).then_some(LRESULT(0)),
            WM_CHAR => {
                // A character past the first 65,536 (an emoji, say) comes in two halves, one
                // message each: the first waits for the second.
                let unit = wparam.0 as u32;
                let c = match (self.half_char.take(), unit) {
                    (_, 0xD800..=0xDBFF) => {
                        self.half_char = Some(unit as u16);
                        None
                    }
                    (Some(first), 0xDC00..=0xDFFF) => char::decode_utf16([first, unit as u16]).next().and_then(Result::ok),
                    _ => char::from_u32(unit),
                };
                if let Some(c) = c.filter(|c| !c.is_control()) {
                    if !key_down(VK_CONTROL.0) || key_down(VK_MENU.0) {
                        let (a, b) = self.selection();
                        self.edit(a, b, c.encode_utf8(&mut [0; 4]), Edit::Typing, app);
                    }
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                self.active = lo_word(wparam.0) != WA_INACTIVE;
                self.invalidate();
                None
            }
            WM_CLOSE => {
                asks.push(Ask::Job(Job::Close));
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                // What was typed last is used before the window goes; Ctrl+Z still takes it back
                // when it's opened again.
                if self.apply_due {
                    asks.extend(self.apply_now(app));
                }
                app.sort_history = Some((self.text.clone(), std::mem::take(&mut self.undo), std::mem::take(&mut self.redo)));
                // SAFETY: stops our own timers.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), APPLY_TIMER);
                    let _ = KillTimer(Some(self.hwnd), WATCH_TIMER);
                }
                None
            }
            WM_TIMER if wparam.0 == APPLY_TIMER => {
                asks.extend(self.apply_now(app));
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == WATCH_TIMER => {
                self.watch(app);
                Some(LRESULT(0))
            }
            WM_APP_LISTED => {
                // A folder was looked for, or listed: the tips about folders that aren't there.
                self.recipe = Recipe::parse(&self.text);
                self.check_places();
                self.suggest(app);
                self.invalidate();
                Some(LRESULT(0))
            }

            _ => None,
        };
        (result, asks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn ctrl_slash_makes_lines_notes_and_back() {
        assert_eq!(toggle_notes("-newest_first"), "# -newest_first");
        assert_eq!(toggle_notes("# -newest_first"), "-newest_first");
        assert_eq!(toggle_notes("  folder_first\n\n  -a_to_z"), "  # folder_first\n\n  # -a_to_z");
        assert_eq!(toggle_notes("  # folder_first\n\n  #-a_to_z"), "  folder_first\n\n  -a_to_z");
        // Some notes, some rules: all become notes.
        assert_eq!(toggle_notes("# a\nb"), "# # a\n# b");
        assert_eq!(toggle_notes("\n"), "\n");
        // A selection ending at the start of a line leaves that line out.
        assert_eq!(whole_lines("a\nb\nc", 0, 2), (0, 1));
        assert_eq!(whole_lines("a\nb\nc", 0, 3), (0, 3));
        assert_eq!(whole_lines("a\nb\nc", 3, 3), (2, 3));
    }

    #[test]
    fn alt_up_and_down_move_lines() {
        let text = "a\nbb\nccc";
        // "bb" up: the first two lines swap; the line went 2 bytes up.
        assert_eq!(swap_lines(text, 2, 4, true), Some(((0, 4), "bb\na".to_string(), -2)));
        // "bb" down.
        assert_eq!(swap_lines(text, 2, 4, false), Some(((2, 8), "ccc\nbb".to_string(), 4)));
        // Nowhere to go.
        assert_eq!(swap_lines(text, 0, 1, true), None);
        assert_eq!(swap_lines(text, 5, 8, false), None);
        // Two lines at once, past an empty last line.
        assert_eq!(swap_lines("a\nb\n", 0, 3, false), Some(((0, 4), "\na\nb".to_string(), 1)));
    }

    #[test]
    fn alt_down_keeps_the_selection_in_the_text() {
        // "a" selected to the start of the last line, then Alt+Down: the selection ended past the
        // text (and Glint closed); it ends at the end now.
        assert_eq!(move_lines("a\nb", 2, 0, false), Some(((0, 3), "b\na".to_string(), 3, 2)));
        // Ctrl+A on rules ending with an empty line, then Alt+Down.
        assert_eq!(move_lines("a\nb\n", 4, 0, false), Some(((0, 4), "\na\nb".to_string(), 4, 1)));
        // The cursor goes with its line.
        assert_eq!(move_lines("a\nbb\nccc", 3, 3, true), Some(((0, 4), "bb\na".to_string(), 1, 1)));
    }

    #[test]
    fn enter_after_an_open_bracket_adds_the_close() {
        let text = "in Downloads (";
        let r = Recipe::parse(text);
        assert_eq!(closing(text, text.len(), &r.unclosed), Some(("\n\n)".to_string(), 1)));
        // Indented, with a note after the bracket.
        let text = "  in Downloads (  # mine";
        assert_eq!(closing(text, text.len(), &Recipe::parse(text).unclosed), Some(("\n  \n  )".to_string(), 3)));
        // Closed already, or the cursor isn't at the end.
        let text = "in Downloads (\n)";
        assert_eq!(closing(text, 14, &Recipe::parse(text).unclosed), None);
        let text = "in Downloads (";
        assert_eq!(closing(text, 5, &Recipe::parse(text).unclosed), None);
        // "in = (", as the suggestions put it in.
        let text = "-newest\nin = (";
        assert_eq!(closing(text, text.len(), &Recipe::parse(text).unclosed), Some(("\n\n)".to_string(), 1)));
    }

    #[test]
    fn a_folder_that_isnt_there_gets_a_tip() {
        let text = "in C:\\Users\\me\\Dowloads (\n-newest_first\n)";
        let r = Recipe::parse(text);
        let w = r.places.iter().find(|w| !w.named).expect("the folder");
        let names = vec!["Desktop".to_string(), "Downloads".to_string()];
        let m = missing_tip(text, w, Some(&names)).expect("a tip");
        assert_eq!(m.text, "Glint can't find C:\\Users\\me\\Dowloads right now. Did you mean Downloads?");
        assert!(!m.error);
        assert_eq!(m.fix.as_deref(), Some("Downloads"));
        assert_eq!(m.span.map(|(a, b)| &text[a..b]), Some("Dowloads"));
        // Nothing close: just the tip.
        let m = missing_tip(text, w, Some(&["Music".to_string()])).expect("a tip");
        assert_eq!((m.fix, m.text.contains("Did you mean")), (None, false));
    }

    #[test]
    fn says_which_rules_sort_the_folder_on_screen() {
        let say = |on: bool, recipe: &str, column_now: bool| {
            let mut app = AppState::for_tests(Settings { custom_sort: on, ..Default::default() });
            app.tabs[0].folder.path = "C:\\t\\sub".into();
            app.tabs[0].column_now = column_now;
            here(&app, &Recipe::parse(recipe))
        };
        let h = say(false, "-a_to_z", false);
        assert_eq!((h.text.as_str(), h.link), ("Custom sort is off, so these rules don't sort anything yet.", Some(HereLink::TurnOn)));
        assert_eq!(say(true, "-a_to_z", true).link, Some(HereLink::UseRules));
        assert_eq!(say(true, "-a_to_z", false).text, "On screen: sub, sorted by the rules for every folder.");
        assert!(say(true, "in C:\\elsewhere (\n-a_to_z\n)", false).text.ends_with("so its column does."));
        let h = say(true, "-a_to_z\nin C:\\t\\sub (\n-newest_first\n)", false);
        assert_eq!((h.text.as_str(), h.line, h.marked.as_slice()), ("On screen: sub, sorted by its own rules (line 2).", Some(2), &[(2, 4)][..]));
        let h = say(true, "in C:\\t (\n-and_subfolders\n-newest_first\n)", false);
        assert_eq!(h.text, "On screen: sub, sorted by the rules for C:\\t and the folders inside it (line 1).");
    }

    #[test]
    fn words_glint_knows_are_coloured() {
        let mut same = Theme::dark();
        same.text_strong = same.text; // a skin whose bright text is its plain one
        for theme in [Theme::dark(), Theme::light(), same] {
            let ink = |w: &str| word_color(&theme, w, false, false);
            let group = group_color(&theme);
            assert_ne!(group, theme.text, "a kind of thing never looks like a word Glint doesn't know");
            assert_ne!(group, theme.accent, "nor like an order rule");
            for w in ["folder", "-file", "-image", "image_first", "-png", "-.blend", "-bigger_than_100mb", "updated_3_days", "-name_has_invoice", "-today", "-music", "-other"] {
                assert_eq!(ink(w), group, "{w}");
            }
            for w in ["-newest", "a_to_z", "newest_by_day", "-large_icons", "-and_subfolders"] {
                assert_eq!(ink(w), theme.accent, "{w}");
            }
            // Unknown, misspelt, or not finished: the plain colour (with the red line).
            for w in ["-imgae", "-zzzz", "-name_has_", "-bigger_than_", "-.", "sort"] {
                assert_eq!(ink(w), theme.text, "{w}");
            }
            assert_eq!(word_color(&theme, "image", true, false), theme.muted, "in a note");
            assert_eq!(word_color(&theme, "image", false, true), theme.text_strong, "on an in line");
        }
    }

    #[test]
    fn the_hint_after_a_line_says_its_mistake() {
        let text = "-newst_first\n-image\n-image -zz_zq\n-png -png";
        let r = Recipe::parse(text);
        // A mistake: in the same words as in the problems under the box.
        let (said, mistake) = cursor_hint(&r, 1).expect("a hint");
        assert!(mistake);
        let m = r.messages.iter().find(|m| m.line == 1 && m.error).expect("the mistake");
        assert_eq!(format!("Line 1: {said}"), problem_text(m));
        assert!(said.contains("Did you mean -newest_first?"), "{said}");
        // ...which the problems list first when the cursor is on its line.
        let first = r.messages.iter().position(|m| m.line == 1 && m.error).unwrap();
        let mut order: Vec<usize> = (0..r.messages.len()).collect();
        order.sort_by_key(|&i| (r.messages[i].line != 1, !r.messages[i].error, r.messages[i].line));
        assert_eq!(order[0], first);
        // No mistake: what the line does.
        assert_eq!(cursor_hint(&r, 2), Some(("Pictures first".to_string(), false)));
        // A tip and a mistake on one line: the mistake, and the red dot in the gutter.
        let (said, mistake) = cursor_hint(&r, 3).expect("a hint");
        assert!(mistake && said.contains("zz_zq"), "{said}");
        let marks = line_marks(&r.messages);
        assert_eq!(marks.iter().find(|(l, _)| *l == 2), Some(&(2, true)));
        assert_eq!(marks.iter().find(|(l, _)| *l == 0), Some(&(0, true)));
        // Just a tip: a grey mark, and the hint says what the line does.
        assert_eq!(marks.iter().find(|(l, _)| *l == 3), Some(&(3, false)));
        assert!(cursor_hint(&r, 4).is_some_and(|(_, mistake)| !mistake));
        assert_eq!(cursor_hint(&r, 9), None);
    }

    /// The guide's words and its example read without a mistake.
    #[test]
    fn the_guide_reads() {
        let example: Vec<&str> = GUIDE.iter().filter_map(|g| g.strip_prefix('|')).collect();
        let r = Recipe::parse(&example.join("\n"));
        assert!(!r.has_errors(), "{:?}", r.messages);
        let mut words = Vec::new();
        for g in GUIDE {
            let Some((head, list)) = g.split_once(":  ") else { continue };
            if ["Sort", "Kinds", "When", "Size", "Name", "View", "More"].contains(&head) {
                words.extend(list.split(", ").filter_map(|w| w.split_whitespace().next()));
            }
        }
        words.extend(["png", ".png", "-newest", "newest", "folder_first", "and_subfolders"]);
        assert!(words.len() > 35, "{words:?}");
        for w in words {
            let r = Recipe::parse(&format!("in C:\\x (\n{w}\n)"));
            assert!(!r.has_errors(), "{w}: {:?}", r.messages);
        }
    }

    #[test]
    fn brackets_are_found_before_any_note() {
        assert_eq!(open_bracket("in Downloads (  # (mine)"), Some(13));
        assert_eq!(close_bracket("  )  # )"), Some(2));
        assert_eq!(open_bracket("-a_to_z"), None);
    }
}
