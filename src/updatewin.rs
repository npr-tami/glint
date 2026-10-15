//! The update prompt: "Glint 0.3.1 is ready (you have 0.3.0)", what's new (the changelog's sections
//! since the version Glint has, scrolling), and the person's four choices: [UPDATE], [REMIND ME AGAIN
//! LATER], a checkbox "Don't remind me about this update", and - quieter, at the bottom left - "Turn
//! off automatic updates". A small window of Glint's own, owned by the main window, drawn with kit.rs
//! in Glint's look (update.rs decides when it shows).
//!
//! How it fits in:
//! - **Its own state.** The prompt only shows what it was given and passes the person's answer on:
//!   it posts WM_APP_UPDATE / ANSWERED to the main window, which carries it out (remembers "later"
//!   or the skipped version, turns automatic checks off, or starts installing). So its state lives
//!   here, on the UI thread, not in the main window's - and never holds the main window's state.
//! - **Never re-entered.** The state is borrowed only while one of its messages is handled; a
//!   message that arrives meanwhile (one Windows sends from inside a call) gets Windows' default
//!   handling. Things that would send it messages while borrowed - destroying it, moving it after a
//!   DPI change, showing it - are done after the borrow ends (`Act`). The main window opens it
//!   hidden and lets it show itself (WM_APP_PROMPT_SHOW), and closes it with a posted message, so
//!   the activation moving between the two never arrives while the main window is busy.
//! - **Closing it** (×, Esc, Alt+F4) is "Remind me again later" - or, with the box ticked, "Don't
//!   remind me about this update". "Turn off automatic updates" closes it too, and Glint stops
//!   looking by itself ("CheckForUpdates" off; Settings > About can still look, and turn it on
//!   again). While installing it stays open, says so, and shows what went wrong if it did (the
//!   window quits and the new Glint starts when it went well).
//!
//! Keyboard: with Glint in front, it pops up without taking the keyboard (the person may be typing
//! a name there). Tab / Shift+Tab go round the checkbox, "Turn off automatic updates" and the two
//! buttons; Enter presses UPDATE (or the button that has the keyboard), Space presses or ticks what
//! has the keyboard - neither in the first second after it pops up, when the person may still be
//! typing in another window; the arrows, PgUp / PgDn, Home / End scroll what's new; Esc closes
//! (remind me again later).

use crate::winutil::{hi_word, lo_word};
use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, InvalidateRect, MonitorFromWindow, RedrawWindow, ScreenToClient, ValidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME,
    VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetAncestor, GetClientRect, GetForegroundWindow, GetWindowRect, IsWindow, KillTimer,
    LoadCursorW, LoadIconW, PostMessageW, RegisterClassExW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, GA_ROOTOWNER,
    HTCAPTION, HTCLIENT, IDC_ARROW, MINMAXINFO, SIZE_MINIMIZED, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_SHOW, SW_SHOWNOACTIVATE, WA_INACTIVE,
    WINDOW_EX_STYLE, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCDESTROY,
    WM_NCHITTEST, WM_PAINT, WM_SIZE, WM_SYSCHAR, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_SYSMENU, WS_THICKFRAME,
};

use crate::frame;
use crate::kit::{self, size as ksize, Look, Scroller};
use crate::log;
use crate::render::{FrameResult, Renderer, TextStyle};
use crate::theme::{size, Color, Theme};
use crate::ui::{Point, Rect};
use crate::update::{self, Release, ANSWERED, ANSWER_LATER, ANSWER_OFF, ANSWER_SKIP, ANSWER_UPDATE, WM_APP_UPDATE};

/// The window's title (Alt+Tab, the taskbar's thumbnails).
const TITLE: &str = "Glint update";
/// Its size when it opens, and the smallest it gets (DIPs).
const DEFAULT_W: f32 = 560.0;
const DEFAULT_H: f32 = 500.0;
const MIN_W: f32 = 420.0;
const MIN_H: f32 = 360.0;
const PAD: f32 = 24.0;
/// Inside the box with what's new.
const BOX_PAD: f32 = 14.0;
const BOX_SCROLLBAR: f32 = 10.0;
const HEADING_FONT: f32 = 18.0;
const CHECK_ROW_H: f32 = 24.0;
const BUTTON_GAP: f32 = 6.0;
/// "Turn off automatic updates": the room round its words, and the least room between it and the
/// buttons when it sits beside them (otherwise it goes on a line of its own above them).
const QUIET_PAD: f32 = 4.0;
const QUIET_GAP: f32 = 16.0;
const STATUS_LINE: f32 = 17.0;
const WHEEL_STEP: f32 = 60.0;
/// Enter and Space press nothing this long (seconds) after the window pops up: the person may still
/// be typing in another window, and those keys were meant for it.
const KEY_GRACE: f64 = 1.0;
/// Shows the window (posted by `open`: see the module comment).
const WM_APP_PROMPT_SHOW: u32 = WM_APP + 71;
/// Closes it without an answer (posted by `close`).
const WM_APP_PROMPT_QUIT: u32 = WM_APP + 72;
/// Trying to draw again after the graphics card refused a frame.
const RETRY_TIMER: usize = 1;
/// The words of the choices (buttons in capitals, like every button of Glint's).
const UPDATE: &str = "UPDATE";
const LATER: &str = "REMIND ME AGAIN LATER";
const SKIP: &str = "Don't remind me about this update";
const OFF: &str = "Turn off automatic updates";

/// The prompt's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctl {
    Check,
    Off,
    Update,
    Later,
}

/// Their Tab order: as they read, the checkbox, then the bottom line from the left.
const TAB_ORDER: [Ctl; 4] = [Ctl::Check, Ctl::Off, Ctl::Update, Ctl::Later];

/// What is under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hot {
    Close,
    Ctl(Ctl),
    Thumb,
}

/// Done after a message, once the state isn't borrowed any more.
enum Act {
    /// Tell the main window what the person said.
    Answer(isize),
    Destroy,
    /// `quiet`: without taking the keyboard (see WM_APP_PROMPT_SHOW).
    Show { quiet: bool },
    Place(RECT),
}

/// What kind of line of the changelog this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// "## 1.0.19-beta.1 · 15/10/2026" - a version, and when it came out.
    Version,
    /// "### Fixes".
    Heading,
    /// A list item.
    Bullet,
    /// Plain text.
    Text,
}

/// One piece of the changelog: a heading, a list item or a paragraph, its Markdown taken out.
#[derive(Clone, Debug, PartialEq)]
struct Block {
    kind: Kind,
    /// How deep a list item sits (0 = not nested).
    depth: usize,
    text: String,
    /// What goes beside a version, in grey: its date.
    aside: String,
}

/// A line of the changelog as laid out (page coordinates inside the box).
struct Line {
    kind: Kind,
    text: String,
    aside: String,
    x: f32,
    y: f32,
    /// A list item's first line: draw its bullet here.
    bullet: Option<f32>,
}

/// Markdown's inline marks taken out: **bold**, `code`, [text](link) -> text.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|end| (mid, mid + 2 + end))) {
            Some((mid, end)) => {
                out.push_str(&rest[..start]);
                out.push_str(&after[..mid]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

/// The changelog's Markdown as blocks: "## " versions ("## 1.0.19-beta.1 · 15/10/2026" becomes
/// "Glint 1.0.19-beta.1" with the date beside it), "### " headings, "- " list items (lines indented
/// under one belong to it), and paragraphs.
fn blocks(md: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    // Whether the next indented or plain line goes on with the last block.
    let mut open = false;
    for raw in md.lines() {
        let line = raw.trim_end();
        let text = line.trim_start();
        if text.is_empty() {
            open = false;
            continue;
        }
        let indent = line.len() - text.len();
        if let Some(h) = text.strip_prefix("## ") {
            let h = plain(h.trim());
            // "## Glint 1.0.19-beta.1 15/10/2026 17:21" too.
            let h = h.strip_prefix("Glint ").map(str::to_string).unwrap_or(h);
            let (version, aside) = h.split_once(char::is_whitespace).unwrap_or((&h, ""));
            let aside = aside.trim().trim_start_matches(['·', ':']).trim().to_string();
            let text = if version.starts_with(|c: char| c.is_ascii_digit()) { format!("Glint {version}") } else { version.to_string() };
            out.push(Block { kind: Kind::Version, depth: 0, text, aside });
            open = false;
        } else if let Some(h) = text.strip_prefix("### ").or_else(|| text.strip_prefix("#### ")) {
            out.push(Block { kind: Kind::Heading, depth: 0, text: plain(h.trim()), aside: String::new() });
            open = false;
        } else if let Some(item) = text.strip_prefix("- ").or_else(|| text.strip_prefix("* ")) {
            out.push(Block { kind: Kind::Bullet, depth: indent / 2, text: plain(item.trim()), aside: String::new() });
            open = true;
        } else if open && out.last().is_some_and(|b| b.kind == Kind::Bullet || b.kind == Kind::Text) {
            if let Some(last) = out.last_mut() {
                last.text.push(' ');
                last.text.push_str(&plain(text));
            }
        } else if !text.starts_with("# ") {
            out.push(Block { kind: Kind::Text, depth: 0, text: plain(text), aside: String::new() });
            open = true;
        }
    }
    out
}

/// The style of a kind of line.
fn style_of(kind: Kind, theme: &Theme) -> TextStyle {
    match kind {
        Kind::Version => TextStyle::new(theme.text_strong).size(VERSION_FONT).semibold(),
        Kind::Heading => TextStyle::new(theme.text).size(12.5).semibold(),
        Kind::Bullet | Kind::Text => TextStyle::new(theme.text),
    }
}

/// The size of a version's line (its date beside it is the same size, in grey).
const VERSION_FONT: f32 = 15.0;

fn line_height(kind: Kind) -> f32 {
    match kind {
        Kind::Version => 24.0,
        Kind::Heading => 20.0,
        Kind::Bullet | Kind::Text => 18.0,
    }
}

struct Prompt {
    hwnd: HWND,
    owner: HWND,
    renderer: Renderer,
    theme: Theme,
    dpi: u32,
    /// Client size in DIPs.
    size: (f32, f32),
    started: std::time::Instant,
    /// When the window last popped up (`now`): Enter and Space wait `KEY_GRACE` after it.
    shown: f64,
    version: String,
    ours: String,
    blocks: Vec<Block>,
    lines: Vec<Line>,
    /// The width the lines were laid out for.
    laid_out_for: f32,
    content_h: f32,
    scroller: Scroller,
    /// An older Windows' scroll bar arrow or track held down, repeating.
    holding: kit::Holding,
    skip: bool,
    focus: Ctl,
    /// Show the keyboard's outline (after a key was used, as in Windows).
    cues: bool,
    hover: Option<Hot>,
    pressed: Option<Hot>,
    /// Dragging the scrollbar's thumb: where in the thumb it was grabbed.
    drag: Option<f32>,
    tracking: bool,
    active: bool,
    installing: bool,
    /// What went wrong installing (shown in red), or None.
    problem: Option<String>,
    /// The checkbox fading in or out after a click (kit.rs).
    flips: kit::Flips,
}

thread_local! {
    /// The prompt, while it's open (UI thread only).
    static PROMPT: RefCell<Option<Prompt>> = const { RefCell::new(None) };
    /// The skin is 98 / 2000's: the window has their frame round it (for `hit_test`, which runs
    /// without the prompt's state).
    static CLASSIC: Cell<bool> = const { Cell::new(false) };
}

/// Opens the prompt for `release` over `owner` (the main window), or brings it up to date if it's
/// open already. False if Windows couldn't make it.
pub fn open(owner: HWND, theme: &Theme, release: &Release, ours: &str) -> bool {
    // Already open: show the newest release in it.
    let open_now = PROMPT.with(|p| {
        let Ok(mut guard) = p.try_borrow_mut() else { return None };
        let prompt = guard.as_mut()?;
        prompt.set_release(release, ours);
        prompt.invalidate();
        Some(prompt.hwnd)
    });
    if let Some(hwnd) = open_now {
        // SAFETY: posting to our own window.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_PROMPT_SHOW, WPARAM(0), LPARAM(0));
        }
        return true;
    }
    let Some(hwnd) = make_window(owner) else { return false };
    // SAFETY: plain query on the window just made.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let renderer = match Renderer::new(hwnd, dpi) {
        Ok(mut r) => {
            // The skin's font, at the standard size (like the Settings window).
            r.set_font(&theme.font, 1.0);
            r.set_title_font(&theme.title_font);
            r
        }
        Err(e) => {
            log::error(&format!("The update window couldn't draw: {e}"));
            // SAFETY: our own window, which has no state yet.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return false;
        }
    };
    let mut prompt = Prompt {
        hwnd,
        owner,
        renderer,
        theme: theme.clone(),
        dpi,
        size: (0.0, 0.0),
        started: std::time::Instant::now(),
        shown: 0.0,
        version: String::new(),
        ours: String::new(),
        blocks: Vec::new(),
        lines: Vec::new(),
        laid_out_for: -1.0,
        content_h: 0.0,
        scroller: Scroller::default(),
        holding: kit::Holding::default(),
        skip: false,
        focus: Ctl::Update,
        cues: false,
        hover: None,
        pressed: None,
        drag: None,
        tracking: false,
        active: true,
        installing: false,
        problem: None,
        flips: kit::Flips::default(),
    };
    prompt.set_release(release, ours);
    CLASSIC.with(|c| c.set(theme.era.classic()));
    let mut rc = RECT::default();
    // SAFETY: our own window and a valid out-struct.
    if unsafe { GetClientRect(hwnd, &mut rc) }.is_ok() {
        prompt.size = prompt.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
    }
    PROMPT.with(|p| {
        if let Ok(mut guard) = p.try_borrow_mut() {
            *guard = Some(prompt);
        }
    });
    frame::apply_theme(hwnd, theme);
    // SAFETY: posting to our own window: it shows itself once the main window's message is done.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_APP_PROMPT_SHOW, WPARAM(0), LPARAM(0));
    }
    true
}

/// Closes the prompt without an answer (the main window went to the background): asked again at
/// the next check.
pub fn close() {
    if let Some(hwnd) = hwnd() {
        // SAFETY: posting to our own window.
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_APP_PROMPT_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// The prompt's window, while it's open.
fn hwnd() -> Option<HWND> {
    PROMPT.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|p| p.hwnd)))
}

/// Whether the prompt is open.
pub fn is_open() -> bool {
    hwnd().is_some()
}

/// Light or dark changed.
pub fn apply_theme(theme: &Theme) {
    let hwnd = PROMPT.with(|p| {
        let Ok(mut guard) = p.try_borrow_mut() else { return None };
        let prompt = guard.as_mut()?;
        prompt.theme = theme.clone();
        CLASSIC.with(|c| c.set(theme.era.classic()));
        // 98 / 2000's frame comes or goes: what's inside it changes size.
        let mut rc = RECT::default();
        // SAFETY: our own window and a valid out-struct.
        if unsafe { GetClientRect(prompt.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
            prompt.size = prompt.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
            prompt.laid_out_for = -1.0;
        }
        // The skin's font: what's new is wrapped again with it.
        let generation = prompt.renderer.font_generation();
        prompt.renderer.set_font(&theme.font, 1.0);
        prompt.renderer.set_title_font(&theme.title_font);
        if prompt.renderer.font_generation() != generation {
            prompt.laid_out_for = -1.0;
        }
        prompt.invalidate();
        Some(prompt.hwnd)
    });
    if let Some(h) = hwnd {
        frame::apply_theme(h, theme);
    }
}

/// Installing failed (or couldn't start): the prompt says why, and its buttons work again.
pub fn problem(text: &str) {
    PROMPT.with(|p| {
        if let Ok(mut guard) = p.try_borrow_mut() {
            if let Some(prompt) = guard.as_mut() {
                prompt.installing = false;
                prompt.problem = Some(text.to_string());
                prompt.invalidate();
            }
        }
    });
}

/// Registers the window class (once; registering again just fails harmlessly) and makes the window,
/// hidden, centred over `owner`.
fn make_window(owner: HWND) -> Option<HWND> {
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: class registration and window creation with valid structs and NUL-terminated strings;
    // plain queries on the owner.
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("GlintUpdateWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let scale = GetDpiForWindow(owner).max(96) as f32 / 96.0;
        let (mut w, mut h) = ((DEFAULT_W * scale) as i32, (DEFAULT_H * scale) as i32);
        let mut main = RECT::default();
        let _ = GetWindowRect(owner, &mut main);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let work = if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() { info.rcWork } else { main };
        w = w.min((work.right - work.left).max(200));
        h = h.min((work.bottom - work.top).max(150));
        let x = ((main.left + main.right) / 2 - w / 2).clamp(work.left, (work.right - w).max(work.left));
        let y = ((main.top + main.bottom) / 2 - h / 2).clamp(work.top, (work.bottom - h).max(work.top));
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), w!("GlintUpdateWindow"), PCWSTR(title.as_ptr()), WS_CAPTION | WS_SYSMENU | WS_THICKFRAME, x, y, w, h, Some(owner), None, Some(instance.into()), None).ok()?;
        frame::init(hwnd);
        // WM_NCCALCSIZE again now that it's handled: the caption goes, as in the main window.
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        Some(hwnd)
    }
}

/// The window procedure: what needs no state is answered here; the rest goes to the state while
/// it isn't busy (see the module comment), and what it asks for is done afterwards.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // SAFETY: lparam is the NCCALCSIZE_PARAMS Windows passed with this message.
        WM_NCCALCSIZE if wparam.0 != 0 => return unsafe { frame::nc_calc_size(hwnd, wparam, lparam) },
        WM_ERASEBKGND => return LRESULT(1),
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
        WM_NCDESTROY => {
            // Its last message: the state goes (not while it's borrowed - it never is here, since
            // the window is only destroyed after a message, but never panic over it).
            let gone = PROMPT.with(|p| p.try_borrow_mut().ok().and_then(|mut g| g.take_if(|pr| pr.hwnd == hwnd)));
            drop(gone);
            // SAFETY: default handling.
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        _ => {}
    }
    let handled = PROMPT.with(|p| {
        let Ok(mut guard) = p.try_borrow_mut() else { return None };
        let prompt = guard.as_mut().filter(|pr| pr.hwnd == hwnd)?;
        Some(prompt.handle(msg, wparam, lparam))
    });
    let Some((result, acts)) = handled else {
        if msg == WM_PAINT {
            // Busy: paint on the next turn of the loop instead.
            // SAFETY: plain invalidate of our own window.
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            return LRESULT(0);
        }
        if msg == WM_CLOSE {
            return LRESULT(0); // closing goes through the state; not now
        }
        // SAFETY: default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    for act in acts {
        // SAFETY: our own window and its owner (posting is safe even if it's gone); no state is
        // borrowed here.
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                break;
            }
            match act {
                Act::Answer(code) => {
                    if let Some(owner) = PROMPT.with(|p| p.try_borrow().ok().and_then(|g| g.as_ref().map(|pr| pr.owner))) {
                        let _ = PostMessageW(Some(owner), WM_APP_UPDATE, WPARAM(ANSWERED), LPARAM(code));
                    }
                }
                Act::Destroy => {
                    let _ = DestroyWindow(hwnd);
                }
                Act::Show { quiet } => {
                    frame::cloak(hwnd, true);
                    let _ = ShowWindow(hwnd, if quiet { SW_SHOWNOACTIVATE } else { SW_SHOW });
                    let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW);
                    frame::cloak(hwnd, false);
                    if !quiet {
                        let _ = SetForegroundWindow(hwnd);
                    }
                    if frame::is_wine() {
                        // Wine shows a frame only once the next one is drawn.
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                Act::Place(r) => {
                    let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                }
            }
        }
    }
    match result {
        Some(r) => r,
        // SAFETY: default handling.
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// WM_NCHITTEST: the edges resize (frame.rs), the empty title bar moves the window, the rest is ours.
fn hit_test(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
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
    let code = if (0.0..size::TITLE_H).contains(&y) && x < w - ksize::CLOSE_W { HTCAPTION } else { HTCLIENT };
    LRESULT(code as isize)
}

impl Prompt {
    fn scale(&self) -> f32 {
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

    /// What it shows: this release, for this version of Glint.
    fn set_release(&mut self, release: &Release, ours: &str) {
        if self.version != release.version {
            self.skip = false;
            self.problem = None;
            self.scroller.jump_to(0.0);
        }
        self.version = release.version.clone();
        self.ours = ours.to_string();
        let mut b = blocks(&release.changelog);
        if b.is_empty() {
            b.push(Block { kind: Kind::Text, depth: 0, text: "This version came without a list of what's new.".into(), aside: String::new() });
        }
        self.blocks = b;
        self.laid_out_for = -1.0;
    }

    // ------------------------------------------------------------ where things are

    fn close_rect(&self) -> Rect {
        Rect::new(self.size.0 - ksize::CLOSE_W, 0.0, ksize::CLOSE_W, size::TITLE_H)
    }

    fn inner_w(&self) -> f32 {
        (self.size.0 - 2.0 * PAD).max(100.0)
    }

    /// The buttons: UPDATE, then REMIND ME AGAIN LATER at the right edge (Windows' order).
    fn buttons(&mut self) -> (Rect, Rect) {
        let y = self.size.1 - 22.0 - ksize::BUTTON_H;
        let later_w = kit::button_width(&mut self.renderer, LATER);
        let update_w = kit::button_width(&mut self.renderer, UPDATE);
        let later = Rect::new(self.size.0 - PAD - later_w, y, later_w, ksize::BUTTON_H);
        // Leaning the same way (as far as the skin leans them), the gap across between them is
        // BUTTON_GAP.
        let lean = self.theme.lean(ksize::BUTTON_LEAN);
        let update = Rect::new(later.x + lean - BUTTON_GAP - update_w, y, update_w, ksize::BUTTON_H);
        (update, later)
    }

    /// "Turn off automatic updates": at the bottom left, beside the buttons - or, in a window too
    /// narrow for that, on a line of its own just above them.
    fn off_rect(&mut self) -> Rect {
        let (update, _) = self.buttons();
        let w = self.renderer.measure(OFF, &TextStyle::new(Color::hex(0))).ceil() + 2.0 * QUIET_PAD;
        // Its words start where the checkbox does.
        let x = PAD + 1.0 - QUIET_PAD;
        let y = if x + w + QUIET_GAP <= update.x { update.y } else { update.y - 6.0 - ksize::BUTTON_H };
        Rect::new(x, y, w, ksize::BUTTON_H)
    }

    /// The checkbox's row (the box and its words), above the buttons (and "Turn off automatic
    /// updates" when that has a line of its own).
    fn check_row(&mut self) -> Rect {
        let off = self.off_rect();
        let gap = if off.y < self.buttons().0.y { 8.0 } else { 14.0 };
        Rect::new(PAD, off.y - gap - CHECK_ROW_H, self.inner_w(), CHECK_ROW_H)
    }

    /// The lines of the status (installing, or what went wrong), wrapped.
    fn status_lines(&mut self) -> Vec<String> {
        let text = match (&self.problem, self.installing) {
            (Some(p), _) => p.clone(),
            (None, true) => "Installing… Glint closes and opens again by itself when it's done.".to_string(),
            (None, false) => return Vec::new(),
        };
        let w = self.inner_w();
        let mut lines = kit::wrap(&mut self.renderer, &text, &TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT + 0.5), w);
        lines.truncate(5);
        lines
    }

    /// The box with what's new: under the heading, above the status and the checkbox.
    fn changelog_rect(&mut self) -> Rect {
        let top = size::TITLE_H + 18.0 + 28.0 + 22.0;
        let status = self.status_lines().len() as f32 * STATUS_LINE;
        let bottom = self.check_row().y - 12.0 - if status > 0.0 { status + 8.0 } else { 0.0 };
        Rect::new(PAD, top, self.inner_w(), (bottom - top).max(40.0))
    }

    fn track(&mut self) -> Rect {
        let b = self.changelog_rect();
        let w = self.theme.scrollbar_w;
        // An older Windows' arrow buttons sit at the two ends (kit::scrollbar).
        let a = crate::retroctl::arrows(&self.theme);
        Rect::new(b.right() - 3.0 - w, b.y + 4.0 + a, w, (b.h - 8.0 - 2.0 * a).max(0.0))
    }

    /// The scroll bar: the track and an older Windows' arrow buttons round it.
    fn bar_place(&mut self) -> kit::BarPlace {
        let track = self.track();
        let (m, a) = (self.theme.scrollbar_margin(), crate::retroctl::arrows(&self.theme));
        kit::BarPlace { lane: track.inset(-m, -m - a), track, arrows: a }
    }

    /// Lays the changelog out for the box's width (again only when that changed).
    fn layout(&mut self) {
        let b = self.changelog_rect();
        let width = (b.w - 2.0 * BOX_PAD - BOX_SCROLLBAR).max(60.0);
        if (width - self.laid_out_for).abs() > 0.1 {
            self.laid_out_for = width;
            let mut lines = Vec::new();
            let mut y = 0.0;
            let theme = self.theme.clone();
            for (n, block) in self.blocks.iter().enumerate() {
                let style = style_of(block.kind, &theme);
                let h = line_height(block.kind);
                y += match block.kind {
                    _ if n == 0 => 0.0,
                    Kind::Version => 14.0,
                    Kind::Heading => 8.0,
                    Kind::Bullet | Kind::Text => 3.0,
                };
                let indent = if block.kind == Kind::Bullet { 4.0 + block.depth as f32 * 16.0 } else { 0.0 };
                let text_x = indent + if block.kind == Kind::Bullet { 14.0 } else { 0.0 };
                for (i, piece) in kit::wrap(&mut self.renderer, &block.text, &style, (width - text_x).max(40.0)).into_iter().enumerate() {
                    let bullet = (block.kind == Kind::Bullet && i == 0).then_some(indent);
                    let aside = if i == 0 { block.aside.clone() } else { String::new() };
                    lines.push(Line { kind: block.kind, text: piece, aside, x: text_x, y, bullet });
                    y += h;
                }
            }
            self.lines = lines;
            self.content_h = y;
        }
        let view = (b.h - 2.0 * BOX_PAD).max(0.0);
        self.scroller.set_extent(self.content_h, view);
    }

    // ------------------------------------------------------------ painting

    fn paint(&mut self) {
        self.layout();
        let now = self.now();
        let more = self.scroller.tick(now) | self.flips.tick(now);
        if !self.renderer.begin() {
            // No surface to draw on right now (a graphics driver being reset): try again soon.
            // SAFETY: a timer on our own window.
            unsafe {
                SetTimer(Some(self.hwnd), RETRY_TIMER, 500, None);
            }
            return;
        }
        let theme = self.theme.clone();
        self.renderer.clear(theme.toolbar);
        // 98 / 2000: everything inside the window's frame.
        let b = self.border();
        self.renderer.set_origin(b, b);
        if theme.toolbar_paint.gradient().is_some() {
            self.renderer.fill_paint(Rect::new(0.0, 0.0, self.size.0, self.size.1), 0.0, &theme.toolbar_paint);
        }
        self.paint_caption(&theme);
        self.paint_body(&theme);
        if b > 0.0 {
            self.renderer.set_origin(0.0, 0.0);
            frame::paint_classic_border(&mut self.renderer, &theme, Rect::new(0.0, 0.0, self.size.0 + 2.0 * b, self.size.1 + 2.0 * b));
        }
        let lost = self.renderer.end() == FrameResult::DeviceLost;
        if more || lost {
            self.invalidate();
        }
    }

    fn paint_caption(&mut self, theme: &Theme) {
        let close = self.close_rect();
        let bar = Rect::new(0.0, 0.0, self.size.0, size::TITLE_H);
        let r = &mut self.renderer;
        kit::title_bar(r, theme, bar, self.active);
        let x = kit::logo(r, theme, bar);
        crate::retro::title(r, theme, Rect::new(x + 10.0, 0.0, (self.size.0 - x - 10.0 - ksize::CLOSE_W).max(0.0), size::TITLE_H), "Update", self.active);
        let hot = self.hover == Some(Hot::Close);
        kit::close_button(r, theme, close, hot, hot && self.pressed == Some(Hot::Close), self.active);
        // The line under the title bar, like the toolbar's (as the skin draws it, or none; a skin
        // with a shine on its bars has its edge there already).
        if theme.toolbar_line_w > 0.0 && theme.highlight.a <= 0.0 {
            r.fill_rect(Rect::new(0.0, size::TITLE_H, self.size.0, theme.toolbar_line_w), theme.toolbar_line);
        }
    }

    fn look(&self, c: Ctl) -> Look {
        let me = Hot::Ctl(c);
        Look {
            hover: self.hover == Some(me),
            pressed: self.pressed == Some(me) && self.hover == Some(me),
            focused: self.cues && self.focus == c,
            enabled: !self.installing,
        }
    }

    fn paint_body(&mut self, theme: &Theme) {
        let w = self.inner_w();
        let top = size::TITLE_H + 18.0;
        let heading = if self.installing { format!("Installing Glint {}…", self.version) } else { format!("Glint {} is ready", self.version) };
        let sub = format!("You have Glint {}. What's new:", self.ours);
        let b = self.changelog_rect();
        let status = self.status_lines();
        let (update, later) = self.buttons();
        let off = self.off_rect();
        let check = self.check_row();
        let track = self.track();
        let (check_look, update_look, later_look, off_look) = (self.look(Ctl::Check), self.look(Ctl::Update), self.look(Ctl::Later), self.look(Ctl::Off));
        let thumb_hot = self.hover == Some(Hot::Thumb) || self.drag.is_some();
        let hold = self.holding.hold_of(0);
        let now = self.now();
        let ticked = self.flips.at("skip", self.skip, now);
        let r = &mut self.renderer;
        r.text(&heading, Rect::new(PAD, top, w, 28.0), &TextStyle::new(theme.text_strong).size(HEADING_FONT).semibold());
        r.text(&sub, Rect::new(PAD, top + 28.0, w, 20.0), &TextStyle::new(theme.muted));

        // What's new, scrolling inside its box.
        kit::card(r, theme, b);
        let inner = Rect::new(b.x + BOX_PAD, b.y + BOX_PAD, (b.w - 2.0 * BOX_PAD - BOX_SCROLLBAR).max(0.0), (b.h - 2.0 * BOX_PAD).max(0.0));
        // (Lines scrolled away stop a little inside the frame, not right at its edge.)
        r.push_clip(Rect::new(b.x + 1.0, b.y + 6.0, (b.w - 2.0).max(0.0), (b.h - 12.0).max(0.0)));
        let dy = inner.y - self.scroller.offset;
        for line in &self.lines {
            let h = line_height(line.kind);
            let y = line.y + dy;
            if y + h < b.y || y > b.bottom() {
                continue;
            }
            let style = style_of(line.kind, theme);
            if let Some(bx) = line.bullet {
                r.fill_rect(Rect::new(r.snap(inner.x + bx + 1.0), r.snap(y + h / 2.0 - 2.0), 4.0, 4.0), theme.accent);
            }
            r.text(&line.text, Rect::new(inner.x + line.x, y, (inner.w - line.x).max(0.0), h), &style);
            if !line.aside.is_empty() {
                let x = line.x + r.measure(&line.text, &style) + 10.0;
                r.text(&line.aside, Rect::new(inner.x + x, y, (inner.w - x).max(0.0), h), &TextStyle::new(theme.muted).size(VERSION_FONT));
            }
        }
        r.pop_clip();
        kit::scrollbar(r, theme, track, &self.scroller, thumb_hot, hold);

        // Installing, or what went wrong.
        let mut y = b.bottom() + 10.0;
        let status_color = if self.problem.is_some() { theme.accent } else { theme.muted };
        for line in &status {
            r.text(line, Rect::new(PAD, y, w, STATUS_LINE), &TextStyle::new(status_color).size(ksize::NOTE_FONT + 0.5));
            y += STATUS_LINE;
        }

        // The checkbox and its words.
        let boxr = Rect::new(check.x + 1.0, check.y + (check.h - ksize::CHECK) / 2.0, ksize::CHECK, ksize::CHECK);
        kit::checkbox(r, theme, boxr, ticked, check_look);
        let words_color = if check_look.enabled { theme.text } else { theme.muted };
        r.text(SKIP, Rect::new(boxr.right() + 10.0, check.y, (check.w - ksize::CHECK - 12.0).max(0.0), check.h), &TextStyle::new(words_color));

        quiet_button(r, theme, off, OFF, off_look);
        kit::primary_button(r, theme, update, UPDATE, update_look);
        kit::button(r, theme, later, LATER, later_look);
    }

    // ------------------------------------------------------------ the mouse

    fn target_at(&mut self, p: Point) -> Option<Hot> {
        if p.y < size::TITLE_H {
            return self.close_rect().contains(p).then_some(Hot::Close);
        }
        let track = self.track();
        if let Some(thumb) = self.scroller.thumb(track) {
            if thumb.inset(-4.0, 0.0).contains(p) {
                return Some(Hot::Thumb);
            }
        }
        let (update, later) = self.buttons();
        if kit::button_contains(update, p) {
            return Some(Hot::Ctl(Ctl::Update));
        }
        if kit::button_contains(later, p) {
            return Some(Hot::Ctl(Ctl::Later));
        }
        if self.off_rect().contains(p) {
            return Some(Hot::Ctl(Ctl::Off));
        }
        let check = self.check_row();
        let words = TextStyle::new(Color::hex(0));
        let words_w = self.renderer.measure(SKIP, &words);
        if Rect::new(check.x - 4.0, check.y, ksize::CHECK + 12.0 + words_w + 8.0, check.h).contains(p) {
            return Some(Hot::Ctl(Ctl::Check));
        }
        None
    }

    fn client_point(&self, lparam: LPARAM) -> Point {
        let (s, b) = (self.scale(), self.border());
        Point::new((lparam.0 & 0xffff) as i16 as f32 / s - b, ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s - b)
    }

    /// How wide 98 / 2000's window frame is round everything (0 for other skins or maximised).
    fn border(&self) -> f32 {
        frame::classic_border(&self.theme, frame::zoomed(self.hwnd))
    }

    /// The client area's size in DIPs, less the frame (from `w` x `h` pixels).
    fn inner_size(&self, w: u32, h: u32) -> (f32, f32) {
        let (s, b) = (self.scale(), self.border());
        ((w as f32 / s - 2.0 * b).max(0.0), (h as f32 / s - 2.0 * b).max(0.0))
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

    fn mouse_down(&mut self, p: Point) {
        self.cues = false;
        // An older Windows' scroll bar: an arrow button moves the notes a line, the track a page,
        // again and again while the button is held.
        let (place, now) = (self.bar_place(), self.now());
        if self.holding.press(self.hwnd, place, &mut self.scroller, 0, p, now) {
            self.invalidate();
            return;
        }
        let target = self.target_at(p);
        // SAFETY: focus and capture on our own window.
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
            SetCapture(self.hwnd);
        }
        self.pressed = target;
        match target {
            Some(Hot::Thumb) => {
                let track = self.track();
                if let Some(thumb) = self.scroller.thumb(track) {
                    self.drag = Some(p.y - thumb.y);
                }
            }
            Some(Hot::Ctl(c)) if !self.installing => self.focus = c,
            _ => {}
        }
        self.invalidate();
    }

    fn mouse_up(&mut self, p: Point, acts: &mut Vec<Act>) {
        if self.holding.release(self.hwnd) {
            // SAFETY: releasing our own capture.
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
            return;
        }
        let was = self.pressed.take();
        self.drag = None;
        // SAFETY: releasing our own capture.
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        if was.is_none() || was != self.target_at(p) {
            return;
        }
        match was {
            Some(Hot::Close) => self.dismiss(acts),
            Some(Hot::Ctl(c)) => self.press(c, acts),
            _ => {}
        }
    }

    // ------------------------------------------------------------ what the person said

    /// A control was clicked, or Space / Enter was pressed on it.
    fn press(&mut self, c: Ctl, acts: &mut Vec<Act>) {
        if self.installing {
            return;
        }
        match c {
            Ctl::Check => {
                self.skip = !self.skip;
                let now = self.now();
                self.flips.start("skip", self.skip, now, kit::CHECK_TIME);
                self.invalidate();
            }
            Ctl::Off => {
                // Glint stops looking by itself; the main window says where to turn it on again.
                acts.push(Act::Answer(ANSWER_OFF));
                acts.push(Act::Destroy);
            }
            Ctl::Update => {
                // The window stays open while installing: it says how it went.
                self.installing = true;
                self.problem = None;
                self.invalidate();
                acts.push(Act::Answer(ANSWER_UPDATE));
            }
            Ctl::Later => self.dismiss(acts),
        }
    }

    /// Closed without updating: remind later - or never about this version, with the box ticked.
    fn dismiss(&mut self, acts: &mut Vec<Act>) {
        if !self.installing {
            acts.push(Act::Answer(if self.skip { ANSWER_SKIP } else { ANSWER_LATER }));
        }
        acts.push(Act::Destroy);
    }

    fn key(&mut self, vk: u16, acts: &mut Vec<Act>) -> bool {
        // SAFETY: plain key-state queries.
        let (shift, alt) = unsafe { (GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0) };
        if alt {
            return false; // Alt+F4, Alt+Space: Windows
        }
        let is = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| vk == k.0;
        let now = self.now();
        let view = (self.changelog_rect().h - 2.0 * BOX_PAD).max(40.0);
        match () {
            _ if is(VK_ESCAPE) => self.dismiss(acts),
            _ if is(VK_TAB) => {
                let n = TAB_ORDER.len() as i32;
                let at = TAB_ORDER.iter().position(|c| *c == self.focus).unwrap_or(2) as i32;
                self.focus = TAB_ORDER[(at + if shift { -1 } else { 1 }).rem_euclid(n) as usize];
                self.cues = true;
                self.invalidate();
            }
            _ if is(VK_SPACE) || is(VK_RETURN) => {
                if self.now() - self.shown < KEY_GRACE {
                    return true; // typed for the main window, just as the window popped up
                }
                self.cues = true;
                // Enter presses UPDATE (or the button that has the keyboard); the checkbox is
                // ticked with Space, as in Windows' own windows.
                let c = if is(VK_RETURN) && self.focus == Ctl::Check { Ctl::Update } else { self.focus };
                self.press(c, acts);
            }
            _ if is(VK_UP) || is(VK_DOWN) => {
                if self.scroller.scroll_by(if is(VK_UP) { -40.0 } else { 40.0 }, now) {
                    self.invalidate();
                }
            }
            _ if is(VK_PRIOR) || is(VK_NEXT) => {
                let step = (view - 30.0).max(30.0);
                if self.scroller.scroll_by(if is(VK_PRIOR) { -step } else { step }, now) {
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

    // ------------------------------------------------------------ messages

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> (Option<LRESULT>, Vec<Act>) {
        let mut acts = Vec::new();
        let result = match msg {
            WM_PAINT => {
                // SAFETY: marks our own window as painted (anything invalidated while painting then
                // asks for one more frame).
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                self.paint();
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
                let mut rc = RECT::default();
                // SAFETY: our own window and a valid out-struct.
                if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
                    self.size = self.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
                }
                self.laid_out_for = -1.0;
                self.invalidate();
                // SAFETY: lparam points to the suggested window rectangle.
                acts.push(Act::Place(unsafe { *(lparam.0 as *const RECT) }));
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
                self.mouse_down(p);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let p = self.client_point(lparam);
                self.mouse_up(p, &mut acts);
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
                let notches = hi_word(wparam.0) as u16 as i16 as f32 / 120.0;
                if self.scroller.scroll_by(-notches * WHEEL_STEP, self.now()) {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_KEYDOWN => self.key(wparam.0 as u16, &mut acts).then_some(LRESULT(0)),
            // Typed letters do nothing here (and don't beep), nor Alt+letter (no menu bar).
            WM_CHAR => Some(LRESULT(0)),
            WM_SYSCHAR if wparam.0 != ' ' as usize => Some(LRESULT(0)),
            WM_ACTIVATE => {
                self.active = lo_word(wparam.0) != WA_INACTIVE;
                self.invalidate();
                None
            }
            WM_CLOSE => {
                self.dismiss(&mut acts);
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
                let (place, now) = (self.bar_place(), self.now());
                if self.holding.fire(self.hwnd, place, &mut self.scroller, now) {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_APP_PROMPT_SHOW => {
                self.shown = self.now();
                // Glint in front (the main window, or a window of its: Settings, the sort editor):
                // the prompt comes up over it but leaves it the keyboard, which may be typing a new
                // name or a folder to go to (a name box that loses the keyboard keeps what's typed
                // so far).
                // SAFETY: plain window queries.
                let quiet = unsafe {
                    let front = GetForegroundWindow();
                    front != self.hwnd && GetAncestor(front, GA_ROOTOWNER) == self.owner
                };
                if quiet {
                    self.active = false;
                }
                acts.push(Act::Show { quiet });
                Some(LRESULT(0))
            }
            WM_APP_PROMPT_QUIT => {
                acts.push(Act::Destroy);
                Some(LRESULT(0))
            }
            _ => None,
        };
        (result, acts)
    }
}

/// A quiet button ("Turn off automatic updates"): only its words, muted, lit and underlined under
/// the mouse - clearly there, but not competing with UPDATE.
fn quiet_button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look) {
    let lit = look.enabled && (look.hover || look.pressed);
    let color = match (look.enabled, lit) {
        (false, _) => theme.muted.with_alpha(theme.muted.a * 0.38),
        (true, true) => theme.text,
        (true, false) => theme.muted,
    };
    let style = TextStyle::new(color);
    let words = Rect::new(rect.x + QUIET_PAD, rect.y, (rect.w - 2.0 * QUIET_PAD).max(0.0), rect.h);
    r.text(label, words, &style);
    if lit {
        let w = r.measure(label, &style).min(words.w);
        r.fill_rect(Rect::new(words.x, r.snap(rect.y + rect.h / 2.0 + 8.0), w, 1.0), color);
    }
    if look.focused {
        kit::focus_ring(r, theme, rect, 0.0);
    }
}

/// For the main window: the answer the prompt posted, as the settings it changes (the version
/// skipped, the time to remind after, or automatic checks off).
pub fn remember_answer(code: isize, release: &Release, s: &mut crate::settings::Settings) {
    match code {
        ANSWER_SKIP => s.skipped_update = Some(release.version.clone()),
        ANSWER_LATER => s.remind_update_after = update::now_seconds() + update::EVERY.as_secs(),
        ANSWER_OFF => s.check_for_updates = false,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_plain_blocks() {
        let md = "## 0.3.1 (2026-10-12): fixes\n\nSome **words** with `code`.\nGoing on.\n\n### The list\n\n- **Ctrl+W** closes the last tab\n  (like File Explorer).\n  - nested [link](https://x.y) here\n- second\n\n# Ignored title\n";
        let b = blocks(md);
        assert_eq!(b[0].aside, "(2026-10-12): fixes");
        let kinds: Vec<(Kind, usize, &str)> = b.iter().map(|b| (b.kind, b.depth, b.text.as_str())).collect();
        assert_eq!(
            kinds,
            vec![
                (Kind::Version, 0, "Glint 0.3.1"),
                (Kind::Text, 0, "Some words with code. Going on."),
                (Kind::Heading, 0, "The list"),
                (Kind::Bullet, 0, "Ctrl+W closes the last tab (like File Explorer)."),
                (Kind::Bullet, 1, "nested link here"),
                (Kind::Bullet, 0, "second"),
            ]
        );
        // Versions as they're written from 1.0.19 on: the date goes beside, in grey.
        let b = blocks("## 1.0.19-beta.1 · 15/10/2026\n\n- Added split view\n- Fixed the outline\n");
        assert_eq!((b[0].text.as_str(), b[0].aside.as_str()), ("Glint 1.0.19-beta.1", "15/10/2026"));
        assert_eq!(b.len(), 3);
        let b = blocks("## Glint 1.0.19-beta.1 15/10/2026 17:21\n\n- Added split view\n");
        assert_eq!((b[0].text.as_str(), b[0].aside.as_str()), ("Glint 1.0.19-beta.1", "15/10/2026 17:21"));
    }

    #[test]
    fn links_and_brackets() {
        assert_eq!(plain("see [the guide](docs/x.md) and [this]"), "see the guide and [this]");
        assert_eq!(plain("a [b] (c) d"), "a [b] (c) d");
        assert_eq!(plain("**Glint** `0.3.1`"), "Glint 0.3.1");
    }

    #[test]
    fn answers_are_remembered() {
        let release = Release { version: "0.3.1".into(), ..Default::default() };
        let mut s = crate::settings::Settings::default();
        remember_answer(ANSWER_SKIP, &release, &mut s);
        assert_eq!(s.skipped_update.as_deref(), Some("0.3.1"));
        let before = update::now_seconds();
        remember_answer(ANSWER_LATER, &release, &mut s);
        assert!(s.remind_update_after >= before + 12 * 3600 && s.remind_update_after <= update::now_seconds() + 12 * 3600);
        let kept = s.clone();
        remember_answer(ANSWER_UPDATE, &release, &mut s);
        assert_eq!(s, kept, "updating changes no setting");
    }

    #[test]
    fn turning_automatic_updates_off_is_remembered() {
        let release = Release { version: "0.3.1".into(), ..Default::default() };
        let mut s = crate::settings::Settings::default();
        assert!(s.check_for_updates, "on unless the person turns it off");
        let kept = s.clone();
        remember_answer(ANSWER_OFF, &release, &mut s);
        assert!(!s.check_for_updates);
        assert_eq!(crate::settings::Settings { check_for_updates: true, ..s.clone() }, kept, "nothing else changes");
        // Looking by hand (Settings > About) still shows what it finds.
        let found = update::Found::Newer(release);
        assert!(update::should_prompt(&found, &s, true, update::now_seconds()));
    }
}
