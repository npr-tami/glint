//! The tray icon, while the main Glint runs hidden in the background ("TrayIcon", on by default):
//! click it to open Glint, right-click it for Open Glint / Quit Glint. It shows only
//! while Glint is hidden; with a window on screen there is nothing to bring back.
//!
//! The icon belongs to the door (life.rs), which gets its clicks as WM_APP_TRAY. If Explorer isn't
//! running yet (Glint started with Windows a moment before the taskbar) or restarts, the icon is
//! added again when Explorer broadcasts "TaskbarCreated".
//!
//! Everything here runs on the UI thread (the door's thread).

use std::cell::Cell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIN_SELECT, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, GetSystemMetrics, LoadImageW, PostMessageW,
    SetForegroundWindow, SetMenuDefaultItem, TrackPopupMenu, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, MF_SEPARATOR, MF_STRING,
    SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, WM_CONTEXTMENU, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP,
};

use crate::life::{Request, WM_APP_TRAY};

/// Our one icon's number.
const ICON_ID: u32 = 1;
/// The tray menu's items.
const PICK_OPEN: usize = 1;
const PICK_QUIT: usize = 2;
/// NIN_KEYSELECT: the icon chosen with the keyboard (Win+B, arrows, Enter).
const NIN_KEYSELECT: u32 = NIN_SELECT + 1;

thread_local! {
    /// The icon should be in the tray (it may be waiting for Explorer to come back).
    static WANTED: Cell<Option<HWND>> = const { Cell::new(None) };
    /// It is in the tray now.
    static SHOWN: Cell<bool> = const { Cell::new(false) };
    /// The icon's picture, loaded once.
    static ICON: Cell<Option<HICON>> = const { Cell::new(None) };
    /// The tray menu is open (a second right-click meanwhile does nothing).
    static MENU_OPEN: Cell<bool> = const { Cell::new(false) };
}

/// Glint's icon (from res/glint.rc) at the tray's size.
fn icon() -> HICON {
    if let Some(i) = ICON.with(Cell::get) {
        return i;
    }
    // SAFETY: loads our own icon resource 1 at the small-icon size; kept until `hide` frees it.
    let loaded = unsafe {
        let instance = GetModuleHandleW(None).ok();
        LoadImageW(
            instance.map(Into::into),
            PCWSTR(std::ptr::without_provenance(1)), // MAKEINTRESOURCE(1)
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
    };
    let i = loaded.map(|h| HICON(h.0)).unwrap_or_default();
    if !i.is_invalid() {
        ICON.with(|c| c.set(Some(i)));
    }
    i
}

fn data(door: HWND) -> NOTIFYICONDATAW {
    let mut d = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: door,
        uID: ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: icon(),
        ..Default::default()
    };
    for (slot, c) in d.szTip.iter_mut().zip("Glint".encode_utf16()) {
        *slot = c;
    }
    d
}

/// Puts the icon in the tray (or keeps trying when Explorer comes back).
pub fn show(door: HWND) {
    WANTED.with(|w| w.set(Some(door)));
    if SHOWN.with(Cell::get) {
        return;
    }
    let d = data(door);
    // SAFETY: `d` is a fully set-up NOTIFYICONDATAW that lives across the call.
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &d) }.as_bool();
    SHOWN.with(|s| s.set(ok));
}

/// Takes the icon out of the tray.
pub fn hide() {
    let Some(door) = WANTED.with(|w| w.take()) else { return };
    if SHOWN.with(|s| s.replace(false)) {
        let d = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: door, uID: ICON_ID, ..Default::default() };
        // SAFETY: names our icon; nothing else is read.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }
    if let Some(i) = ICON.with(|c| c.take()) {
        // SAFETY: the icon was loaded by `icon` (not shared) and the tray no longer uses it.
        unsafe {
            let _ = DestroyIcon(i);
        }
    }
}

/// Explorer (re)started: its tray is empty, so the icon goes back in if it should be there.
pub fn taskbar_created() {
    if let Some(door) = WANTED.with(Cell::get) {
        SHOWN.with(|s| s.set(false));
        show(door);
    }
}

/// A click on the icon (WM_APP_TRAY at the door): what the person wants.
pub fn message(door: HWND, lparam: LPARAM) -> Option<Request> {
    match lparam.0 as u32 {
        WM_LBUTTONUP | NIN_SELECT | NIN_KEYSELECT => Some(Request::Open(None)),
        WM_RBUTTONUP | WM_CONTEXTMENU => menu(door),
        _ => None,
    }
}

/// The right-click menu: Open Glint (in bold: what a click does) and Quit Glint.
fn menu(door: HWND) -> Option<Request> {
    if MENU_OPEN.with(|m| m.replace(true)) {
        return None;
    }
    // SAFETY: the menu is made, shown (modal) and destroyed here. Windows wants the owner in front
    // while a tray menu is open (so clicking elsewhere closes it), and a message afterwards.
    let picked = unsafe {
        let mut picked = 0usize;
        if let Ok(h) = CreatePopupMenu() {
            let _ = AppendMenuW(h, MF_STRING, PICK_OPEN, w!("Open Glint"));
            let _ = AppendMenuW(h, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(h, MF_STRING, PICK_QUIT, w!("Quit Glint"));
            let _ = SetMenuDefaultItem(h, PICK_OPEN as u32, 0);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(door);
            picked = TrackPopupMenu(h, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_RIGHTALIGN | TPM_BOTTOMALIGN, pt.x, pt.y, None, door, None).0 as usize;
            let _ = PostMessageW(Some(door), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(h);
        }
        picked
    };
    MENU_OPEN.with(|m| m.set(false));
    match picked {
        PICK_OPEN => Some(Request::Open(None)),
        PICK_QUIT => Some(Request::Quit),
        _ => None,
    }
}
