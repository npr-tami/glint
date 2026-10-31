//! The main window: creates it, owns the parts (title bar, toolbar, sidebar, file list, status bar),
//! routes Windows' messages to them and carries out what they ask for (see ui.rs for the contract).
//!
//! Safety model. The window's state (`Window`) lives in a Box whose pointer is kept in the window's
//! GWLP_USERDATA. Windows calls `wndproc` for every message, sometimes from *inside* another call we
//! made (SetFocus, SetWindowPos, a popup menu's own message loop...). To never have two `&mut Window`
//! alive at once, a thread-local BUSY flag is set while a message is being handled; a message that
//! arrives while BUSY gets Windows' default handling. Things that run their own message loop (popup
//! menus) or resize the window are queued as `Deferred` and run after the handler has returned.
//!
//! The file list's right-click menus (`Effect::FilesMenu`) can carry Windows' own items, which the
//! shell thread builds (shellmenu.rs). Such a menu waits for them in `pending_menu` - at most
//! MENU_WAIT_MS, then it opens without them - and opens when WM_APP_SHELL_MENU says they're ready.
//!
//! The Settings window (settingswin.rs) is a window of its own, but its state lives here and its
//! messages come through `settings_message`, under the same BUSY guard, so it can change the app's
//! settings directly and they are saved like any other change.

use crate::winutil::{hi_word, lo_word};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::{FILETIME, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateSolidBrush, DeleteObject, InvalidateRect, RedrawWindow, ScreenToClient, SetBkColor, SetTextColor, ValidateRect,
    COLOR_WINDOW, HDC, HGDIOBJ, RDW_ALLCHILDREN, RDW_INVALIDATE,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, GetKeyboardLayout, VkKeyScanExW, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
    VK_BACK, VK_OEM_5, VK_CONTROL, VK_F3, VK_F4, VK_F5, VK_F6, VK_LEFT, VK_MENU, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos,
    GetWindowLongPtrW, IsChild, IsWindow, IsWindowVisible, IsZoomed, KillTimer, LoadCursorW, LoadIconW, PostQuitMessage,
    RegisterClassExW, SetCursor, SetWindowLongPtrW, SC_KEYMENU, WM_SYSCHAR, WM_SYSCOMMAND,
    SetTimer, SetWindowPos, ShowWindow, SystemParametersInfoW, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT,
    GWLP_USERDATA, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON, IDC_ARROW, IDC_HAND, IDC_IBEAM, IDC_SIZEWE,
    MINMAXINFO, SHOW_WINDOW_CMD, SIZE_MINIMIZED, SPI_GETWORKAREA, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, WA_INACTIVE,
    WINDOW_EX_STYLE, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CHAR, WM_COMMAND, WM_CTLCOLOREDIT, WM_DESTROY,
    WM_DEVICECHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE,
    WM_NCMOUSEMOVE, WM_PAINT, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SIZE, WM_SYSKEYDOWN, WM_TIMER,
    WM_XBUTTONUP, WNDCLASSEXW, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};
// Remembering the window (settings.rs) and submenus.
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, IsIconic, SIZE_MAXIMIZED, WM_ENDSESSION, WM_MOVE, WM_SETTINGCHANGE};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowPlacement, DBT_DEVICEQUERYREMOVE, WINDOWPLACEMENT};

use windows::Win32::System::DataExchange::{AddClipboardFormatListener, RemoveClipboardFormatListener};
use windows::Win32::UI::WindowsAndMessaging::WM_CLIPBOARDUPDATE;

use crate::app::{AppState, WM_APP_LOADED};
use crate::ask::{Answer, AskBox};
use crate::frame;
use crate::list::{ListView, BAR_REPEAT_TIMER, SLOW_CLICK_TIMER};
use crate::log;
use crate::render::{FrameResult, Renderer, TextStyle};
use crate::settings::{self, Backspace, Settings, ThemeMode, ViewMode};
use crate::sidebar::Sidebar;
use crate::status::StatusBar;
// Right-click menus with Windows' own items (shellmenu.rs, popup.rs, menu.rs).
use crate::menu;
use crate::popup::{self, Pick};
use crate::shell::{self, WM_APP_NOTE};
use crate::shellmenu::{self, Built, WM_APP_SHELL_MENU};
use crate::ui::{FilesMenu, ShellTarget, MORE_OPTIONS};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_OEM_3, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::WM_CONTEXTMENU;
use crate::textbox::{self, TextEvent, TextEventKind, WM_APP_TEXT};
// How Glint runs (life.rs): background mode, requests from other glint.exe's, Ctrl+N and Ctrl+Q.
use crate::life::{self, Request, WM_APP_LIFE};
use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, SC_CLOSE, SW_HIDE, WM_CLOSE};
use crate::theme::size;
use crate::titlebar::TitleBar;
use crate::toolbar::{self, Toolbar};
use crate::ui::{Command, Component, Ctx, CursorKind, Effect, Hit, Menu, Mods, MouseButton, MouseEvent, Point, Rect};
// Drag and drop (dragdrop.rs, and the window's side in window/drag.rs).
use crate::dragdrop::{self, WM_APP_DRAG, WM_APP_DROP_DONE};
use crate::ui::DragOut;

mod drag;
pub mod sorting;
// The Settings window (Ctrl+,).
use crate::settingswin::{self, SettingsWindow};
use crate::skin::WM_APP_SKINS;
use crate::skinwin::{self, SkinWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_OEM_COMMA;
// Updates (update.rs) and their prompt (updatewin.rs).
use crate::update::{self, Release, WM_APP_UPDATE};
use crate::updatewin;
// The details panel (Alt+P) and the Space preview, on the right of the list (side.rs), and the
// thumbnails they show (thumbs.rs).
use crate::side::Side;
use crate::thumbs;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_SPACE;

/// Asks the window to close itself once the first folder is on screen (for start-up timing runs).
const WM_APP_BENCH_DONE: u32 = WM_APP + 3;
/// The timer that stops waiting for Windows' items for a right-click menu (see `pending_menu`).
const SHELL_MENU_TIMER: usize = 20;
/// How long a right-click menu waits for Windows' items before it opens without them. Usually
/// they take a few milliseconds; the first time (apps' menu code loading) or on a slow network
/// folder they can take longer.
const MENU_WAIT_MS: u32 = 1000;
/// The same for Windows' whole menu, which was asked for on purpose (Shift+right-click).
const WHOLE_MENU_WAIT_MS: u32 = 4000;
/// The timer that takes a status-bar note away again ("Path copied").
const NOTE_TIMER: usize = 1;
/// The timer that switches the list to "Loading…" when a folder is slow to arrive (app::LOADING_DELAY).
const LOADING_TIMER: usize = 2;
/// The timer that tries drawing again after the graphics card refused several frames in a row.
const RETRY_TIMER: usize = 3;
/// Frames that may fail in a row before retries slow down to one every RETRY_MS (a driver being
/// reset can take a few seconds; trying every frame meanwhile would only burn CPU).
const FAST_RETRIES: u32 = 3;
const RETRY_MS: u32 = 500;
/// Split view: the strip with each half's folder name (DIPs).
const SPLIT_STRIP_H: f32 = 24.0;
/// The timer that saves the settings a moment after something worth remembering changed (a tab,
/// a choice, the window's place), so a burst of changes is one write.
const SAVE_TIMER: usize = 4;
const SAVE_DELAY_MS: u32 = 1500;
/// When the window closes (or Windows shuts down), the settings are written before Glint ends -
/// waiting at most this long for a slow disk.
const EXIT_SAVE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Called by the panic hook (log.rs) just before it shows its message: from then on the window
/// gets only Windows' default handling, since a panic may have left its state half changed. The
/// panic may be a background job's, on another thread, so this is for every thread (`busy`).
pub fn freeze_for_crash() {
    FROZEN.store(true, Ordering::SeqCst);
}

/// Glint crashed (`freeze_for_crash`).
static FROZEN: AtomicBool = AtomicBool::new(false);

/// Whether the window's state must be left alone: a message is being handled (BUSY), or Glint
/// crashed.
fn busy() -> bool {
    BUSY.with(|b| b.get()) || FROZEN.load(Ordering::SeqCst)
}

thread_local! {
    /// A message is being handled right now (see the module comment).
    static BUSY: Cell<bool> = const { Cell::new(false) };
    /// Something is animating: the message loop (main.rs) keeps painting frames.
    static ANIMATING: Cell<bool> = const { Cell::new(false) };
    /// Text-box colours (background COLORREF, text COLORREF, brush), for WM_CTLCOLOREDIT.
    static EDIT_COLORS: Cell<(u32, u32, isize)> = const { Cell::new((0, 0, 0)) };
    /// Close requests and door requests that came while busy, posted again afterwards (wndproc).
    static MISSED: std::cell::RefCell<Vec<(u32, usize, isize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether the message loop should keep painting frames.
pub fn animating() -> bool {
    ANIMATING.with(|a| a.get())
}

/// The window's parts, for routing the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Title,
    Toolbar,
    Sidebar,
    List,
    /// Split view's other half (`list2`).
    List2,
    Status,
    /// The details panel or the Space preview, right of the list (side.rs).
    Side,
}

/// Work that must happen outside message handling.
enum Deferred {
    Menu(Part, Menu),
    /// A right-click menu for files, with Windows' items when they came (popup.rs).
    FilesMenu(Part, FilesMenu, Option<Built>),
    Show(SHOW_WINDOW_CMD),
    Place(RECT),
    Close,
    /// Drag items out of the file list: Windows' drag loop (window/drag.rs).
    DragOut(DragOut),
    /// Background mode: hide, then the tray icon and giving memory back (life.rs).
    Hide,
    /// Show the window if it's hidden (or restore it if minimised) and bring it to the front.
    Front,
    /// Make the Settings window (or bring it to the front), at this page.
    OpenSettings(Option<usize>),
    /// Settings at this page, with the keyboard on the row with this title.
    OpenSetting(usize, String),
    /// Something the Settings window asked for that runs its own message loop or moves it
    /// (settingswin::run_job).
    Settings(settingswin::Job),
    /// Make the skin customiser (or bring it to the front).
    OpenSkins,
    /// Something the customiser asked for that runs its own message loop (skinwin::run_job).
    Skins(skinwin::Job),
    /// Make the sort order's editor (or bring it to the front); window/sorting.rs.
    OpenSort,
    /// Something the sort order's editor asked for (sortwin::run_job).
    Sort(crate::sortwin::Job),
}

pub struct Window {
    hwnd: HWND,
    app: AppState,
    renderer: Renderer,
    titlebar: TitleBar,
    toolbar: Toolbar,
    sidebar: Sidebar,
    list: ListView,
    /// Split view's other half (see `follow_split`).
    list2: ListView,
    /// Split view: the half Glint works in is the left one.
    split_left: bool,
    /// The tabs the two halves showed at the last check: (the active tab, the other half's).
    split_shown: (u64, Option<u64>),
    /// Split view's strips with each half's folder name, over `list` and over `list2`.
    split_strips: [Rect; 2],
    /// The line between the halves (wider than it looks, to grab), and the room both halves share.
    split_divider: Rect,
    split_area: Rect,
    /// The line between the halves is being dragged.
    split_drag: bool,
    status: StatusBar,
    /// Shows `app.question` ("Delete permanently?"); while one is open it gets all input.
    ask: AskBox,
    /// Shows `app.form` (a set's name, how to rename many); while one is open it gets the mouse.
    form: crate::form::FormBox,
    /// The copy queue's list, bottom right (queuebox.rs).
    queue: crate::queuebox::QueueBox,
    /// The command box (Ctrl+U, palette.rs); while it's open it gets the mouse and the keys.
    palette: crate::palette::CommandBox,
    dpi: u32,
    /// Client size in DIPs.
    size: (f32, f32),
    hover: Option<Part>,
    capture: Option<Part>,
    last_mouse: Point,
    tracking: bool,
    tracking_nc: bool,
    active: bool,
    started: Instant,
    first_frame: bool,
    bench: bool,
    /// GLINT_FRAME_LOG=1: write every frame's drawing time to the log (for measuring).
    frame_log: bool,
    /// Frames in a row that couldn't be drawn (no render target, device lost).
    failed_frames: u32,
    /// A right-click menu waiting for Windows' items (see the module comment).
    pending_menu: Option<PendingMenu>,
    /// The text box that had the keyboard when the window was switched away from; it gets it back
    /// when the window is active again.
    focus_on_return: Option<HWND>,
    deferred: Vec<Deferred>,
    /// Where the window sits when it's neither maximised nor minimised, in screen pixels - what
    /// settings.json remembers, and where it comes back to from maximised.
    normal_rect: RECT,
    /// The DPI `normal_rect` was taken at (a maximised window's scale can change since).
    normal_dpi: u32,
    /// Maximised (or minimised from maximised, so it comes back maximised), from WM_SIZE.
    maximized: bool,
    /// Whether settings are written this run (not for --bench, nor when the file couldn't be read).
    saving: bool,
    /// What was last written (or read at start); a save happens only when this differs.
    last_saved: Settings,
    /// SAVE_TIMER is running.
    save_pending: bool,
    /// Something dragged over the window, or out of it (window/drag.rs).
    drag: drag::DragState,
    /// The main window (life.rs): closing it hides Glint in the background (when "Keep Glint running
    /// in the background" is on), and folders from other programs come to it. An extra window
    /// (Ctrl+N) just closes, and leaves the tabs and place in settings.json to the main one.
    main: bool,
    /// It has been on screen (a Glint started with Windows waits hidden until it's wanted).
    ever_shown: bool,
    /// Started hidden (with Windows): the status bar's start-up time would mean nothing.
    started_hidden: bool,
    /// Quitting for real (Ctrl+Q, the tray icon's Quit, `--quit`): no hiding in the background.
    quitting: bool,
    /// The Settings window, while it's open (settingswin.rs).
    settings_win: Option<SettingsWindow>,
    /// The skin customiser, while it's open (skinwin.rs).
    skin_win: Option<SkinWindow>,
    /// The custom sort order's editor, while it's open (sortwin.rs, window/sorting.rs).
    sort_win: Option<crate::sortwin::SortWindow>,
    /// The skin the window last followed (`AppState::theme_generation`; see `sync_theme`).
    theme_generation: u64,
    /// When to look for a new version (update.rs; the main window only).
    updates: update::Schedule,
    /// The new version the update prompt shows (updatewin.rs), for its answer.
    prompted: Option<Release>,
    /// The details panel or the Space preview on the right of the list (side.rs).
    side: Side,
}

/// A right-click menu waiting for Windows' items: shellmenu.rs builds them as `session`.
struct PendingMenu {
    part: Part,
    menu: FilesMenu,
    session: u64,
}

/// Options from the command line.
pub struct Options {
    pub bench: bool,
    /// When main() began (for the start-up timing).
    pub started: Instant,
    /// The main window (see `Window::main`); false for an extra window and for --bench.
    pub main: bool,
    /// Start hidden in the background (`--background`, Start with Windows).
    pub hidden: bool,
}

/// Creates the main window (hidden behind a cloak until its first frame) and shows it.
pub fn create(app: AppState, options: Options) -> Result<HWND> {
    let started = options.started;
    // SAFETY: standard window-class registration and creation. The Window box is leaked into
    // GWLP_USERDATA here and reclaimed in WM_NCDESTROY.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1)) /* MAKEINTRESOURCE(1): the icon in res/glint.rc */).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("GlintWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        // Where it was last time, if that screen is still there; otherwise sized for the screen it
        // opens on: 1180 x 760 DIPs, centred in the work area. An extra window (Ctrl+N) isn't put
        // on top of the main one: Windows places it (cascaded), at the usual size.
        let saved = if options.main { saved_rect(app.settings.bounds, app.settings.bounds_dpi) } else { None };
        let (x, y, w, h) = match saved {
            Some(r) => (r.left, r.top, r.right - r.left, r.bottom - r.top),
            None => {
                let dpi = GetDpiForSystem().max(96);
                let scale = dpi as f32 / 96.0;
                let mut work = RECT::default();
                let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
                let (ww, wh) = (work.right - work.left, work.bottom - work.top);
                let w = ((1180.0 * scale) as i32).min(ww - 40).max(320);
                let h = ((760.0 * scale) as i32).min(wh - 40).max(240);
                let (x, y) = if ww > 0 && options.main { (work.left + (ww - w) / 2, work.top + (wh - h) / 2) } else { (CW_USEDEFAULT, CW_USEDEFAULT) };
                (x, y, w, h)
            }
        };
        let maximized = app.settings.maximized && options.main;

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GlintWindow"),
            w!("Glint"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let dpi = GetDpiForWindow(hwnd).max(96);
        let renderer = Renderer::new(hwnd, dpi)?;
        textbox::set_font_family(&renderer.font_family());
        let mut list = ListView::new();
        list.set_column_widths(&app.settings.column_width_map);
        let mut list2 = ListView::new();
        list2.set_column_widths(&app.settings.column_width_map);
        let mut normal_rect = RECT::default();
        let _ = GetWindowRect(hwnd, &mut normal_rect);
        let saving = !options.bench && settings::saving_allowed();
        let mut window = Box::new(Window {
            hwnd,
            app,
            renderer,
            titlebar: TitleBar::new(),
            toolbar: Toolbar::new(hwnd),
            sidebar: Sidebar::new(),
            list,
            list2,
            split_left: true,
            split_shown: (0, None),
            split_strips: [Rect::default(); 2],
            split_divider: Rect::default(),
            split_area: Rect::default(),
            split_drag: false,
            status: StatusBar::default(),
            ask: AskBox::default(),
            form: Default::default(),
            queue: Default::default(),
            palette: Default::default(),
            dpi,
            size: (0.0, 0.0),
            hover: None,
            capture: None,
            last_mouse: Point::default(),
            tracking: false,
            tracking_nc: false,
            active: true,
            started,
            first_frame: true,
            bench: options.bench,
            frame_log: std::env::var_os("GLINT_FRAME_LOG").is_some(),
            failed_frames: 0,
            pending_menu: None,
            focus_on_return: None,
            deferred: Vec::new(),
            normal_rect,
            normal_dpi: dpi,
            maximized,
            saving,
            last_saved: Settings::default(),
            save_pending: false,
            drag: drag::DragState::default(),
            main: options.main,
            ever_shown: false,
            started_hidden: options.hidden,
            quitting: false,
            settings_win: None,
            skin_win: None,
            sort_win: None,
            theme_generation: 0,
            updates: update::Schedule::default(),
            prompted: None,
            side: Side::new(),
        });
        if options.main && !options.bench {
            // About 30 seconds from now (or once the window is first shown), then every 12 hours.
            window.updates.start(hwnd, !options.hidden);
        }
        window.app.attach_window(hwnd);
        thumbs::set_window(hwnd);
        window.apply_theme();
        // What's on disk now; a save happens once something differs - or right away (in the
        // background) when the file had to be repaired.
        window.last_saved = window.snapshot();
        if window.saving && settings::needs_rewrite() {
            settings::save_in_background(window.last_saved.clone());
        }
        log::line(&format!(
            "window created {:.1} ms after main() began; font \"{}\"; dpi {dpi}; wine: {}; {}",
            started.elapsed().as_secs_f64() * 1000.0,
            window.renderer.font_family(),
            frame::is_wine(),
            crate::status::memory_report()
        ));
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(window) as isize);
        // Told when the clipboard changes, so files cut in Glint stop looking faded once something
        // else is copied. Optional: without it they only look normal again on the next repaint.
        let _ = AddClipboardFormatListener(hwnd);
        // Files can be dropped onto the window (from Explorer, other programs, Glint itself).
        dragdrop::register(hwnd);

        frame::init(hwnd);
        // Re-run WM_NCCALCSIZE now that our handler is in place (it removes the caption).
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        if options.hidden {
            // Started with Windows: wait hidden in the background until a window is wanted
            // (`Deferred::Front` shows it, at its place from last time).
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Window;
            // SAFETY: the state was just put there and no message is being handled.
            if let Some(w) = ptr.as_mut() {
                w.app.go_to_background();
            }
            log::line("started hidden in the background");
            return Ok(hwnd);
        }
        frame::cloak(hwnd, true);
        // Maximised last time: made at its normal place first (so un-maximising goes back there),
        // then maximised on that screen.
        let _ = ShowWindow(hwnd, if maximized { SW_MAXIMIZE } else { SW_SHOW });
        let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | windows::Win32::Graphics::Gdi::RDW_UPDATENOW);
        frame::cloak(hwnd, false);
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Window;
        // SAFETY: as above; the window may have been closed while it was shown (then it's gone).
        if let Some(w) = ptr.as_mut() {
            w.ever_shown = true;
        }
        Ok(hwnd)
    }
}

/// Where the window opens when settings.json says where it was: that rectangle, resized if the
/// screen's scaling changed since, and moved fully onto its screen (which may have become smaller).
/// None when nothing was saved or that screen is gone (a laptop away from its second monitor):
/// then the window opens centred on the main screen.
fn saved_rect(bounds: Option<[i32; 4]>, saved_dpi: u32) -> Option<RECT> {
    let [x, y, w, h] = bounds?;
    let r = RECT { left: x, top: y, right: x.saturating_add(w), bottom: y.saturating_add(h) };
    // SAFETY: plain monitor queries with valid in/out structs.
    unsafe {
        let monitor = MonitorFromRect(&r, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() {
            return None;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let (mut dpi, mut dpi_y) = (0u32, 0u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() || dpi < 96 {
            dpi = saved_dpi.max(96);
        }
        fit_rect([x, y, w, h], saved_dpi, dpi, info.rcWork)
    }
}

/// The arithmetic of `saved_rect`: scale by `dpi / saved_dpi` (0 = unknown, no scaling), keep at
/// least the smallest window size, and fit inside `work` (the screen minus the taskbar).
fn fit_rect([x, y, w, h]: [i32; 4], saved_dpi: u32, dpi: u32, work: RECT) -> Option<RECT> {
    let (work_w, work_h) = (work.right.saturating_sub(work.left), work.bottom.saturating_sub(work.top));
    if work_w <= 0 || work_h <= 0 {
        return None;
    }
    let k = if saved_dpi >= 96 { dpi as f64 / saved_dpi as f64 } else { 1.0 };
    let scale = dpi.max(96) as f64 / 96.0;
    let w = ((w as f64 * k) as i32).max((size::MIN_W as f64 * scale) as i32).min(work_w);
    let h = ((h as f64 * k) as i32).max((size::MIN_H as f64 * scale) as i32).min(work_h);
    let left = x.max(work.left).min(work.right - w);
    let top = y.max(work.top).min(work.bottom - h);
    Some(RECT { left, top, right: left + w, bottom: top + h })
}

/// Milliseconds since this process was created (includes Windows/Wine loading the exe).
fn ms_since_process_start() -> Option<f64> {
    let (mut created, mut a, mut b, mut c) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: valid out-pointers; GetCurrentProcess is a pseudo-handle.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &mut created, &mut a, &mut b, &mut c).ok()?;
        let now = GetSystemTimePreciseAsFileTime();
        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        Some(t(now).saturating_sub(t(created)) as f64 / 10_000.0)
    }
}

/// Signed x/y from an LPARAM (they are negative on monitors left of / above the main one).
fn point_from(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 }
}

/// The other way round: a point packed as Windows packs it into an LPARAM.
fn pack(p: POINT) -> isize {
    ((p.x as u16 as u32) | ((p.y as u16 as u32) << 16)) as i32 as isize
}

fn mods() -> Mods {
    // SAFETY: plain key-state queries.
    unsafe {
        Mods {
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

/// Whether `vk` (with `shift`, Ctrl held and Alt not) is Ctrl+\: the \ key of US and UK keyboards
/// (VK_OEM_5), or the key that types "\" on the keyboard layout in use.
fn is_ctrl_backslash(vk: u16, shift: bool) -> bool {
    // SAFETY: plain keyboard-layout queries.
    let typed_by = unsafe { VkKeyScanExW('\\' as u16, GetKeyboardLayout(0)) };
    (vk == VK_OEM_5.0 && !shift) || backslash_key(typed_by) == Some((vk, shift))
}

/// The key (and Shift or not) for Ctrl+\ from what VkKeyScanExW says types "\": that key, also when
/// it takes AltGr ("\" is AltGr+\u{df} on German keyboards, so Ctrl+\u{df} there), since Ctrl+AltGr
/// would come as Ctrl+Alt. None when no key types it, or it's a letter or a digit (AltGr+8 on French
/// ones): Ctrl and those are other shortcuts already.
fn backslash_key(typed_by: i16) -> Option<(u16, bool)> {
    if typed_by == -1 {
        return None;
    }
    let (vk, shifts) = ((typed_by as u16) & 0xFF, (typed_by as u16) >> 8);
    // The shift state's bits: 1 Shift, 2 Ctrl, 4 Alt (AltGr is Ctrl and Alt); others are the layout's own.
    if shifts & !7 != 0 || (b'0' as u16..=b'9' as u16).contains(&vk) || (b'A' as u16..=b'Z' as u16).contains(&vk) {
        return None;
    }
    Some((vk, shifts & 1 != 0))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // While a menu with Windows' items is open, the messages those items need go to their maker
    // (popup.rs). They come from inside the menu's own loop, so no state is touched here.
    // The skin's own menus (oldmenu.rs) first: those items are Glint's.
    if let Some(r) = crate::oldmenu::handle(hwnd, msg, wparam, lparam) {
        return r;
    }
    if let Some(r) = popup::forward(msg, wparam, lparam) {
        return r;
    }
    // Messages that need no state (some arrive before the state exists).
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
                info.ptMinTrackSize = POINT { x: (size::MIN_W * scale) as i32, y: (size::MIN_H * scale) as i32 };
            }
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: the pointer was put there by `create` and is cleared in WM_NCDESTROY.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Window;
    if msg == WM_NCDESTROY && !ptr.is_null() {
        // SAFETY: last message for this window; we take the box back and drop it.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(ptr));
        }
        return LRESULT(0);
    }
    if ptr.is_null() || busy() {
        if !ptr.is_null() && (msg == WM_CLOSE || msg == WM_APP_LIFE || (msg == WM_SYSCOMMAND && (wparam.0 & 0xFFF0) == SC_CLOSE as usize)) {
            // Closing (which may only hide the window) and requests from the door are decided by
            // the state: posted again once the message being handled is done (see below).
            MISSED.with(|m| m.borrow_mut().push((msg, wparam.0, lparam.0)));
            return LRESULT(0);
        }
        if msg == WM_NCHITTEST {
            if let Some(edge) = frame::resize_edge(hwnd, wparam, lparam) {
                return LRESULT(edge as isize);
            }
        }
        if msg == WM_PAINT {
            // Re-entered while busy: paint on the next turn of the loop instead. (Not after a
            // crash: it would only come back at once, again and again, while the message is up.)
            // SAFETY: plain invalidate of our own window.
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
                if !FROZEN.load(Ordering::SeqCst) {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            return LRESULT(0);
        }
        // SAFETY: default handling.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    BUSY.with(|b| b.set(true));
    // SAFETY: BUSY guarantees this is the only live reference; it ends before BUSY is cleared.
    let handled = unsafe { (*ptr).handle(msg, wparam, lparam) };
    if !QUIET_MESSAGES.contains(&msg) {
        // SAFETY: as above.
        unsafe { (*ptr).note_changes() };
    }
    BUSY.with(|b| b.set(false));
    for (m, w, l) in MISSED.with(|m| std::mem::take(&mut *m.borrow_mut())) {
        // SAFETY: posting to our own window.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(hwnd), m, WPARAM(w), LPARAM(l));
        }
    }
    run_deferred(hwnd, ptr);

    match handled {
        Some(r) => r,
        // SAFETY: default handling (may run modal loops, e.g. moving the window - we hold no borrow).
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Messages that come often and never change anything settings.json remembers, so after them the
/// window doesn't look for changes to save (see `Window::note_changes`).
const QUIET_MESSAGES: [u32; 9] =
    [WM_MOUSEMOVE, WM_NCMOUSEMOVE, WM_SETCURSOR, WM_NCHITTEST, WM_PAINT, WM_MOUSELEAVE, WM_NCMOUSELEAVE, WM_TIMER, WM_APP_LOADED];

/// Whether `ptr` is still the live state of `hwnd`. A popup menu runs its own message loop, and
/// in it the window can be destroyed (a WM_CLOSE from another program, Windows shutting down) -
/// which frees the state. Checked after every queued job, before `ptr` is used again.
fn still_alive(hwnd: HWND, ptr: *mut Window) -> bool {
    // SAFETY: plain queries; WM_NCDESTROY clears GWLP_USERDATA before the state is freed.
    unsafe { IsWindow(Some(hwnd)).as_bool() && GetWindowLongPtrW(hwnd, GWLP_USERDATA) == ptr as isize }
}

/// Runs queued work with no borrow of the Window held (menus run their own message loop).
fn run_deferred(hwnd: HWND, ptr: *mut Window) {
    loop {
        if !still_alive(hwnd, ptr) {
            return;
        }
        // SAFETY: alive and not BUSY, so no other reference exists; the borrow ends at the semicolon.
        let jobs = std::mem::take(unsafe { &mut (*ptr).deferred });
        if jobs.is_empty() {
            return;
        }
        for job in jobs {
            if !still_alive(hwnd, ptr) {
                return;
            }
            match job {
                Deferred::Menu(part, menu) => {
                    // SAFETY: as above, a short borrow to read the scale.
                    let scale = unsafe { (*ptr).scale() };
                    let picked = if part == Part::Toolbar && toolbar::drawn_menu(menu.tag) {
                        // SAFETY: as above, a short borrow to read the skin.
                        let look = unsafe { crate::droplist::DropLook::of(&(*ptr).app.theme) };
                        drawn_menu(hwnd, &menu, scale, look)
                    } else {
                        show_menu(hwnd, &menu, scale)
                    };
                    if picked != 0 && still_alive(hwnd, ptr) {
                        BUSY.with(|b| b.set(true));
                        // SAFETY: BUSY again guards the only live reference.
                        unsafe {
                            (*ptr).menu_picked(part, menu.tag, picked);
                            (*ptr).note_changes();
                        }
                        BUSY.with(|b| b.set(false));
                    }
                }
                Deferred::FilesMenu(part, menu, built) => run_files_menu(hwnd, ptr, part, menu, built),
                // SAFETY: window operations on our own window, no borrow held.
                Deferred::Show(cmd) => unsafe {
                    let _ = ShowWindow(hwnd, cmd);
                },
                Deferred::Place(r) => unsafe {
                    let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                },
                Deferred::Close => unsafe {
                    let _ = DestroyWindow(hwnd);
                    return;
                },
                Deferred::DragOut(d) => drag::run_drag_out(hwnd, ptr, d),
                // SAFETY: our own window, no borrow held; the state is read once it's still alive.
                Deferred::Hide => unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    if still_alive(hwnd, ptr) && (*ptr).app.in_background() {
                        life::entered_background((*ptr).app.settings.tray_icon);
                    }
                },
                Deferred::Front => bring_to_front(hwnd, ptr),
                Deferred::OpenSettings(page) => open_settings(hwnd, ptr, page),
                Deferred::OpenSetting(page, title) => {
                    open_settings(hwnd, ptr, Some(page));
                    if still_alive(hwnd, ptr) {
                        BUSY.with(|b| b.set(true));
                        // SAFETY: BUSY guards the only live reference.
                        unsafe {
                            if let Some(s) = (*ptr).settings_win.as_mut() {
                                s.point_at(&title);
                            }
                        }
                        BUSY.with(|b| b.set(false));
                    }
                }
                Deferred::Settings(job) => {
                    // SAFETY: as above, a short borrow to read where the Settings window is.
                    let Some((swin, scale)) = (unsafe { (*ptr).settings_win.as_ref().map(|s| (s.hwnd(), s.scale())) }) else { continue };
                    let done = settingswin::run_job(swin, scale, job);
                    if done != settingswin::JobDone::Nothing && still_alive(hwnd, ptr) {
                        BUSY.with(|b| b.set(true));
                        // SAFETY: BUSY guards the only live reference.
                        unsafe {
                            (*ptr).settings_job_done(done);
                            (*ptr).note_changes();
                        }
                        BUSY.with(|b| b.set(false));
                    }
                }
                Deferred::OpenSkins => open_skins(hwnd, ptr),
                Deferred::OpenSort => sorting::open_sort(hwnd, ptr),
                Deferred::Sort(job) => sorting::run_sort_job(hwnd, ptr, job),
                Deferred::Skins(job) => {
                    // SAFETY: as above, a short borrow to read where the customiser is.
                    let Some((swin, scale)) = (unsafe { (*ptr).skin_win.as_ref().map(|s| (s.hwnd(), s.scale())) }) else { continue };
                    let done = skinwin::run_job(swin, scale, job, show_menu);
                    if done != skinwin::JobDone::Nothing && still_alive(hwnd, ptr) {
                        BUSY.with(|b| b.set(true));
                        // SAFETY: BUSY guards the only live reference.
                        unsafe {
                            (*ptr).skin_job_done(done);
                            (*ptr).note_changes();
                        }
                        BUSY.with(|b| b.set(false));
                    }
                }
            }
        }
    }
}

/// Shows the window if it's hidden (its first frame drawn behind the cloak, as at start) or
/// restores it if it's minimised, and brings it to the front. Run with no borrow of the state held:
/// showing sends WM_SIZE and WM_PAINT straight away.
fn bring_to_front(hwnd: HWND, ptr: *mut Window) {
    // SAFETY: window calls on our own window; the state is only touched in short borrows while it's
    // still alive and no message is being handled.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            // The first time (started with Windows), it opens maximised if it was last time.
            let maximize = !(*ptr).ever_shown && (*ptr).maximized;
            (*ptr).ever_shown = true;
            if !maximize && !IsZoomed(hwnd).as_bool() && !IsIconic(hwnd).as_bool() {
                onto_a_screen(hwnd);
            }
            frame::cloak(hwnd, true);
            let _ = ShowWindow(hwnd, if maximize { SW_MAXIMIZE } else { SW_SHOW });
            // Minimised before it was closed (from the taskbar, say): it comes back restored.
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            if still_alive(hwnd, ptr) {
                let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | windows::Win32::Graphics::Gdi::RDW_UPDATENOW);
            }
            frame::cloak(hwnd, false);
            if frame::is_wine() && still_alive(hwnd, ptr) {
                // Wine shows a frame only once the next one is drawn (see ListView::wine_nudge).
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        } else if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let front = SetForegroundWindow(hwnd).as_bool();
        log::line(&format!("brought to the front (visible: {}, in front: {front})", IsWindowVisible(hwnd).as_bool()));
    }
}

/// A window hidden in the background may have been left on a screen that is gone since (a laptop
/// taken away from its second monitor): it's moved onto the nearest screen there is, as at start.
fn onto_a_screen(hwnd: HWND) {
    let mut r = RECT::default();
    // SAFETY: plain window and monitor queries with valid in/out structs, on our own window.
    unsafe {
        if GetWindowRect(hwnd, &mut r).is_err() || !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid() {
            return; // still (at least partly) on a screen
        }
        let monitor = MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return;
        }
        let dpi = GetDpiForWindow(hwnd).max(96);
        if let Some(f) = fit_rect([r.left, r.top, r.right - r.left, r.bottom - r.top], 0, dpi, info.rcWork) {
            let _ = SetWindowPos(hwnd, None, f.left, f.top, f.right - f.left, f.bottom - f.top, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// Makes the Settings window, or brings it to the front if it's open (run outside message
/// handling: making a window sends it messages, and showing it lays it out and paints it).
fn open_settings(hwnd: HWND, ptr: *mut Window, page: Option<usize>) {
    // SAFETY: as in run_deferred: short borrows with no message handling in between, BUSY around
    // the ones that change the state.
    unsafe {
        // A state whose window is gone (it can't happen, but if it did, Settings could never open
        // again) is let go first.
        if (*ptr).settings_win.as_ref().is_some_and(|s| !IsWindow(Some(s.hwnd())).as_bool()) {
            BUSY.with(|b| b.set(true));
            (*ptr).settings_win = None;
            BUSY.with(|b| b.set(false));
        }
        if (*ptr).settings_win.is_some() {
            BUSY.with(|b| b.set(true));
            if let Some(s) = (*ptr).settings_win.as_mut() {
                let asks = s.reopen(page);
                (*ptr).settings_asks(asks);
            }
            BUSY.with(|b| b.set(false));
            return;
        }
        let theme = (*ptr).app.theme.clone();
        let (opens, place) = ((*ptr).app.settings.settings_opens, (*ptr).app.settings.settings_place);
        // Opened without a page: the last one it showed, when it should reopen there.
        let page = page.unwrap_or(if (*ptr).app.settings.settings_last_page { (*ptr).app.settings.settings_page } else { 0 });
        // Glued to the side, Settings takes the sort editor's and the customiser's place.
        if opens == settings::SettingsOpens::Side {
            if let Some(s) = (*ptr).sort_win.as_ref() {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(s.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            if let Some(s) = (*ptr).skin_win.as_ref() {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(s.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        let list = (*ptr).app.settings.settings_pages == settings::SettingsPages::List;
        let Some(win) = settingswin::create(hwnd, &theme, place, opens, list, page) else {
            BUSY.with(|b| b.set(true));
            (*ptr).app.say("Windows couldn't open the Settings window right now");
            (*ptr).invalidate();
            BUSY.with(|b| b.set(false));
            return;
        };
        if !still_alive(hwnd, ptr) {
            return;
        }
        BUSY.with(|b| b.set(true));
        (*ptr).settings_win = Some(win);
        (*ptr).deferred.push(Deferred::Settings(settingswin::Job::Show));
        (*ptr).invalidate(); // its tab, when it opens as one
        BUSY.with(|b| b.set(false));
    }
}

/// Makes the skin customiser, or brings it to the front if it's open (run outside message handling,
/// like `open_settings`).
fn open_skins(hwnd: HWND, ptr: *mut Window) {
    // SAFETY: as in open_settings.
    unsafe {
        // The skins folder is listed again (in the background) whenever the customiser opens, so
        // the names it gives new and renamed skins know every file there - also files put there by
        // hand while the folder wasn't watched yet.
        BUSY.with(|b| b.set(true));
        (*ptr).app.skins.watch_folder();
        (*ptr).app.skins.refresh();
        BUSY.with(|b| b.set(false));
        if (*ptr).skin_win.as_ref().is_some_and(|s| !IsWindow(Some(s.hwnd())).as_bool()) {
            BUSY.with(|b| b.set(true));
            (*ptr).skin_win = None;
            BUSY.with(|b| b.set(false));
        }
        if (*ptr).skin_win.is_some() {
            BUSY.with(|b| b.set(true));
            if let Some(s) = (*ptr).skin_win.as_mut() {
                let asks = s.reopen();
                (*ptr).skin_asks(asks);
            }
            BUSY.with(|b| b.set(false));
            return;
        }
        // The customiser takes over from Settings and the sort editor (one docked window at a time).
        if let Some(s) = (*ptr).settings_win.as_ref() {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(s.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(s) = (*ptr).sort_win.as_ref() {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(s.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        let theme = (*ptr).app.theme.clone();
        let Some(win) = skinwin::create(hwnd, &theme) else {
            BUSY.with(|b| b.set(true));
            (*ptr).app.say("Windows couldn't open the skin customiser right now");
            (*ptr).invalidate();
            BUSY.with(|b| b.set(false));
            return;
        };
        if !still_alive(hwnd, ptr) {
            return;
        }
        BUSY.with(|b| b.set(true));
        (*ptr).skin_win = Some(win);
        (*ptr).deferred.push(Deferred::Skins(skinwin::Job::Show));
        BUSY.with(|b| b.set(false));
    }
}

/// A message of the skin customiser: handled by its state (skinwin.rs), which this window keeps,
/// under the same guard as this window's own messages (see `settings_message`).
pub fn skin_message(owner: HWND, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if owner.is_invalid() {
        return None;
    }
    // SAFETY: the pointer was put there by `create` and is cleared in WM_NCDESTROY (see wndproc).
    let ptr = unsafe { GetWindowLongPtrW(owner, GWLP_USERDATA) } as *mut Window;
    if ptr.is_null() || busy() {
        return None;
    }
    // SAFETY: not BUSY, so no other reference exists; a short borrow.
    if unsafe { (*ptr).skin_win.as_ref().map(|s| s.hwnd()) } != Some(hwnd) {
        return None;
    }
    BUSY.with(|b| b.set(true));
    // SAFETY: BUSY guarantees this is the only live reference; it ends before BUSY is cleared.
    let handled = unsafe { (*ptr).skin_handle(msg, wparam, lparam) };
    if !QUIET_MESSAGES.contains(&msg) {
        // SAFETY: as above.
        unsafe { (*ptr).note_changes() };
    }
    BUSY.with(|b| b.set(false));
    run_deferred(owner, ptr);
    handled
}

/// A message of the Settings window: handled by its state (settingswin.rs), which this window
/// keeps, under the same guard as this window's own messages, then this window's queued work runs
/// as after any message. None: the state isn't there or is busy (Windows' default handling).
pub fn settings_message(owner: HWND, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if owner.is_invalid() {
        return None;
    }
    // SAFETY: the pointer was put there by `create` and is cleared in WM_NCDESTROY (see wndproc).
    let ptr = unsafe { GetWindowLongPtrW(owner, GWLP_USERDATA) } as *mut Window;
    if ptr.is_null() || busy() {
        return None;
    }
    // SAFETY: not BUSY, so no other reference exists; a short borrow.
    if unsafe { (*ptr).settings_win.as_ref().map(|s| s.hwnd()) } != Some(hwnd) {
        return None;
    }
    BUSY.with(|b| b.set(true));
    // SAFETY: BUSY guarantees this is the only live reference; it ends before BUSY is cleared.
    let handled = unsafe { (*ptr).settings_handle(msg, wparam, lparam) };
    if !QUIET_MESSAGES.contains(&msg) {
        // SAFETY: as above.
        unsafe { (*ptr).note_changes() };
    }
    BUSY.with(|b| b.set(false));
    run_deferred(owner, ptr);
    handled
}

/// Shows a button's menu drawn by Glint (droplist.rs), hanging from `menu.at`, and returns the
/// picked item's id (0 = none).
fn drawn_menu(hwnd: HWND, menu: &Menu, scale: f32, look: crate::droplist::DropLook) -> u32 {
    let mut pt = POINT { x: (menu.at.x * scale).round() as i32, y: (menu.at.y * scale).round() as i32 };
    // SAFETY: converts in place for our own window.
    unsafe {
        let _ = ClientToScreen(hwnd, &mut pt);
    }
    let under = RECT { left: pt.x, top: pt.y, right: pt.x, bottom: pt.y };
    crate::droplist::show(hwnd, &crate::droplist::DropList { items: menu.items.clone(), under, look, menu: true, right: menu.right_aligned })
}

/// Shows a native popup menu (popup.rs: with its items' dots and pictures, and its bold item) and
/// returns the picked item's id (0 = none).
fn show_menu(hwnd: HWND, menu: &Menu, scale: f32) -> u32 {
    let mut pt = POINT { x: (menu.at.x * scale).round() as i32, y: (menu.at.y * scale).round() as i32 };
    // SAFETY: converts in place for our own window.
    unsafe {
        let _ = ClientToScreen(hwnd, &mut pt);
    }
    let bold = menu.items.iter().find(|i| i.bold && i.submenu.is_empty() && !i.label.is_empty()).map_or(0, |i| i.id);
    // (No Windows items here, so every id is one of the menu's own.)
    match popup::show(hwnd, pt, menu, bold, None, false) {
        Pick::Glint(id) | Pick::Windows(id) => id,
        Pick::None => 0,
    }
}

/// WM_CTLCOLOREDIT: text boxes in the theme's input colours.
fn edit_colors(hdc: HDC) -> LRESULT {
    let (bg, fg, brush) = EDIT_COLORS.with(|c| c.get());
    // SAFETY: `hdc` is the control's DC for this paint; the brush lives until the theme changes.
    unsafe {
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(fg));
        SetBkColor(hdc, windows::Win32::Foundation::COLORREF(bg));
    }
    if brush == 0 {
        return LRESULT(COLOR_WINDOW.0 as isize + 1);
    }
    LRESULT(brush)
}

/// Shows a right-click menu for files (no borrow of the Window held, as for any menu) and carries
/// out the pick: Glint's own items go to the part's `menu_pick`; Windows' commands run on the shell
/// thread - except the few Glint does itself (menu::command_for_verb); "Show more options" opens
/// Windows' whole menu at the same place. The Windows menu session is let go in every case.
fn run_files_menu(hwnd: HWND, ptr: *mut Window, part: Part, mut menu: FilesMenu, built: Option<Built>) {
    // SAFETY: as in run_deferred, a short borrow to read the scale.
    let scale = unsafe { (*ptr).scale() };
    let mut at = POINT { x: (menu.menu.at.x * scale).round() as i32, y: (menu.menu.at.y * scale).round() as i32 };
    // SAFETY: converts in place for our own window.
    unsafe {
        let _ = ClientToScreen(hwnd, &mut at);
    }
    let whole = menu.windows.as_ref().is_some_and(|w| w.full) && built.is_some();
    let mut pick = popup::show(hwnd, at, &menu.menu, menu.default_item, built.as_ref(), whole);
    if pick == Pick::Glint(MORE_OPTIONS) {
        match &built {
            Some(b) => pick = popup::show(hwnd, at, &menu.menu, 0, Some(b), true),
            None => {
                // Windows' items didn't come in time for the first menu: ask again for the whole one.
                if let Some(w) = menu.windows.as_mut() {
                    w.full = true;
                }
                if still_alive(hwnd, ptr) {
                    BUSY.with(|b| b.set(true));
                    // SAFETY: BUSY guards the only live reference.
                    unsafe { (*ptr).open_files_menu(part, menu) };
                    BUSY.with(|b| b.set(false));
                }
                return;
            }
        }
    }
    let release = |b: Option<Built>| {
        if let Some(b) = b {
            shellmenu::release(b.session);
        }
    };
    if !still_alive(hwnd, ptr) {
        release(built);
        return;
    }
    match pick {
        Pick::None | Pick::Glint(MORE_OPTIONS) => release(built),
        Pick::Glint(id) => {
            release(built);
            BUSY.with(|b| b.set(true));
            // SAFETY: BUSY guards the only live reference.
            unsafe {
                // The pick is about what was right-clicked, even if the selection changed meanwhile.
                if (*ptr).app.reselect_for_menu(&menu.about) {
                    (*ptr).menu_picked(part, menu.menu.tag, id);
                }
                (*ptr).invalidate();
                (*ptr).note_changes(); // "Show hidden files", say
            }
            BUSY.with(|b| b.set(false));
        }
        Pick::Windows(cmd) => {
            let Some(b) = built else { return };
            BUSY.with(|bz| bz.set(true));
            // SAFETY: BUSY guards the only live reference.
            let ours = unsafe {
                let verb = b.verb_of(cmd);
                // Commands Glint does itself work on the selection: first make it what the menu was
                // opened for again (Windows' own commands carry their items with them).
                if menu::is_glint_verb(verb) && !(*ptr).app.reselect_for_menu(&menu.about) {
                    shellmenu::release(b.session);
                    (*ptr).invalidate();
                    BUSY.with(|bz| bz.set(false));
                    return;
                }
                menu::command_for_verb(verb, &(*ptr).app, mods().shift)
            };
            match ours {
                Some(c) => {
                    shellmenu::release(b.session);
                    // SAFETY: as above.
                    unsafe { (*ptr).apply(part, Effect::Command(c)) };
                }
                None => {
                    // Windows' "New ›" in the empty space: the item it makes gets the name box when
                    // it turns up (app/files.rs), as in Explorer.
                    let background = matches!(menu.windows.as_ref().map(|w| &w.target), Some(ShellTarget::Background(_)));
                    if background && b.in_submenu_of("new", cmd) {
                        // SAFETY: as above.
                        unsafe { (*ptr).app.rename_what_appears() };
                    }
                    let m = mods();
                    shellmenu::invoke(hwnd, b.session, cmd, at, m.shift, m.ctrl);
                }
            }
            BUSY.with(|bz| bz.set(false));
        }
    }
}

impl Window {
    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            hwnd: self.hwnd,
            scale: self.scale(),
            now: self.now(),
            // SAFETY: plain query on our own window.
            maximized: unsafe { IsZoomed(self.hwnd).as_bool() },
            active: self.active,
        }
    }

    fn invalidate(&self) {
        // SAFETY: plain invalidate of our own window.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Calls `f` with one part and the app state (two separate borrows of self).
    /// (Split view's other half sees its own tab as the active one meanwhile.)
    fn with_part<R>(&mut self, part: Part, f: impl FnOnce(&mut dyn Component, &mut AppState) -> R) -> R {
        let Window { titlebar, toolbar, sidebar, list, list2, status, side, app, .. } = self;
        let c: &mut dyn Component = match part {
            Part::Title => titlebar,
            Part::Toolbar => toolbar,
            Part::Sidebar => sidebar,
            Part::List => list,
            Part::List2 => {
                let Some(other) = app.split_index() else { return f(list2, app) };
                let keep = std::mem::replace(&mut app.active, other);
                let r = f(list2, app);
                app.active = keep;
                return r;
            }
            Part::Status => status,
            Part::Side => side,
        };
        f(c, app)
    }

    const PARTS: [Part; 7] = [Part::Title, Part::Toolbar, Part::Sidebar, Part::List, Part::List2, Part::Status, Part::Side];

    /// The parts on screen: split view's second half only while split view is open. (Closed, the
    /// hidden half would act on the tab Glint works in: with no size of its own it scrolled Home
    /// back to the top every frame.)
    fn parts_on(&self) -> Vec<Part> {
        let split = self.app.split_index().is_some();
        Self::PARTS.into_iter().filter(|&p| p != Part::List2 || split).collect()
    }

    fn part_at(&self, p: Point) -> Option<Part> {
        let rect = |part| match part {
            Part::Title => self.titlebar.rect(),
            Part::Toolbar => self.toolbar.rect(),
            Part::Sidebar => self.sidebar.rect(),
            Part::List => self.list.rect(),
            Part::List2 if self.app.split.is_some() => self.list2.rect(),
            Part::List2 => Rect::default(),
            Part::Status => self.status.rect(),
            Part::Side => self.side.rect(),
        };
        Self::PARTS.into_iter().find(|&part| rect(part).contains(p))
    }

    /// Where every part sits, from the window size.
    fn layout(&mut self) {
        // 98 / 2000: everything sits inside the window's raised frame (frame.rs).
        let b = frame::classic_border(&self.app.theme, self.maximized);
        let (w, h) = ((self.size.0 - 2.0 * b).max(0.0), (self.size.1 - 2.0 * b).max(0.0));
        let ctx = self.ctx();
        let gap = self.app.theme.gap();
        let body_y = b + size::TITLE_H + size::TOOLBAR_H + gap;
        // "Show the status bar" off: the list goes down to the bottom (notes then show over it).
        let status_h = if self.app.settings.show_status_bar { size::STATUS_H } else { gap };
        let status = Rect::new(b, b + h - status_h, w, if self.app.settings.show_status_bar { status_h } else { 0.0 });
        let body_h = (b + h - status_h - body_y).max(0.0);
        // Ctrl+B hides the sidebar ("ShowSidebar"): the list then takes its room.
        let sidebar_w = if self.app.settings.show_sidebar { size::SIDEBAR_W } else { 0.0 };
        // The details panel or the Space preview takes its width from the list's room, with the
        // same gap before it as between the other panels (side.rs; 0 when neither shows).
        let room = (w - sidebar_w - 2.0 * gap).max(0.0);
        let pane = self.side.pane_width(&self.app, room);
        let list_w = if pane > 0.0 { room - pane - gap } else { room };
        let list = Rect::new(b + sidebar_w + gap, body_y, list_w, body_h);
        let side = if pane > 0.0 { Rect::new(list.right(), body_y, gap + pane, body_h) } else { Rect::default() };
        // Split view: two halves (the left one as wide as dragged), the gap between them, and a
        // strip with its folder's name over each (the half Glint works in on its side).
        let split_gap = gap.max(4.0);
        self.split_area = list;
        let (mine, other) = if self.app.split_index().is_some() {
            let ratio = self.app.settings.split_ratio.clamp(0.2, 0.8);
            let left_w = ((list.w - split_gap) * ratio).floor().max(0.0);
            let strip = |x: f32, w: f32| Rect::new(x, list.y, w, SPLIT_STRIP_H);
            let body = |x: f32, w: f32| Rect::new(x, list.y + SPLIT_STRIP_H, w, (list.h - SPLIT_STRIP_H).max(0.0));
            let right_x = list.x + left_w + split_gap;
            let right_w = (list.w - left_w - split_gap).max(0.0);
            self.split_divider = Rect::new(list.x + left_w - 3.0, list.y, split_gap + 6.0, list.h);
            let (left, right) = ((strip(list.x, left_w), body(list.x, left_w)), (strip(right_x, right_w), body(right_x, right_w)));
            let (mine, other) = if self.split_left { (left, right) } else { (right, left) };
            self.split_strips = [mine.0, other.0];
            (mine.1, other.1)
        } else {
            self.split_strips = [Rect::default(); 2];
            self.split_divider = Rect::default();
            (list, Rect::default())
        };
        let rects = [
            (Part::Title, Rect::new(b, b, w, size::TITLE_H)),
            (Part::Toolbar, Rect::new(b, b + size::TITLE_H, w, size::TOOLBAR_H)),
            (Part::Sidebar, Rect::new(b, body_y, sidebar_w, body_h)),
            (Part::List, mine),
            (Part::List2, other),
            (Part::Status, status),
            (Part::Side, side),
        ];
        for (part, rect) in rects {
            self.with_part(part, |c, app| c.layout(rect, &ctx, app));
        }
        // Split view: one wallpaper across both halves.
        let wall = self.app.split_index().is_some().then_some(list);
        self.list.set_wall_area(wall);
        self.list2.set_wall_area(wall);
    }

    /// Follows the skin: its font and text size, the text boxes' colours, the frame and Windows'
    /// menus, the Settings window (and the parts are laid out again: lines and slants changed).
    fn apply_theme(&mut self) {
        self.theme_generation = self.app.theme_generation;
        self.renderer.set_font(&self.app.theme.font, self.app.theme.font_size / size::FONT);
        self.renderer.set_title_font(&self.app.theme.title_font);
        textbox::set_font_family(&self.renderer.font_family());
        self.layout();
        let theme = &self.app.theme;
        let (bg, fg) = (theme.input.to_colorref(), theme.text.to_colorref());
        // SAFETY: a new brush replaces the old one, which is deleted (no control paints in between:
        // we are on the UI thread).
        unsafe {
            let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(bg));
            let old = EDIT_COLORS.with(|c| c.replace((bg, fg, brush.0 as isize))).2;
            if old != 0 {
                let _ = DeleteObject(HGDIOBJ(old as *mut _));
            }
            frame::apply_theme(self.hwnd, theme);
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
        // Windows' suggestions under the path box, in the boxes' colours.
        textbox::set_suggestion_colors(theme.dark, bg, fg);
        if let Some(s) = self.settings_win.as_mut() {
            s.apply_theme(&self.app.theme);
        }
        if let Some(s) = self.skin_win.as_mut() {
            s.apply_theme(&self.app.theme);
        }
        if let Some(s) = self.sort_win.as_mut() {
            s.apply_theme(&self.app.theme);
        }
        updatewin::apply_theme(&self.app.theme);
    }

    /// Carries out what a part asked for.
    fn apply(&mut self, part: Part, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Redraw => self.invalidate(),
            Effect::Animate => {
                ANIMATING.with(|a| a.set(true));
                self.invalidate();
            }
            Effect::Command(cmd) => {
                self.run_command(cmd);
                self.invalidate();
                self.keyboard_to_question();
            }
            Effect::Menu(menu) => self.deferred.push(Deferred::Menu(part, menu)),
            Effect::FilesMenu(menu) => self.open_files_menu(part, menu),
            Effect::DragOut(d) => self.start_drag_out(d),
        }
    }

    /// A question just opened (ask.rs): the keyboard comes to the window, so Enter and Esc reach it
    /// even when a text box had the keyboard (the name box that asked about an extension, say).
    fn keyboard_to_question(&mut self) {
        if self.app.question.is_some() {
            // SAFETY: focus query and change on our own window.
            unsafe {
                if GetFocus() != self.hwnd {
                    let _ = SetFocus(Some(self.hwnd));
                }
            }
        }
    }

    /// What the form box said: yes does what it's about (if what's typed is fine), no closes it.
    fn form_answered(&mut self, a: Answer) {
        match a {
            Answer::Nothing => {}
            Answer::Redraw => self.invalidate(),
            Answer::Yes | Answer::No => {
                let values = (a == Answer::Yes).then(|| self.form.values());
                if self.app.form_answer(values) {
                    self.form.close(self.hwnd);
                }
                self.invalidate();
            }
        }
    }

    /// While a form is open: whether it has been drawn yet (see `question_shown`).
    fn form_shown(&self) -> bool {
        self.app.form.as_ref().is_some_and(|f| self.form.shown(f.id))
    }

    /// What the command box said: close it, or close it and do what was picked.
    fn palette_chose(&mut self, c: crate::palette::Choice) {
        use crate::palette::{Action, Choice};
        let action = match c {
            Choice::Nothing => return,
            Choice::Redraw => return self.invalidate(),
            // Gone to another window (Alt+Tab): it stays open, and gets the keyboard back after.
            Choice::LostFocus if !self.active => return,
            Choice::Close | Choice::LostFocus => {
                self.palette.close(self.hwnd);
                return self.invalidate();
            }
            Choice::Run(a) => a,
        };
        self.palette.close(self.hwnd);
        match action {
            Action::Run(cmd) => self.apply(Part::List, Effect::Command(cmd)),
            Action::Skin(id) => {
                self.app.pick_skin(&id);
                self.sync_theme();
            }
            Action::SelectAll => self.app.tab_mut().select_all(),
            Action::SelectPattern(pattern) => {
                if crate::palette::select_matching(self.app.tab_mut(), &pattern) == 0 {
                    self.app.say(format!("Nothing here matches {pattern}"));
                }
            }
            // (The box keeps these itself.)
            Action::Fill(_) => {}
            Action::FolderSizes(on) => self.app.set_folder_sizes(on),
            Action::RenameMany if self.app.tab().selected_entries().len() < 2 => self.app.say("Select two or more items to rename them all at once"),
            Action::RenameMany => self.apply(Part::List, Effect::Command(Command::RenameMany)),
            Action::Search(text) => {
                // As Ctrl+F and typing: the text in the filter box, and the cursor there.
                self.apply(Part::List, Effect::Command(Command::SetFilter(text)));
                let now = self.now();
                self.toolbar.tick(now, &mut self.app);
                self.apply(Part::List, Effect::Command(Command::FocusFilter));
            }
        }
        self.invalidate();
    }

    /// What the question box said: yes runs the job asked about.
    fn answered(&mut self, a: Answer) {
        match a {
            Answer::Nothing => {}
            Answer::Redraw => self.invalidate(),
            Answer::Yes | Answer::No => {
                self.app.answer(a == Answer::Yes);
                self.invalidate();
            }
        }
    }

    /// Shows or hides the sidebar (Ctrl+B, the Settings window).
    fn show_sidebar(&mut self, show: bool, now: f64) {
        self.app.settings.show_sidebar = show;
        self.layout();
        // (Wine shows a frame only once the next is drawn: paint a moment longer there.)
        if self.list.wine_nudge(now) {
            ANIMATING.with(|a| a.set(true));
        }
        self.app.say(if show { "Sidebar shown (Ctrl+B hides it)" } else { "Sidebar hidden (Ctrl+B shows it)" });
    }

    /// Shows or hides the details panel (Alt+P, the Settings window).
    fn show_details(&mut self, show: bool, now: f64) {
        self.app.settings.show_details = show;
        self.side_changed(now);
        if self.side.preview_open() {
            // The preview is in its place; the panel comes back when the preview closes.
            self.app.say(if show { "The details panel shows when the preview closes" } else { "Details panel hidden" });
        } else if show && self.side.rect().is_empty() {
            // No room for it next to the list (side.rs): it shows once the window is wider.
            self.app.say("Make the window wider to show the details panel");
        } else {
            self.app.say(if show { "Details panel shown (Alt+P hides it)" } else { "Details panel hidden (Alt+P shows it)" });
        }
    }

    /// Space: opens the preview of the selected file, or closes it.
    fn toggle_preview(&mut self, now: f64) {
        if self.side.preview_open() {
            self.side.set_preview(false);
        } else if self.app.tab().selected_entries().is_empty() {
            self.app.say("Select a file to preview");
            return;
        } else {
            self.side.set_preview(true);
        }
        self.side_changed(now);
        if self.side.preview_open() && self.side.rect().is_empty() {
            self.app.say("Make the window wider to show the preview");
        }
    }

    /// Shows or hides the status bar (the Settings window's "Show the status bar").
    fn show_status_bar(&mut self, show: bool, now: f64) {
        self.app.settings.show_status_bar = show;
        self.side_changed(now);
    }

    /// The pane right of the list opened, closed or changed width, or the status bar came or went:
    /// the list makes room for it.
    fn side_changed(&mut self, now: f64) {
        self.layout();
        // (Wine shows a frame only once the next is drawn: paint a moment longer there.)
        if self.list.wine_nudge(now) {
            ANIMATING.with(|a| a.set(true));
        }
    }

    fn run_command(&mut self, cmd: Command) {
        let ctx = self.ctx();
        // Deleting, renaming or moving the selection (cut, it's pasted elsewhere): a video the
        // preview plays from it lets go of its file first - Windows won't touch a file that's open.
        if matches!(cmd, Command::Delete { .. } | Command::Rename { .. } | Command::RenameMany | Command::CutFiles | Command::SendToOtherSide { moving: true }) {
            self.side.release_files();
        }
        match cmd {
            Command::Minimize => self.deferred.push(Deferred::Show(SW_MINIMIZE)),
            Command::ToggleMaximize => self.deferred.push(Deferred::Show(if ctx.maximized { SW_RESTORE } else { SW_MAXIMIZE })),
            Command::Close => self.close_asked(true),
            // Closing the last tab (Ctrl+W, its ×, middle-click, its menu) closes the window, as in
            // File Explorer - which in background mode hides it.
            // (A pinned last tab stays: app.rs says why.)
            Command::CloseTab(_) if self.app.tabs.len() <= 1 && !self.app.tab().pinned => self.close_asked(true),
            Command::ForceCloseTab(_) if self.app.tabs.len() <= 1 => self.close_asked(true),
            Command::Quit => self.quit(),
            Command::NewWindow => life::open_new_window(shell::Owner::new(self.hwnd), &self.app.tab().path),
            Command::ToggleTheme => {
                let dark = !self.app.dark;
                let before = self.app.settings.theme;
                self.app.settings.theme = if dark { ThemeMode::Dark } else { ThemeMode::Light };
                if !self.set_dark(dark) {
                    // Nothing changed on screen, so "Light or dark" stays as it was too.
                    self.app.settings.theme = before;
                    self.app.say(if dark { "This skin has no dark version" } else { "This skin has no light version: make one called \u{201c}\u{2026} (Light)\u{201d}" });
                }
            }
            Command::SetTheme(mode) => {
                self.app.settings.theme = mode;
                self.set_dark(settings::dark_for(mode));
            }
            Command::EditPath => {
                let e = self.toolbar.begin_path_edit(&ctx, &mut self.app);
                self.apply(Part::Toolbar, e);
            }
            Command::FocusFilter => {
                let e = self.toolbar.focus_filter(&ctx, &mut self.app);
                self.apply(Part::Toolbar, e);
            }
            Command::ToggleSidebar => self.show_sidebar(!self.app.settings.show_sidebar, ctx.now),
            Command::ShowSidebar(show) => self.show_sidebar(show, ctx.now),
            Command::ToggleDetails => self.show_details(!self.app.settings.show_details, ctx.now),
            Command::ShowDetails(show) => self.show_details(show, ctx.now),
            Command::TogglePreview => self.toggle_preview(ctx.now),
            Command::ClosePreview => {
                self.side.set_preview(false);
                self.side_changed(ctx.now);
            }
            Command::SideWidth(w) => {
                self.side.set_width(&mut self.app, w);
                self.side_changed(ctx.now);
            }
            Command::ShowStatusBar(show) => self.show_status_bar(show, ctx.now),
            Command::ToggleSubfolders => {
                // The switch, then the cursor in the filter, ready to type what to look for.
                self.app.run(Command::ToggleSubfolders);
                if !self.app.tab().path.is_empty() {
                    let e = self.toolbar.focus_filter(&ctx, &mut self.app);
                    self.apply(Part::Toolbar, e);
                }
            }
            Command::BeginRename => {
                let e = self.list.begin_rename_selected(&ctx, &mut self.app);
                self.apply(Part::List, e);
            }
            Command::OpenSettings(page) => self.deferred.push(Deferred::OpenSettings(page)),
            Command::OpenSetting(page, title) => self.deferred.push(Deferred::OpenSetting(page, title)),
            Command::OpenSkinEditor => self.deferred.push(Deferred::OpenSkins),
            Command::CloseSettings => {
                if self.settings_win.is_some() {
                    self.deferred.push(Deferred::Settings(settingswin::Job::Close));
                }
            }
            Command::OpenSortEditor => self.deferred.push(Deferred::OpenSort),
            Command::CheckForUpdates => {
                // Only the main Glint looks (Settings > About says so in an extra window).
                if self.main && !self.bench && update::start_check(shell::Owner::new(self.hwnd), &self.app.settings, true) {
                    self.refresh_settings();
                }
            }
            other => {
                self.app.run(other);
            }
        }
        self.follow_split();
    }

    /// Split view (app/split.rs): `list` is always the half Glint works in, `list2` the other
    /// half. When the app swapped the halves' tabs (a click in the other half, F6, its tab picked
    /// in the tab row), the two lists change places too, so each half keeps its tab, its scroll
    /// and where it sits; split view on or off lays the window out again.
    fn follow_split(&mut self) {
        let now = (self.app.tab().id, self.app.split_index().map(|i| self.app.tabs[i].id));
        // The halves sit in the tabs' order: the tab further left in the tab row on the left.
        let left = self.app.split_index().is_none_or(|other| self.app.active < other);
        if now == self.split_shown && left == self.split_left {
            return;
        }
        match (now, self.split_shown) {
            ((a, Some(b)), (old_a, Some(old_b))) if a == old_b && b == old_a => {
                std::mem::swap(&mut self.list, &mut self.list2);
                // Column widths set in the half just left carry over (they're saved from `list`).
                let widths = self.list2.column_widths();
                self.list.set_column_widths(&widths);
            }
            _ => {}
        }
        self.split_left = left;
        let relayout = now.1.is_some() != self.split_shown.1.is_some() || now.1.is_some();
        self.split_shown = now;
        if relayout {
            self.layout();
            self.invalidate();
        }
    }

    // ------------------------------------------------------------ how Glint runs (life.rs)

    /// The window is asked to close. `by_person`: the close button, Alt+F4, the taskbar's "Close
    /// window" - then the main window only hides when "Keep Glint running in the background" is on.
    /// Otherwise (another program or Task Manager sent WM_CLOSE)
    /// it really closes.
    fn close_asked(&mut self, by_person: bool) {
        if by_person && self.main && !self.quitting && self.app.settings.stay_running && !self.bench {
            self.go_to_background();
        } else {
            self.deferred.push(Deferred::Close);
        }
    }

    /// Quits completely (Ctrl+Q, the tray icon's Quit, `glint.exe --quit`): even in background mode.
    /// In an extra window, a main Glint that is only hidden in the background quits too.
    fn quit(&mut self) {
        self.quitting = true;
        life::quit_hidden_main_too();
        self.deferred.push(Deferred::Close);
    }

    /// Hides the main window instead of closing it, so the next window opens instantly. Like a
    /// real close, what settings.json remembers is written now; then nothing runs while hidden: no
    /// timers, no folder watch, no animation, and memory goes back to Windows (Deferred::Hide).
    fn go_to_background(&mut self) {
        if self.app.in_background() {
            return;
        }
        // A question nobody answered is a no: nothing happens to files while Glint is hidden.
        if self.app.question.is_some() {
            self.app.answer(false);
        }
        self.drop_pending_menu();
        // The Settings window and the skin customiser go too (they're owned by this one, which
        // hides); the customiser's last changes are saved as it goes.
        if self.settings_win.is_some() {
            self.deferred.push(Deferred::Settings(settingswin::Job::Close));
        }
        if self.skin_win.is_some() {
            self.deferred.push(Deferred::Skins(skinwin::Job::Close));
        }
        if self.sort_win.is_some() {
            self.deferred.push(Deferred::Sort(crate::sortwin::Job::Close));
        }
        // SAFETY: stops our own timers (fine if one isn't running).
        unsafe {
            for timer in [
                SAVE_TIMER,
                NOTE_TIMER,
                LOADING_TIMER,
                RETRY_TIMER,
                SHELL_MENU_TIMER,
                SLOW_CLICK_TIMER,
                BAR_REPEAT_TIMER,
                crate::sidebar::BAR_REPEAT_TIMER,
                drag::DRAG_SCROLL_TIMER,
                drag::TAB_SWITCH_TIMER,
            ] {
                let _ = KillTimer(Some(self.hwnd), timer);
            }
        }
        self.save_pending = false;
        if self.saving {
            self.save_soon_done();
        }
        // No looking for updates while hidden; an open prompt goes unanswered and comes back a
        // little after the window does.
        self.updates.hidden(self.hwnd);
        if updatewin::is_open() {
            self.updates.ask_again_when_shown();
            updatewin::close();
        }
        ANIMATING.with(|a| a.set(false));
        // The preview's full-size picture and the thumbnails go too: memory goes back to Windows.
        if self.side.preview_open() {
            self.side.set_preview(false);
            self.layout();
        }
        thumbs::clear();
        // ...and the renderer's copies of them now, not at the next frame (none is drawn while
        // hidden, so a big preview picture would otherwise stay in memory the whole time).
        self.renderer.drop_unused_pictures();
        self.app.go_to_background();
        self.deferred.push(Deferred::Hide);
        log::line("closed to the background: still running (Ctrl+Q or the tray icon's Quit ends it)");
    }

    /// Back from the background (or shown for the first time after starting with Windows), with
    /// a folder from outside if one came.
    fn come_back(&mut self, open: Option<life::Target>) {
        life::left_background();
        self.app.back_from_background(open.as_ref(), self.ever_shown);
        self.deferred.push(Deferred::Front);
        // A check that came due while hidden runs a little later.
        self.updates.shown(self.hwnd);
    }

    // ------------------------------------------------------------ updates (update.rs, updatewin.rs)

    /// The Settings window shows what's new about updates (About).
    fn refresh_settings(&mut self) {
        if let Some(s) = self.settings_win.as_mut() {
            s.refresh();
        }
    }

    /// WM_APP_UPDATE: a check or an install is done, or the prompt was answered.
    fn update_message(&mut self, what: usize, detail: isize) {
        match what {
            update::CHECKED => {
                self.updates.checked(self.hwnd);
                self.refresh_settings();
                let asked = detail != 0;
                let Some((found, _)) = update::status().last else { return };
                if self.app.in_background() {
                    // A check that was under way when the window hid: nothing is timed while hidden,
                    // and a new version is asked about a little after the window is back.
                    self.updates.hidden(self.hwnd);
                    if matches!(found, update::Found::Newer(_)) {
                        self.updates.ask_again_when_shown();
                    }
                    return;
                }
                if update::should_prompt(&found, &self.app.settings, asked, update::now_seconds()) {
                    if let update::Found::Newer(release) = found {
                        if updatewin::open(self.hwnd, &self.app.theme, &release, &update::ours().to_string()) {
                            self.prompted = Some(release);
                        }
                    }
                } else if asked && self.settings_win.is_none() {
                    // Asked for, and the Settings window that asked is gone: the status bar says it.
                    let (said, _) = update::about(true, self.app.settings.check_for_updates);
                    self.app.say(said);
                    self.invalidate();
                }
            }
            update::ANSWERED => {
                let Some(release) = self.prompted.clone() else { return };
                if detail == update::ANSWER_UPDATE {
                    log::line(&format!("update: installing {} (asked in the prompt)", release.version));
                    if !update::start_install(shell::Owner::new(self.hwnd), release) {
                        updatewin::problem("Glint is busy looking for updates right now. Try again in a moment.");
                    }
                    self.refresh_settings();
                } else {
                    // (A closed required fix remembers nothing: no "later" that would also hold
                    // back a newer ordinary release.)
                    if !release.required {
                        updatewin::remember_answer(detail, &release, &mut self.app.settings);
                    }
                    self.prompted = None;
                    if release.required {
                        // Its install failed and the prompt was closed: Glint stays usable, the
                        // status bar says the fix is still needed, and it's asked again at the next start.
                        self.app.update_needed = Some(release.version.clone());
                        self.invalidate();
                    }
                    if detail == update::ANSWER_OFF {
                        // Saved like any choice; checking by hand in Settings > About still works.
                        log::line("update: automatic checks turned off (in the prompt)");
                        self.app.say("Automatic updates are off. Turn them on again in Settings > About.");
                        self.refresh_settings();
                        self.invalidate();
                    }
                }
            }
            update::INSTALLED => {
                self.refresh_settings();
                match update::take_installed() {
                    Some(Ok(exe)) => {
                        // The new Glint opens the tabs open now: written now, even when nothing
                        // changed since the start (with "Home folder" the file may still hold an
                        // older session's tabs, which closing alone wouldn't write over).
                        if self.saving {
                            let s = self.snapshot();
                            settings::save_and_wait(s.clone(), EXIT_SAVE_WAIT);
                            self.last_saved = s;
                        }
                        // main.rs starts the new exe once the window is gone, hidden if Glint went
                        // to the background meanwhile.
                        update::restart_after_quit(exe, self.app.in_background());
                        self.quit();
                    }
                    Some(Err(why)) => {
                        if updatewin::is_open() {
                            updatewin::problem(&why);
                        } else {
                            self.app.say(format!("Glint couldn't update itself: {why}"));
                            self.invalidate();
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// WM_APP_LIFE: requests from the door (another glint.exe, the tray icon, Win+E).
    fn life_requests(&mut self) {
        // Once one of them has brought the window back, the others open their folders as on screen
        // (`ever_shown` changes only once it's shown, after this).
        let mut came_back = false;
        for r in life::take_requests() {
            log::line(&format!("request from outside: {r:?} (hidden: {})", self.app.in_background()));
            match r {
                Request::Open(open) => {
                    // A zip or 7z file opened from outside may have just been set to open in Glint
                    // ("Open with > Always"): its icon follows "Zip icon" from now on.
                    if open.as_ref().is_some_and(|t| crate::archive::is_archive_name(&t.folder)) {
                        life::sync_zip_icon(self.app.settings.zip_icon);
                    }
                    if !came_back && (self.app.in_background() || !self.ever_shown) {
                        self.come_back(open);
                        came_back = true;
                    } else {
                        if let Some(t) = &open {
                            self.app.open_from_outside(t);
                        }
                        self.deferred.push(Deferred::Front);
                    }
                }
                Request::Quit => self.quit(),
                Request::QuitIfHidden if self.app.in_background() => self.quit(),
                Request::QuitIfHidden => {}
            }
        }
        self.invalidate(); // a tab may have opened
    }

    /// Switches to the dark or light version of the skin (if it isn't already). False when the
    /// skin has none.
    fn set_dark(&mut self, dark: bool) -> bool {
        let done = dark == self.app.dark || self.app.set_dark(dark);
        self.sync_theme();
        done
    }

    /// The skin changed (picked, switched to light or dark, its text edited, its file changed): the
    /// window and the Settings window follow it.
    fn sync_theme(&mut self) {
        if self.theme_generation != self.app.theme_generation {
            self.apply_theme();
            self.invalidate();
            // Under Wine a frame shows only once the next one is drawn (ListView::wine_nudge): the
            // new look must not wait for the mouse (a skin edited in another window).
            if self.list.wine_nudge(self.now()) {
                ANIMATING.with(|a| a.set(true));
            }
        }
    }

    // ------------------------------------------------------------ remembering

    /// Everything settings.json should hold right now: the app's settings and tabs, the column
    /// widths and where the window is.
    fn snapshot(&self) -> Settings {
        let mut s = self.app.snapshot();
        // Rounded, so a column dragged to 163.333 DIPs doesn't fill the file with digits.
        s.column_width_map = self.list.column_widths().into_iter().map(|(k, w)| (k, (w * 10.0).round() / 10.0)).collect();
        let r = self.normal_rect;
        if r.right > r.left && r.bottom > r.top {
            s.bounds = Some([r.left, r.top, r.right - r.left, r.bottom - r.top]);
            s.bounds_dpi = self.normal_dpi;
        }
        s.maximized = self.maximized;
        if !self.main {
            // An extra window remembers only choices (settings.rs, `set_extra_window`).
            s.forget_window_state();
        }
        s
    }

    /// After a message: if something worth remembering changed, save a moment later (SAVE_TIMER),
    /// so a burst of changes - dragging a column, clicking through folders - is one write.
    fn note_changes(&mut self) {
        // A skin picked or changed (here, in the Settings window or the customiser): follow it.
        self.sync_theme();
        // The Settings window shows a choice changed here (Ctrl+H, say).
        if let Some(s) = self.settings_win.as_mut() {
            s.watch(&self.app.settings);
        }
        if self.saving && !self.save_pending && self.snapshot() != self.last_saved {
            // SAFETY: a timer on our own window.
            unsafe {
                SetTimer(Some(self.hwnd), SAVE_TIMER, SAVE_DELAY_MS, None);
            }
            self.save_pending = true;
        }
    }

    /// SAVE_TIMER: write the settings on a background thread.
    fn save_soon_done(&mut self) {
        self.save_pending = false;
        let s = self.snapshot();
        if s != self.last_saved && settings::save_in_background(s.clone()) {
            self.last_saved = s;
        }
    }

    /// The window is closing, or Windows is shutting down: write the settings now (waiting a little
    /// for the disk), since the process may end right after.
    fn save_before_exit(&mut self) {
        // SAFETY: stops our own timer (fine if it isn't running).
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SAVE_TIMER);
        }
        self.save_pending = false;
        // Skin changes the customiser hasn't saved yet go to the skin file worker now (main.rs
        // waits for it before the process ends).
        if let Some(s) = self.skin_win.as_mut() {
            s.save_now(&mut self.app);
        }
        self.app.skins.save_all();
        if !self.saving {
            return;
        }
        let s = self.snapshot();
        // (Also when the last save didn't reach the disk: something held the file.)
        if settings::needs_saving(&s, &self.last_saved) {
            settings::save_and_wait(s.clone(), EXIT_SAVE_WAIT);
            self.last_saved = s;
        }
    }

    /// The window moved or changed size: remember where it is when it's neither maximised nor
    /// minimised (an Aero-snapped window counts as its snapped place). A maximised window taken to
    /// another screen (Win+Shift+Arrow) takes the place Windows would put it back to on that
    /// screen, so the next start maximises it there.
    fn remember_normal_rect(&mut self) {
        let mut r = RECT::default();
        // SAFETY: plain queries on our own window and its screen, with valid out-structs.
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                return;
            }
            if !IsZoomed(self.hwnd).as_bool() {
                if GetWindowRect(self.hwnd, &mut r).is_ok() && r.right > r.left && r.bottom > r.top {
                    self.normal_rect = r;
                    self.normal_dpi = self.dpi;
                }
                return;
            }
            let here = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            if MonitorFromRect(&self.normal_rect, MONITOR_DEFAULTTONEAREST) == here {
                return;
            }
            let mut place = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if GetWindowPlacement(self.hwnd, &mut place).is_err() || !GetMonitorInfoW(here, &mut info).as_bool() {
                return;
            }
            // Windows keeps that place in "workspace" pixels, which start at the corner of the
            // screen's work area (past a taskbar at the top or on the left).
            let (dx, dy) = (info.rcWork.left - info.rcMonitor.left, info.rcWork.top - info.rcMonitor.top);
            let n = place.rcNormalPosition;
            r = RECT { left: n.left + dx, top: n.top + dy, right: n.right + dx, bottom: n.bottom + dy };
            if r.right > r.left && r.bottom > r.top && MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST) == here {
                self.normal_rect = r;
                // Its size is what un-maximising gives there: at that screen's scale.
                let (mut dpi, mut dpi_y) = (0u32, 0u32);
                self.normal_dpi = if GetDpiForMonitor(here, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_ok() && dpi >= 96 { dpi } else { self.dpi };
            }
        }
    }

    fn menu_picked(&mut self, part: Part, tag: u32, item: u32) {
        let ctx = self.ctx();
        let effect = self.with_part(part, |c, app| c.menu_pick(&ctx, tag, item, app));
        self.apply(part, effect);
        // A pick that asked a question itself (the Recycle Bin's Delete permanently, say).
        self.keyboard_to_question();
        self.invalidate();
    }

    // ------------------------------------------------------------ painting

    fn paint(&mut self) {
        let frame_start = Instant::now();
        // The first frame takes much longer than the others: where its time goes, for the timing log.
        let mut marks = log::Marks::new(self.first_frame && log::timing_on());
        let ctx = self.ctx();
        // Take in finished folder reads first, so a frame never shows stale data needlessly.
        if self.app.receive() {
            self.drag_listing_changed(); // rows may have moved under something being dragged
        }
        marks.mark("receive");
        self.sync_theme();
        self.app.icons.begin_frame();
        // A new generation for the thumbnails: what isn't asked for again in this frame or the next
        // isn't on screen any more (thumbs.rs).
        self.follow_split();
        thumbs::begin_frame();
        let mut more = false;
        for part in self.parts_on() {
            more |= self.with_part(part, |c, app| c.tick(ctx.now, app));
        }
        if self.palette.is_open() && (self.app.question.is_some() || self.app.form.is_some()) {
            self.palette.close(self.hwnd);
        }
        marks.mark("theme and ticks");
        let shown = self.renderer.begin() && {
            marks.mark("begin (render target)");
            let Window { renderer, app, titlebar, toolbar, sidebar, list, list2, split_strips, split_area, status, side, ask, form, queue, palette, hwnd, size: client, maximized, .. } = self;
            // (7's panes are parted by thin pale lines: all that shows of the window there.)
            let seven = app.theme.era == crate::theme::Era::Win7;
            renderer.clear(if seven { crate::theme::Color::hex(0xd5dfe5) } else { app.theme.window });
            if app.theme.window_paint.gradient().is_some() && !seven {
                // A shaded background (in the gaps between the panels).
                renderer.fill_paint(Rect::new(0.0, 0.0, client.0, client.1), 0.0, &app.theme.window_paint);
            }
            marks.mark("clear");
            titlebar.paint(renderer, &app.theme, app, &ctx);
            marks.mark("title bar");
            toolbar.paint(renderer, &app.theme, app, &ctx);
            titlebar.join_toolbar(renderer, &app.theme, app);
            marks.mark("toolbar");
            if app.settings.show_sidebar {
                sidebar.paint(renderer, &app.theme, app, &ctx);
            }
            marks.mark("sidebar");
            list.paint(renderer, &app.theme, app, &ctx);
            marks.mark("list");
            if let Some(other) = app.split_index() {
                // Split view: the other half shows its own tab; a line in the skin's colour across
                // the top of the half Glint works in.
                let keep = std::mem::replace(&mut app.active, other);
                list2.paint(renderer, &app.theme, app, &ctx);
                app.active = keep;
                // Each half's folder name over it; the half Glint works in has the skin's colour.
                for (strip, i, mine) in [(split_strips[0], app.active, true), (split_strips[1], other, false)] {
                    let t = &app.theme;
                    renderer.fill_rect(strip, t.panel);
                    renderer.fill_rect(Rect::new(strip.x, strip.bottom() - 2.0, strip.w, 2.0), if mine { t.accent } else { t.line });
                    let style = if mine { TextStyle::new(t.text_strong).bold() } else { TextStyle::new(t.muted) };
                    let name = app.display_name(&app.tabs[i].path);
                    renderer.text(&name, Rect::new(strip.x + 8.0, strip.y, (strip.w - 16.0).max(0.0), strip.h - 2.0), &style);
                }
            }
            side.paint(renderer, &app.theme, app, &ctx);
            // The copy queue's list: over the bottom right of the files (in split view, of both).
            let files = match app.split_index() {
                Some(_) => { let (a, b) = (list.rect(), list2.rect()); if a.x > b.x { a } else { b } }
                None => list.rect(),
            };
            queue.paint(renderer, &app.theme, files);
            if app.settings.show_status_bar {
                status.paint(renderer, &app.theme, app, &ctx);
            } else {
                // Without the status bar, its notes ("Path copied") show at the bottom of the list.
                crate::status::paint_note(renderer, &app.theme, app, list.rect());
            }
            let b = frame::classic_border(&app.theme, *maximized);
            if let Some(q) = &app.question {
                // Over everything below the toolbar (see ask.rs for why not the toolbar too).
                let top = b + size::TITLE_H + size::TOOLBAR_H;
                ask.paint(renderer, &app.theme, q, Rect::new(b, top, (client.0 - 2.0 * b).max(0.0), (client.1 - top - b).max(0.0)));
            } else if let Some(f) = &app.form {
                let top = b + size::TITLE_H + size::TOOLBAR_H;
                let preview = app.form_preview(&form.values());
                let area = Rect::new(b, top, (client.0 - 2.0 * b).max(0.0), (client.1 - top - b).max(0.0));
                form.paint(renderer, &app.theme, *hwnd, ctx.scale, f, &preview, area);
            } else {
                // The command box: at the top of the files (both halves in split view).
                palette.paint(renderer, &app.theme, ctx.scale, *split_area);
            }
            if b > 0.0 {
                frame::paint_classic_border(renderer, &app.theme, Rect::new(0.0, 0.0, client.0, client.1));
            }
            marks.mark("the rest");
            let shown = renderer.end() == FrameResult::Shown;
            marks.mark("end (EndDraw, present)");
            shown
        };
        if shown {
            self.failed_frames = 0;
        } else {
            // The device was lost (the render target is made again next frame) or none could be made.
            // Try again at once a few times, then only now and then, so a long driver reset doesn't
            // keep a CPU core busy.
            self.failed_frames += 1;
            if self.failed_frames == 1 {
                log::line("a frame couldn't be drawn (graphics device lost or busy); trying again");
            }
            if self.failed_frames <= FAST_RETRIES {
                more = true;
            } else {
                // SAFETY: a timer on our own window; setting it again just moves it.
                unsafe {
                    SetTimer(Some(self.hwnd), RETRY_TIMER, RETRY_MS, None);
                }
            }
        }
        if self.frame_log {
            log::line(&format!("frame: {:.1} ms (drawing and handing it to the screen)", frame_start.elapsed().as_secs_f64() * 1000.0));
        }
        ANIMATING.with(|a| a.set(more));
        if let Some(left) = self.app.note_time_left() {
            // Paint once more when the note runs out (a one-off timer: no CPU while waiting).
            // SAFETY: a timer on our own window; setting it again just moves it.
            unsafe {
                SetTimer(Some(self.hwnd), NOTE_TIMER, left.as_millis() as u32 + 30, None);
            }
        }
        if let Some(left) = self.app.tab().waiting_long_in().filter(|d| !d.is_zero()) {
            // A folder on its way: paint again when the list should switch to "Loading…".
            // SAFETY: as above.
            unsafe {
                SetTimer(Some(self.hwnd), LOADING_TIMER, left.as_millis() as u32 + 10, None);
            }
        }
        if more {
            self.invalidate();
        }
        if self.first_frame && shown {
            self.first_frame = false;
            let ms = ms_since_process_start();
            // Started hidden with Windows: the time since the process began says nothing.
            if !self.started_hidden {
                self.app.startup_ms = ms;
            }
            let shown = self.app.tab().loading.is_none();
            log::line(&format!(
                "first frame on screen {:.1} ms after process start ({:.1} ms after main() began, this frame took {:.1} ms); files on it: {}; {}",
                ms.unwrap_or(-1.0),
                self.now() * 1000.0,
                (self.now() - ctx.now) * 1000.0,
                if shown { "yes" } else { "not yet" },
                crate::status::memory_report()
            ));
            log::line(&format!("first frame, ms per step: {}; graphics warm-up: {}", marks.summary(), crate::render::warm_up_report()));
            crate::render::warm_up_done();
            self.invalidate(); // once more, so the status bar shows the start-up time
        }
        if self.bench && self.app.tab().loading.is_none() && self.app.startup_ms.is_some() {
            self.bench = false;
            log::line(&format!(
                "bench: all shown {:.1} ms after process start; {}",
                ms_since_process_start().unwrap_or(-1.0),
                crate::status::memory_report()
            ));
            // SAFETY: posting to our own window.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(self.hwnd), WM_APP_BENCH_DONE, WPARAM(0), LPARAM(0));
            }
        }
    }

    // ------------------------------------------------------------ mouse

    fn client_point(&self, lparam: LPARAM, screen: bool) -> Point {
        let mut pt = point_from(lparam);
        if screen {
            // SAFETY: converts in place for our own window.
            unsafe {
                let _ = ScreenToClient(self.hwnd, &mut pt);
            }
        }
        let s = self.scale();
        Point::new(pt.x as f32 / s, pt.y as f32 / s)
    }

    fn track_leave(&mut self, nonclient: bool) {
        if (nonclient && self.tracking_nc) || (!nonclient && self.tracking) {
            return;
        }
        let mut tme = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: if nonclient { TME_LEAVE | TME_NONCLIENT } else { TME_LEAVE },
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        // SAFETY: valid struct for our own window.
        if unsafe { TrackMouseEvent(&mut tme) }.is_ok() {
            if nonclient {
                self.tracking_nc = true;
            } else {
                self.tracking = true;
            }
        }
    }

    /// While a question is open: whether it has been drawn yet, so input may answer it (ask.rs).
    fn question_shown(&self) -> bool {
        self.app.question.as_ref().is_some_and(|q| self.ask.shown(q.id))
    }

    fn mouse_move(&mut self, p: Point) {
        self.last_mouse = p;
        if self.split_drag {
            let a = self.split_area;
            if a.w > 0.0 {
                self.app.settings.split_ratio = ((p.x - a.x) / a.w).clamp(0.2, 0.8);
                self.layout();
                self.invalidate();
            }
            return;
        }
        if self.app.question.is_some() && self.capture.is_none() {
            if !self.question_shown() {
                return;
            }
            let a = self.ask.mouse_move(p);
            self.answered(a);
            return;
        }
        if self.app.form.is_some() && self.capture.is_none() {
            if self.form_shown() {
                let a = self.form.mouse_move(p);
                self.form_answered(a);
            }
            return;
        }
        if self.palette.is_open() && self.capture.is_none() {
            let c = self.palette.mouse_move(p);
            self.palette_chose(c);
            return;
        }
        if self.queue.mouse_move(p) {
            self.invalidate();
        }
        if self.capture.is_none() && self.queue.contains(p) {
            return;
        }
        let e = MouseEvent { pos: p, button: MouseButton::Left, mods: mods(), wheel: 0.0 };
        let ctx = self.ctx();
        let target = self.capture.or_else(|| self.part_at(p));
        if self.capture.is_none() && target != self.hover {
            if let Some(old) = self.hover {
                let effect = self.with_part(old, |c, app| c.mouse_leave(&ctx, app));
                self.apply(old, effect);
            }
            self.hover = target;
        }
        if let Some(part) = target {
            let effect = self.with_part(part, |c, app| c.mouse_move(&ctx, &e, app));
            self.apply(part, effect);
        }
    }

    fn mouse_leave(&mut self) {
        if self.capture.is_some() {
            return;
        }
        if let Some(old) = self.hover.take() {
            let ctx = self.ctx();
            let effect = self.with_part(old, |c, app| c.mouse_leave(&ctx, app));
            self.apply(old, effect);
        }
    }

    fn mouse_down(&mut self, p: Point, button: MouseButton, double: bool) {
        self.last_mouse = p;
        if self.app.question.is_some() {
            // The question box takes every click; one beside it answers "no".
            let a = if button == MouseButton::Left && self.question_shown() { self.ask.mouse_down(p) } else { Answer::Nothing };
            self.answered(a);
            return;
        }
        if self.app.form.is_some() {
            let a = if button == MouseButton::Left && self.form_shown() { self.form.mouse_down(p) } else { Answer::Nothing };
            self.form_answered(a);
            return;
        }
        if self.palette.is_open() {
            // A click on a row runs it; one outside closes the box (and does nothing else).
            let c = self.palette.mouse_down(p, button == MouseButton::Left);
            self.palette_chose(c);
            return;
        }
        if self.queue.contains(p) {
            if button == MouseButton::Left {
                self.queue.click(p);
                self.invalidate();
            }
            return;
        }
        if self.app.split_index().is_some() && button == MouseButton::Left {
            if self.split_divider.contains(p) {
                // The line between the halves: drag it to share the room; a double-click evens them.
                if double {
                    self.app.settings.split_ratio = 0.5;
                    self.layout();
                    self.invalidate();
                } else {
                    self.split_drag = true;
                    // SAFETY: capturing the mouse for our own window.
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                return;
            }
            if self.split_strips[1].contains(p) {
                self.app.run(Command::OtherPane);
                self.follow_split();
                self.invalidate();
                return;
            }
        }
        let Some(mut part) = self.part_at(p) else { return };
        self.drop_pending_menu();
        if part == Part::List2 {
            // A click in split view's other half: Glint works there now (and the click goes on).
            self.app.run(Command::OtherPane);
            self.follow_split();
            self.hover = None;
            part = Part::List;
        }
        // Clicking our own drawing takes the keyboard back from a text box.
        // SAFETY: focus queries/changes on our own windows.
        unsafe {
            if GetFocus() != self.hwnd {
                let _ = SetFocus(Some(self.hwnd));
            }
            SetCapture(self.hwnd);
        }
        self.capture = Some(part);
        let e = MouseEvent { pos: p, button, mods: mods(), wheel: 0.0 };
        let ctx = self.ctx();
        let effect = self.with_part(part, |c, app| c.mouse_down(&ctx, &e, app));
        self.apply(part, effect);
        if double {
            let effect = self.with_part(part, |c, app| c.double_click(&ctx, &e, app));
            self.apply(part, effect);
        }
    }

    fn mouse_up(&mut self, p: Point, button: MouseButton) {
        self.last_mouse = p;
        if self.split_drag {
            self.split_drag = false;
            // SAFETY: releasing our own capture.
            unsafe {
                let _ = ReleaseCapture();
            }
            return;
        }
        if self.app.question.is_some() && self.capture.is_none() {
            let a = if button == MouseButton::Left && self.question_shown() { self.ask.mouse_up(p) } else { Answer::Nothing };
            self.answered(a);
            return;
        }
        if self.app.form.is_some() && self.capture.is_none() {
            let a = if button == MouseButton::Left && self.form_shown() { self.form.mouse_up(p) } else { Answer::Nothing };
            self.form_answered(a);
            return;
        }
        if self.palette.is_open() && self.capture.is_none() {
            return;
        }
        let target = self.capture.take().or_else(|| self.part_at(p));
        // SAFETY: releasing our own capture.
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(part) = target {
            let e = MouseEvent { pos: p, button, mods: mods(), wheel: 0.0 };
            let ctx = self.ctx();
            let effect = self.with_part(part, |c, app| c.mouse_up(&ctx, &e, app));
            self.apply(part, effect);
        }
        // The pointer may now be over another part.
        let now_over = self.part_at(p);
        if now_over != self.hover {
            self.mouse_move(p);
        }
    }

    fn wheel(&mut self, p: Point, notches: f32) {
        // The command box's guide scrolls; nothing else does while the box is open.
        if self.palette.is_open() {
            let c = self.palette.wheel(p, notches);
            return self.palette_chose(c);
        }
        if self.app.question.is_some() || self.app.form.is_some() {
            return;
        }
        let Some(part) = self.part_at(p) else { return };
        let e = MouseEvent { pos: p, button: MouseButton::Middle, mods: mods(), wheel: notches };
        let ctx = self.ctx();
        let effect = self.with_part(part, |c, app| c.wheel(&ctx, &e, app));
        self.apply(part, effect);
    }

    fn cursor(&self, p: Point) -> CursorKind {
        if self.split_drag || (self.capture.is_none() && self.app.split_index().is_some() && self.split_divider.contains(p)) {
            return CursorKind::SizeWE;
        }
        if self.capture.is_none() && self.palette.contains(p) {
            return CursorKind::Arrow;
        }
        let part = self.capture.or_else(|| self.part_at(p));
        match part {
            Some(Part::Title) => self.titlebar.cursor(p, &self.app),
            Some(Part::Toolbar) => self.toolbar.cursor(p, &self.app),
            Some(Part::Sidebar) => self.sidebar.cursor(p, &self.app),
            Some(Part::List) => self.list.cursor(p, &self.app),
            Some(Part::List2) => CursorKind::Arrow,
            Some(Part::Status) => self.status.cursor(p, &self.app),
            Some(Part::Side) => self.side.cursor(p, &self.app),
            None => CursorKind::Arrow,
        }
    }

    // ------------------------------------------------------------ keyboard

    /// Window-wide shortcuts first; everything else goes to the file list. `repeat`: the key is
    /// held down and this is Windows repeating it.
    fn key_down(&mut self, vk: u16, m: Mods, repeat: bool) -> bool {
        if self.app.question.is_some() {
            if m.alt {
                return false; // Alt+F4 and friends still reach Windows
            }
            if repeat || !self.question_shown() {
                // A key still held from before the question opened (Enter held a moment too long in
                // the name box that asked it, say), or one pressed before it was even drawn, must
                // not answer it.
                return true;
            }
            let a = self.ask.key(vk);
            self.answered(a);
            return true;
        }
        if self.app.form.is_some() {
            if m.alt {
                return false;
            }
            if !repeat && self.form_shown() {
                let a = self.form.key(vk);
                self.form_answered(a);
            }
            return true;
        }
        if self.palette.is_open() {
            if m.alt {
                return false;
            }
            let c = self.palette.key(vk, m);
            self.palette_chose(c);
            return true;
        }
        let key = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| vk == k.0;
        let letter = |c: char| vk == c as u16;
        // Shift, Ctrl or Alt on its own does nothing, so it leaves a right-click menu that waits for
        // Windows' items alone (Shift held for Shift+right-click keeps coming, repeated by Windows).
        if !(key(VK_SHIFT) || key(VK_CONTROL) || key(VK_MENU)) {
            self.drop_pending_menu();
        }
        // AltGr (the right Alt on German, Polish... keyboards) comes as Ctrl+Alt: AltGr+Q is @, not
        // Ctrl+Q. So the Ctrl shortcuts want Ctrl without Alt, and the Alt+letter ones Alt without Ctrl.
        let ctrl = m.ctrl && !m.alt;
        if ctrl && !m.shift && letter('U') {
            // The command box (palette.rs).
            self.palette.open(self.hwnd, &self.app);
            self.invalidate();
            return true;
        }
        let cmd = if ctrl && m.shift && letter('L') {
            Some(Command::ToggleTheme)
        } else if ctrl && m.shift && letter('T') {
            Some(Command::ReopenClosedTab)
        } else if ctrl && !m.shift && letter('H') {
            Some(Command::ToggleHidden)
        } else if ctrl && !m.shift && letter('D') {
            // Pin the folder on screen to the sidebar, or unpin it.
            Some(Command::TogglePin(self.app.tab().path.clone()))
        } else if ctrl && letter('T') {
            Some(Command::NewTab)
        } else if ctrl && letter('W') {
            Some(Command::CloseTab(self.app.active))
        } else if ctrl && !m.shift && letter('N') {
            Some(Command::NewWindow) // (Ctrl+Shift+N is New folder, in the list)
        } else if ctrl && !m.shift && letter('Q') {
            Some(Command::Quit)
        } else if ctrl && key(VK_TAB) {
            Some(if m.shift { Command::PrevTab } else { Command::NextTab })
        } else if let Some(view) = (ctrl && m.shift && (b'1'..=b'6').contains(&(vk as u8)) && vk < 0x100).then(|| ViewMode::for_key((vk as u8 - b'0') as u32)).flatten() {
            // Ctrl+Shift+1 to 6: the views, with Explorer's numbers (extra large icons ... details).
            Some(Command::SetView(view))
        } else if ctrl && (b'1'..=b'9').contains(&(vk as u8)) && vk < 0x100 {
            Some(Command::SwitchTab((vk as u8 - b'1') as usize))
        } else if (ctrl && letter('L')) || (m.alt && !m.ctrl && letter('D')) || key(VK_F4) && !m.alt {
            Some(Command::EditPath)
        } else if ctrl && is_ctrl_backslash(vk, m.shift) {
            Some(Command::ToggleSplit)
        } else if key(VK_F6) && !m.ctrl && !m.alt && self.app.split.is_some() {
            Some(Command::OtherPane)
        } else if ctrl && m.shift && letter('F') {
            // The filter searches subfolders too (or not any more).
            Some(Command::ToggleSubfolders)
        } else if (ctrl && letter('F')) || (key(VK_F3) && !m.ctrl && !m.alt) {
            Some(Command::FocusFilter)
        } else if ctrl && !m.shift && letter('B') {
            Some(Command::ToggleSidebar)
        } else if key(VK_F5) || (ctrl && letter('R')) {
            Some(Command::Refresh)
        } else if key(VK_BACK) && !m.alt && !m.ctrl {
            // Up, or back like a web browser ("Backspace" setting).
            Some(if self.app.settings.backspace == Backspace::Back { Command::Back } else { Command::Up })
        } else if m.alt && key(VK_UP) {
            Some(Command::Up)
        } else if m.alt && key(VK_LEFT) {
            Some(Command::Back)
        } else if m.alt && key(VK_RIGHT) {
            Some(Command::Forward)
        } else if m.alt && key(VK_RETURN) {
            Some(Command::Properties)
        } else if ctrl && !m.shift && key(VK_OEM_COMMA) {
            Some(Command::OpenSettings(None))
        } else if ctrl && key(VK_OEM_3) {
            // Ctrl+` (the key left of 1): a terminal in the folder on screen.
            Some(Command::OpenTerminal(self.app.tab().path.clone()))
        } else if m.alt && !m.ctrl && !m.shift && letter('P') {
            // The details panel on the right of the list (side.rs).
            Some(Command::ToggleDetails)
        } else if key(VK_SPACE) && !m.ctrl && !m.alt && !m.shift && !self.list.typing(self.now()) {
            // The Space preview, unless Space is part of a name being typed to jump to it.
            Some(Command::TogglePreview)
        } else {
            None
        };
        if let Some(cmd) = cmd {
            self.apply(Part::List, Effect::Command(cmd));
            return true;
        }
        // While the preview is open, Esc closes it and Left / Right step through the files; the
        // other keys (Up, Down...) go on to the list, and the preview follows its selection.
        let effect = self.side.preview_key(vk, m, &mut self.app);
        if !matches!(effect, Effect::None) {
            self.apply(Part::Side, effect);
            return true;
        }
        if m.alt {
            return false; // leave Alt+F4, Alt+Space etc. to Windows
        }
        let ctx = self.ctx();
        let effect = self.list.key_down(&ctx, vk, m, &mut self.app);
        let used = !matches!(effect, Effect::None);
        self.apply(Part::List, effect);
        used
    }

    // ------------------------------------------------------------ messages

    /// Handles one message. None = let Windows do its default.
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                // Validate first: anything invalidated while painting (animation, icons still
                // coming) then asks for one more frame instead of being swallowed.
                // SAFETY: marks our own window as painted.
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                self.paint();
                Some(LRESULT(0))
            }
            // Minimised: nothing to show, so keep everything as it is (shrinking the drawing
            // surface to nothing and back again is wasted work, and confuses Wine).
            WM_SIZE if wparam.0 == SIZE_MINIMIZED as usize => Some(LRESULT(0)),
            WM_SIZE => {
                // (Minimised keeps `maximized` as it was: that's how the window comes back.)
                self.maximized = wparam.0 == SIZE_MAXIMIZED as usize;
                self.remember_normal_rect();
                if let Some(s) = self.settings_win.as_mut() {
                    s.follow_owner();
                }
                if let Some(s) = self.skin_win.as_mut() {
                    s.follow_owner();
                }
                self.follow_sort();
                let (w, h) = (lo_word(lparam.0 as usize), hi_word(lparam.0 as usize));
                self.renderer.resize(w, h);
                if w > 0 && h > 0 {
                    let s = self.scale();
                    self.size = (w as f32 / s, h as f32 / s);
                    self.layout();
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = lo_word(wparam.0).max(96);
                self.renderer.set_dpi(self.dpi);
                // Lay out for the new scale now: the same pixels are fewer DIPs. When the window
                // keeps its pixel size (maximised, say) no WM_SIZE follows to do it.
                let mut rc = RECT::default();
                // SAFETY: our own window and a valid out-struct.
                if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left && rc.bottom > rc.top {
                    let s = self.scale();
                    self.size = ((rc.right - rc.left) as f32 / s, (rc.bottom - rc.top) as f32 / s);
                    self.layout();
                }
                self.invalidate();
                // SAFETY: lparam points to the suggested window rectangle.
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                self.deferred.push(Deferred::Place(suggested));
                Some(LRESULT(0))
            }
            WM_NCHITTEST => {
                if let Some(edge) = frame::resize_edge(self.hwnd, wparam, lparam) {
                    return Some(LRESULT(edge as isize));
                }
                let p = self.client_point(lparam, true);
                if !self.titlebar.rect().contains(p) {
                    return Some(LRESULT(HTCLIENT as isize));
                }
                let code = match self.titlebar.hit_test(p, &self.app) {
                    Hit::Client => HTCLIENT,
                    Hit::Caption => HTCAPTION,
                    Hit::Minimize => HTMINBUTTON,
                    Hit::Maximize => HTMAXBUTTON,
                    Hit::Close => HTCLOSE,
                };
                Some(LRESULT(code as isize))
            }
            WM_SETCURSOR => {
                if lo_word(lparam.0 as usize) != HTCLIENT {
                    return None;
                }
                // Where the pointer is now (WM_SETCURSOR comes before the WM_MOUSEMOVE that would
                // tell us, so `last_mouse` can be one move behind).
                let mut pt = POINT::default();
                // SAFETY: a valid out-struct.
                let p = if unsafe { GetCursorPos(&mut pt) }.is_ok() { self.client_point(LPARAM(pack(pt)), true) } else { self.last_mouse };
                let id = match self.cursor(p) {
                    CursorKind::Arrow => IDC_ARROW,
                    CursorKind::Hand => IDC_HAND,
                    CursorKind::IBeam => IDC_IBEAM,
                    CursorKind::SizeWE => IDC_SIZEWE,
                };
                // SAFETY: loads a shared system cursor.
                unsafe {
                    SetCursor(LoadCursorW(None, id).ok());
                }
                Some(LRESULT(1))
            }
            WM_MOUSEMOVE => {
                self.track_leave(false);
                let p = self.client_point(lparam, false);
                self.mouse_move(p);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.mouse_leave();
                Some(LRESULT(0))
            }
            WM_NCMOUSEMOVE => {
                self.track_leave(true);
                let p = self.client_point(lparam, true);
                self.mouse_move(p);
                // Our own caption buttons: keep Windows from drawing its old ones.
                matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE).then_some(LRESULT(0))
            }
            WM_NCMOUSELEAVE => {
                self.tracking_nc = false;
                self.mouse_leave();
                Some(LRESULT(0))
            }
            WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
                let p = self.client_point(lparam, true);
                self.mouse_down(p, MouseButton::Left, false);
                Some(LRESULT(0))
            }
            WM_NCLBUTTONUP if matches!(wparam.0 as u32, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => {
                let p = self.client_point(lparam, true);
                self.mouse_up(p, MouseButton::Left);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK | WM_MBUTTONDBLCLK => {
                let p = self.client_point(lparam, false);
                let button = match msg {
                    WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => MouseButton::Left,
                    WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => MouseButton::Right,
                    _ => MouseButton::Middle,
                };
                // A double-click arrives as "down, then double_click" (see ui.rs).
                self.mouse_down(p, button, matches!(msg, WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK | WM_MBUTTONDBLCLK));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                let p = self.client_point(lparam, false);
                let button = match msg {
                    WM_LBUTTONUP => MouseButton::Left,
                    WM_RBUTTONUP => MouseButton::Right,
                    _ => MouseButton::Middle,
                };
                self.mouse_up(p, button);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                // Someone else took the mouse (a menu, Alt+Tab): finish any drag (the split view's
                // divider too).
                if self.capture.is_some() || self.split_drag {
                    let p = self.last_mouse;
                    self.mouse_up(p, MouseButton::Left);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let p = self.client_point(lparam, true);
                let delta = hi_word(wparam.0) as u16 as i16 as f32 / 120.0;
                self.wheel(p, delta);
                Some(LRESULT(0))
            }
            WM_XBUTTONUP => {
                // (Not while a question or form is open: like every other click, the box takes it,
                // and the folder it asks about stays on screen.)
                if self.app.question.is_none() && self.app.form.is_none() {
                    let cmd = if hi_word(wparam.0) == 1 { Command::Back } else { Command::Forward };
                    self.apply(Part::List, Effect::Command(cmd));
                }
                Some(LRESULT(1))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                // Bit 30 of lparam: the key was already down (Windows repeating a held key).
                let repeat = (lparam.0 >> 30) & 1 != 0;
                let used = self.key_down(wparam.0 as u16, mods(), repeat);
                used.then_some(LRESULT(0))
            }
            // Alt+letter: there is no menu bar to look the letter up in, so Windows would only beep
            // (Alt+D is ours, for the path). Alt+Space still opens the window menu.
            WM_SYSCHAR if wparam.0 != ' ' as usize => Some(LRESULT(0)),
            // Alt pressed and let go on its own would put the window in "menu mode" for a menu bar
            // it doesn't have, and the next arrow key would open the window menu instead of moving
            // in the list. (Alt+Space comes with the space in lparam and still works.)
            WM_SYSCOMMAND if (wparam.0 & 0xFFF0) == SC_KEYMENU as usize && lparam.0 == 0 => Some(LRESULT(0)),
            // While a question is open, typed letters don't jump around in the list behind it.
            WM_CHAR if self.app.question.is_some() || self.app.form.is_some() => Some(LRESULT(0)),
            // (While the command box is open, typing is for it, not for jumping to a name.)
            WM_CHAR if self.palette.is_open() => Some(LRESULT(0)),
            WM_CHAR => {
                if let Some(ch) = char::from_u32(wparam.0 as u32) {
                    // + and - zoom the Space preview's picture (instead of jumping to a name).
                    let effect = self.side.preview_char(ch);
                    if !matches!(effect, Effect::None) {
                        self.apply(Part::Side, effect);
                        return Some(LRESULT(0));
                    }
                    let ctx = self.ctx();
                    let effect = self.list.char_input(&ctx, ch, &mut self.app);
                    self.apply(Part::List, effect);
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                self.active = lo_word(wparam.0) != WA_INACTIVE;
                if self.active {
                    // Colour tags another Glint window changed meanwhile (tags.rs).
                    self.app.tags_refresh();
                }
                self.invalidate();
                // SAFETY: focus queries and changes on our own windows.
                unsafe {
                    if !self.active {
                        // Remember a text box that has the keyboard (the filter, say)...
                        let f = GetFocus();
                        self.focus_on_return = (!f.is_invalid() && IsChild(self.hwnd, f).as_bool()).then_some(f);
                        return None;
                    }
                    // ...and give it back on the way in, instead of Windows' default (the window).
                    if let Some(f) = self.focus_on_return.take() {
                        if IsWindow(Some(f)).as_bool() && IsWindowVisible(f).as_bool() {
                            let _ = SetFocus(Some(f));
                            return Some(LRESULT(0));
                        }
                    }
                }
                None
            }
            WM_MOVE => {
                self.remember_normal_rect();
                // Settings stays in the middle of the main window; the sort editor beside it.
                if let Some(s) = self.settings_win.as_mut() {
                    s.follow_owner();
                }
                if let Some(s) = self.skin_win.as_mut() {
                    s.follow_owner();
                }
                self.follow_sort();
                None
            }
            WM_TIMER if wparam.0 == SAVE_TIMER => {
                // SAFETY: stops our own timer.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), SAVE_TIMER);
                }
                self.save_soon_done();
                Some(LRESULT(0))
            }
            // Windows' app mode (light/dark) or another setting changed: follow it if asked to.
            WM_SETTINGCHANGE => {
                if self.app.settings.theme == ThemeMode::System {
                    self.set_dark(settings::dark_for(ThemeMode::System));
                }
                None
            }
            // Windows is shutting down or signing out: the process may end without WM_DESTROY.
            WM_ENDSESSION if wparam.0 != 0 => {
                self.save_before_exit();
                Some(LRESULT(0))
            }
            WM_TIMER if matches!(wparam.0, NOTE_TIMER | LOADING_TIMER | RETRY_TIMER) => {
                // All three are one-off: paint again now.
                // SAFETY: stops our own timer.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), wparam.0);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            // A held scroll bar arrow or track repeats (an older Windows' bars).
            WM_TIMER if wparam.0 == BAR_REPEAT_TIMER => {
                let ctx = self.ctx();
                let e = self.list.repeat_due(&ctx, &mut self.app);
                self.apply(Part::List, e);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == crate::sidebar::BAR_REPEAT_TIMER => {
                let ctx = self.ctx();
                let e = self.sidebar.repeat_due(&ctx);
                self.apply(Part::Sidebar, e);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == SLOW_CLICK_TIMER => {
                // A slow second click on a selected name: renaming starts (list.rs).
                // SAFETY: stops our own one-off timer.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), wparam.0);
                }
                let ctx = self.ctx();
                let e = self.list.slow_click_due(&ctx, &mut self.app);
                self.apply(Part::List, e);
                Some(LRESULT(0))
            }
            WM_CLIPBOARDUPDATE => {
                if self.app.clipboard_changed() {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_APP_LOADED => {
                // The copy queue's list follows its copies (copyqueue.rs wakes the window).
                if self.queue.shown || crate::copyqueue::busy() {
                    self.invalidate();
                }
                if self.app.receive() {
                    if self.list.wine_nudge(self.now()) {
                        ANIMATING.with(|a| a.set(true));
                    }
                    self.invalidate();
                    // A tab may have followed its folder to a new name (watch.rs): worth remembering.
                    self.note_changes();
                    // Rows may have moved under something being dragged (window/drag.rs).
                    self.drag_listing_changed();
                }
                Some(LRESULT(0))
            }
            // The skins folder changed, was listed, or a skin file was written (skin.rs): the look
            // follows a skin whose text changed, and the windows that list skins show the news.
            WM_APP_SKINS => {
                self.app.skins_event(wparam.0);
                self.sync_theme();
                self.invalidate();
                if let Some(s) = self.settings_win.as_mut() {
                    s.watch(&self.app.settings);
                }
                if let Some(s) = self.skin_win.as_mut() {
                    s.skins_news(&mut self.app);
                }
                Some(LRESULT(0))
            }
            WM_APP_TEXT => {
                let ev = TextEvent { id: wparam.0 as u16, kind: TextEventKind::from_number(lparam.0) };
                self.text_event(ev);
                Some(LRESULT(0))
            }
            WM_APP_SHELL_MENU => {
                // Windows' items for a right-click menu are ready (or Windows has none).
                let session = wparam.0 as u64;
                let built = shellmenu::take(session).flatten();
                self.open_pending_menu(Some(session), built);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == SHELL_MENU_TIMER => {
                // Windows' items are late: the menu opens without them.
                self.open_pending_menu(None, None);
                Some(LRESULT(0))
            }
            WM_APP_NOTE => {
                // A background job (terminal, Open with, Properties) has something to say.
                for note in shell::take_notes() {
                    self.app.say(note);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_CONTEXTMENU => {
                // Right-click menus open on the button (the file list's when it comes up, so it can
                // start a right-button drag first) and on the Apps key / Shift+F10 (list.rs); the
                // WM_CONTEXTMENU Windows sends after those isn't wanted (under Wine it
                // even keeps the open menu from closing on Esc). Over the title bar it goes on to
                // Windows, which shows its window menu (Move, Size, Close) for the empty space there.
                let pt = point_from(lparam);
                let keyboard = pt.x == -1 && pt.y == -1;
                if !keyboard && self.titlebar.rect().contains(self.client_point(lparam, true)) {
                    return None;
                }
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                // EN_CHANGE (0x0300) from a text box, unless we changed the text ourselves.
                if hi_word(wparam.0) == 0x0300 && !textbox::setting_text() {
                    self.text_event(TextEvent { id: lo_word(wparam.0) as u16, kind: TextEventKind::Changed });
                }
                Some(LRESULT(0))
            }
            WM_DEVICECHANGE => {
                // A drive or card came or went (the drive list is read again), or a drive is about
                // to be ejected and the folder watch lets go of it (app.rs, watch.rs) - and so does
                // a video in the preview. TRUE: yes, Windows may go ahead.
                if wparam.0 as u32 == DBT_DEVICEQUERYREMOVE {
                    self.side.release_files();
                }
                self.app.device_change(wparam.0 as u32, lparam.0);
                Some(LRESULT(1))
            }
            WM_APP_DRAG => Some(self.drag_message(lparam)),
            WM_APP_DROP_DONE => {
                // A dropped file job is done: the program the drag came from hears it (dragdrop.rs).
                dragdrop::finish_async(wparam.0 as u64, lparam.0 != 0);
                Some(LRESULT(0))
            }
            WM_TIMER if matches!(wparam.0, drag::DRAG_SCROLL_TIMER | drag::TAB_SWITCH_TIMER) => {
                self.drag_timer(wparam.0);
                Some(LRESULT(0))
            }
            WM_APP_BENCH_DONE => {
                self.deferred.push(Deferred::Close);
                Some(LRESULT(0))
            }
            // A person closing the window (Alt+F4, the window menu, the taskbar's "Close window"):
            // the main window may only hide (background mode). A plain WM_CLOSE from another
            // program or Task Manager really closes.
            WM_SYSCOMMAND if (wparam.0 & 0xFFF0) == SC_CLOSE as usize => {
                self.close_asked(true);
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.close_asked(false);
                Some(LRESULT(0))
            }
            // Requests from the door: another glint.exe, the tray icon, Win+E (life.rs).
            WM_APP_LIFE => {
                self.life_requests();
                Some(LRESULT(0))
            }
            // Time to look for a new version (update.rs) - never while hidden in the background.
            WM_TIMER if wparam.0 == update::UPDATE_TIMER => {
                if self.app.in_background() {
                    // (Can't happen - hiding stops the timer - but never look while hidden.)
                    self.updates.hidden(self.hwnd);
                } else if self.updates.due_now(self.hwnd, &self.app.settings) && update::start_check(shell::Owner::new(self.hwnd), &self.app.settings, false) {
                    self.refresh_settings();
                }
                Some(LRESULT(0))
            }
            WM_APP_UPDATE => {
                self.update_message(wparam.0, lparam.0);
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                self.save_before_exit();
                dragdrop::unregister(self.hwnd);
                // SAFETY: undoes AddClipboardFormatListener in `create`, then ends the message loop.
                unsafe {
                    let _ = RemoveClipboardFormatListener(self.hwnd);
                    PostQuitMessage(0);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn text_event(&mut self, ev: TextEvent) {
        // The command box's (palette.rs).
        if let Some(c) = self.palette.text_event(&ev) {
            self.palette_chose(c);
            return;
        }
        // The form's own boxes (form.rs).
        if let Some(a) = self.form.text_event(&ev) {
            if self.app.form.is_some() {
                self.form_answered(a);
            }
            return;
        }
        let ctx = self.ctx();
        for part in self.parts_on() {
            let effect = self.with_part(part, |c, app| c.text_event(&ctx, &ev, app));
            self.apply(part, effect);
        }
    }

    // ------------------------------------------------------------ the Settings window

    /// One of the Settings window's messages (see `settings_message`).
    fn settings_handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let Window { settings_win, app, .. } = self;
        let s = settings_win.as_mut()?;
        let (result, asks) = s.handle(msg, wparam, lparam, app);
        self.settings_asks(asks);
        if msg == WM_NCDESTROY {
            // Its last message: the state goes with it (and its tab, if it had one).
            self.settings_win = None;
            self.invalidate();
        }
        result
    }

    /// What a job for the Settings window brought back (a drop-down's pick, a picked folder).
    fn settings_job_done(&mut self, done: settingswin::JobDone) {
        let Window { settings_win, app, .. } = self;
        let Some(s) = settings_win.as_mut() else { return };
        let asks = s.job_done(done, app);
        self.settings_asks(asks);
    }

    /// One of the skin customiser's messages (see `skin_message`).
    fn skin_handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let Window { skin_win, app, .. } = self;
        let s = skin_win.as_mut()?;
        let (result, asks) = s.handle(msg, wparam, lparam, app);
        self.skin_asks(asks);
        if msg == WM_NCDESTROY {
            // Its last message: what it hasn't saved yet goes to the file worker, and the state goes.
            if let Some(s) = self.skin_win.as_mut() {
                s.save_now(&mut self.app);
            }
            self.skin_win = None;
        }
        result
    }

    /// What a job for the customiser brought back (a drop-down's pick, a colour, a file).
    fn skin_job_done(&mut self, done: skinwin::JobDone) {
        let Window { skin_win, app, .. } = self;
        let Some(s) = skin_win.as_mut() else { return };
        let asks = s.job_done(done, app);
        self.skin_asks(asks);
    }

    /// Carries out what the customiser asked for.
    fn skin_asks(&mut self, asks: Vec<skinwin::Ask>) {
        for ask in asks {
            match ask {
                skinwin::Ask::Command(cmd) => {
                    self.run_command(cmd);
                    self.invalidate();
                }
                skinwin::Ask::Job(job) => self.deferred.push(Deferred::Skins(job)),
                skinwin::Ask::RedrawMain => self.invalidate(),
            }
        }
        // A skin changed there shows here at once.
        self.sync_theme();
    }

    /// Carries out what the Settings window asked for.
    fn settings_asks(&mut self, asks: Vec<settingswin::Ask>) {
        for ask in asks {
            match ask {
                settingswin::Ask::Command(cmd) => {
                    self.run_command(cmd);
                    self.invalidate();
                }
                settingswin::Ask::Job(job) => self.deferred.push(Deferred::Settings(job)),
                settingswin::Ask::RedrawMain => self.invalidate(),
            }
        }
    }

    // ------------------------------------------------------------ right-click menus with Windows' items

    /// Opens a right-click menu for files: at once without Windows' items, or once the shell
    /// thread has built them (WM_APP_SHELL_MENU), or after MENU_WAIT_MS without them.
    fn open_files_menu(&mut self, part: Part, menu: FilesMenu) {
        self.drop_pending_menu();
        if let Some(req) = menu.windows.clone() {
            let wait = if req.full { WHOLE_MENU_WAIT_MS } else { MENU_WAIT_MS };
            if let Some(session) = shellmenu::request(self.hwnd, req) {
                // SAFETY: a one-off timer on our own window.
                unsafe {
                    SetTimer(Some(self.hwnd), SHELL_MENU_TIMER, wait, None);
                }
                self.pending_menu = Some(PendingMenu { part, menu, session });
                return;
            }
        }
        self.deferred.push(Deferred::FilesMenu(part, menu, None));
    }

    /// Forgets a right-click menu still waiting for Windows' items (a click or a key came first).
    /// Its Windows menu is let go when it arrives (WM_APP_SHELL_MENU then finds nothing waiting).
    fn drop_pending_menu(&mut self) {
        if self.pending_menu.take().is_some() {
            // SAFETY: stops our own timer.
            unsafe {
                let _ = KillTimer(Some(self.hwnd), SHELL_MENU_TIMER);
            }
        }
    }

    /// The waiting menu opens now: with Windows' items (`built`), or without them (late, or
    /// Windows has none). `session`: only if it's the one waiting for that session; None: whichever
    /// waits (the timer ran out).
    fn open_pending_menu(&mut self, session: Option<u64>, built: Option<Built>) {
        // (Windows' items for an older menu, one that was dropped, leave the one waiting now alone.)
        let Some(p) = self.pending_menu.take_if(|p| session.is_none_or(|s| s == p.session)) else {
            if let Some(b) = built {
                shellmenu::release(b.session);
            }
            return;
        };
        // SAFETY: stops our own timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SHELL_MENU_TIMER);
        }
        if built.is_none() && p.menu.windows.as_ref().is_some_and(|w| w.full) {
            self.app.say(if session.is_some() { "Windows has no menu of its own here" } else { "Windows' menu took too long; here is Glint's" });
            self.invalidate();
        }
        self.deferred.push(Deferred::FilesMenu(p.part, p.menu, built));
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // What Windows' own Copy put on the clipboard stays there after Glint ends.
        shellmenu::shutdown();
        let brush = EDIT_COLORS.with(|c| c.replace((0, 0, 0))).2;
        if brush != 0 {
            // SAFETY: the brush is ours and no control will paint any more.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(brush as *mut _));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1040 };

    #[test]
    fn ctrl_backslash_on_any_keyboard() {
        // US and UK: the \\ key itself.
        assert_eq!(backslash_key(0x00DC), Some((0xDC, false)));
        // German (AltGr+\u{df}): Ctrl+\u{df}, without the AltGr; Swiss (AltGr+<): Ctrl+<.
        assert_eq!(backslash_key(0x06DB), Some((0xDB, false)));
        assert_eq!(backslash_key(0x06E2), Some((0xE2, false)));
        // Shifted: Ctrl+Shift and that key.
        assert_eq!(backslash_key(0x01BF), Some((0xBF, true)));
        // French (AltGr+8) and a layout's own shift states: Ctrl+8 stays the eighth tab.
        assert_eq!(backslash_key(0x0638), None);
        assert_eq!(backslash_key(0x0651), None);
        assert_eq!(backslash_key(0x08DC), None);
        assert_eq!(backslash_key(-1), None);
        // The \\ key of US and UK keyboards always works, whatever the layout.
        assert!(is_ctrl_backslash(VK_OEM_5.0, false));
        assert!(!is_ctrl_backslash(b'S' as u16, true));
        assert!(!is_ctrl_backslash(b'8' as u16, false));
    }

    #[test]
    fn a_saved_place_that_fits_stays() {
        let r = fit_rect([100, 50, 1180, 760], 96, 96, WORK).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (100, 50, 1280, 810));
    }

    #[test]
    fn a_saved_place_is_moved_onto_a_smaller_screen() {
        // Saved on a big screen at the far right; now the screen is 1366 x 728.
        let small = RECT { left: 0, top: 0, right: 1366, bottom: 728 };
        let r = fit_rect([1500, 600, 1600, 900], 96, 96, small).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 1366, 728));
        // A screen left of the main one (negative coordinates) works the same way.
        let left = RECT { left: -1280, top: 0, right: 0, bottom: 984 };
        let r = fit_rect([-1300, -20, 800, 600], 96, 96, left).unwrap();
        assert_eq!((r.left, r.top, r.right - r.left, r.bottom - r.top), (-1280, 0, 800, 600));
    }

    #[test]
    fn the_size_follows_a_scaling_change() {
        // Saved at 100 %, the screen is at 150 % now: 1.5 times the pixels, the same size to the eye.
        let r = fit_rect([0, 0, 1000, 600], 96, 144, WORK).unwrap();
        assert_eq!((r.right - r.left, r.bottom - r.top), (1500, 900));
        // Never smaller than the smallest window.
        let r = fit_rect([0, 0, 10, 10], 0, 96, WORK).unwrap();
        assert_eq!((r.right - r.left, r.bottom - r.top), (size::MIN_W as i32, size::MIN_H as i32));
        assert!(fit_rect([0, 0, 800, 600], 96, 96, RECT::default()).is_none(), "no work area");
    }
}
