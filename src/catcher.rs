//! "Win+E after quitting" (Settings > General, under "Win+E opens Glint"): a tiny glint.exe that
//! only catches Win+E, so Win+E opens Glint even after Ctrl+Q.
//!
//! Why: newer Windows 11 (seen on 25H2, build 26200) no longer reads the registry key that "Win+E
//! opens Glint" sets (registry.rs): Win+E goes straight to File Explorer's own start place. The
//! running Glint still catches Win+E itself (winekey.rs), so this keeps a catcher running when
//! Glint doesn't: `glint.exe --win-e-catcher`, no window on screen, no tray icon, a few MB.
//!
//! - It starts with Windows (its own Run value, "Glint Win+E"), and the main Glint starts it too when
//!   it isn't running (at start, and when the setting is switched on). Ctrl+Q leaves it running.
//! - On Win+E: the main Glint (if one runs) is asked to open a window; otherwise glint.exe starts.
//! - Every half minute it checks the setting and Win+E are still on (switched off in any way, it
//!   ends) and that its glint.exe wasn't replaced by an update (then the new one takes over).
//! - Switching the setting (or Win+E) off ends it and removes its Run value.
//! - Glint's folder moved while it runs: it doesn't take the missing settings for "switched off"
//!   (it keeps its Run value and keeps running); once a Glint started from the new place has
//!   repaired the registry, that Glint ends it and starts its own catcher from the new place.
//!   (Nothing is held open in Glint's folder: that would stop the folder from being moved.)
//! - One per Windows session and set of files (`life::own_name`), like the main Glint.

use std::path::Path;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::SystemTime;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowExW, FindWindowW, GetMessageW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, KillTimer, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetTimer, TranslateMessage, ASFW_ANY, HWND_MESSAGE, MSG, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_HOTKEY, WM_TIMER,
    WNDCLASSEXW, WS_POPUP,
};

use crate::folder::wide;
use crate::life::{self, Request, WM_APP_WIN_E};
use crate::registry::{self, Places};
use crate::{log, winekey};

/// The switch that starts the catcher.
pub const SWITCH: &str = "--win-e-catcher";
/// Its mutex and window class (made this Glint's own with `life::own_name`).
const MUTEX_NAME: &str = "Local\\Glint.WinE";
const CLASS_NAME: &str = "GlintWinE";
/// How often it checks it's still wanted (ms).
const CHECK_EVERY: u32 = 30_000;
const CHECK_TIMER: usize = 1;
/// How often once glint.exe was moved (the new place's Glint takes over soon after it starts).
const CHECK_MOVED_EVERY: u32 = 2_000;

/// The catcher's window (in the catcher process).
static WINDOW: AtomicIsize = AtomicIsize::new(0);

fn class_name() -> Vec<u16> {
    wide(&life::own_name_here(CLASS_NAME))
}

/// The catcher's window, if one runs (in this session, for these files).
fn running_window() -> Option<HWND> {
    let class = class_name();
    // SAFETY: a plain lookup by class name; `class` is NUL-terminated and outlives the call.
    unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.ok().filter(|h| !h.is_invalid())
}

/// Whether the catcher should run: the setting is on and Win+E opens this Glint.
pub fn wanted(setting: bool) -> bool {
    setting && life::win_e_on()
}

/// Brings the catcher in step with `on` (the setting, with Win+E on): its Run value, and the process
/// itself - started if it should run and doesn't, told to end if it runs and shouldn't. Test runs
/// with their own settings folder leave the registry and processes alone.
pub fn sync(on: bool) {
    if life::testing_without_registry() {
        return;
    }
    let Some(exe) = registry::this_exe() else { return };
    let places = Places::current();
    if on {
        if let Err(e) = registry::set_catcher_run(&places, Some(&catcher_command(&exe))) {
            log::error(&e);
        }
        // Catchers of other copies of Glint (with files of their own, so another window class):
        // the ones out of date end now, not at their next check.
        close_other_stale(running_window());
    } else {
        remove_run_if_ours(&places, &exe);
    }
    match (on, running_window()) {
        (true, None) => start(&exe),
        // One that is out of date (its glint.exe was moved away, or Win+E now opens another
        // glint.exe - this one): it ends, and one from this glint.exe starts (it waits for the
        // old one to let go).
        (true, Some(w)) if out_of_date(w) => {
            // SAFETY: a post to a window handle (fails harmlessly if it's gone).
            unsafe {
                let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            start(&exe);
        }
        // SAFETY: a post to a window handle (fails harmlessly if it's gone).
        (false, Some(w)) => unsafe {
            let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        _ => {}
    }
}

/// The title every catcher window has (whatever its class), so any copy of Glint finds them all.
const TITLE: &str = "Glint Win+E";

/// Removes the Run value only if it starts this glint.exe's catcher.
fn remove_run_if_ours(places: &Places, exe: &str) {
    if registry::catcher_run(places).is_some_and(|c| c.eq_ignore_ascii_case(&catcher_command(exe))) {
        if let Err(e) = registry::set_catcher_run(places, None) {
            log::error(&e);
        }
    }
}

/// Tells every other catcher (but `own`) that is out of date to end.
fn close_other_stale(own: Option<HWND>) {
    let title = wide(TITLE);
    let mut after: Option<HWND> = None;
    for _ in 0..16 {
        // SAFETY: a plain lookup among message-only windows; `title` outlives the call.
        let Some(w) = (unsafe { FindWindowExW(Some(HWND_MESSAGE), after, PCWSTR::null(), PCWSTR(title.as_ptr())) }).ok().filter(|h| !h.is_invalid()) else { break };
        if Some(w) != own && out_of_date(w) {
            // SAFETY: a post to a window handle (fails harmlessly if it's gone).
            unsafe {
                let _ = PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        after = Some(w);
    }
}

/// Asked of a running catcher: 1 back if it's out of date (see `stale`).
const WM_APP_STALE: u32 = WM_APP + 0x4E;

/// Whether the catcher with window `w` says it's out of date.
fn out_of_date(w: HWND) -> bool {
    let mut answer = 0usize;
    // SAFETY: a message to a window handle, with a timeout (a gone or hung window answers nothing).
    let sent = unsafe { SendMessageTimeoutW(w, WM_APP_STALE, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 1000, Some(&mut answer)) };
    sent.0 != 0 && answer == 1
}

/// In the catcher: its glint.exe is gone from its place, or Win+E no longer opens it.
fn stale() -> bool {
    registry::this_exe().is_none_or(|exe| !Path::new(&exe).exists()) || !life::win_e_on()
}

/// Whether "Temp Win+E Fix" is on in settings.json, read straight from the file: nothing else of
/// the settings is needed here, and a damaged file is the main Glint's to deal with (off then).
pub fn setting_on() -> bool {
    let Ok(text) = std::fs::read_to_string(crate::settings::file_path()) else { return false };
    serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')).ok().and_then(|v| v.get("WinEAfterQuit").and_then(serde_json::Value::as_bool)).unwrap_or(false)
}

/// The Run value's command: `"glint.exe" --win-e-catcher`.
pub fn catcher_command(exe: &str) -> String {
    format!("\"{exe}\" {SWITCH}")
}

/// Starts `glint.exe --win-e-catcher`.
fn start(exe: &str) {
    let (verb, file, args) = (wide("open"), wide(exe), wide(SWITCH));
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe {
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Starts glint.exe (no folder: it opens as Win+E would).
fn open_glint(exe: &str) {
    let (verb, file) = (wide("open"), wide(exe));
    // SAFETY: NUL-terminated strings that outlive the call; any process may come to the front, since
    // this one just got the key press.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// When the exe file was last changed (an update puts a new one in its place).
fn exe_time(exe: &str) -> Option<SystemTime> {
    std::fs::metadata(exe).and_then(|m| m.modified()).ok()
}

/// `glint.exe --win-e-catcher`: runs until it's no longer wanted. Returns at once if a catcher
/// runs already, or if it isn't wanted.
pub fn run() {
    let Some(exe) = registry::this_exe() else { return };
    // (The setting is checked on the first round, not now: just switched on, it may not be saved yet.)
    if life::testing_without_registry() || !life::win_e_on() {
        return;
    }
    let mutex = wide(&life::own_name_here(MUTEX_NAME));
    // SAFETY: a named mutex, only to see that no other catcher runs; kept until the process ends.
    // (A catcher being told to end may still hold it for a moment: a few tries.)
    let mut held = None;
    for _ in 0..30 {
        held = unsafe {
            match CreateMutexW(None, false, PCWSTR(mutex.as_ptr())) {
                Ok(h) if GetLastError() == ERROR_ALREADY_EXISTS => {
                    let _ = CloseHandle(h);
                    None
                }
                Ok(h) => Some(h),
                Err(_) => None,
            }
        };
        if held.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let Some(held) = held else { return };
    let class = class_name();
    // SAFETY: a window class and a message-only window of our own, on this thread; the message loop
    // runs until WM_QUIT.
    let replaced = unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, lpfnWndProc: Some(proc_), hInstance: instance.into(), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
        RegisterClassExW(&wc);
        let title = wide(TITLE);
        let Ok(window) = CreateWindowExW(WINDOW_EX_STYLE(0), PCWSTR(class.as_ptr()), PCWSTR(title.as_ptr()), WS_POPUP, 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None) else {
            return;
        };
        WINDOW.store(window.0 as isize, Ordering::SeqCst);
        winekey::start(window);
        SetTimer(Some(window), CHECK_TIMER, CHECK_EVERY, None);
        life::trim_memory();
        let born = exe_time(&exe);
        let (mut replaced, mut moved) = (false, false);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER && msg.hwnd == window && msg.wParam.0 == CHECK_TIMER {
                if !Path::new(&exe).exists() {
                    // Moved (or deleted): its settings may have moved with it, so they aren't read
                    // (missing, they'd look switched off). It ends once Win+E no longer points at
                    // the old place (a Glint from the new place repaired it, and starts its own
                    // catcher, or another program took Win+E). The Run value is left to that Glint.
                    if !moved {
                        moved = true;
                        SetTimer(Some(window), CHECK_TIMER, CHECK_MOVED_EVERY, None);
                    }
                    if !life::win_e_on() {
                        break;
                    }
                    continue;
                }
                if !wanted(setting_on()) {
                    // Switched off some other way (another program took Win+E, say): its Run value
                    // goes too - if it's still this glint.exe's (another copy of Glint may have
                    // put its own there).
                    remove_run_if_ours(&Places::current(), &exe);
                    break;
                }
                if exe_time(&exe) != born {
                    replaced = true;
                    break;
                }
                life::trim_memory();
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        winekey::stop();
        let _ = KillTimer(Some(window), CHECK_TIMER);
        replaced
    };
    // SAFETY: our own mutex handle, closed once, before a new catcher may take it.
    unsafe {
        let _ = CloseHandle(held);
    }
    if replaced {
        // A new glint.exe is in place: it takes over (with its own, newer code).
        start(&exe);
    }
}

/// The catcher window's procedure: Win+E from the hook (or the hotkey) opens Glint; WM_CLOSE ends it.
unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP_WIN_E => {
            win_e();
            LRESULT(0)
        }
        WM_HOTKEY if wparam.0 == winekey::HOTKEY_ID as usize => {
            win_e();
            LRESULT(0)
        }
        WM_APP_STALE => LRESULT(stale() as isize),
        WM_CLOSE => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Win+E: the running Glint opens a window (as it does for its own Win+E); with none, glint.exe starts.
fn win_e() {
    if life::send(&Request::Open(None), std::time::Duration::ZERO) {
        return;
    }
    if let Some(exe) = registry::this_exe() {
        open_glint(&exe);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_command_starts_the_catcher_of_this_exe() {
        assert_eq!(catcher_command(r"C:\Tools\Glint\Glint.exe"), r#""C:\Tools\Glint\Glint.exe" --win-e-catcher"#);
    }

    #[test]
    fn a_test_run_never_starts_a_catcher_or_touches_the_registry() {
        // cargo test runs with its own settings folder (GLINT_SETTINGS_DIR) and no test registry.
        if life::testing_without_registry() {
            sync(true);
            assert!(running_window().is_none());
            run(); // returns at once
        }
    }
}
