//! Showing the file list's right-click menus (menu.rs makes them): a native popup menu with Glint's
//! items and submenus, Windows' items for the same files merged in where the menu has its
//! `SHELL_SLOT` - or Windows' whole menu on its own. See shellmenu.rs for where Windows' items
//! come from. Tag ›'s colours get a little round picture of their colour beside their words.
//!
//! Windows' items are copied into Glint's menu as they are: their text, their pictures (an app's
//! bitmap, or "the app draws it"), their submenus (Send to, Give access to, New...) and their
//! command ids, which are all SHELL_FIRST and up. While the menu is open, the menu messages those
//! items need go to their maker on the shell thread ([`forward`], called first thing by the
//! window's message handler). A submenu copied this way belongs to Windows' menu too, so it is
//! taken out of Glint's menu again (not destroyed) before Glint's menu is destroyed.

use std::cell::Cell;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateDIBSection, DeleteObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ};
use windows::Win32::Graphics::Imaging::{GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, GetMenuItemCount, GetMenuItemInfoW, InsertMenuItemW, RemoveMenu, SetMenuDefaultItem,
    SetMenuItemInfoW, TrackPopupMenuEx, HMENU, MENUITEMINFOW, MENU_ITEM_STATE, MENU_ITEM_TYPE, MFS_DEFAULT, MFT_OWNERDRAW, MF_BYPOSITION,
    MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MIIM_BITMAP, MIIM_CHECKMARKS, MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, MIIM_SUBMENU, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TPM_TOPALIGN, WM_INITMENUPOPUP,
};

use crate::folder::wide;
use crate::menu;
use crate::log;
use crate::shellmenu::{self, Built, Forwarded, ShellItem, SHELL_FIRST, SHELL_LAST};
use crate::theme::Color;
use crate::ui::{Menu, MenuItem, MenuPicture, SHELL_SLOT};

/// MFT_BITMAP: an old-style item whose "text" is a bitmap handle (not copied).
const MFT_BITMAP: u32 = 0x4;

thread_local! {
    /// The Windows menu session whose items are on screen right now (0: none), for `forward`.
    static SHOWING: Cell<u64> = const { Cell::new(0) };
    /// A session whose maker didn't answer a menu message in time: its other messages aren't sent
    /// any more (each would freeze the open menu for up to a second); its items just stay plain.
    static HUNG: Cell<u64> = const { Cell::new(0) };
}

/// What was picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    None,
    /// One of Glint's own items (its id).
    Glint(u32),
    /// One of Windows' commands (its id, SHELL_FIRST and up).
    Windows(u32),
}

/// Shows a files menu at `at` (screen pixels) and returns the pick. With `built`, Windows' items
/// are merged in at SHELL_SLOT - or, with `whole`, Windows' menu is shown on its own. The menu is
/// modal: this returns when it closes.
pub fn show(owner: HWND, at: POINT, menu: &Menu, default_item: u32, built: Option<&Built>, whole: bool) -> Pick {
    let side = if menu.right_aligned { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
    let flags = (TPM_RETURNCMD | TPM_RIGHTBUTTON | side | TPM_TOPALIGN).0;
    if let Some(b) = built.filter(|b| whole && b.hmenu != 0) {
        SHOWING.with(|s| s.set(b.session));
        // SAFETY: Windows' menu stays alive until the session is released, which only happens after
        // this returns (the window releases it).
        let picked = unsafe { TrackPopupMenuEx(HMENU(b.hmenu as *mut _), flags, at.x, at.y, owner, None) };
        SHOWING.with(|s| s.set(0));
        log::line(&format!("shell menu {}: Windows' menu closed, picked {}", b.session, picked.0));
        return to_pick(picked.0, &[]);
    }
    let mut shared = Vec::new();
    // SAFETY: the menu is built, shown (modal) and destroyed here. Submenus that belong to
    // Windows' menu are taken out before the destroy.
    unsafe {
        let Some(h) = build(&menu.items, built, &mut shared) else { return Pick::None };
        if default_item != 0 {
            let _ = SetMenuDefaultItem(h, default_item, 0);
        }
        crate::oldmenu::dress(h, &shared);
        // The colours' dots (Tag ›), on the items the dressing left for Windows to draw.
        let mut dots = Vec::new();
        add_dots(h, &menu.items, GetDpiForWindow(owner), &mut dots);
        SHOWING.with(|s| s.set(built.map_or(0, |b| b.session)));
        let picked = TrackPopupMenuEx(h, flags, at.x, at.y, owner, None);
        SHOWING.with(|s| s.set(0));
        if let Some(b) = built {
            log::line(&format!("shell menu {}: menu closed, picked {}", b.session, picked.0));
        }
        if !shared.is_empty() {
            let n = GetMenuItemCount(Some(h)).max(0) as u32;
            for pos in (0..n).rev() {
                let mut mii = MENUITEMINFOW { cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_SUBMENU, ..Default::default() };
                if GetMenuItemInfoW(h, pos, true, &mut mii).is_ok() && shared.contains(&(mii.hSubMenu.0 as isize)) {
                    let _ = RemoveMenu(h, pos, MF_BYPOSITION);
                }
            }
        }
        let _ = DestroyMenu(h);
        // (A menu doesn't delete its items' pictures.)
        for picture in dots {
            let _ = DeleteObject(HGDIOBJ(picture.0));
        }
        to_pick(picked.0, &menu.items)
    }
}

/// Puts each item's dot (`MenuItem::dot`: Tag ›'s colours) or picture (`MenuItem::picture`) beside
/// its words - on the items Windows draws: a skin's own menus (oldmenu.rs) draw their items
/// themselves, words only. The pictures go in `made`, to be deleted once the menu is gone.
///
/// # Safety
/// `h` must be a live menu.
unsafe fn add_dots(h: HMENU, items: &[MenuItem], dpi: u32, made: &mut Vec<HBITMAP>) {
    let scale = dpi.max(96) as f32 / 96.0;
    for item in items {
        // SAFETY: as for this function.
        unsafe { add_dots(h, &item.submenu, dpi, made) };
        if !item.submenu.is_empty() || item.id == 0 {
            continue;
        }
        let art = match (item.dot, item.picture) {
            (Some(color), _) => {
                let side = (16.0 * scale).round() as usize;
                Some(Art { width: side, height: side, pixels: dot_pixels(color, side, scale) })
            }
            (None, Some(picture)) => picture_art(picture, scale),
            (None, None) => None,
        };
        let Some(art) = art else { continue };
        let size = std::mem::size_of::<MENUITEMINFOW>() as u32;
        let mut now = MENUITEMINFOW { cbSize: size, fMask: MIIM_FTYPE, ..Default::default() };
        // (By its id: Windows finds it in the submenus too.)
        // SAFETY: a live menu; the call fills in `now`.
        if unsafe { GetMenuItemInfoW(h, item.id, false, &mut now) }.is_err() || now.fType.0 & MFT_OWNERDRAW.0 != 0 {
            continue;
        }
        let Some(picture) = bitmap(&art) else { continue };
        let set = MENUITEMINFOW { cbSize: size, fMask: MIIM_BITMAP, hbmpItem: picture, ..Default::default() };
        // SAFETY: as above. The picture lives until the caller deletes it, after the menu.
        if unsafe { SetMenuItemInfoW(h, item.id, false, &set) }.is_ok() {
            made.push(picture);
        } else {
            // SAFETY: a picture made above and handed to nothing.
            let _ = unsafe { DeleteObject(HGDIOBJ(picture.0)) };
        }
    }
}

/// A menu item's picture: its size in pixels and its pixels, top row first, each see-through, red,
/// green and blue from the top byte down, the colours already times the see-through.
struct Art {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

/// The pixels of `picture` at `scale`: an icon 16 DIPs square, the logos 16 DIPs high.
fn picture_art(picture: MenuPicture, scale: f32) -> Option<Art> {
    let high = (16.0 * scale).round().max(1.0) as usize;
    match picture {
        MenuPicture::Icon(number) => icon_art(number, high),
        MenuPicture::Wordmark { caps, dark } => {
            let source = crate::logo::pixels(dark, caps, crate::logo::pick(scale))?;
            // The wordmark fills a 40-DIP title bar; here it is as high as the icon beside its words.
            let height = (high as f32 * 1.6).round() as u32;
            let width = (source.width as f32 * height as f32 / source.height as f32).round().max(1.0) as u32;
            shrink(source, width, height)
        }
        MenuPicture::Box(color) => Some(box_art(color, high, scale)),
    }
}

/// glint.exe's icon `number` (res/glint.rc), `side` pixels square.
fn icon_art(number: u16, side: usize) -> Option<Art> {
    let wic = crate::thumbs::wic_factory()?;
    // SAFETY: the icon is loaded from this exe and destroyed here; WIC's objects are owned
    // wrappers; the buffer holds exactly `side` rows of `side * 4` bytes.
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        let handle = LoadImageW(Some(module.into()), PCWSTR(number as usize as *const u16), IMAGE_ICON, side as i32, side as i32, LR_DEFAULTCOLOR).ok()?;
        let icon = HICON(handle.0);
        let pixels = (|| {
            let from = wic.CreateBitmapFromHICON(icon).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            from.GetSize(&mut w, &mut h).ok()?;
            let conv = wic.CreateFormatConverter().ok()?;
            conv.Initialize(&from, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
            let mut buf = vec![0u8; w as usize * h as usize * 4];
            conv.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
            Some(Art { width: w as usize, height: h as usize, pixels: words(&buf) })
        })();
        let _ = DestroyIcon(icon);
        pixels.filter(|a| a.width > 0 && a.height > 0)
    }
}

/// `source` shrunk smoothly to `width` x `height`.
fn shrink(source: &crate::thumbs::Pixels, width: u32, height: u32) -> Option<Art> {
    let wic = crate::thumbs::wic_factory()?;
    // SAFETY: WIC copies `source`'s pixels when it makes the bitmap; the buffer holds exactly
    // `height` rows of `width * 4` bytes.
    unsafe {
        let from = wic.CreateBitmapFromMemory(source.width, source.height, &GUID_WICPixelFormat32bppPBGRA, source.width * 4, &source.pixels).ok()?;
        let scaler = wic.CreateBitmapScaler().ok()?;
        scaler.Initialize(&from, width, height, WICBitmapInterpolationModeHighQualityCubic).ok()?;
        let mut buf = vec![0u8; width as usize * height as usize * 4];
        scaler.CopyPixels(std::ptr::null(), width * 4, &mut buf).ok()?;
        Some(Art { width: width as usize, height: height as usize, pixels: words(&buf) })
    }
}

/// Blue, green, red, see-through bytes as one number a pixel.
fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])).collect()
}

/// The slanted box logo in `color`: `high` pixels high, its right edge leaning in at the bottom
/// (edges smoothed).
fn box_art(color: Color, high: usize, scale: f32) -> Art {
    const SAMPLES: usize = 4;
    let width = (26.0 * scale).round() as usize;
    let (top, bottom) = (2.0 * scale, high as f32 - 2.0 * scale);
    let lean = 5.0 * scale;
    let n = (SAMPLES * SAMPLES) as f32;
    let byte = |v: f32| ((v / n).clamp(0.0, 1.0) * 255.0).round() as u32;
    let mut pixels = vec![0u32; width * high];
    for y in 0..high {
        for x in 0..width {
            let mut a = 0.0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    let right = width as f32 - lean * (py - top) / (bottom - top).max(1.0);
                    if py >= top && py <= bottom && px <= right {
                        a += 1.0;
                    }
                }
            }
            pixels[y * width + x] = (byte(a) << 24) | (byte(a * color.r) << 16) | (byte(a * color.g) << 8) | byte(a * color.b);
        }
    }
    Art { width, height: high, pixels }
}

/// `art` as a 32-bit picture for a menu item.
fn bitmap(art: &Art) -> Option<HBITMAP> {
    if art.width == 0 || art.height == 0 || art.pixels.len() != art.width * art.height {
        return None;
    }
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: art.width as i32,
        biHeight: -(art.height as i32), // the top row first
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: a new 32-bit picture; `bits` points to its width * height pixels, filled here before
    // anything else sees it.
    unsafe {
        let picture = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(picture.0));
            return None;
        }
        std::ptr::copy_nonoverlapping(art.pixels.as_ptr(), bits as *mut u32, art.pixels.len());
        Some(picture)
    }
}

/// The pixels of a `side` x `side` picture of a round dot in `color`, 10 DIPs across at `scale`
/// (whole pixels, its edge smoothed), with a thin darker rim so a light colour holds up on a light
/// menu. Top row first; each pixel see-through, red, green and blue from the top byte down, the
/// colours already times the see-through, as Windows wants them.
fn dot_pixels(color: Color, side: usize, scale: f32) -> Vec<u32> {
    const SAMPLES: usize = 4;
    let d = (10.0 * scale).round().min(side as f32);
    // Its edges where pixels' edges are: an odd width centred on a pixel, an even one between two.
    let c = ((side as f32 - d) / 2.0).floor() + d / 2.0;
    let r = d / 2.0;
    let inner = (r - scale.max(1.0)).max(0.0);
    let rim = color.darker(0.3);
    let n = (SAMPLES * SAMPLES) as f32;
    let byte = |v: f32| ((v / n).clamp(0.0, 1.0) * 255.0).round() as u32;
    let mut out = vec![0u32; side * side];
    for y in 0..side {
        for x in 0..side {
            let (mut a, mut rgb) = (0.0, [0.0f32; 3]);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let dx = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32 - c;
                    let dy = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32 - c;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if dist <= r {
                        let k = if dist > inner { rim } else { color };
                        a += 1.0;
                        rgb[0] += k.r;
                        rgb[1] += k.g;
                        rgb[2] += k.b;
                    }
                }
            }
            out[y * side + x] = (byte(a) << 24) | (byte(rgb[0]) << 16) | (byte(rgb[1]) << 8) | byte(rgb[2]);
        }
    }
    out
}

/// What a picked id means. Windows' commands are SHELL_FIRST and up; anything else must be one of
/// Glint's own items (`ours`). Another app's item with an id outside the range it was given (a bug
/// in that app) could otherwise pass for one of Glint's - Delete, say - so it counts as no pick.
fn to_pick(id: i32, ours: &[MenuItem]) -> Pick {
    fn has(items: &[MenuItem], id: u32) -> bool {
        items.iter().any(|i| (i.submenu.is_empty() && !i.label.is_empty() && i.id == id) || has(&i.submenu, id))
    }
    match id.max(0) as u32 {
        0 => Pick::None,
        id if (SHELL_FIRST..=SHELL_LAST).contains(&id) => Pick::Windows(id),
        id if has(ours, id) => Pick::Glint(id),
        _ => Pick::None,
    }
}

/// For the window's message handler, before anything else: while Windows' items are on screen,
/// the menu messages they need go to their maker. Some(result) when it handled one.
pub fn forward(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    let session = SHOWING.with(|s| s.get());
    if session == 0 || !shellmenu::is_menu_message(msg, lparam) {
        return None;
    }
    if HUNG.with(|h| h.get()) == session {
        // An earlier message wasn't answered in time. Asking again while the shell thread still
        // works on it would only wait again; once it's done, the other items are asked again.
        if shellmenu::forward_in_progress() {
            return None;
        }
        HUNG.with(|h| h.set(0));
        log::line(&format!("shell menu {session}: Windows' items answer again"));
    }
    let answer = shellmenu::forward(session, msg, wparam, lparam);
    if msg == WM_INITMENUPOPUP {
        // What a submenu had when it opened (an empty one doesn't open at all): for the log.
        // SAFETY: wparam is the submenu's handle, as the menu passed it.
        let n = unsafe { GetMenuItemCount(Some(HMENU(wparam.0 as *mut _))) };
        let how = match answer { Forwarded::Handled(_) => "answered", Forwarded::NotHandled => "not wanted", Forwarded::TimedOut => "too slow" };
        log::line(&format!("shell menu {session}: submenu opening, Windows' items {how}, {n} entries"));
    }
    match answer {
        Forwarded::Handled(r) => Some(r),
        Forwarded::NotHandled => None,
        Forwarded::TimedOut => {
            HUNG.with(|h| h.set(session));
            log::line(&format!("shell menu {session}: Windows' items didn't answer in time (message {msg:#x}); the menu carries on without them until they do"));
            None
        }
    }
}

/// Builds a native menu from Glint's items, with Windows' (from `built`) at SHELL_SLOT. Submenus
/// borrowed from Windows' menu are added to `shared`.
///
/// # Safety
/// The handles in `built` must be alive (its session not yet released).
unsafe fn build(items: &[MenuItem], built: Option<&Built>, shared: &mut Vec<isize>) -> Option<HMENU> {
    // SAFETY: (the whole body) a fresh menu; label buffers outlive the calls that copy them. A
    // submenu appended with MF_POPUP belongs to its parent and is destroyed with it.
    unsafe {
        let h = CreatePopupMenu().ok()?;
        for item in items {
            if item.id == SHELL_SLOT && item.submenu.is_empty() {
                if let Some(b) = built {
                    let keys: Vec<String> = items.iter().filter(|i| !i.label.is_empty()).map(|i| menu::label_key(&i.label)).collect();
                    add_windows_items(h, &b.items, &keys, shared);
                }
                continue;
            }
            if item.label.is_empty() {
                let _ = AppendMenuW(h, MF_SEPARATOR, 0, PCWSTR::null());
                continue;
            }
            let label = wide(&item.label);
            let mut flags = MF_STRING;
            if item.checked {
                flags |= MF_CHECKED;
            }
            if item.disabled {
                flags |= MF_GRAYED;
            }
            let mut id = item.id as usize;
            if !item.submenu.is_empty() {
                match build(&item.submenu, None, shared) {
                    Some(sub) => {
                        flags |= MF_POPUP;
                        id = sub.0 as usize;
                    }
                    None => continue,
                }
            }
            let _ = AppendMenuW(h, flags, id, PCWSTR(label.as_ptr()));
        }
        Some(h)
    }
}

/// Copies Windows' items that Glint doesn't have itself into `h`, as one section: a line before
/// it, Windows' own lines inside it (never two in a row, none at the end).
///
/// # Safety
/// As for `build`.
unsafe fn add_windows_items(h: HMENU, items: &[ShellItem], glint_keys: &[String], shared: &mut Vec<isize>) {
    let mut line_due = true;
    let mut added = false;
    for it in items {
        if it.is_separator() {
            line_due |= added;
            continue;
        }
        if it.ftype & MFT_BITMAP != 0 || !menu::merge_item(it, glint_keys) {
            continue;
        }
        if line_due {
            // SAFETY: a valid menu.
            let _ = unsafe { AppendMenuW(h, MF_SEPARATOR, 0, PCWSTR::null()) };
            line_due = false;
        }
        // SAFETY: as for this function.
        if unsafe { copy_item(h, it) } {
            added = true;
            if it.submenu != 0 {
                shared.push(it.submenu);
            }
        }
    }
}

/// Adds one of Windows' items at the end of `h`. Its bold "default" look is dropped: in Glint's
/// menu, Glint's own Open is the default.
///
/// # Safety
/// As for `build`.
unsafe fn copy_item(h: HMENU, it: &ShellItem) -> bool {
    let mut text = wide(&it.text);
    let mut mii = MENUITEMINFOW {
        cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_DATA | MIIM_CHECKMARKS,
        fType: MENU_ITEM_TYPE(it.ftype),
        fState: MENU_ITEM_STATE(it.fstate & !MFS_DEFAULT.0),
        wID: it.id,
        hSubMenu: HMENU(it.submenu as *mut _),
        hbmpChecked: shellmenu::bitmap(it.checked_bitmap),
        hbmpUnchecked: shellmenu::bitmap(it.unchecked_bitmap),
        dwItemData: it.data,
        ..Default::default()
    };
    if !it.is_owner_drawn() {
        mii.fMask |= MIIM_STRING;
        mii.dwTypeData = PWSTR(text.as_mut_ptr());
        mii.cch = text.len().saturating_sub(1) as u32;
    }
    if it.bitmap != 0 {
        // An app's own picture, "the app draws it" (HBMMENU_CALLBACK), or one of Windows' standard ones.
        mii.fMask |= MIIM_BITMAP;
        mii.hbmpItem = shellmenu::bitmap(it.bitmap);
    }
    // SAFETY: `text` outlives the call, which copies it.
    unsafe {
        let n = GetMenuItemCount(Some(h)).max(0) as u32;
        InsertMenuItemW(h, n, true, &mii).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_glints_own_items_count_as_glints() {
        let ours = vec![
            MenuItem::new(1, "Open"),
            MenuItem::separator(),
            MenuItem::submenu("Sort by", vec![MenuItem::new(30, "Name")]),
            MenuItem::new(SHELL_SLOT, ""),
        ];
        assert_eq!(to_pick(1, &ours), Pick::Glint(1));
        assert_eq!(to_pick(30, &ours), Pick::Glint(30), "in a submenu");
        assert_eq!(to_pick(SHELL_FIRST as i32 + 4, &ours), Pick::Windows(SHELL_FIRST + 4));
        assert_eq!(to_pick(9, &ours), Pick::None, "an app's stray id isn't Glint's Delete");
        assert_eq!(to_pick(SHELL_SLOT as i32, &ours), Pick::None, "the slot itself is never an item");
        assert_eq!(to_pick(0, &ours), Pick::None);
        assert_eq!(to_pick(1, &[]), Pick::None, "Windows' whole menu has none of Glint's items");
    }

    #[test]
    fn menu_pictures_are_whole_and_never_panic() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let b = box_art(Color::rgba(200, 30, 40, 1.0), (16.0 * scale) as usize, scale);
            assert_eq!(b.pixels.len(), b.width * b.height);
            // Solid in the middle, see-through above and below the box and at the leaning corner.
            assert_eq!(b.pixels[(b.height / 2) * b.width + 2] >> 24, 255);
            assert_eq!(b.pixels[0] >> 24, 0);
            assert_eq!(b.pixels[(b.height - 1) * b.width + b.width - 1] >> 24, 0);
            for p in [MenuPicture::Icon(1), MenuPicture::Icon(2), MenuPicture::Icon(9999), MenuPicture::Wordmark { caps: true, dark: false }] {
                if let Some(a) = picture_art(p, scale) {
                    assert_eq!(a.pixels.len(), a.width * a.height);
                }
            }
        }
        assert!(bitmap(&Art { width: 0, height: 0, pixels: Vec::new() }).is_none());
        assert!(bitmap(&Art { width: 4, height: 4, pixels: vec![0; 3] }).is_none(), "wrong size");
    }

    #[test]
    fn a_menu_dot_is_round_with_see_through_corners() {
        let side = 16;
        let px = dot_pixels(Color::hex(0xff0000), side, 1.0);
        let at = |x: usize, y: usize| px[y * side + x];
        let alpha = |p: u32| p >> 24;
        assert_eq!(alpha(at(0, 0)), 0, "a corner");
        assert_eq!(at(8, 8), 0xffff_0000, "the middle: solid red");
        // No colour is more than its see-through allows (Windows draws it wrong otherwise).
        assert!(px.iter().all(|&p| (p >> 16 & 0xff).max(p >> 8 & 0xff).max(p & 0xff) <= alpha(p)));
        // Round: the same seen from each side.
        assert!((0..side).all(|y| (0..side).all(|x| at(x, y) == at(side - 1 - x, y) && at(x, y) == at(x, side - 1 - y) && at(x, y) == at(y, x))));
        // 10 pixels across at 100 %, 15 at 150 % (in a 24-pixel picture).
        let across = |px: &[u32], side: usize, row: usize| (0..side).filter(|&x| px[row * side + x] >> 24 > 0).count();
        assert_eq!(across(&px, side, 8), 10);
        assert_eq!(across(&dot_pixels(Color::hex(0xff0000), 24, 1.5), 24, 11), 15);
    }
}
