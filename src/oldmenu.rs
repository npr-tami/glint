//! Popup menus as an older Windows drew them, for a skin with menu colours (`menu`,
//! `menu-text`, `menu-selection`, `menu-selection-text`, `menu-border`; the era skins have them).
//! Windows draws popup menus itself, in today's look; for these skins Glint's own items are
//! switched to "the owner draws it" just before a menu opens ([`dress`]) and drawn here when the
//! menu asks ([`handle`], first thing in each window's message handler):
//!
//! - 98 / 2000: the grey face, a navy (or the scheme's) bar over the item under the mouse, white
//!   words on it, greyed items pressed into the face, etched lines between groups;
//! - XP: white, the blue bar, grey lines;
//! - 7: light grey with the picture column on the left and its double line, the rounded pale blue
//!   box over the item under the mouse, black words.
//!
//! Windows' items in a file menu are drawn the same way (their words; their little pictures are
//! left out, as the old menus had none), but the submenus that are Windows' own (Send to...) and
//! items Windows' helpers draw themselves stay as Windows draws them. Skins without menu colours
//! never come here: their menus are Windows' own.

use std::cell::RefCell;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetDC, GetTextExtentPoint32W, ReleaseDC,
    RoundRect, SelectObject, SetBkMode, SetTextColor, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, FW_BOLD, FW_NORMAL, HBRUSH, HDC,
    HFONT, HGDIOBJ, NULL_BRUSH, PS_SOLID, TRANSPARENT, GetStockObject,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, MEASUREITEMSTRUCT, ODS_CHECKED, ODS_DISABLED, ODS_GRAYED, ODS_SELECTED, ODT_MENU};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetMenuItemCount, GetMenuItemInfoW, SetMenuInfo, SetMenuItemInfoW, HMENU, MENUINFO, MENUITEMINFOW, MENU_ITEM_TYPE, MFS_DEFAULT,
    MFT_BITMAP, MFT_OWNERDRAW, MFT_SEPARATOR, MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING, MIIM_SUBMENU, MIM_BACKGROUND, WM_DRAWITEM,
    WM_MEASUREITEM,
};

use crate::theme::{Color, Era, Theme};

/// The menu colours and font of the skin in use, when it has them.
#[derive(Clone)]
struct Style {
    era: Era,
    back: Color,
    text: Color,
    selection: Color,
    selection_text: Color,
    border: Color,
    disabled: Color,
    /// The light and shadow of 98 / 2000's etched lines and pressed-in words.
    light: Color,
    shadow: Color,
    /// The edge's other two colours, for the menu's frame (the face's light, the dark).
    light_face: Color,
    dark: Color,
    font: Vec<u16>,
    /// The font's size in points.
    points: f32,
}

/// One of Glint's items, as the menu draws it.
struct Item {
    /// The words before a tab, and the key after it ("Ctrl+C"), without the end zero.
    text: Vec<u16>,
    key: Vec<u16>,
    separator: bool,
    default: bool,
}

thread_local! {
    static STYLE: RefCell<Option<Style>> = const { RefCell::new(None) };
    /// The items of the menus dressed so far (their addresses are the items' data, so each is
    /// boxed: its address stays put while the list grows). Kept until the next menu is dressed:
    /// by then the last one has closed.
    #[allow(clippy::vec_box)]
    static ITEMS: RefCell<Vec<Box<Item>>> = const { RefCell::new(Vec::new()) };
    /// The brush the menus are filled with (0: none yet).
    static BRUSH: RefCell<isize> = const { RefCell::new(0) };
}

// ---------------------------------------------------------------- 98 / 2000's menu frame
//
// Windows draws a popup menu's frame itself: today's thin grey line and soft shadow. 98 and 2000
// gave menus a raised 3D edge and no shadow, so for those skins every menu window this thread
// makes (class "#32768") is caught as it is made (a hook on the thread's sent messages) and its
// frame drawn again after Windows has drawn it (the window's procedure is wrapped).

thread_local! {
    /// The hook watching for menu windows (0: none yet).
    static HOOK: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
    /// The menu windows wrapped so far and their own procedures.
    static WRAPPED: RefCell<Vec<(isize, isize)>> = const { RefCell::new(Vec::new()) };
    /// The menu class's drop shadow was taken off (98 / 2000) and is to come back for other skins.
    static SHADOW_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The colours of the frame when the skin is 98 / 2000's: (outer top / left, inner top / left,
/// inner bottom / right, outer bottom / right, face).
fn frame_colours() -> Option<[Color; 5]> {
    STYLE.with(|s| {
        let s = s.borrow();
        let s = s.as_ref().filter(|s| s.era.classic())?;
        Some([s.light_face, s.light, s.shadow, s.dark, s.back])
    })
}

/// Starts watching for menu windows (once; it stays for the life of the thread, and does nothing
/// for skins other than 98 / 2000's).
fn watch_menus() {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_CALLWNDPROCRET};
    HOOK.with(|h| {
        if h.get() != 0 {
            return;
        }
        // SAFETY: a hook on our own thread, with a procedure of the right signature.
        if let Ok(hook) = unsafe { SetWindowsHookExW(WH_CALLWNDPROCRET, Some(menu_hook), None, GetCurrentThreadId()) } {
            h.set(hook.0 as isize);
        }
    });
}

/// The hook: a window of the menu class was just made.
unsafe extern "system" fn menu_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetClassLongPtrW, GetClassNameW, SetClassLongPtrW, SetWindowLongPtrW, CS_DROPSHADOW, CWPRETSTRUCT, GCL_STYLE, GWLP_WNDPROC, HHOOK,
        WM_CREATE,
    };
    // SAFETY: (the whole body) for a hook code of 0 or more, lparam points to the message's
    // CWPRETSTRUCT; the window is one of this thread's.
    unsafe {
        if code >= 0 && lparam.0 != 0 {
            let m = &*(lparam.0 as *const CWPRETSTRUCT);
            if m.message == WM_CREATE {
                let mut name = [0u16; 16];
                let n = GetClassNameW(m.hwnd, &mut name) as usize;
                if name[..n] == *"#32768".encode_utf16().collect::<Vec<u16>>() {
                    let classic = frame_colours().is_some();
                    let style = GetClassLongPtrW(m.hwnd, GCL_STYLE);
                    if classic && style & CS_DROPSHADOW.0 as usize != 0 {
                        SetClassLongPtrW(m.hwnd, GCL_STYLE, (style & !(CS_DROPSHADOW.0 as usize)) as isize);
                        SHADOW_OFF.with(|s| s.set(true));
                    } else if !classic && SHADOW_OFF.with(|s| s.replace(false)) {
                        SetClassLongPtrW(m.hwnd, GCL_STYLE, (style | CS_DROPSHADOW.0 as usize) as isize);
                    }
                    let key = m.hwnd.0 as isize;
                    if !WRAPPED.with(|w| w.borrow().iter().any(|(h, _)| *h == key)) {
                        let old = SetWindowLongPtrW(m.hwnd, GWLP_WNDPROC, menu_proc as *const () as isize);
                        if old != 0 {
                            WRAPPED.with(|w| w.borrow_mut().push((key, old)));
                        }
                    }
                }
            }
        }
        let hook = HOOK.with(|h| h.get());
        CallNextHookEx(Some(HHOOK(hook as *mut _)), code, wparam, lparam)
    }
}

/// A menu window's procedure, wrapped: Windows' own, then 98 / 2000's frame over the edge it drew.
unsafe extern "system" fn menu_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::Graphics::Gdi::{GetWindowDC, HDC};
    use windows::Win32::UI::WindowsAndMessaging::{CallWindowProcW, SetWindowLongPtrW, GWLP_WNDPROC, PRF_NONCLIENT, WM_NCDESTROY, WM_NCPAINT, WM_PAINT, WM_PRINT, WNDPROC};
    let key = hwnd.0 as isize;
    let Some(old) = WRAPPED.with(|w| w.borrow().iter().find(|(h, _)| *h == key).map(|(_, p)| *p)) else {
        return LRESULT(0);
    };
    // SAFETY: (the whole body) `old` is the procedure Windows gave the window, called with its own
    // message; the DCs are this window's (or the one WM_PRINT passed) and released here.
    unsafe {
        let proc: WNDPROC = std::mem::transmute::<isize, WNDPROC>(old);
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_WNDPROC, old);
            WRAPPED.with(|w| w.borrow_mut().retain(|(h, _)| *h != key));
            return CallWindowProcW(proc, hwnd, msg, wparam, lparam);
        }
        let result = CallWindowProcW(proc, hwnd, msg, wparam, lparam);
        if let Some(colours) = frame_colours() {
            match msg {
                WM_NCPAINT | WM_PAINT => {
                    let dc = GetWindowDC(Some(hwnd));
                    if !dc.is_invalid() {
                        paint_frame(hwnd, dc, colours);
                        ReleaseDC(Some(hwnd), dc);
                    }
                }
                WM_PRINT if lparam.0 as u32 & PRF_NONCLIENT as u32 != 0 && wparam.0 != 0 => paint_frame(hwnd, HDC(wparam.0 as *mut _), colours),
                _ => {}
            }
        }
        result
    }
}

/// 98 / 2000's menu frame in the window's outer three pixels: a raised edge and a row of face.
///
/// # Safety
/// `dc` is a DC of the whole window `hwnd`.
unsafe fn paint_frame(hwnd: HWND, dc: HDC, [outer_tl, inner_tl, inner_br, outer_br, face]: [Color; 5]) {
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    // SAFETY: (the whole body) plain GDI on the caller's DC.
    unsafe {
        let mut wr = RECT::default();
        if GetWindowRect(hwnd, &mut wr).is_err() {
            return;
        }
        let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
        if w < 8 || h < 8 {
            return;
        }
        // One ring `k` pixels in: `tl` along the top and left, `br` along the bottom and right.
        let ring = |k: i32, tl: Color, br: Color| {
            fill(dc, RECT { left: k, top: k, right: w - k - 1, bottom: k + 1 }, tl);
            fill(dc, RECT { left: k, top: k, right: k + 1, bottom: h - k - 1 }, tl);
            fill(dc, RECT { left: k, top: h - k - 1, right: w - k, bottom: h - k }, br);
            fill(dc, RECT { left: w - k - 1, top: k, right: w - k, bottom: h - k }, br);
        };
        ring(0, outer_tl, outer_br);
        ring(1, inner_tl, inner_br);
        ring(2, face, face);
    }
}

/// Takes the skin's menu colours (none: Windows draws the menus as usual).
pub fn set_style(theme: &Theme) {
    let style = (theme.menu.a > 0.0).then(|| {
        let (light, light_face, shadow, dark) = crate::retro::bevel_colours(theme, theme.menu);
        let family = theme.font.split(',').next().unwrap_or("Segoe UI").trim().to_string();
        Style {
            era: theme.era,
            back: theme.menu,
            text: theme.menu_text,
            selection: theme.menu_selection,
            selection_text: theme.menu_selection_text,
            border: theme.menu_border,
            disabled: theme.disabled,
            light,
            shadow,
            light_face,
            dark,
            font: family.encode_utf16().chain(std::iter::once(0)).collect(),
            points: if theme.era == Era::Win7 { 9.0 } else { 8.0 },
        }
    });
    let back = style.as_ref().map(|s| s.back);
    let classic = style.as_ref().is_some_and(|s| s.era.classic());
    STYLE.with(|s| *s.borrow_mut() = style);
    if classic {
        watch_menus();
    }
    BRUSH.with(|b| {
        let mut b = b.borrow_mut();
        if *b != 0 {
            // SAFETY: our own brush, no longer selected anywhere.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(*b as *mut _));
            }
            *b = 0;
        }
        if let Some(c) = back {
            // SAFETY: a plain solid brush.
            *b = unsafe { CreateSolidBrush(COLORREF(c.to_colorref())) }.0 as isize;
        }
    });
}

/// Switches the items of `h` (and of its submenus, but not those in `skip`: Windows' own) to
/// "the owner draws it", and fills the menus with the skin's colour. Nothing when the skin has no
/// menu colours.
///
/// # Safety
/// `h` must be a live menu.
pub unsafe fn dress(h: HMENU, skip: &[isize]) {
    if STYLE.with(|s| s.borrow().is_none()) {
        return;
    }
    ITEMS.with(|i| i.borrow_mut().clear());
    // SAFETY: as for this function.
    unsafe { dress_one(h, skip) };
}

/// # Safety
/// As for `dress`.
unsafe fn dress_one(h: HMENU, skip: &[isize]) {
    // SAFETY: (the whole body) `h` is live; the text buffer outlives the call that fills it.
    unsafe {
        let brush = BRUSH.with(|b| *b.borrow());
        if brush != 0 {
            let info = MENUINFO { cbSize: std::mem::size_of::<MENUINFO>() as u32, fMask: MIM_BACKGROUND, hbrBack: HBRUSH(brush as *mut _), ..Default::default() };
            let _ = SetMenuInfo(h, &info);
        }
        let n = GetMenuItemCount(Some(h)).max(0) as u32;
        for pos in 0..n {
            let mut buf = [0u16; 260];
            let mut mii = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_DATA | MIIM_STRING,
                dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
                cch: buf.len() as u32 - 1,
                ..Default::default()
            };
            if GetMenuItemInfoW(h, pos, true, &mut mii).is_err() {
                continue;
            }
            let sub = mii.hSubMenu.0 as isize;
            if sub != 0 && !skip.contains(&sub) {
                dress_one(mii.hSubMenu, skip);
            }
            // Items their maker draws, and pictures, stay as they are.
            if mii.fType.0 & (MFT_OWNERDRAW.0 | MFT_BITMAP.0) != 0 {
                continue;
            }
            let separator = mii.fType.0 & MFT_SEPARATOR.0 != 0;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
            let words = &buf[..len];
            let (text, key) = match words.iter().position(|&c| c == '\t' as u16) {
                Some(t) => (words[..t].to_vec(), words[t + 1..].to_vec()),
                None => (words.to_vec(), Vec::new()),
            };
            let item = Box::new(Item { text, key, separator, default: mii.fState.0 & MFS_DEFAULT.0 != 0 });
            let data = &*item as *const Item as usize;
            ITEMS.with(|i| i.borrow_mut().push(item));
            let set = MENUITEMINFOW {
                cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_DATA,
                fType: MENU_ITEM_TYPE(mii.fType.0 | MFT_OWNERDRAW.0),
                dwItemData: data,
                ..Default::default()
            };
            let _ = SetMenuItemInfoW(h, pos, true, &set);
        }
    }
}

/// The item behind a menu item's data, if it is one of ours.
fn ours(data: usize) -> bool {
    data != 0 && ITEMS.with(|i| i.borrow().iter().any(|b| &**b as *const Item as usize == data))
}

/// For the windows' message handlers: draws and measures Glint's items while a dressed menu is
/// open. Some(result) when the message was for one of them.
pub fn handle(hwnd: HWND, msg: u32, _wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if (msg != WM_MEASUREITEM && msg != WM_DRAWITEM) || lparam.0 == 0 {
        return None;
    }
    let style = STYLE.with(|s| s.borrow().clone())?;
    // SAFETY: for these two messages lparam points to the struct Windows passed; its first field
    // says what kind of control it is about.
    unsafe {
        if *(lparam.0 as *const u32) != ODT_MENU.0 {
            return None;
        }
        // SAFETY: (the rest) the items stay alive while their menu is open (see ITEMS).
        let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
        if msg == WM_MEASUREITEM {
            let m = &mut *(lparam.0 as *mut MEASUREITEMSTRUCT);
            if !ours(m.itemData) {
                return None;
            }
            let item = &*(m.itemData as *const Item);
            let (w, h) = measure(hwnd, &style, item, scale);
            m.itemWidth = w;
            m.itemHeight = h;
            return Some(LRESULT(1));
        }
        let d = &*(lparam.0 as *const DRAWITEMSTRUCT);
        if !ours(d.itemData) {
            return None;
        }
        let item = &*(d.itemData as *const Item);
        draw(d, &style, item, scale);
        Some(LRESULT(1))
    }
}

/// The left column (the check mark's, 7's picture column) and the room after the words.
fn gutters(style: &Style) -> (f32, f32) {
    match style.era {
        Era::Win7 => (30.0, 24.0),
        Era::Xp => (24.0, 22.0),
        _ => (20.0, 20.0),
    }
}

/// # Safety
/// Plain GDI calls on a screen DC of `hwnd`.
unsafe fn measure(hwnd: HWND, style: &Style, item: &Item, scale: f32) -> (u32, u32) {
    if item.separator {
        return (10, ((if style.era == Era::Win7 { 6.0 } else { 9.0 }) * scale).round() as u32);
    }
    // SAFETY: (the whole body) the DC and font are ours and released / deleted here.
    unsafe {
        let dc = GetDC(Some(hwnd));
        let font = make_font(style, scale, item.default);
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let mut size = SIZE::default();
        let mut key = SIZE::default();
        let text = strip_amp(&item.text);
        let _ = GetTextExtentPoint32W(dc, &text, &mut size);
        if !item.key.is_empty() {
            let _ = GetTextExtentPoint32W(dc, &item.key, &mut key);
        }
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
        ReleaseDC(Some(hwnd), dc);
        let (left, right) = gutters(style);
        let w = (left + 6.0) * scale + size.cx as f32 + if key.cx > 0 { 24.0 * scale + key.cx as f32 } else { 0.0 } + right * scale;
        let h = (if style.era == Era::Win7 { 22.0 } else { 19.0 }) * scale;
        (w.round() as u32, h.round() as u32)
    }
}

/// The words without their "&" marks (for measuring).
fn strip_amp(text: &[u16]) -> Vec<u16> {
    let amp = '&' as u16;
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == amp && i + 1 < text.len() {
            i += 1;
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// # Safety
/// Creates a GDI font; the caller deletes it.
unsafe fn make_font(style: &Style, scale: f32, bold: bool) -> HFONT {
    let height = -((style.points * 96.0 / 72.0 * scale).round() as i32);
    // SAFETY: the face name is zero-terminated and outlives the call.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::DEFAULT_CHARSET,
            windows::Win32::Graphics::Gdi::OUT_DEFAULT_PRECIS,
            windows::Win32::Graphics::Gdi::CLIP_DEFAULT_PRECIS,
            // 98 / 2000 drew menus in crisp, unsmoothed letters.
            if style.era.classic() { windows::Win32::Graphics::Gdi::NONANTIALIASED_QUALITY } else { windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY },
            0,
            PCWSTR(style.font.as_ptr()),
        )
    }
}

fn cref(c: Color) -> COLORREF {
    COLORREF(c.to_colorref())
}

/// # Safety
/// Plain GDI calls on the DC Windows passed.
unsafe fn fill(dc: HDC, rc: RECT, c: Color) {
    // SAFETY: a brush made and deleted here.
    unsafe {
        let b = CreateSolidBrush(cref(c));
        FillRect(dc, &rc, b);
        let _ = DeleteObject(HGDIOBJ(b.0));
    }
}

/// # Safety
/// As for `fill`.
unsafe fn hline(dc: HDC, x0: i32, x1: i32, y: i32, c: Color) {
    // SAFETY: as for this function.
    unsafe { fill(dc, RECT { left: x0, top: y, right: x1, bottom: y + 1 }, c) };
}

/// # Safety
/// Plain GDI calls on the DC in `d`.
unsafe fn draw(d: &DRAWITEMSTRUCT, style: &Style, item: &Item, scale: f32) {
    let dc = d.hDC;
    let rc = d.rcItem;
    let px = |v: f32| (v * scale).round() as i32;
    let selected = d.itemState.0 & ODS_SELECTED.0 != 0;
    let disabled = d.itemState.0 & (ODS_GRAYED.0 | ODS_DISABLED.0) != 0;
    let checked = d.itemState.0 & ODS_CHECKED.0 != 0;
    let (left, _) = gutters(style);
    // SAFETY: (the whole body) GDI objects made here are deleted here; the DC is Windows'.
    unsafe {
        fill(dc, rc, style.back);
        if style.era == Era::Win7 {
            // The picture column and its double line.
            let x = rc.left + px(left) - px(2.0);
            fill(dc, RECT { left: rc.left, top: rc.top, right: x, bottom: rc.bottom }, Color::hex(0xf1f1f1));
            fill(dc, RECT { left: x, top: rc.top, right: x + 1, bottom: rc.bottom }, Color::hex(0xe2e3e3));
            fill(dc, RECT { left: x + 1, top: rc.top, right: x + 2, bottom: rc.bottom }, Color::hex(0xffffff));
        }
        if item.separator {
            let y = (rc.top + rc.bottom) / 2;
            match style.era {
                Era::Win98 | Era::Win2000 => {
                    hline(dc, rc.left + 1, rc.right - 1, y - 1, style.shadow);
                    hline(dc, rc.left + 1, rc.right - 1, y, style.light);
                }
                Era::Win7 => {
                    let x = rc.left + px(left) + px(2.0);
                    hline(dc, x, rc.right, y - 1, Color::hex(0xe0e0e0));
                    hline(dc, x, rc.right, y, Color::hex(0xffffff));
                }
                _ => hline(dc, rc.left + px(2.0), rc.right - px(2.0), y, style.border),
            }
            return;
        }
        let lit = selected && (!disabled || style.era.classic());
        let mut ink = style.text;
        if lit {
            match style.era {
                Era::Win7 => {
                    let r = RECT { left: rc.left + px(2.0), top: rc.top, right: rc.right - px(2.0), bottom: rc.bottom };
                    fill(dc, RECT { left: r.left + 1, top: r.top + 1, right: r.right - 1, bottom: r.bottom - 1 }, style.selection);
                    let pen = CreatePen(PS_SOLID, 1, COLORREF(Color::hex(0xaecff7).to_colorref()));
                    let old_pen = SelectObject(dc, HGDIOBJ(pen.0));
                    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                    let _ = RoundRect(dc, r.left, r.top, r.right, r.bottom, px(4.0), px(4.0));
                    SelectObject(dc, old_brush);
                    SelectObject(dc, old_pen);
                    let _ = DeleteObject(HGDIOBJ(pen.0));
                    ink = style.selection_text;
                }
                _ => {
                    fill(dc, rc, style.selection);
                    ink = style.selection_text;
                }
            }
        }
        let font = make_font(style, scale, item.default);
        let old = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, TRANSPARENT);
        let mut text_rc = RECT { left: rc.left + px(left + 6.0), top: rc.top, right: rc.right - px(20.0), bottom: rc.bottom };
        let mut text = strip_amp(&item.text);
        let mut key = item.key.clone();
        // A menu opened with the mouse shows no underlined letters (Windows 2000 and later).
        let flags = DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX;
        if disabled && style.era.classic() && !lit {
            // Pressed into the face: a light copy one pixel down and right, the shadow over it.
            SetTextColor(dc, cref(style.light));
            let mut moved = RECT { left: text_rc.left + 1, top: text_rc.top + 1, right: text_rc.right + 1, bottom: text_rc.bottom + 1 };
            DrawTextW(dc, &mut text, &mut moved, flags);
            ink = style.shadow;
        } else if disabled {
            ink = if style.era.classic() { style.shadow } else { style.disabled };
        }
        SetTextColor(dc, cref(ink));
        DrawTextW(dc, &mut text, &mut text_rc, flags);
        if !key.is_empty() {
            let mut key_rc = RECT { left: rc.left, top: rc.top, right: rc.right - px(20.0), bottom: rc.bottom };
            DrawTextW(dc, &mut key, &mut key_rc, DT_RIGHT | DT_SINGLELINE | DT_VCENTER);
        }
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
        if checked {
            // The tick: Windows' 7 by 7 one, every column 3 pixels tall.
            let s = scale.max(1.0).round() as i32;
            let (x0, y0) = (rc.left + px(left / 2.0) - 3 * s, (rc.top + rc.bottom) / 2 - 3 * s);
            for (col, top) in [(0, 2), (1, 3), (2, 4), (3, 3), (4, 2), (5, 1), (6, 0)] {
                let x = x0 + col * s;
                let y = y0 + top * s;
                fill(dc, RECT { left: x, top: y, right: x + s, bottom: y + 3 * s }, ink);
            }
        }
    }
}
