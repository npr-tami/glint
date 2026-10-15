//! Small window helpers the windows share.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};

/// The part of `hwnd` you can see (screen pixels): Windows 10 / 11 add invisible borders round a
/// window's rectangle.
pub fn visible_rect(hwnd: HWND) -> RECT {
    let mut r = RECT::default();
    // SAFETY: plain queries on our own windows with valid out-structs.
    unsafe {
        if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32).is_err() || r.right <= r.left {
            let _ = GetWindowRect(hwnd, &mut r);
        }
    }
    r
}

/// Places `hwnd` so the part of it you can see is `x, y, w, h`, its invisible borders outside it.
pub fn place_visible(hwnd: HWND, (x, y, w, h): (i32, i32, i32, i32)) {
    let mut whole = RECT::default();
    // SAFETY: a plain query and a move of our own window.
    unsafe {
        let _ = GetWindowRect(hwnd, &mut whole);
        let seen = visible_rect(hwnd);
        let (l, t) = (seen.left - whole.left, seen.top - whole.top);
        let (r, b) = (whole.right - seen.right, whole.bottom - seen.bottom);
        let _ = SetWindowPos(hwnd, None, x - l, y - t, w + l + r, h + t + b, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// The low and high 16 bits of a message's WPARAM or LPARAM.
pub fn lo_word(v: usize) -> u32 {
    (v & 0xffff) as u32
}

pub fn hi_word(v: usize) -> u32 {
    ((v >> 16) & 0xffff) as u32
}
