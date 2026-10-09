//! Showing the file list's right-click menus (menu.rs makes them): a native popup menu with Glint's
//! items and submenus, Windows' items for the same files merged in where the menu has its
//! `SHELL_SLOT` - or Windows' whole menu on its own. See shellmenu.rs for where Windows' items
//! come from.
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
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetMenuItemCount, GetMenuItemInfoW, InsertMenuItemW, RemoveMenu, SetMenuDefaultItem,
    TrackPopupMenuEx, HMENU, MENUITEMINFOW, MENU_ITEM_STATE, MENU_ITEM_TYPE, MFS_DEFAULT, MF_BYPOSITION, MF_CHECKED, MF_GRAYED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, MIIM_BITMAP, MIIM_CHECKMARKS, MIIM_DATA, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING,
    MIIM_SUBMENU, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TPM_TOPALIGN,
};

use crate::folder::wide;
use crate::menu;
use crate::log;
use crate::shellmenu::{self, Built, Forwarded, ShellItem, SHELL_FIRST, SHELL_LAST};
use crate::ui::{Menu, MenuItem, SHELL_SLOT};

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
        SHOWING.with(|s| s.set(built.map_or(0, |b| b.session)));
        let picked = TrackPopupMenuEx(h, flags, at.x, at.y, owner, None);
        SHOWING.with(|s| s.set(0));
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
        to_pick(picked.0, &menu.items)
    }
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
    if session == 0 || HUNG.with(|h| h.get()) == session || !shellmenu::is_menu_message(msg, lparam) {
        return None;
    }
    match shellmenu::forward(session, msg, wparam, lparam) {
        Forwarded::Handled(r) => Some(r),
        Forwarded::NotHandled => None,
        Forwarded::TimedOut => {
            HUNG.with(|h| h.set(session));
            log::line(&format!("shell menu {session}: Windows' items didn't answer in time; the menu carries on without asking them"));
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
}
