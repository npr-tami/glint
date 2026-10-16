//! A text box for typing (the path box, the filter box): a real Win32 EDIT control placed over
//! our own drawing, so typing, the caret, selection, copy/paste and input methods all work exactly
//! like everywhere else in Windows.
//!
//! The box tells its owner what happens through [`TextEvent`]s, which the window hands to every
//! part's `text_event` (see ui.rs): `Changed` while typing, `Enter`, `Escape`, `LostFocus`.
//! Colours come from the theme (`input` background, `text` colour); window.rs answers the EDIT's
//! WM_CTLCOLOREDIT with them. Windows' folder suggestions under the path box (SHAutoComplete's
//! list) follow dark skins too, and never show as an empty strip (`set_suggestion_colors`).
//!
//! A paste is one tidy line: the first line with something on it, without the spaces round it; in
//! the path box a whole line (copied with its line break) goes there at once, as in Explorer.

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
    GetFocus, GetKeyState, SetFocus, VK_BACK, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_F5, VK_INSERT, VK_MENU, VK_OEM_3, VK_OEM_COMMA, VK_RETURN, VK_SHIFT, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetParent, GetWindowTextLengthW, GetWindowTextW, PostMessageW, SendMessageW, SetWindowPos,
    SetWindowTextW, ShowWindow, ES_AUTOHSCROLL, ES_LEFT, HMENU, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CHAR, WM_KEYDOWN, WM_KILLFOCUS, WM_PASTE, WM_SETFONT, WS_CHILD,
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
    /// Tab (Shift+Tab sends it too): a box in a form of several hands the keyboard on (form.rs).
    Tab,
}

impl TextEventKind {
    pub fn from_number(n: isize) -> TextEventKind {
        match n {
            1 => TextEventKind::Enter,
            2 => TextEventKind::Escape,
            3 => TextEventKind::LostFocus,
            4 => TextEventKind::Down,
            5 => TextEventKind::Tab,
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
            TextEventKind::Tab => 5,
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
/// ...and: a pasted line that ends in a line break goes at once, as if Enter was pressed (see
/// `TextBox::go_on_pasted_line`).
const GO_ON_PASTED_LINE: usize = 0x2_0000;

/// What a paste puts in a box (they're one line): the first line with something on it, without
/// the spaces and line breaks around it, and whether a line break came after it (the line was
/// copied whole: "X:\glint-qa\r\n"). None: the text goes in as it is (one line, nothing round it).
pub fn tidy_paste(text: &str) -> Option<(String, bool)> {
    let breaks = ['\r', '\n'];
    if !text.contains(breaks) && text.trim() == text {
        return None;
    }
    let mut rest = text;
    loop {
        let (line, after) = match rest.find(breaks) {
            Some(i) => (&rest[..i], Some(&rest[i..])),
            None => (rest, None),
        };
        let line = line.trim();
        match after {
            Some(after) if line.is_empty() => rest = after.trim_start_matches(breaks),
            _ => return Some((line.to_string(), after.is_some())),
        }
    }
}

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

// ---------------------------------------------------------------- Windows' path suggestions

/// The window class of the list Windows shows under a box with SHAutoComplete (the path box's
/// folder suggestions, toolbar.rs). It is Windows' own, drawn white whatever the skin.
const SUGGESTIONS_CLASS: &str = "Auto-Suggest Dropdown";

/// How Windows' suggestion list is dressed: the visual style for its list and scroll bars (None:
/// Windows' own) and its colours as COLORREFs, background then text (None: Windows' own).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuggestionLook {
    pub style: Option<&'static str>,
    pub colors: Option<(u32, u32)>,
}

/// The look for a skin: in a dark one, Windows' dark list and scroll bars in the boxes' colours
/// (`bg`, `fg`, as the path box has them); in a light one, Windows' list just as it comes.
pub fn suggestion_look(dark: bool, bg: u32, fg: u32) -> SuggestionLook {
    if dark {
        SuggestionLook { style: Some("DarkMode_Explorer"), colors: Some((bg, fg)) }
    } else {
        SuggestionLook { style: None, colors: None }
    }
}

thread_local! {
    /// The look the suggestion list gets each time it shows (None until a dark skin was used:
    /// light skins never touch it).
    static SUGGESTIONS: Cell<Option<SuggestionLook>> = const { Cell::new(None) };
    /// The hook that sees the list show (`watch_suggestions`).
    static SUGGESTIONS_HOOK: Cell<isize> = const { Cell::new(0) };
    /// The suggestion list hidden here for being empty (0: none), shown again once it has rows.
    static HIDDEN_EMPTY: Cell<isize> = const { Cell::new(0) };
}

/// Follows the skin with Windows' suggestion list under the path box (window.rs calls this with
/// the boxes' colours whenever the skin changes). It's dressed each time it shows.
pub fn set_suggestion_colors(dark: bool, bg: u32, fg: u32) {
    // (Watched in light skins too: an empty list is hidden in any skin.)
    watch_suggestions();
    if !dark && SUGGESTIONS.with(|s| s.get()).is_none() {
        return; // never dark: Windows' list stays exactly as it looks
    }
    SUGGESTIONS.with(|s| s.set(Some(suggestion_look(dark, bg, fg))));
}

/// Whether the suggestion list should be hidden: it's showing with no suggestion in it (Windows
/// shows an empty strip when what's typed is exactly a folder's path and there's nothing more).
pub fn hide_suggestions(visible: bool, rows: usize) -> bool {
    visible && rows == 0
}

/// Hides the suggestion list `list` if it shows nothing, or shows it again if it was hidden here
/// and has suggestions now (Windows still takes it for shown, so it wouldn't show it itself).
fn hide_if_empty(list: HWND) {
    use windows::Win32::UI::Controls::LVM_GETITEMCOUNT;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, IsWindowVisible, SW_SHOWNOACTIVATE};
    // SAFETY: plain queries and messages to the live list and its own list view, on their thread.
    unsafe {
        let Ok(view) = FindWindowExW(Some(list), None, w!("SysListView32"), PCWSTR::null()) else { return };
        let rows = SendMessageW(view, LVM_GETITEMCOUNT, None, None).0.max(0) as usize;
        if hide_suggestions(IsWindowVisible(list).as_bool(), rows) {
            let _ = ShowWindow(list, SW_HIDE);
            HIDDEN_EMPTY.with(|h| h.set(list.0 as isize));
        } else if rows > 0 && HIDDEN_EMPTY.with(|h| h.get()) == list.0 as isize {
            HIDDEN_EMPTY.with(|h| h.set(0));
            let _ = ShowWindow(list, SW_SHOWNOACTIVATE);
        }
    }
}

/// Starts watching for the suggestion list showing (once; the hook stays for the life of the
/// thread). The list is made and shown by the box's own thread, this one.
fn watch_suggestions() {
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{SetWindowsHookExW, WH_CALLWNDPROCRET};
    SUGGESTIONS_HOOK.with(|h| {
        if h.get() != 0 {
            return;
        }
        // SAFETY: a hook on our own thread, with a procedure of the right signature.
        if let Ok(hook) = unsafe { SetWindowsHookExW(WH_CALLWNDPROCRET, Some(suggestions_hook), None, GetCurrentThreadId()) } {
            h.set(hook.0 as isize);
        }
    });
}

/// Whether `hwnd`'s window class is `class`.
fn class_is(hwnd: HWND, class: &str) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut name = [0u16; 64];
    // SAFETY: a plain query into our buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut name) }.max(0) as usize;
    name[..n].iter().copied().eq(class.encode_utf16())
}

/// The hook: a window was shown (ShowWindow, or SetWindowPos with SWP_SHOWWINDOW, which Windows'
/// list uses); if it's the suggestion list, it gets the skin's look. Each time the list is shown,
/// moved or sized, or its list view told how many suggestions there are, an empty list is hidden.
unsafe extern "system" fn suggestions_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    use windows::Win32::UI::Controls::LVM_SETITEMCOUNT;
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, CWPRETSTRUCT, HHOOK, SWP_SHOWWINDOW, WINDOWPOS, WM_SHOWWINDOW, WM_WINDOWPOSCHANGED};
    // SAFETY: (the whole body) for a hook code of 0 or more, lparam points to the message's
    // CWPRETSTRUCT, and a WM_WINDOWPOSCHANGED's own lparam to its WINDOWPOS; the window is one of
    // this thread's.
    unsafe {
        if code >= 0 && lparam.0 != 0 {
            let m = &*(lparam.0 as *const CWPRETSTRUCT);
            match m.message {
                WM_SHOWWINDOW | WM_WINDOWPOSCHANGED if class_is(m.hwnd, SUGGESTIONS_CLASS) => {
                    let shown = match m.message {
                        WM_SHOWWINDOW => m.wParam.0 != 0,
                        _ => m.lParam.0 != 0 && (*(m.lParam.0 as *const WINDOWPOS)).flags.contains(SWP_SHOWWINDOW),
                    };
                    if shown {
                        HIDDEN_EMPTY.with(|h| h.set(0)); // Windows showed it again itself
                        if let Some(look) = SUGGESTIONS.with(|s| s.get()) {
                            dress_suggestions(m.hwnd, look);
                        }
                    }
                    hide_if_empty(m.hwnd);
                }
                LVM_SETITEMCOUNT => {
                    if let Some(list) = GetParent(m.hwnd).ok().filter(|p| class_is(*p, SUGGESTIONS_CLASS)) {
                        hide_if_empty(list);
                    }
                }
                _ => {}
            }
        }
        let hook = SUGGESTIONS_HOOK.with(|h| h.get());
        CallNextHookEx(Some(HHOOK(hook as *mut _)), code, wparam, lparam)
    }
}

/// Gives the suggestion list (its list view and scroll bars) `look`.
fn dress_suggestions(list: HWND, look: SuggestionLook) {
    use windows::Win32::Foundation::TRUE;
    use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;
    unsafe extern "system" fn each(child: HWND, lparam: LPARAM) -> windows::core::BOOL {
        use windows::Win32::Graphics::Gdi::{GetSysColor, InvalidateRect, COLOR_WINDOW, COLOR_WINDOWTEXT};
        use windows::Win32::UI::Controls::{SetWindowTheme, LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR};
        // SAFETY: lparam is the `look` below, alive for the whole walk.
        let look = unsafe { *(lparam.0 as *const SuggestionLook) };
        let style: Vec<u16> = look.style.unwrap_or_default().encode_utf16().chain([0]).collect();
        // SAFETY: plain calls and messages to a live child window of the list, on its own thread;
        // the style name outlives the call. (Both names null: Windows' own style back.)
        unsafe {
            let style = if look.style.is_some() { PCWSTR(style.as_ptr()) } else { PCWSTR::null() };
            if class_is(child, "SysListView32") {
                let _ = SetWindowTheme(child, style, PCWSTR::null());
                let (bg, fg) = look.colors.unwrap_or_else(|| (GetSysColor(COLOR_WINDOW), GetSysColor(COLOR_WINDOWTEXT)));
                SendMessageW(child, LVM_SETBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg as isize)));
                SendMessageW(child, LVM_SETTEXTBKCOLOR, Some(WPARAM(0)), Some(LPARAM(bg as isize)));
                SendMessageW(child, LVM_SETTEXTCOLOR, Some(WPARAM(0)), Some(LPARAM(fg as isize)));
                let _ = InvalidateRect(Some(child), None, true);
            } else if class_is(child, "ScrollBar") {
                let _ = SetWindowTheme(child, style, PCWSTR::null());
                let _ = InvalidateRect(Some(child), None, true);
            }
        }
        TRUE
    }
    // SAFETY: walks the list's own children with a callback of the right signature.
    unsafe {
        let _ = EnumChildWindows(Some(list), Some(each), LPARAM(&look as *const SuggestionLook as isize));
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
        self.add_flag(PASS_SHORTCUTS);
    }

    /// A whole line pasted in (a path copied with its line break, "X:\glint-qa\r\n") is taken at
    /// once, as Explorer's address bar does: the box tells Enter. For the path box.
    pub fn go_on_pasted_line(&self) {
        self.add_flag(GO_ON_PASTED_LINE);
    }

    fn add_flag(&self, flag: usize) {
        use windows::Win32::UI::Shell::GetWindowSubclass;
        // SAFETY: the same subclass as in `new`, its data read back and the flag added to it.
        unsafe {
            let mut data = self.id as usize;
            let _ = GetWindowSubclass(self.hwnd, Some(edit_proc), 1, Some(&mut data));
            let _ = SetWindowSubclass(self.hwnd, Some(edit_proc), 1, data | flag);
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

/// Whether a message to a box asks it to paste: WM_PASTE, Ctrl+V's character, Shift+Insert.
fn is_paste(msg: u32, wparam: WPARAM) -> bool {
    match msg {
        WM_PASTE => true,
        WM_CHAR => wparam.0 == 0x16 && ctrl_only(),
        // SAFETY: plain key-state queries.
        WM_KEYDOWN => wparam.0 == VK_INSERT.0 as usize && unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 && GetKeyState(VK_CONTROL.0 as i32) >= 0 },
        _ => false,
    }
}

/// Pastes the clipboard's text into a box, tidied (`tidy_paste`); a whole line goes at once in a box
/// that wants it (`post` tells Enter). False for text that needs no tidying, or no text: the box's
/// own paste does it then.
fn pasted(hwnd: HWND, data: usize, post: impl Fn(TextEventKind)) -> bool {
    let tidy = crate::codebox::clipboard_text(hwnd).as_deref().and_then(tidy_paste);
    let Some((line, whole)) = tidy else { return false };
    if line.is_empty() {
        return true; // only spaces and line breaks: nothing to paste
    }
    let wide: Vec<u16> = line.encode_utf16().chain([0]).collect();
    // SAFETY: a plain message to our own EDIT; the text outlives the call. As a normal edit, so the
    // box reports the change and Ctrl+Z takes it back.
    unsafe {
        SendMessageW(hwnd, EM_REPLACESEL, Some(WPARAM(1)), Some(LPARAM(wide.as_ptr() as isize)));
    }
    if whole && !line.is_empty() && data & GO_ON_PASTED_LINE != 0 {
        post(TextEventKind::Enter);
    }
    true
}

/// Whether Ctrl is held without Alt: AltGr, which types letters, holds both.
fn ctrl_only() -> bool {
    // SAFETY: plain key-state queries.
    unsafe { GetKeyState(VK_CONTROL.0 as i32) < 0 && GetKeyState(VK_MENU.0 as i32) >= 0 }
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
        // Ctrl+A selects everything (old EDIT controls don't). Not AltGr+A, which holds Ctrl and
        // Alt down to type a letter (Polish ą).
        // SAFETY: plain key-state query and a message to this same control.
        WM_KEYDOWN if wparam.0 == 'A' as usize && ctrl_only() => {
            unsafe { SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1))) };
            return LRESULT(0);
        }
        // The down arrow: told (the filter hands the keyboard to the list), and handled as usual.
        WM_KEYDOWN if wparam.0 == VK_DOWN.0 as usize => post(TextEventKind::Down),
        // Tab: told (a form moves to its next box). (Ctrl+Tab is a window shortcut, above.)
        WM_KEYDOWN if wparam.0 == VK_TAB.0 as usize => {
            post(TextEventKind::Tab);
            return LRESULT(0);
        }
        WM_CHAR if wparam.0 == 0x09 => return LRESULT(0),
        // Ctrl+Backspace takes away the word before the cursor (old EDIT controls type a box).
        WM_KEYDOWN if wparam.0 == VK_BACK.0 as usize && ctrl_only() => {
            delete_word_before(hwnd);
            return LRESULT(0);
        }
        // Swallow the characters of Enter, Escape, Ctrl+A and Ctrl+Backspace so the EDIT doesn't
        // beep or type them.
        WM_CHAR if matches!(wparam.0, 0x0d | 0x1b | 0x01 | 0x7f) => return LRESULT(0),
        WM_KILLFOCUS => {
            // Typing is over: a suggestion list hidden for being empty stays hidden.
            HIDDEN_EMPTY.with(|h| h.set(0));
            post(TextEventKind::LostFocus);
        }
        // A paste (Ctrl+V, Shift+Insert, the right-click menu's Paste): one tidy line. (Text that
        // needs no tidying: the box pastes it as usual.)
        WM_PASTE | WM_CHAR | WM_KEYDOWN if is_paste(msg, wparam) && pasted(hwnd, data, post) => return LRESULT(0),
        _ => {}
    }
    // SAFETY: hands everything else to the EDIT's own handling.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_are_dark_only_in_dark_skins() {
        // A dark skin: Windows' dark list and scroll bars, in the path box's own colours.
        let dark = suggestion_look(true, 0x0020_2020, 0x00f0_f0f0);
        assert_eq!(dark, SuggestionLook { style: Some("DarkMode_Explorer"), colors: Some((0x0020_2020, 0x00f0_f0f0)) });
        // A light one: Windows' own list, untouched.
        assert_eq!(suggestion_look(false, 0x00ff_ffff, 0), SuggestionLook { style: None, colors: None });
    }

    #[test]
    fn light_skins_leave_the_list_alone_until_a_dark_one_was_used() {
        // (Each test runs on a thread of its own, so this one starts with nothing set.)
        set_suggestion_colors(false, 0x00ff_ffff, 0);
        assert_eq!(SUGGESTIONS.with(|s| s.get()), None);
        set_suggestion_colors(true, 0x0020_2020, 0x00f0_f0f0);
        assert_eq!(SUGGESTIONS.with(|s| s.get()), Some(suggestion_look(true, 0x0020_2020, 0x00f0_f0f0)));
        // Back to a light skin: the list gets Windows' own look back the next time it shows.
        set_suggestion_colors(false, 0x00ff_ffff, 0);
        assert_eq!(SUGGESTIONS.with(|s| s.get()), Some(SuggestionLook { style: None, colors: None }));
    }

    #[test]
    fn an_empty_suggestion_list_is_hidden() {
        // An exact match with nothing more to suggest: Windows' empty strip goes.
        assert!(hide_suggestions(true, 0));
        // Suggestions to show, or nothing showing: left alone.
        assert!(!hide_suggestions(true, 3));
        assert!(!hide_suggestions(false, 0));
    }

    #[test]
    fn a_pasted_path_is_one_tidy_line() {
        // One line with nothing round it: pasted as it is.
        assert_eq!(tidy_paste("X:\\glint-qa"), None);
        assert_eq!(tidy_paste("my notes"), None);
        // Copied with its line break: the path, taken at once.
        assert_eq!(tidy_paste("X:\\glint-qa\r\n"), Some(("X:\\glint-qa".to_string(), true)));
        assert_eq!(tidy_paste("X:\\glint-qa\n"), Some(("X:\\glint-qa".to_string(), true)));
        // Spaces round it go, but it isn't a whole line.
        assert_eq!(tidy_paste("  X:\\glint qa\t"), Some(("X:\\glint qa".to_string(), false)));
        // Several lines: the first with something on it.
        assert_eq!(tidy_paste("C:\\one\r\nC:\\two\r\n"), Some(("C:\\one".to_string(), true)));
        assert_eq!(tidy_paste("\r\n  \r\n C:\\after blank lines \nmore"), Some(("C:\\after blank lines".to_string(), true)));
        // Nothing but line breaks and spaces: nothing goes in, and nothing happens.
        assert_eq!(tidy_paste(" \r\n\t\n"), Some((String::new(), false)));
    }
}
