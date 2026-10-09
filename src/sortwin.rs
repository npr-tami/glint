//! The custom sort order's editor (SORT > Edit custom order…, Settings > Files and folders > EDIT…):
//! a window of Glint's own, drawn in the Settings window's style (kit.rs), docked beside the main
//! window and moving and resizing with it (window/sorting.rs keeps it there).
//!
//! - **The rules box**, drawn by Glint with a monospaced font (so it can show more than an EDIT
//!   could): order rules in the accent colour, notes muted, a red line under a word Glint doesn't
//!   know, and suggestions like Minecraft's command box (complete.rs) - the first one as grey text
//!   after the cursor, up to eight in a list under the line. Tab (or Enter while the list is open)
//!   takes one, Up / Down pick, Esc closes the list.
//! - **Problems** under it: the line's mistake first; one with a fix ("did you mean -newest?")
//!   is a link that fixes it.
//! - **The switch** (custom sort on or off), **START FROM…** (ready-made orders) and a compact
//!   guide that can be hidden ("SortGuideHidden").
//! - **A preview** of the folder on screen sorted by the rules, with a heading for each group
//!   ("pictures  ·  newest first"). Example files when no folder is open.
//!
//! Every change is used a moment after typing stops (PREVIEW_TIMER): the preview, every open tab,
//! and settings.json ("SortRecipe"). Like the skin customiser, its state lives in the main window's
//! state and its messages go through the main window's guard; menus, showing and closing come back
//! as a [`Job`].

use std::sync::Arc;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, InvalidateRect, MonitorFromWindow, ScreenToClient, ValidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP, VK_V, VK_X, VK_Z,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindow, GetWindowRect, IsIconic, KillTimer, LoadCursorW, LoadIconW,
    RegisterClassExW, SetCursor, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, CS_DBLCLKS, GW_OWNER, HTCLIENT, IDC_ARROW,
    IDC_IBEAM, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE, SW_SHOWNOACTIVATE, WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE,
    WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_PAINT, WM_SETCURSOR, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW,
    WS_CLIPCHILDREN, WS_POPUP,
};

use crate::app::AppState;
use crate::complete::{self, Completion, Lister, Place};
use crate::folder::{self, Entry, EntryKind};
use crate::frame;
use crate::kit::{self, size as ksize, Look};
use crate::log;
use crate::render::{Align, FrameResult, Renderer, TextStyle};
use crate::shell;
use crate::sortrecipe::{self, Clock, Recipe};
use crate::theme::{size, Color, Theme};
use crate::ui::{Menu, MenuItem, Point, Rect};

pub const TITLE: &str = "Custom sort order";

/// The window's width beside the main window, in DIPs.
const WIDTH: f32 = 470.0;
const PAD: f32 = 16.0;
/// The rules box's text: size and line height.
const MONO: f32 = 12.5;
const LINE_H: f32 = 19.0;
const BOX_PAD: f32 = 8.0;
const PROBLEM_H: f32 = 18.0;
const GUIDE_LINE: f32 = 16.0;
const ROW_H: f32 = 22.0;
const POPUP_ROW: f32 = 22.0;
const POPUP_MAX: usize = 8;
const POPUP_W: f32 = 330.0;
/// The red of a word Glint doesn't know.
const PROBLEM_RED: Color = Color::hex(0xe5484d);

/// A folder's subfolders were listed for the suggestions (complete::Lister).
const WM_APP_LISTED: u32 = WM_APP + 86;
/// Typing stopped: use the rules (preview, tabs, settings).
const APPLY_TIMER: usize = 1;
const APPLY_DELAY_MS: u32 = 250;
/// Looks for a new folder on screen in the main window (the preview follows it).
const WATCH_TIMER: usize = 2;
const WATCH_MS: u32 = 400;
const MENU_START_FROM: u32 = 1;
/// How many items the preview shows at most.
const PREVIEW_MAX: usize = 400;

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
    Switch,
    StartFrom,
    Guide,
    Fix(usize),
    Editor,
    Suggestion(usize),
}

/// A line of the preview.
#[derive(Clone, Debug)]
enum Row {
    Heading(String),
    Item { name: String, folder: bool, date: String, size: String },
}

/// Where everything is, worked out before each frame.
#[derive(Clone, Copy, Debug, Default)]
struct Layout {
    close: Rect,
    switch: Rect,
    start_from: Rect,
    guide_button: Rect,
    editor: Rect,
    problems: Rect,
    guide: Rect,
    preview: Rect,
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
    /// The rules box's scroll, in DIPs.
    scroll: (f32, f32),
    char_w: f32,
    recipe: Recipe,
    completion: Completion,
    pick: usize,
    /// Esc closed the list: it stays closed until the next change.
    list_closed: bool,
    places: Vec<Place>,
    lister: Lister,
    /// The text last used (in the tabs and settings.json).
    applied: String,
    apply_due: bool,
    /// The preview: its rows, scroll, what it shows and what it was made from.
    rows: Vec<Row>,
    preview_scroll: kit::Scroller,
    preview_title: String,
    seen: (u64, String, usize, u64),
    /// The problems' links, as last drawn: (where, message number).
    fixes: Vec<(Rect, usize)>,
    /// Suggestions' rows, as last drawn.
    suggestion_rects: Vec<Rect>,
    flips: kit::Flips,
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

/// Makes the editor (hidden: show it with [`Job::Show`] once its state is kept), owned by `owner`
/// and docked beside it. None if Windows can't make it.
pub fn create(owner: HWND, app: &AppState) -> Option<SortWindow> {
    register_class();
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    let r = dock_rect(owner);
    // SAFETY: window creation and plain queries on our own windows.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GlintSortWindow"),
            PCWSTR(title.as_ptr()),
            WS_POPUP | WS_CLIPCHILDREN,
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
            scroll: (0.0, 0.0),
            char_w: 0.0,
            completion: Completion::default(),
            pick: 0,
            list_closed: true,
            places: Vec::new(),
            lister: Lister::default(),
            apply_due: false,
            rows: Vec::new(),
            preview_scroll: kit::Scroller::default(),
            preview_title: String::new(),
            seen: (0, String::new(), usize::MAX, 0),
            fixes: Vec::new(),
            suggestion_rects: Vec::new(),
            flips: kit::Flips::default(),
        };
        window.apply_theme(&app.theme);
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_ok() {
            window.size = ((rc.right - rc.left) as f32 / window.scale(), (rc.bottom - rc.top) as f32 / window.scale());
        }
        window.renderer.resize(rc.right as u32, rc.bottom as u32);
        window.places = places(app);
        window.make_preview(app);
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
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        // It sits beside the main window and goes where it goes: nothing to drag or resize.
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
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

/// The folders "in" can be followed by: the user's places, pinned folders, the open tabs' folders,
/// the subfolders of the folder on screen, and the drives.
fn places(app: &AppState) -> Vec<Place> {
    let mut out: Vec<Place> = shell::known_folders().into_iter().map(|k| Place { name: k.name.to_string(), path: k.path, hint: "Quick access".into() }).collect();
    let name = |p: &str| folder::file_name(p).to_string();
    for p in &app.settings.pinned {
        out.push(Place { name: name(p), path: p.clone(), hint: "Pinned".into() });
    }
    for t in &app.tabs {
        if !t.path.is_empty() && !crate::recycle::is_bin(&t.path) {
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

/// Example files for the preview when no folder is open.
fn examples(now: u64) -> Vec<Entry> {
    const MIN: u64 = 600_000_000;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    let make = |name: &str, folder: bool, size: u64, ago: u64| Entry {
        name: name.into(),
        lower: name.to_lowercase(),
        kind: if folder { EntryKind::Folder } else { EntryKind::File },
        size,
        modified: now.saturating_sub(ago),
        created: now.saturating_sub(ago).saturating_sub(HOUR),
        attributes: 0,
        ext: if folder { String::new() } else { folder::extension(name) },
        type_name: Arc::from(if folder { "File folder".to_string() } else { format!("{} File", folder::extension(name).to_uppercase()) }),
        target: None,
        free: 0,
    };
    vec![
        make("Projects", true, 0, 12 * DAY),
        make("Photos 2026", true, 0, 3 * HOUR),
        make("Old stuff", true, 0, 300 * DAY),
        make("setup.exe", false, 84_000_000, HOUR),
        make("report.pdf", false, 1_200_000, 5 * HOUR),
        make("notes.txt", false, 2_000, 20 * MIN),
        make("holiday.jpg", false, 3_400_000, DAY),
        make("screenshot.png", false, 900_000, 4 * DAY),
        make("design.psd", false, 90_000_000, DAY + 2 * HOUR),
        make("budget.xlsx", false, 45_000, 3 * DAY),
        make("script.ps1", false, 4_000, 2 * DAY),
        make("archive.zip", false, 250_000_000, 9 * DAY),
        make("clip.mp4", false, 120_000_000, 15 * DAY),
        make("song.mp3", false, 6_100_000, 40 * DAY),
        make("Work.lnk", false, 1_000, 60 * DAY),
        make("installer.msi", false, 30_000_000, 20 * DAY),
        make("taxes 2024.pdf", false, 310_000, 400 * DAY),
        make("Screenshot 2026-09-30.png", false, 1_400_000, 7 * DAY),
    ]
}

/// Reads the clipboard's text (None when there's none).
fn clipboard_text(owner: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and closed here; the memory is locked only while copied.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(|h: HANDLE| {
            let mem = HGLOBAL(h.0);
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                return None;
            }
            let max = GlobalSize(mem) / 2;
            let len = (0..max).find(|&i| *p.add(i) == 0).unwrap_or(max);
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
}

fn lo_word(v: usize) -> u32 {
    (v & 0xffff) as u32
}

fn hi_word(v: usize) -> u32 {
    ((v >> 16) & 0xffff) as u32
}

fn key_down(vk: u16) -> bool {
    // SAFETY: a plain key-state query.
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// The byte offset of the start of the line `pos` is on, and the line's number (from 0).
fn line_of(text: &str, pos: usize) -> (usize, usize) {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    (start, text[..start].matches('\n').count())
}

fn line_end(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |i| pos + i)
}

/// The byte offset of column `col` (in characters) of the line starting at `start`.
fn at_col(text: &str, start: usize, col: usize) -> usize {
    let end = line_end(text, start);
    text[start..end].char_indices().nth(col).map_or(end, |(i, _)| start + i)
}

fn col_of(text: &str, pos: usize) -> usize {
    let (start, _) = line_of(text, pos);
    text[start..pos].chars().count()
}

fn prev_char(text: &str, pos: usize) -> usize {
    text[..pos].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_char(text: &str, pos: usize) -> usize {
    text[pos..].chars().next().map_or(pos, |c| pos + c.len_utf8())
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
    let mut p = pos;
    while p < text.len() && is_word(text[p..].chars().next().unwrap_or(' ')) {
        p = next_char(text, p);
    }
    while p < text.len() && !is_word(text[p..].chars().next().unwrap_or('a')) && !text[p..].starts_with('\n') {
        p = next_char(text, p);
    }
    p
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
        self.char_w = 0.0;
        frame::apply_theme(self.hwnd, theme);
        self.invalidate();
    }

    /// Opened again while it's open.
    pub fn reopen(&mut self) -> Vec<Ask> {
        self.invalidate();
        vec![Ask::Job(Job::Activate)]
    }

    /// The main window moved or changed size: follow it.
    pub fn dock(&self, owner: HWND) -> Option<Job> {
        // SAFETY: plain queries on our own windows.
        if unsafe { IsIconic(owner).as_bool() } {
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

    // ------------------------------------------------------------ the text

    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    /// Replaces `a..b` with `with`, the cursor after it; remembered for Ctrl+Z.
    fn replace(&mut self, a: usize, b: usize, with: &str, app: &AppState) {
        self.undo.push((self.text.clone(), self.caret));
        if self.undo.len() > 300 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.text.replace_range(a..b, with);
        self.caret = a + with.len();
        self.anchor = self.caret;
        self.changed(app);
    }

    fn insert(&mut self, s: &str, app: &AppState) {
        let (a, b) = self.selection();
        self.replace(a, b, s, app);
    }

    /// The text changed: read it again, suggest again, and use it a moment after typing stops.
    fn changed(&mut self, app: &AppState) {
        self.recipe = Recipe::parse(&self.text);
        self.list_closed = false;
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
        let lister = &self.lister;
        let c = complete::complete(&self.text, self.caret, &self.places, &|dir| lister.get(dir));
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

    /// Scrolls the rules box so the cursor shows.
    fn reveal_caret(&mut self) {
        let e = self.lay.editor;
        if e.w <= 0.0 || self.char_w <= 0.0 {
            return;
        }
        let (_, line) = line_of(&self.text, self.caret);
        let (vh, vw) = (e.h - 2.0 * BOX_PAD, e.w - 2.0 * BOX_PAD);
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
        let col = ((p.x - e.x - BOX_PAD + self.scroll.0) / self.char_w.max(1.0)).round().max(0.0) as usize;
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
        Point::new(e.x + BOX_PAD + col_of(&self.text, pos) as f32 * self.char_w - self.scroll.0, e.y + BOX_PAD + line as f32 * LINE_H - self.scroll.1)
    }

    // ------------------------------------------------------------ using the rules

    /// Uses the rules now: in every tab, and saved (the main window saves what changed).
    fn apply_now(&mut self, app: &mut AppState) -> Vec<Ask> {
        self.apply_due = false;
        // SAFETY: stops our own timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), APPLY_TIMER);
        }
        self.make_preview(app);
        if self.text == self.applied {
            return Vec::new();
        }
        self.applied = self.text.clone();
        let on = app.settings.custom_sort;
        app.set_custom_sort(on, Some(self.text.clone()));
        vec![Ask::RedrawMain]
    }

    /// The preview: the folder on screen (or example files) sorted by the rules as typed.
    fn make_preview(&mut self, app: &AppState) {
        let tab = app.tab();
        let clock = Clock::now();
        let real = !tab.folder.path.is_empty() && !tab.folder.entries.is_empty() && !crate::recycle::is_bin(&tab.folder.path);
        let (entries, path) = if real { (tab.folder.entries.clone(), tab.folder.path.clone()) } else { (examples(clock.now), String::new()) };
        let mut view: Vec<u32> = (0..entries.len() as u32).filter(|&i| real.then_some(&entries[i as usize]).is_none_or(|e| e.is_visible(tab.show_hidden, tab.show_system))).collect();
        self.recipe.sort(&entries, &mut view, &path, &clock);
        let groups = self.recipe.has_groups(&path);
        self.rows.clear();
        let mut last = None;
        for &i in view.iter().take(PREVIEW_MAX) {
            let e = &entries[i as usize];
            if groups {
                let g = self.recipe.group_number(e, &path, &clock);
                if last != Some(g) {
                    last = Some(g);
                    self.rows.push(Row::Heading(self.recipe.group_heading(e, &path, &clock)));
                }
            }
            let folder = e.is_container();
            let date = folder::format_date_as(e.modified, crate::settings::DateFormat::Relative);
            let size = if folder { String::new() } else { folder::format_size(e.size) };
            self.rows.push(Row::Item { name: e.name.clone(), folder, date, size });
        }
        self.preview_title = if real {
            let name = folder::file_name(&path).to_string();
            match self.recipe.folder_label(&path) {
                Some(label) => format!("Preview  \u{00b7}  {name}  (in {label}:)"),
                None => format!("Preview  \u{00b7}  {name}"),
            }
        } else {
            "Preview  \u{00b7}  example files".into()
        };
        if view.len() > PREVIEW_MAX {
            self.preview_title.push_str(&format!("  \u{00b7}  first {PREVIEW_MAX}"));
        }
        self.seen = (tab.id, tab.folder.path.clone(), tab.folder.entries.len(), tab.load_ms.map_or(0, f64::to_bits));
    }

    /// WATCH_TIMER: another folder on screen (or it changed): the preview and the folders follow.
    fn watch(&mut self, app: &AppState) {
        let tab = app.tab();
        let seen = (tab.id, tab.folder.path.clone(), tab.folder.entries.len(), tab.load_ms.map_or(0, f64::to_bits));
        if seen != self.seen {
            self.places = places(app);
            self.make_preview(app);
            self.invalidate();
        }
    }

    // ------------------------------------------------------------ layout and painting

    fn layout(&mut self, app: &AppState) {
        let (w, h) = self.size;
        let mut l = Layout { close: Rect::new(w - ksize::CLOSE_W, 0.0, ksize::CLOSE_W, size::TITLE_H), ..Layout::default() };
        let mut y = size::TITLE_H + 10.0;
        l.switch = Rect::new(PAD, y + 4.0, ksize::TOGGLE_W, ksize::TOGGLE_H);
        let bw = kit::button_width(&mut self.renderer, "START FROM\u{2026}");
        l.start_from = Rect::new(w - PAD - bw, y, bw, ksize::BUTTON_H);
        if app.settings.sort_guide_hidden {
            let gw = kit::button_width(&mut self.renderer, "GUIDE");
            l.guide_button = Rect::new(l.start_from.x - 8.0 - gw, y, gw, ksize::BUTTON_H);
        }
        y += ksize::BUTTON_H + 12.0;
        let rest = h - y - PAD;
        let editor_h = (rest * 0.36).clamp(7.0 * LINE_H, 16.0 * LINE_H) + 2.0 * BOX_PAD;
        l.editor = Rect::new(PAD, y, w - 2.0 * PAD, editor_h);
        y += editor_h + 6.0;
        let problems = self.shown_problems().len().min(3);
        l.problems = Rect::new(PAD, y, w - 2.0 * PAD, problems as f32 * PROBLEM_H);
        y += l.problems.h + if problems > 0 { 8.0 } else { 2.0 };
        if !app.settings.sort_guide_hidden {
            let lines = self.guide_lines(w - 2.0 * PAD).len();
            l.guide = Rect::new(PAD, y, w - 2.0 * PAD, 22.0 + lines as f32 * GUIDE_LINE);
            y += l.guide.h + 10.0;
        }
        l.preview = Rect::new(PAD, y, w - 2.0 * PAD, (h - y - 8.0).max(0.0));
        self.lay = l;
    }

    /// The problems to show, as message numbers: the cursor line's first, then mistakes, then tips.
    fn shown_problems(&self) -> Vec<usize> {
        let (_, line) = line_of(&self.text, self.caret);
        let mut v: Vec<usize> = (0..self.recipe.messages.len()).collect();
        v.sort_by_key(|&i| {
            let m = &self.recipe.messages[i];
            (m.line != line + 1, !m.error, m.line)
        });
        v.truncate(3);
        v
    }

    /// The compact guide, broken to fit.
    fn guide_lines(&mut self, width: f32) -> Vec<String> {
        const GUIDE: [&str; 5] = [
            "Groups, in order:  -folder -image -video -audio -document -archive -program -code -hidden -png \u{2026} -other",
            "Order in groups:  -newest -oldest -biggest -smallest -a_to_z -by_type  (-newest_by_day)",
            "_first / _last: very top or bottom.  After a group, an order is its own:  -image -biggest",
            "Exact:  -updated_3_days  -older_than_1_year  -bigger_than_100mb  -name_has_invoice",
            "in Downloads:  rules for one folder.   # a note.   Spaces work: newest first",
        ];
        let style = TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT);
        GUIDE.iter().flat_map(|g| kit::wrap(&mut self.renderer, g, &style, width)).collect()
    }

    fn paint(&mut self, app: &mut AppState) {
        if self.char_w <= 0.0 {
            self.char_w = self.renderer.mono_char_width(MONO);
        }
        self.layout(app);
        let now = self.now();
        let mut more = self.preview_scroll.tick(now) | self.flips.tick(now);
        if !self.renderer.begin() {
            return;
        }
        let theme = app.theme.clone();
        let l = self.lay;
        self.renderer.clear(theme.window);
        kit::backdrop(&mut self.renderer, &theme, Rect::new(0.0, 0.0, self.size.0, self.size.1));
        // The title bar: the logo, the name, ×.
        {
            let r = &mut self.renderer;
            let bar = Rect::new(0.0, 0.0, self.size.0, size::TITLE_H);
            kit::title_bar(r, &theme, bar);
            let x = kit::logo(r, &theme, bar);
            let color = if self.active { theme.text } else { theme.muted };
            r.text("Sort order", Rect::new(x + 10.0, 0.0, (l.close.x - x - 10.0).max(0.0), size::TITLE_H), &TextStyle::new(color).size(13.0).semibold());
            let hot = self.hover == Some(Hot::Close);
            kit::close_button(r, &theme, l.close, hot, hot && self.pressed == Some(Hot::Close), self.active);
            // A hairline down the side it shares with the main window.
            r.fill_rect(Rect::new(0.0, 0.0, 1.0, self.size.1), theme.line);
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
            if l.guide_button.w > 0.0 {
                kit::button(r, &theme, l.guide_button, "GUIDE", guide_look);
            }
        }
        self.paint_editor(&theme);
        self.paint_problems(&theme);
        if l.guide.h > 0.0 {
            let lines = self.guide_lines(l.guide.w);
            let r = &mut self.renderer;
            r.text("Guide", Rect::new(l.guide.x, l.guide.y, 100.0, 18.0), &TextStyle::new(theme.text).semibold());
            let hide = Rect::new(l.guide.right() - 40.0, l.guide.y, 40.0, 18.0);
            let hot = self.hover == Some(Hot::Guide);
            r.text("Hide", hide, &TextStyle::new(if hot { theme.text } else { theme.accent }).align(Align::Right));
            let style = TextStyle::new(theme.muted).size(ksize::NOTE_FONT);
            for (i, line) in lines.iter().enumerate() {
                r.text(line, Rect::new(l.guide.x, l.guide.y + 22.0 + i as f32 * GUIDE_LINE, l.guide.w, GUIDE_LINE), &style);
            }
        }
        self.paint_preview(&theme);
        self.paint_suggestions(&theme);
        if self.renderer.end() == FrameResult::DeviceLost {
            more = true;
        }
        if more {
            self.invalidate();
        }
    }

    fn paint_editor(&mut self, theme: &Theme) {
        let e = self.lay.editor;
        let typing = self.active;
        let cw = self.char_w;
        let caret_at = self.caret_point(self.caret);
        let ghost = if self.list_open() { self.completion.ghost(&self.text) } else { None };
        let r = &mut self.renderer;
        kit::field(r, theme, e, None, typing, Look::enabled());
        let inner = e.inset(BOX_PAD, BOX_PAD);
        r.push_clip(inner);
        let (sel_a, sel_b) = (self.caret.min(self.anchor), self.caret.max(self.anchor));
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
                let row = &self.text[start..end];
                let x0 = inner.x - self.scroll.0;
                let col = |pos: usize| self.text[start..pos.clamp(start, end)].chars().count() as f32;
                // The selection.
                if sel_a != sel_b && sel_a <= end && sel_b >= start {
                    let (a, b) = (col(sel_a.max(start)), col(sel_b.min(end)) + if sel_b > end { 0.5 } else { 0.0 });
                    r.fill_rect(Rect::new(x0 + a * cw, y, ((b - a) * cw).max(2.0), LINE_H), theme.accent_tint);
                }
                // The words, coloured: notes muted, "in …:" strong, order rules in the accent.
                let trimmed = row.trim_start();
                let note = row.find('#').filter(|&i| i == 0 || row[..i].ends_with(char::is_whitespace));
                let is_in = trimmed.len() > 3 && trimmed[..3].eq_ignore_ascii_case("in ");
                let mut i = 0;
                for piece in row.split_inclusive(char::is_whitespace) {
                    let word = piece.trim_end();
                    let at = start + i;
                    i += piece.len();
                    if word.is_empty() {
                        continue;
                    }
                    let color = if note.is_some_and(|n| at - start >= n) {
                        theme.muted
                    } else if is_in {
                        theme.text_strong
                    } else if sortrecipe::is_order_word(word) {
                        theme.accent
                    } else {
                        theme.text
                    };
                    let x = x0 + col(at) * cw;
                    r.mono_text(word, Rect::new(x, y, word.chars().count() as f32 * cw + cw, LINE_H), MONO, color);
                }
                // A red line under words Glint doesn't know.
                for &(a, b) in &errors {
                    if a < end.max(start + 1) && b > start && a >= start {
                        let (ca, cb) = (col(a), col(b.min(end)));
                        r.fill_rect(Rect::new(x0 + ca * cw, y + LINE_H - 3.0, ((cb - ca) * cw).max(cw), 1.5), PROBLEM_RED);
                    }
                }
            }
            if end >= self.text.len() {
                break;
            }
            start = end + 1;
            line += 1;
        }
        // The first suggestion as grey text after the cursor, and the cursor.
        if self.active {
            let c = caret_at;
            if let Some(ghost) = ghost {
                let ghost: String = ghost.chars().take(80).collect();
                r.mono_text(&ghost, Rect::new(c.x, c.y, ghost.chars().count() as f32 * cw + cw, LINE_H), MONO, theme.muted.with_alpha(0.75));
            }
            r.fill_rect(Rect::new(c.x.round() - 0.5, c.y + 2.0, 1.5, LINE_H - 4.0), theme.text);
        }
        r.pop_clip();
    }

    fn paint_problems(&mut self, theme: &Theme) {
        self.fixes.clear();
        let p = self.lay.problems;
        if p.h <= 0.0 {
            return;
        }
        let shown = self.shown_problems();
        for (n, &i) in shown.iter().enumerate() {
            let m = &self.recipe.messages[i];
            let y = p.y + n as f32 * PROBLEM_H;
            let (mark, color) = if m.error { ("\u{25cf}", PROBLEM_RED) } else { ("\u{25b2}", theme.muted) };
            let r = &mut self.renderer;
            r.text(mark, Rect::new(p.x, y, 14.0, PROBLEM_H), &TextStyle::new(color).size(9.0));
            let mut text = if m.line > 0 { format!("Line {}: {}", m.line, m.text) } else { m.text.clone() };
            let mut x_end = p.right();
            if let Some(fix) = &m.fix {
                let label = format!("Fix: {fix}");
                let style = TextStyle::new(theme.accent).semibold();
                let w = r.measure(&label, &style) + 4.0;
                let rect = Rect::new(p.right() - w, y, w, PROBLEM_H);
                let hot = self.hover == Some(Hot::Fix(i));
                r.text(&label, rect, &style.color(if hot { theme.text_strong } else { theme.accent }));
                if hot {
                    r.fill_rect(Rect::new(rect.x, rect.bottom() - 3.0, rect.w - 4.0, 1.0), theme.text_strong);
                }
                self.fixes.push((rect, i));
                x_end = rect.x - 8.0;
                text = text.replace(" Did you mean", "  Did you mean");
            }
            r.text(&text, Rect::new(p.x + 16.0, y, (x_end - p.x - 16.0).max(0.0), PROBLEM_H), &TextStyle::new(if m.error { theme.text } else { theme.muted }).size(ksize::NOTE_FONT));
        }
    }

    fn paint_preview(&mut self, theme: &Theme) {
        let p = self.lay.preview;
        if p.h < 40.0 {
            return;
        }
        let r = &mut self.renderer;
        r.text(&self.preview_title, Rect::new(p.x, p.y, p.w, 20.0), &TextStyle::new(theme.text).semibold());
        let list = Rect::new(p.x, p.y + 24.0, p.w, p.h - 24.0);
        kit::card(r, theme, list);
        let inner = list.inset(1.0, 1.0);
        self.preview_scroll.set_extent(self.rows.len() as f32 * ROW_H + 8.0, inner.h);
        r.push_clip(inner);
        let first = (self.preview_scroll.offset / ROW_H).floor().max(0.0) as usize;
        let date_w = 110.0;
        let size_w = 70.0;
        // The sizes end clear of the scrollbar, however thick the skin makes it.
        let bar = kit::Bar::of(theme);
        let right = inner.right() - 1.0 - if theme.scrollbar_track.is_clear() { bar.w + bar.margin } else { theme.scrollbar_lane() };
        for (i, row) in self.rows.iter().enumerate().skip(first) {
            let y = inner.y + 4.0 + i as f32 * ROW_H - self.preview_scroll.offset;
            if y > inner.bottom() {
                break;
            }
            match row {
                Row::Heading(text) => {
                    // "PICTURES  biggest first": the group, then its own order in softer text.
                    let (group, order) = text.split_once("  \u{00b7}  ").unwrap_or((text.as_str(), ""));
                    // In a skin with sidebar panels, on a header bar like theirs.
                    let bar = kit::panels(theme);
                    let (group_color, order_color) = if bar { (theme.panel_header_text, theme.panel_header_text.with_alpha(theme.panel_header_text.a * 0.75)) } else { (theme.accent, theme.muted) };
                    if bar {
                        let band = Rect::new(inner.x + 4.0, y + 2.0, (inner.w - 8.0).max(0.0), ROW_H - 4.0);
                        let radius = theme.corner(band.h).min(6.0);
                        r.fill_paint(band, radius, &theme.panel_header);
                        if theme.highlight.a > 0.0 {
                            r.fill_rect(Rect::new(band.x + radius, band.y + 1.0, (band.w - 2.0 * radius).max(0.0), 1.0), theme.highlight);
                        }
                    }
                    let style = TextStyle::new(group_color).semibold().size(11.5);
                    let up = group.to_uppercase();
                    let w = r.measure(&up, &style);
                    r.text(&up, Rect::new(inner.x + 10.0, y, inner.w - 20.0, ROW_H), &style);
                    if !order.is_empty() {
                        r.text(order, Rect::new(inner.x + 18.0 + w, y, (inner.w - w - 28.0).max(0.0), ROW_H), &TextStyle::new(order_color).size(11.5));
                    }
                    if !bar {
                        r.fill_rect(Rect::new(inner.x + 10.0, y + ROW_H - 2.0, inner.w - 20.0, 1.0), theme.line);
                    }
                }
                Row::Item { name, folder, date, size } => {
                    let style = TextStyle::new(theme.text);
                    let mark = if *folder { "\u{25b8}" } else { "" };
                    r.text(mark, Rect::new(inner.x + 12.0, y, 12.0, ROW_H), &TextStyle::new(theme.muted).size(10.0));
                    r.text(name, Rect::new(inner.x + 26.0, y, (right - size_w - date_w - inner.x - 26.0).max(0.0), ROW_H), &if *folder { style.semibold() } else { style });
                    r.text(date, Rect::new(right - size_w - date_w, y, date_w, ROW_H), &TextStyle::new(theme.muted).size(11.0));
                    r.text(size, Rect::new(right - size_w, y, size_w, ROW_H), &TextStyle::new(theme.muted).size(11.0).align(Align::Right));
                }
            }
        }
        if self.rows.is_empty() {
            r.text("Nothing here", inner.inset(12.0, 0.0), &TextStyle::new(theme.muted));
        }
        r.pop_clip();
        kit::scrollbar(r, theme, bar.track(list), &self.preview_scroll, false);
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
        let s = self.scale();
        Point::new((lparam.0 & 0xffff) as i16 as f32 / s, ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s)
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
        if kit::toggle_contains(l.switch, p) || Rect::new(l.switch.right(), l.switch.y - 4.0, 180.0, l.switch.h + 8.0).contains(p) {
            return Some(Hot::Switch);
        }
        if l.start_from.contains(p) {
            return Some(Hot::StartFrom);
        }
        if l.guide_button.contains(p) || (l.guide.h > 0.0 && Rect::new(l.guide.right() - 40.0, l.guide.y, 40.0, 18.0).contains(p)) {
            return Some(Hot::Guide);
        }
        if let Some((_, i)) = self.fixes.iter().find(|(r, _)| r.contains(p)) {
            return Some(Hot::Fix(*i));
        }
        l.editor.contains(p).then_some(Hot::Editor)
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
            Some(Hot::StartFrom) => {
                let b = self.lay.start_from;
                let items = sortrecipe::PRESETS.iter().enumerate().map(|(i, (name, _))| MenuItem::new(i as u32 + 1, name)).collect();
                asks.push(Ask::Job(Job::Menu(Menu { tag: MENU_START_FROM, at: Point::new(b.right(), b.bottom() + 2.0), items, right_aligned: true })));
            }
            Some(Hot::Guide) => {
                app.settings.sort_guide_hidden = !app.settings.sort_guide_hidden;
            }
            Some(Hot::Fix(i)) => self.fix(i, app),
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
        if self.selecting {
            self.selecting = false;
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

    fn wheel(&mut self, p: Point, notches: f32) {
        if self.lay.editor.contains(p) {
            let lines = self.text.matches('\n').count() as f32 + 1.0;
            let max = (lines * LINE_H - (self.lay.editor.h - 2.0 * BOX_PAD)).max(0.0);
            self.scroll.1 = (self.scroll.1 - notches * 3.0 * LINE_H).clamp(0.0, max);
        } else {
            let now = self.now();
            self.preview_scroll.scroll_by(-notches * 3.0 * ROW_H, now);
        }
        self.invalidate();
    }

    /// A key in the rules box (the window's only place for the keyboard). True when it was used.
    fn key(&mut self, vk: u16, app: &mut AppState, asks: &mut Vec<Ask>) -> bool {
        let ctrl = key_down(VK_CONTROL.0);
        let open = self.list_open();
        let text_len = self.text.len();
        match vk {
            _ if vk == VK_ESCAPE.0 => {
                if open {
                    self.list_closed = true;
                } else {
                    asks.push(Ask::Job(Job::Close));
                }
            }
            _ if vk == VK_TAB.0 => {
                if open {
                    self.accept(self.pick, app);
                }
            }
            _ if vk == VK_RETURN.0 => {
                if open {
                    self.accept(self.pick, app);
                } else {
                    self.insert("\n", app);
                    self.list_closed = true;
                }
            }
            _ if vk == VK_UP.0 || vk == VK_DOWN.0 => {
                let down = vk == VK_DOWN.0;
                if open {
                    let n = self.completion.items.len();
                    self.pick = if down { (self.pick + 1) % n } else { (self.pick + n - 1) % n };
                } else {
                    self.line_move(if down { 1 } else { -1 });
                    self.suggest(app);
                }
            }
            _ if vk == VK_PRIOR.0 || vk == VK_NEXT.0 => {
                let lines = ((self.lay.editor.h - 2.0 * BOX_PAD) / LINE_H) as i32;
                self.line_move(if vk == VK_NEXT.0 { lines } else { -lines });
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
                let (a, b) = if a != b {
                    (a, b)
                } else if vk == VK_BACK.0 {
                    (if ctrl { word_left(&self.text, a) } else { prev_char(&self.text, a) }, a)
                } else {
                    (a, if ctrl { word_right(&self.text, a) } else { next_char(&self.text, a) })
                };
                if a != b {
                    self.replace(a, b, "", app);
                }
            }
            _ if ctrl && vk == VK_A.0 => {
                self.anchor = 0;
                self.caret = text_len;
            }
            _ if ctrl && (vk == VK_C.0 || vk == VK_X.0) => {
                let (a, b) = self.selection();
                if a != b {
                    let _ = shell::copy_text(self.hwnd, &self.text[a..b].replace('\n', "\r\n"));
                    if vk == VK_X.0 {
                        self.replace(a, b, "", app);
                    }
                }
            }
            _ if ctrl && vk == VK_V.0 => {
                if let Some(t) = clipboard_text(self.hwnd) {
                    let t = t.replace("\r\n", "\n").replace('\r', "\n").replace('\t', " ");
                    self.insert(&t, app);
                    self.list_closed = true;
                }
            }
            _ if ctrl && vk == VK_Z.0 => {
                let (from, to) = if key_down(VK_SHIFT.0) { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
                if let Some((text, caret)) = from.pop() {
                    to.push((std::mem::replace(&mut self.text, text), self.caret));
                    self.caret = caret.min(self.text.len());
                    while !self.text.is_char_boundary(self.caret) {
                        self.caret -= 1;
                    }
                    self.anchor = self.caret;
                    let undo = std::mem::take(&mut self.undo);
                    let redo = std::mem::take(&mut self.redo);
                    self.changed(app);
                    (self.undo, self.redo) = (undo, redo);
                    self.list_closed = true;
                }
            }
            _ => return false,
        }
        self.invalidate();
        true
    }

    /// What a job brought back (START FROM…'s pick).
    pub fn job_done(&mut self, done: JobDone, app: &mut AppState) -> Vec<Ask> {
        if let JobDone::Picked(n) = done {
            if let Some((_, text)) = n.checked_sub(1).and_then(|i| sortrecipe::PRESETS.get(i as usize)) {
                self.set_text(text, app);
                return self.apply_now(app);
            }
        }
        Vec::new()
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
                    let s = self.scale();
                    self.size = (w as f32 / s, h as f32 / s);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = lo_word(wparam.0).max(96);
                self.renderer.set_dpi(self.dpi);
                self.char_w = 0.0;
                if let Ok(owner) = unsafe { GetWindow(self.hwnd, GW_OWNER) } {
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
                    let s = self.scale();
                    let p = Point::new(pt.x as f32 / s, pt.y as f32 / s);
                    let beam = self.hot_at(p) == Some(Hot::Editor);
                    SetCursor(LoadCursorW(None, if beam { IDC_IBEAM } else { IDC_ARROW }).ok());
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
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                // SAFETY: converts in place for our own window.
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let s = self.scale();
                self.wheel(Point::new(pt.x as f32 / s, pt.y as f32 / s), hi_word(wparam.0) as u16 as i16 as f32 / 120.0);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => self.key(wparam.0 as u16, app, &mut asks).then_some(LRESULT(0)),
            WM_CHAR => {
                if let Some(c) = char::from_u32(wparam.0 as u32).filter(|c| !c.is_control()) {
                    if !key_down(VK_CONTROL.0) || key_down(windows::Win32::UI::Input::KeyboardAndMouse::VK_MENU.0) {
                        self.insert(c.encode_utf8(&mut [0; 4]), app);
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
                // What was typed last is used before the window goes.
                if self.apply_due {
                    asks.extend(self.apply_now(app));
                }
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
                self.suggest(app);
                self.invalidate();
                Some(LRESULT(0))
            }
            _ => None,
        };
        (result, asks)
    }
}
