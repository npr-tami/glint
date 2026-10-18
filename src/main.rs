//! Glint: a fast, lightweight file explorer for Windows that draws everything itself, on the graphics card.
//!
//! This file starts things up and runs the message loop. The map of the code is in ARCHITECTURE.md;
//! in short: window.rs is the window, ui.rs the contract between the window and its parts,
//! render.rs the drawing, app.rs the state, folder.rs the disk, list.rs the file list.
//!
//! Usage: glint.exe [folder] [--light | --dark] [--bench]
//!   folder   open this folder (with "Tabs from last time", next to last time's tabs); a file opens
//!            its folder with the file selected. If Glint already runs, the folder opens there in a
//!            new tab instead (life.rs)
//!   --light  start in the skin's light version this time (--dark: its dark one), whatever the
//!            settings say
//!   --bench  close as soon as the first folder is on screen (start-up timing, written to the log in
//!            %TEMP%); settings are read but not written
//! How Glint runs (life.rs):
//!   --background          start hidden in the background (Start with Windows); nothing if Glint runs
//!   --quit                ask the running Glint to quit completely
//!   --new-window folder   an extra window (Ctrl+N)
//!   --set-default [--with-win-e], --restore-explorer
//!                         folders and drives (and Win+E) open in Glint / in File Explorer again
//! Updates (update.rs):
//!   --version             print the version ("Glint 0.8.0") and end; no window
//!   --after-update        started by the Glint that just updated itself: open last time's tabs
//!
//! Settings are in %APPDATA%\Glint\config\settings.json, or in Glint data\config\ next to
//! glint.exe when that folder exists (portable); see settings.rs.
//!
//! For measuring and testing (environment variables):
//!   GLINT_TIMING_LOG=1       write start-up and folder times to the log, %TEMP%\glint.log (off otherwise;
//!                            the *_LOG switches below turn it on too)
//!   GLINT_FRAME_LOG=1        write how long every frame took to the log
//!   GLINT_RENDER=software    draw with Windows' software renderer instead of the graphics card
//!   GLINT_WARM_UP=off        skip warming up the graphics driver, fonts and icons at start (render.rs), to compare
//!   GLINT_SETTINGS_DIR=dir   keep settings, cache and logs in this folder instead
//!   GLINT_TEST_CRASH=1       crash on purpose once the window is up, to see the crash report work
//!   GLINT_WATCH_LOG=1        log every change the folder watch reports, every re-read and drive message
//!   GLINT_SHELL_MENU_DELAY=ms make Windows' right-click items that much slower, to test waiting for them
//!   GLINT_TEST_REGISTRY=key  default explorer, Win+E and Start with Windows go under HKCU\key instead
//!   GLINT_WIN_E_HOOK=1       catch Win+E with the keyboard hook even where the hotkey would work
//!   GLINT_TEST_VERSION=0.2.9 take this for Glint's own version (to try the updater)
//!   GLINT_THUMB_LOG=1        log every thumbnail made (how, how big, how long it took)

#![windows_subsystem = "windows"]

mod anim;
mod archive;
mod app;
mod codebox;
mod complete;
mod ask;
mod details;
mod eraicons;
mod oldmenu;
mod dragdrop;
mod dupes;
mod fileops;
mod folder;
mod foldersize;
mod form;
mod frame;
mod icons;
mod life;
mod kit;
mod home;
mod list;
mod log;
// The chrome logo in the title bars (its pictures are baked in from res/logo).
mod logo;
mod media;
mod menu;
mod palette;
mod pathbar;
mod peek;
mod popup;
mod preview;
mod recent;
mod recycle;
mod retro;
mod retrotool;
mod retrolist;
mod retroctl;
mod registry;
mod render;
mod search;
mod settings;
mod settingswin;
mod shell;
mod shellmenu;
mod skin;
mod skincode;
mod skinwin;
mod sortrecipe;
mod sortwin;
mod side;
mod sidebar;
mod status;
mod tags;
mod textbox;
mod theme;
mod thumbs;
mod titlebar;
mod toolbar;
mod tray;
mod ui;
mod undo;
mod copyqueue;
mod queuebox;
mod update;
mod updatewin;
// Where the file list puts its items in each view (details, list, icons, tree).
mod video;
mod views;
mod watch;
mod window;
mod winekey;
mod winutil;

use std::time::{Duration, Instant};

use windows::Win32::Graphics::Gdi::{RedrawWindow, RDW_INVALIDATE, RDW_UPDATENOW};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX};
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, IsIconic, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT};
use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;

/// Shortest time between animation frames when the screen doesn't pace us (about 144 per second).
const MIN_FRAME: Duration = Duration::from_millis(7);

fn main() {
    let started = Instant::now();
    // `glint.exe --version`: the version, and nothing else (the updater asks a new Glint.exe this
    // before it puts it in place).
    if update::wants_version(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        update::print_version();
        return;
    }
    // Dropped last, when main ends: waits for copies still running, so closing Glint never cuts
    // one short (Windows' progress window stays on screen meanwhile).
    let _finish_file_jobs = fileops::FinishOnExit;
    // SAFETY: process-wide set-up calls with constant arguments, made once before any window exists.
    unsafe {
        // An empty DVD drive or card reader must give an error code, not a "no disk" dialog.
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
        // The manifest already asks for this; this covers being started in odd ways.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // The shell (icons, type names, known folders) needs COM on the UI thread.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES };
        let _ = InitCommonControlsEx(&icc);
    }

    // A crash from here on is written to the error log and shown, instead of Glint just vanishing.
    log::install_panic_hook();

    let mut start: Option<String> = None;
    let (mut dark, mut bench, mut after_update) = (None, false, false);
    let mut switches = life::Switches::default();
    // (args_os: `args` would end Glint over a name that isn't valid Unicode, which a folder's name
    // on Windows can be. Such a name can't be opened, but Glint starts and says so.)
    for arg in std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()) {
        match arg.as_str() {
            "--light" => dark = Some(false),
            "--dark" => dark = Some(true),
            "--bench" => bench = true,
            update::AFTER_UPDATE => after_update = true,
            _ if switches.take(&arg) => {}
            _ => start = Some(arg),
        }
    }
    if bench {
        log::ask_for_timing_log();
    }
    log::tidy_timing_log();
    // --set-default / --restore-explorer: registry only, no settings needed.
    if let Some(code) = life::registry_switch(&switches) {
        std::process::exit(code);
    }
    if switches.quit {
        life::quit_running();
        return;
    }
    // Control Panel, "shell:" places and the like: File Explorer shows them.
    if let Some(place) = start.as_deref().filter(|s| life::is_shell_location(s)) {
        life::open_in_explorer_now(place);
        return;
    }
    // A relative folder ("glint ." in a terminal) is taken from the folder Glint was started in;
    // without this the tab would hold "." and say it isn't a full path. A file
    // opens its folder with the file selected.
    let cwd = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
    let target = start.map(|s| life::resolve_target(&s, &cwd));
    // One main Glint: when one runs already (maybe hidden in the background), it gets the folder and
    // this one ends. An extra window (Ctrl+N) and --bench are windows of their own.
    let main = if bench || switches.new_window {
        false
    } else {
        match life::start(target.clone(), switches.background) {
            life::Start::Main => true,
            life::Start::HandedOver => return,
            life::Start::Extra => false,
        }
    };
    if !main {
        settings::set_extra_window();
    }
    let hidden = main && switches.background;
    // The first frame's one-time costs (the graphics driver, the fonts, the shell's icons) on
    // threads of their own, while the settings load and the window is made (render.rs).
    if !hidden {
        render::warm_up();
    }

    // Glint's files where they belong (a Portable switch moves them now), before anything reads them.
    let unsettled = if main { settings::settle() } else { Vec::new() };
    let loaded = settings::load();
    log::set_error_log(loaded.error_log);
    for problem in unsettled {
        log::error(&problem);
    }
    let tray_icon = loaded.tray_icon;
    let zip_icon = loaded.zip_icon;
    let dark = dark.unwrap_or_else(|| settings::dark_for(loaded.theme));
    // Just updated (update.rs): the same tabs as before, whatever "When Glint starts, open" says.
    let same_tabs = after_update.then(|| settings::Settings { startup: settings::Startup::Restore, ..loaded.clone() });
    let (tabs, active) = match &target {
        // An extra window (Ctrl+N) opens just its folder.
        Some(t) if !main => (vec![settings::SavedTab { path: t.folder.clone(), sort_column: loaded.sort_column, sort_descending: loaded.sort_descending, view: None, pinned: false }], 0),
        _ => settings::initial_tabs(same_tabs.as_ref().unwrap_or(&loaded), target.as_ref().map(|t| t.folder.as_str()), &loaded.home_place()),
    };
    log::line(&format!(
        "start: {:?} (tab {} of {}); settings {}; {}",
        tabs[active].path,
        active + 1,
        tabs.len(),
        settings::file_path().display(),
        status::memory_report()
    ));

    // Start reading the first folder now, while the window is being made.
    let reopen_split = (main && loaded.remember_split).then(|| loaded.split_folder.clone()).flatten();
    let mut app = app::AppState::new(loaded, &tabs, active, dark);
    if let Some(folder) = reopen_split {
        app.reopen_split(folder);
    }
    if let Some(name) = target.and_then(|t| t.select) {
        app.select_when_loaded(name);
    }
    if after_update {
        app.say(format!("Glint is updated to {}", update::ours()));
        log::line(&format!("started after updating to {}", update::ours()));
    }
    let hwnd = match window::create(app, window::Options { bench, started, main, hidden }) {
        Ok(h) => h,
        Err(e) => {
            log::line(&format!("could not create the window: {e}"));
            log::error(&format!("Glint couldn't create its window: {e}"));
            return;
        }
    };
    if main {
        // From now on other glint.exe's, the tray icon and Win+E reach this window (life.rs).
        life::open_door(hwnd);
        life::main_started(zip_icon);
        if hidden {
            life::entered_background(tray_icon);
        }
        // What an update left next to glint.exe (Glint.old.exe) goes (on a thread of its own) - but
        // not until the new version has been closed normally once: the Glint from before stays in
        // case the new one doesn't work out.
        update::remove_old_exes(after_update);
    }
    if std::env::var_os("GLINT_TEST_CRASH").is_some_and(|v| !v.is_empty()) {
        panic!("GLINT_TEST_CRASH is set: this is a test of the crash report, not a real problem");
    }

    // The message loop. Idle: wait for messages (0 % CPU). Animating: handle what's waiting, then
    // paint the next frame right away - presenting waits for the screen's refresh, so frames come
    // exactly as fast as the monitor shows them. Minimised, there is nothing to paint (Windows sends
    // no WM_PAINT then, so an animation could never finish): wait for messages; restoring the window
    // repaints it and picks the animation up again.
    // Hidden in the background, there is nothing to paint either.
    let mut msg = MSG::default();
    // SAFETY: plain queries on our own window.
    let can_paint = || unsafe { !IsIconic(hwnd).as_bool() && IsWindowVisible(hwnd).as_bool() };
    // SAFETY: the standard Win32 message loop on the thread that owns the window.
    unsafe {
        'running: loop {
            if window::animating() && can_paint() {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        break 'running;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if window::animating() && can_paint() {
                    let t0 = Instant::now();
                    let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW);
                    let spent = t0.elapsed();
                    if spent < MIN_FRAME {
                        std::thread::sleep(MIN_FRAME - spent);
                    }
                }
            } else {
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    break 'running;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
    // The window is gone: the tray icon and Win+E go, and the next glint.exe becomes the main one -
    // even while this process still waits for copies to finish (`_finish_file_jobs`).
    life::shutdown();
    // A normal quit: a version an update just put in place has proven itself (update.rs).
    if main {
        update::session_ended_cleanly();
    }
    // A skin changed just before quitting is still being written: let it finish (at most 3 s).
    skin::wait_for_writes(Duration::from_secs(3));
    // Glint just put a new version of itself in place: it starts now, as the main Glint.
    update::restart_if_installed();
}
