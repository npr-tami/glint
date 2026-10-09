//! The window frame: Glint draws its own title bar, so Windows' frame is mostly switched off - but the
//! parts people rely on stay: resizing from every edge, Aero snap, the drop shadow, rounded corners on
//! Windows 11, and a maximised window that fits the screen exactly.
//!
//! How (the same trick Windows Terminal uses): in WM_NCCALCSIZE we let Windows compute the normal
//! frame and then take the top part (caption + top border) back into our client area. The left,
//! right and bottom borders stay; on Windows 10/11 they are invisible but still resize the window
//! and carry the shadow. The top edge is resized through WM_NCHITTEST (a thin band at the top of
//! our title bar). When maximised, Windows makes the window bigger than the screen by the frame
//! thickness, so the top is pushed down by that much.
//!
//! Under Wine there are no invisible borders (they'd be drawn as grey bars), so there the whole
//! window is client area and all edges resize through WM_NCHITTEST.

use std::cell::Cell;

use windows::core::{s, w, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CLOAK, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, GetClientRect, IsZoomed, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP,
    HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS, SM_CXFRAME, SM_CXPADDEDBORDER, SM_CYFRAME, WM_NCCALCSIZE, WM_NCHITTEST,
};

use crate::theme::Theme;

thread_local! {
    static WINE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Whether we run under Wine (Linux/macOS) rather than real Windows.
pub fn is_wine() -> bool {
    WINE.with(|w| {
        if let Some(v) = w.get() {
            return v;
        }
        // SAFETY: looks up an export by name in an already loaded module; nothing is called.
        let v = unsafe { GetModuleHandleW(w!("ntdll.dll")).map(|m| GetProcAddress(m, s!("wine_get_version")).is_some()).unwrap_or(false) };
        w.set(Some(v));
        v
    })
}

/// How far Windows' (invisible) frame reaches, in pixels, at this DPI.
fn frame_size(dpi: u32) -> (i32, i32) {
    // SAFETY: plain metric queries.
    unsafe {
        let pad = GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
        (GetSystemMetricsForDpi(SM_CXFRAME, dpi) + pad, GetSystemMetricsForDpi(SM_CYFRAME, dpi) + pad)
    }
}

/// WM_NCCALCSIZE with wParam = TRUE: decides which part of the window is ours to draw.
///
/// # Safety
/// `lparam` must be the NCCALCSIZE_PARAMS pointer Windows passed with the message.
pub unsafe fn nc_calc_size(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: the caller passes the message's own parameters; Windows guarantees the pointer.
    unsafe {
        let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let (fx, fy) = frame_size(dpi);
        let zoomed = IsZoomed(hwnd).as_bool();
        if is_wine() {
            if zoomed {
                let r = &mut params.rgrc[0];
                r.left += fx;
                r.right -= fx;
                r.top += fy;
                r.bottom -= fy;
            }
            return LRESULT(0);
        }
        let top = params.rgrc[0].top;
        let result = DefWindowProcW(hwnd, WM_NCCALCSIZE, wparam, lparam);
        params.rgrc[0].top = top + if zoomed { fy } else { 0 };
        result
    }
}

/// The resize edges inside our client area: the top band (and, under Wine, every edge).
/// Returns a WM_NCHITTEST code, or None if the point is an ordinary client point.
pub fn resize_edge(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> Option<u32> {
    // SAFETY: plain window queries on our own window; `pt` and `rc` are valid out-structs.
    unsafe {
        if IsZoomed(hwnd).as_bool() {
            return None;
        }
        if !is_wine() {
            // Windows' own borders (left, right, bottom) answer first.
            let hit = DefWindowProcW(hwnd, WM_NCHITTEST, wparam, lparam).0 as u32;
            if hit != HTCLIENT {
                return Some(hit);
            }
        }
        let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
        let _ = ScreenToClient(hwnd, &mut pt);
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        let band = (5.0 * scale).round() as i32;
        let corner = band * 3;
        let top = pt.y < band;
        if !is_wine() {
            return top.then(|| {
                if pt.x < corner {
                    HTTOPLEFT
                } else if pt.x >= rc.right - corner {
                    HTTOPRIGHT
                } else {
                    HTTOP
                }
            });
        }
        let (left, right, bottom) = (pt.x < band, pt.x >= rc.right - band, pt.y >= rc.bottom - band);
        match (top, bottom, left, right) {
            (true, _, true, _) => Some(HTTOPLEFT),
            (true, _, _, true) => Some(HTTOPRIGHT),
            (_, true, true, _) => Some(HTBOTTOMLEFT),
            (_, true, _, true) => Some(HTBOTTOMRIGHT),
            (true, ..) => Some(HTTOP),
            (_, true, ..) => Some(HTBOTTOM),
            (_, _, true, _) => Some(HTLEFT),
            (_, _, _, true) => Some(HTRIGHT),
            _ => None,
        }
    }
}

/// One-time frame set-up: keep the shadow and ask Windows 11 for rounded corners.
pub fn init(hwnd: HWND) {
    let margins = MARGINS { cxLeftWidth: 0, cxRightWidth: 0, cyTopHeight: 1, cyBottomHeight: 0 };
    let round = DWMWCP_ROUND;
    // SAFETY: the values live across the calls and their sizes are passed. Failures (old Windows,
    // Wine) are fine: the window just looks a little plainer.
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
    }
}

/// Matches the bits Windows still draws (the 1-pixel border, menus) to the skin: light or dark.
pub fn apply_theme(hwnd: HWND, theme: &Theme) {
    let dark = BOOL::from(theme.dark);
    let border = theme.line.to_colorref();
    // SAFETY: as in `init`.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
    }
    dark_menus(theme.menus_dark);
}

/// Hides the window from the screen until its first frame is ready (no white flash at start).
pub fn cloak(hwnd: HWND, on: bool) {
    let v = BOOL::from(on);
    // SAFETY: as in `init`.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, &v as *const _ as *const _, 4);
    }
}

/// Native popup menus in dark or light. Uses uxtheme's SetPreferredAppMode (ordinal 135) and
/// FlushMenuThemes (136), which Windows 10 1903+ has but doesn't document; skipped if missing.
fn dark_menus(dark: bool) {
    type SetMode = unsafe extern "system" fn(i32) -> i32;
    type Flush = unsafe extern "system" fn();
    // SAFETY: the two functions are looked up by ordinal and only called if present, with the
    // signatures Windows has used since 1903 (int -> int, void -> void).
    unsafe {
        // Normally already loaded (the common controls use it); load it only if not, so switching
        // the theme back and forth doesn't pile up references to the DLL.
        let Ok(ux) = GetModuleHandleW(w!("uxtheme.dll")).or_else(|_| LoadLibraryW(w!("uxtheme.dll"))) else { return };
        let set = GetProcAddress(ux, windows::core::PCSTR(135usize as *const u8));
        let flush = GetProcAddress(ux, windows::core::PCSTR(136usize as *const u8));
        if let (Some(set), Some(flush)) = (set, flush) {
            let set: SetMode = std::mem::transmute(set);
            let flush: Flush = std::mem::transmute(flush);
            set(if dark { 2 } else { 3 }); // ForceDark / ForceLight
            flush();
        }
    }
}
