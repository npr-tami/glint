//! GLINT_TEST_HIDDEN: for the test copies only. Every window the UI thread makes stays off the
//! screen (cloaked, no taskbar button) and never takes the focus from whatever the person is using,
//! so a test driver can work Glint through window messages and take pictures of it (PrintWindow)
//! while the person keeps working. Off unless the variable is set; a normal start never sees it.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::Graphics::Gdi::InvalidateRect;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, RegisterWindowMessageW, SetWindowsHookExW, CBT_CREATEWNDW, CWPRETSTRUCT, HCBT_CREATEWND, SetForegroundWindow, WH_CALLWNDPROC, WH_CALLWNDPROCRET, WH_CBT, CWPSTRUCT, WINDOWPOS, WM_WINDOWPOSCHANGING, SWP_NOACTIVATE, WM_CREATE,
    WM_SHOWWINDOW, WS_CHILD, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, GetWindowLongW, GWL_STYLE,
};

/// A window a test driver asked a picture of (0: none). See `take`.
static WANTED: AtomicIsize = AtomicIsize::new(0);

/// The message a test driver sends a window for a picture of it ("GlintTestShot"). Since a Windows
/// update (15 Nov 2026) PrintWindow of a hidden window comes back white, so Glint saves the picture
/// itself: the window's next frame is copied to `shot_path` (a .bmp, only Glint's own drawing; the
/// driver adds the text boxes).
fn shot_message() -> u32 {
    static M: OnceLock<u32> = OnceLock::new();
    // SAFETY: a constant, NUL-terminated name.
    *M.get_or_init(|| unsafe { RegisterWindowMessageW(w!("GlintTestShot")) })
}

/// Where the picture goes: glint-shot.bmp in %TEMP% (the test copy's own temp folder).
pub fn shot_path() -> Option<std::path::PathBuf> {
    std::env::var_os("TEMP").map(|t| std::path::Path::new(&t).join("glint-shot.bmp"))
}

/// Whether the frame being drawn for `hwnd` should be saved as the asked-for picture (asked once,
/// answered once).
pub fn take(hwnd: HWND) -> bool {
    on() && WANTED.compare_exchange(hwnd.0 as isize, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

/// Whether this Glint runs hidden for a test driver.
pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GLINT_TEST_HIDDEN").is_some_and(|v| !v.is_empty()))
}

/// Starts keeping this thread's windows hidden (when `on`). Call on the UI thread before any window
/// is made.
pub fn start() {
    if !on() {
        return;
    }
    // SAFETY: hooks on our own thread, with procedures of the right signatures; they stay for the
    // life of the thread.
    unsafe {
        let _ = SetWindowsHookExW(WH_CBT, Some(cbt), None, GetCurrentThreadId());
        let _ = SetWindowsHookExW(WH_CALLWNDPROCRET, Some(made), None, GetCurrentThreadId());
        let _ = SetWindowsHookExW(WH_CALLWNDPROC, Some(placing), None, GetCurrentThreadId());
    }
}

/// SetForegroundWindow, except in a test copy run hidden: there nothing of Glint's comes to the
/// front.
pub fn front(hwnd: HWND) -> BOOL {
    if on() {
        return BOOL::from(false);
    }
    // SAFETY: a plain request about a window handle.
    unsafe { SetForegroundWindow(hwnd) }
}

fn cloak(hwnd: HWND) {
    let v = BOOL::from(true);
    // SAFETY: a DWM attribute on one of our own windows.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const _ as *const _, 4);
    }
}

fn top_level(hwnd: HWND) -> bool {
    // SAFETY: a plain query on one of our own windows.
    (unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32 & WS_CHILD.0) == 0
}

/// A window about to be made: a top-level one gets no taskbar button and never becomes the active
/// window by being shown or clicked (the person's window keeps the focus).
unsafe extern "system" fn cbt(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for HCBT_CREATEWND lparam points to the CBT_CREATEWNDW and its CREATESTRUCTW, which
    // the hook may change.
    unsafe {
        if code == HCBT_CREATEWND as i32 && lparam.0 != 0 {
            let c = &mut *(lparam.0 as *mut CBT_CREATEWNDW);
            if !c.lpcs.is_null() {
                let cs = &mut *c.lpcs;
                if (cs.style as u32 & WS_CHILD.0) == 0 {
                    cs.dwExStyle = (cs.dwExStyle | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE) & !WS_EX_APPWINDOW;
                    cloak(HWND(wparam.0 as *mut _));
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// A window was made or is being shown: cloaked (again), so it never shows on the screen.
unsafe extern "system" fn made(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for a code of 0 or more, lparam points to the message's CWPRETSTRUCT; the window is
    // one of this thread's.
    unsafe {
        if code >= 0 && lparam.0 != 0 {
            let m = &*(lparam.0 as *const CWPRETSTRUCT);
            if (m.message == WM_CREATE || m.message == WM_SHOWWINDOW) && top_level(m.hwnd) {
                cloak(m.hwnd);
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

/// A window being shown or moved: never activated by it (showing a window normally makes it the
/// active one, and with it the foreground window, taking the focus from the person).
unsafe extern "system" fn placing(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: for a code of 0 or more, lparam points to the message's CWPSTRUCT; for
    // WM_WINDOWPOSCHANGING its lparam points to the WINDOWPOS Windows reads back afterwards.
    unsafe {
        if code >= 0 && lparam.0 != 0 {
            let m = &*(lparam.0 as *const CWPSTRUCT);
            if m.message == shot_message() && m.message != 0 {
                WANTED.store(m.hwnd.0 as isize, Ordering::SeqCst);
                let _ = InvalidateRect(Some(m.hwnd), None, false);
            }
            if m.message == WM_WINDOWPOSCHANGING && m.lParam.0 != 0 && top_level(m.hwnd) {
                let wp = &mut *(m.lParam.0 as *mut WINDOWPOS);
                wp.flags |= SWP_NOACTIVATE;
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}
