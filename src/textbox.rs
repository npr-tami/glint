//! A text box for typing (the path box, the filter box): a real Win32 EDIT control placed over
//! our own drawing, so typing, the caret, selection, copy/paste and input methods all work exactly
//! like everywhere else in Windows.
//!
//! The box tells its owner what happens through [`TextEvent`]s, which the window hands to every
//! part's `text_event` (see ui.rs): `Changed` while typing, `Enter`, `Escape`, `LostFocus`.
//! Colours come from the theme (`input` background, `text` colour); window.rs answers the EDIT's
//! WM_CTLCOLOREDIT with them.

use std::cell::{Cell, RefCell};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE,
    FW_NORMAL, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{EM_GETSEL, EM_REPLACESEL, EM_SETCUEBANNER, EM_SETMARGINS, EM_SETSEL};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, SetFocus, VK_BACK, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_F5, VK_MENU, VK_OEM_3, VK_OEM_COMMA, VK_RETURN, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetParent, GetWindowTextLengthW, GetWindowTextW, PostMessageW, SendMessageW, SetWindowPos,
    SetWindowTextW, ShowWindow, ES_AUTOHSCROLL, ES_LEFT, HMENU, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_KEYDOWN, WM_KILLFOCUS, WM_SETFONT, WS_CHILD,
};

use crate::ui::Rect;

/// Posted to the main window: wParam = box id, lParam = TextEventKind as a number.
pub const WM_APP_TEXT: u32 = WM_APP + 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextEventKind {
    /// The text changed (typing, paste, delete). Not sent for [`TextBox::set_text`].
    Changed,
    Enter,
    Escape,
    /// Focus went elsewhere (a click outside, Alt+Tab).
    LostFocus,
    /// The down arrow (the filter box sends the keyboard on to the list). The box still handles the
    /// key as usual (Windows' path suggestions use it).
    Down,
}

impl TextEventKind {
    pub fn from_number(n: isize) -> TextEventKind {
        match n {
            1 => TextEventKind::Enter,
            2 => TextEventKind::Escape,
            3 => TextEventKind::LostFocus,
            4 => TextEventKind::Down,
            _ => TextEventKind::Changed,
        }
    }
    fn number(self) -> isize {
        match self {
            TextEventKind::Changed => 0,
            TextEventKind::Enter => 1,
            TextEventKind::Escape => 2,
            TextEventKind::LostFocus => 3,
            TextEventKind::Down => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextEvent {
    /// The id you gave [`TextBox::new`].
    pub id: u16,
    pub kind: TextEventKind,
}

thread_local! {
    /// True while we change a box's text ourselves (its EN_CHANGE is then not a person typing).
    static SETTING: Cell<bool> = const { Cell::new(false) };
    /// The font family the renderer picked; text boxes use the same. The number goes up when it
    /// changes (another skin), so every box makes its font again.
    static FAMILY: RefCell<String> = RefCell::new("Segoe UI".to_string());
    static FAMILY_GENERATION: Cell<u32> = const { Cell::new(0) };
    /// A shortcut key went to the window (see `TextBox::pass_shortcuts`): the control character
    /// Windows makes from it (Ctrl+T is character 20) must not be typed into the box.
    static SKIP_CHAR: Cell<bool> = const { Cell::new(false) };
}

/// In the subclass data next to the box's id: this box passes window shortcuts on.
const PASS_SHORTCUTS: usize = 0x1_0000;

/// Whether a change notification right now comes from [`TextBox::set_text`] (window.rs checks this).
pub fn setting_text() -> bool {
    SETTING.with(|s| s.get())
}

/// Use this font family for text boxes (window.rs passes the renderer's choice, which follows the
/// skin). Boxes made earlier take the new font when they're next placed.
pub fn set_font_family(name: &str) {
    let changed = FAMILY.with(|f| {
        let mut f = f.borrow_mut();
        let changed = *f != name;
        *f = name.to_string();
        changed
    });
    if changed {
        FAMILY_GENERATION.with(|g| g.set(g.get().wrapping_add(1)));
    }
}

/// A text box. Ids 100-199 are for the toolbar, 200-299 for the sidebar, 300-399 for the file list.
pub struct TextBox {
    hwnd: HWND,
    id: u16,
    font: HFONT,
    font_scale: f32,
    /// The font family it was made with (`FAMILY_GENERATION`).
    font_family: u32,
    /// The text size in DIPs (the normal size unless `set_font_size` says otherwise).
    font_size: f32,
    visible: bool,
}

impl TextBox {
    /// Creates the (hidden) box as a child of the main window.
    pub fn new(parent: HWND, id: u16) -> TextBox {
        // SAFETY: creates a child EDIT of our live window; the subclass proc is a plain function and
        // the id fits in the dwRefData slot.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None).ok();
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!(""),
                WS_CHILD | WINDOW_STYLE((ES_LEFT | ES_AUTOHSCROLL) as u32),
                0,
                0,
                0,
                0,
                Some(parent),
                Some(HMENU(id as usize as *mut _)),
                instance.map(|m| m.into()),
                None,
            )
            .unwrap_or_default();
            if !hwnd.is_invalid() {
                let _ = SetWindowSubclass(hwnd, Some(edit_proc), 1, id as usize);
                SendMessageW(hwnd, EM_SETMARGINS, Some(WPARAM(3)), Some(LPARAM(0)));
            }
            hwnd
        };
        TextBox { hwnd, id, font: HFONT::default(), font_scale: 0.0, font_family: 0, font_size: crate::theme::size::FONT, visible: false }
    }

    /// Window shortcuts typed while this box has the keyboard (Ctrl+Shift+F, Ctrl+T, Ctrl+W, Ctrl+1,
    /// F5... - Ctrl with a key a text box has no use for) go to the window, as if the list had the
    /// keyboard. Editing keys (Ctrl+A, C, V, X, Y,
    /// Z, the arrows, Delete, Backspace) stay the box's. For the path and filter boxes; not for the
    /// name box, where a tab opening would end the renaming.
    pub fn pass_shortcuts(&self) {
        // SAFETY: the same subclass as in `new`, with the flag added to its data.
        unsafe {
            let _ = SetWindowSubclass(self.hwnd, Some(edit_proc), 1, self.id as usize | PASS_SHORTCUTS);
        }
    }

    /// Text this big (DIPs), from the next `place` on: the file list's name box follows the list's
    /// text size.
    pub fn set_font_size(&mut self, dips: f32) {
        if (dips - self.font_size).abs() > 0.001 {
            self.font_size = dips;
            self.font_scale = 0.0; // made again at the next `place`
        }
    }

    pub fn id(&self) -> u16 {
        self.id
    }

    /// The EDIT control itself (for extras such as Windows' path suggestions).
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Places the box so its text sits vertically centred in `rect` (DIPs), `pad` DIPs in from the left.
    pub fn place(&mut self, rect: Rect, pad: f32, scale: f32) {
        let family = FAMILY_GENERATION.with(Cell::get);
        if (scale - self.font_scale).abs() > 0.001 || family != self.font_family {
            self.font_family = family;
            self.make_font(scale);
        }
        let font_px = (self.font_size * scale).round();
        let h = (font_px * 1.45).round() as i32;
        let x = ((rect.x + pad) * scale).round() as i32;
        let w = (((rect.w - 2.0 * pad) * scale).round() as i32).max(1);
        let y = ((rect.y * scale) + ((rect.h * scale) - h as f32) / 2.0).round() as i32;
        // SAFETY: moves our own child window.
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    fn make_font(&mut self, scale: f32) {
        let family: Vec<u16> = FAMILY.with(|f| f.borrow().encode_utf16().chain(std::iter::once(0)).collect());
        let height = -((self.font_size * scale).round() as i32);
        // SAFETY: creates a GDI font (deleted in Drop or when replaced); `family` outlives the call.
        unsafe {
            let font = CreateFontW(
                height,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                PCWSTR(family.as_ptr()),
            );
            SendMessageW(self.hwnd, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            if !self.font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            self.font = font;
        }
        self.font_scale = scale;
    }

    /// Grey hint text shown while the box is empty ("Filter").
    pub fn set_hint(&self, hint: &str) {
        let w: Vec<u16> = hint.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the string lives across the (synchronous) call.
        unsafe {
            SendMessageW(self.hwnd, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(w.as_ptr() as isize)));
        }
    }

    pub fn show(&mut self) {
        // SAFETY: shows our own child window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
        self.visible = true;
    }

    pub fn hide(&mut self) {
        // SAFETY: hides our own child window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Puts the typing cursor in the box.
    pub fn focus(&self) {
        // SAFETY: focuses our own child window.
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    pub fn has_focus(&self) -> bool {
        // SAFETY: plain query.
        unsafe { GetFocus() == self.hwnd }
    }

    /// Gives the keyboard back to the main window (after Enter/Escape).
    pub fn give_focus_back(&self) {
        // SAFETY: focuses our own parent window.
        unsafe {
            if let Ok(parent) = GetParent(self.hwnd) {
                let _ = SetFocus(Some(parent));
            }
        }
    }

    pub fn text(&self) -> String {
        // SAFETY: the buffer is sized from the reported length and written by the call.
        unsafe {
            let len = GetWindowTextLengthW(self.hwnd).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(self.hwnd, &mut buf).max(0) as usize;
            String::from_utf16_lossy(&buf[..n])
        }
    }

    /// Changes the text (without a `Changed` event).
    pub fn set_text(&self, text: &str) {
        let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        SETTING.with(|s| s.set(true));
        // SAFETY: the string lives across the call.
        unsafe {
            let _ = SetWindowTextW(self.hwnd, PCWSTR(w.as_ptr()));
        }
        SETTING.with(|s| s.set(false));
    }

    pub fn select_all(&self) {
        // SAFETY: plain message to our own child.
        unsafe {
            SendMessageW(self.hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
        }
    }

    /// Selects characters `start..end`, counted in UTF-16 units (a file name without its extension).
    pub fn select_range(&self, start: usize, end: usize) {
        // SAFETY: plain message to our own child.
        unsafe {
            SendMessageW(self.hwnd, EM_SETSEL, Some(WPARAM(start)), Some(LPARAM(end as isize)));
        }
    }
}

impl Drop for TextBox {
    fn drop(&mut self) {
        if !self.font.is_invalid() {
            // SAFETY: the font is ours. (The EDIT itself goes away with the main window.)
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
    }
}

/// Where the word before `caret` starts (UTF-16 units): spaces before the caret are skipped, then
/// everything up to the previous space or backslash.
fn word_start(text: &[u16], caret: usize) -> usize {
    let gap = |c: u16| c == ' ' as u16 || c == '\\' as u16 || c == '/' as u16;
    let mut start = caret.min(text.len());
    while start > 0 && gap(text[start - 1]) {
        start -= 1;
    }
    while start > 0 && !gap(text[start - 1]) {
        start -= 1;
    }
    start
}

/// Ctrl+Backspace in a box: the word before the cursor goes (or the selection, if there is one).
/// Done as a normal edit, so the box reports the change and Ctrl+Z brings the word back.
fn delete_word_before(hwnd: HWND) {
    // SAFETY: plain messages to our own EDIT; the buffers are sized from what it reports and
    // outlive the calls.
    unsafe {
        let (mut from, mut to) = (0u32, 0u32);
        SendMessageW(hwnd, EM_GETSEL, Some(WPARAM(&mut from as *mut u32 as usize)), Some(LPARAM(&mut to as *mut u32 as isize)));
        if from == to {
            let len = GetWindowTextLengthW(hwnd).max(0) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
            from = word_start(&buf[..n], to as usize) as u32;
            SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(from as usize)), Some(LPARAM(to as isize)));
        }
        let nothing = [0u16];
        SendMessageW(hwnd, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(nothing.as_ptr() as isize)));
    }
}

/// Whether a key pressed in a box is one of the window's shortcuts the box has no use for (see
/// `TextBox::pass_shortcuts`).
fn is_window_shortcut(vk: u16, ctrl: bool, alt: bool) -> bool {
    if alt {
        return false;
    }
    if vk == VK_F5.0 {
        return true;
    }
    if !ctrl {
        return false;
    }
    let letter = (b'A' as u16..=b'Z' as u16).contains(&vk) && !matches!(vk as u8, b'A' | b'C' | b'V' | b'X' | b'Y' | b'Z');
    letter || (b'1' as u16..=b'9' as u16).contains(&vk) || vk == VK_TAB.0 || vk == VK_OEM_3.0 || vk == VK_OEM_COMMA.0
}

/// Catches Enter, Escape and focus loss in the EDIT and tells the main window.
unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _uid: usize, data: usize) -> LRESULT {
    let id = data & 0xFFFF;
    if msg == WM_KEYDOWN {
        SKIP_CHAR.with(|s| s.set(false));
        // SAFETY: plain key-state queries.
        let (ctrl, alt) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0) };
        if data & PASS_SHORTCUTS != 0 && is_window_shortcut(wparam.0 as u16, ctrl, alt) {
            // A Ctrl+letter also makes a control character next: it isn't typed.
            SKIP_CHAR.with(|s| s.set(ctrl && (b'A' as usize..=b'Z' as usize).contains(&wparam.0)));
            // Sent, not posted, so the window sees the keys still held (Ctrl, Shift) as they are now.
            // SAFETY: a message to our own parent window, on this same thread.
            unsafe {
                if let Ok(parent) = GetParent(hwnd) {
                    SendMessageW(parent, WM_KEYDOWN, Some(wparam), Some(lparam));
                }
            }
            return LRESULT(0);
        }
    }
    if msg == WM_CHAR && wparam.0 < 0x20 && SKIP_CHAR.with(|s| s.replace(false)) {
        return LRESULT(0);
    }
    let post = |kind: TextEventKind| {
        // SAFETY: posting to our own parent window.
        unsafe {
            if let Ok(parent) = GetParent(hwnd) {
                let _ = PostMessageW(Some(parent), WM_APP_TEXT, WPARAM(id), LPARAM(kind.number()));
            }
        }
    };
    match msg {
        WM_KEYDOWN if wparam.0 == VK_RETURN.0 as usize => {
            post(TextEventKind::Enter);
            return LRESULT(0);
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            post(TextEventKind::Escape);
            return LRESULT(0);
        }
        // Ctrl+A selects everything (old EDIT controls don't).
        // SAFETY: plain key-state query and a message to this same control.
        WM_KEYDOWN if wparam.0 == 'A' as usize && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
            unsafe { SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1))) };
            return LRESULT(0);
        }
        // The down arrow: told (the filter hands the keyboard to the list), and handled as usual.
        WM_KEYDOWN if wparam.0 == VK_DOWN.0 as usize => post(TextEventKind::Down),
        // Ctrl+Backspace takes away the word before the cursor (old EDIT controls type a box).
        // SAFETY: plain key-state query.
        WM_KEYDOWN if wparam.0 == VK_BACK.0 as usize && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => {
            delete_word_before(hwnd);
            return LRESULT(0);
        }
        // Swallow the characters of Enter, Escape, Ctrl+A and Ctrl+Backspace so the EDIT doesn't
        // beep or type them.
        WM_CHAR if matches!(wparam.0, 0x0d | 0x1b | 0x01 | 0x7f) => return LRESULT(0),
        WM_KILLFOCUS => post(TextEventKind::LostFocus),
        _ => {}
    }
    // SAFETY: hands everything else to the EDIT's own handling.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}
