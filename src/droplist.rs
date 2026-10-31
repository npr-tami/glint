//! A drop-down list that really drops down: the open part of a drop-down box, drawn by Glint right
//! under the box (above it when the screen ends below), as wide as the box, the way Windows' own
//! drop-down lists open. The Settings window's drop-downs use it instead of a right-click style
//! menu.
//!
//! It works like a menu: [`show`] keeps the mouse and the keyboard until a choice is clicked or
//! the list is closed (Esc, a click anywhere else, Glint losing the front), and returns the pick.
//! The arrows move the highlight, Enter picks it, the first letter jumps to a choice.

use std::sync::Arc;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, ValidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_NEXT, VK_PRIOR, VK_RETURN, VK_SPACE, VK_TAB, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow, GetMessageW, LoadCursorW, PostQuitMessage, RegisterClassExW, ShowWindow, TranslateMessage, IDC_ARROW,
    MSG, SW_SHOWNOACTIVATE, WM_CHAR, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCLBUTTONDOWN, WM_NCRBUTTONDOWN,
    WM_PAINT, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::log;
use crate::render::{Renderer, TextStyle};
use crate::theme::{Color, Theme};
use crate::thumbs::Picture;
use crate::ui::{MenuItem, Point, Rect};

/// One choice's row, in DIPs.
const ROW_H: f32 = 22.0;
/// A separator's row.
const LINE_H: f32 = 7.0;
/// Room left of a choice's words (and right of them, in the width).
const PAD: f32 = 8.0;
/// At most this many rows show; the rest scroll (the wheel, or the arrows).
const MOST: usize = 12;
const WHEEL_ROWS: f32 = 3.0;
/// A picture beside a choice (the Logo and Zip icon lists): this high, then this gap.
const PICTURE_H: f32 = 16.0;
const PICTURE_GAP: f32 = 6.0;

/// How the list looks: the skin's colours and font, taken when it opens.
#[derive(Clone, Debug)]
pub struct DropLook {
    pub back: Color,
    pub line: Color,
    pub text: Color,
    pub muted: Color,
    pub pick: Color,
    pub on_pick: Color,
    pub font: String,
}

impl DropLook {
    pub fn of(theme: &Theme) -> DropLook {
        let back = if theme.input.a > 0.5 { theme.input } else { theme.panel };
        DropLook {
            back: back.with_alpha(1.0),
            line: if theme.era.old() { theme.text } else { theme.muted.with_alpha(0.6) },
            text: theme.text,
            muted: theme.muted,
            pick: theme.accent,
            on_pick: theme.on_accent,
            font: theme.font.clone(),
        }
    }
}

/// A list to open under a box.
#[derive(Clone, Debug)]
pub struct DropList {
    /// The choices: an empty label is a line between groups, a disabled item a group's title.
    pub items: Vec<MenuItem>,
    /// The closed box, in screen pixels - for a menu, the corner it hangs from (top-left, or
    /// top-right when `right`).
    pub under: RECT,
    pub look: DropLook,
    /// A button's menu (SORT, VIEW...) rather than a drop-down box's list: as wide as its words,
    /// ticks beside what's on, shortcuts on the right, nothing lit until the mouse comes.
    pub menu: bool,
    pub right: bool,
}

/// A menu's words and its shortcut (after a tab), with "&&" back to "&".
fn split(label: &str) -> (String, Option<String>) {
    let mut parts = label.splitn(2, '\t');
    let words = parts.next().unwrap_or("").replace("&&", "&");
    (words, parts.next().map(str::to_string))
}

/// The room a menu keeps on the left for ticks.
const TICK_W: f32 = 22.0;
/// The gap between a menu's words and its shortcut.
const KEYS_GAP: f32 = 28.0;

/// Where each row is, top to bottom, in DIPs from the list's top (inside its frame).
fn rows(items: &[MenuItem]) -> Vec<(f32, f32)> {
    let mut y = 0.0;
    items
        .iter()
        .map(|it| {
            let h = if it.label.is_empty() { LINE_H } else { ROW_H };
            let r = (y, h);
            y += h;
            r
        })
        .collect()
}

fn pickable(it: &MenuItem) -> bool {
    !it.label.is_empty() && !it.disabled && it.submenu.is_empty()
}

/// The next choice that can be picked from `from` going `step` (1 or -1), or `from` itself at
/// either end.
fn next_pick(items: &[MenuItem], from: Option<usize>, step: i32) -> Option<usize> {
    let n = items.len() as i32;
    let mut at = from.map_or(if step > 0 { -1 } else { n }, |f| f as i32);
    loop {
        at += step;
        if at < 0 || at >= n {
            return from.or_else(|| items.iter().position(pickable));
        }
        if pickable(&items[at as usize]) {
            return Some(at as usize);
        }
    }
}

/// The first choice after `from` (round the end) whose words start with `c`.
fn by_letter(items: &[MenuItem], from: Option<usize>, c: char) -> Option<usize> {
    let n = items.len();
    let c = c.to_lowercase().next()?;
    (1..=n).map(|k| (from.unwrap_or(n - 1) + k) % n).find(|&i| pickable(&items[i]) && items[i].label.chars().next().and_then(|f| f.to_lowercase().next()) == Some(c))
}

fn register_class() {
    // SAFETY: plain class registration with a static name; Windows' default procedure.
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("GlintDropList"),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: Windows' default handling for our own window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The open list's state while it shows.
struct Open {
    hwnd: HWND,
    renderer: Renderer,
    /// The list's size in DIPs (frame included).
    size: (f32, f32),
    rows: Vec<(f32, f32)>,
    top: f32,
    hot: Option<usize>,
    pictures: Vec<Option<(Arc<Picture>, f32, f32)>>,
}

impl Open {
    fn inner_h(&self) -> f32 {
        self.size.1 - 2.0
    }

    fn extent(&self) -> f32 {
        self.rows.last().map_or(0.0, |(y, h)| y + h)
    }

    fn scroll_to_show(&mut self, i: usize) {
        let (y, h) = self.rows[i];
        if y < self.top {
            self.top = y;
        } else if y + h > self.top + self.inner_h() {
            self.top = y + h - self.inner_h();
        }
    }

    fn scroll_by(&mut self, dy: f32) {
        self.top = (self.top + dy).clamp(0.0, (self.extent() - self.inner_h()).max(0.0));
    }

    /// The row at `p` (client DIPs), if it's inside the list.
    fn row_at(&self, p: Point) -> Option<usize> {
        if p.x < 0.0 || p.y < 1.0 || p.x >= self.size.0 || p.y >= self.size.1 - 1.0 {
            return None;
        }
        let y = p.y - 1.0 + self.top;
        self.rows.iter().position(|(top, h)| y >= *top && y < top + h)
    }

    fn inside(&self, p: Point) -> bool {
        p.x >= 0.0 && p.y >= 0.0 && p.x < self.size.0 && p.y < self.size.1
    }

    fn paint(&mut self, items: &[MenuItem], look: &DropLook, menu: bool) {
        if !self.renderer.begin() {
            return;
        }
        let r = &mut self.renderer;
        let (w, h) = self.size;
        r.clear(look.back);
        r.push_clip(Rect::new(1.0, 1.0, w - 2.0, h - 2.0));
        let has_pictures = self.pictures.iter().any(Option::is_some);
        for (i, it) in items.iter().enumerate() {
            let (top, rh) = self.rows[i];
            let y = top - self.top + 1.0;
            if y + rh < 0.0 || y > h {
                continue;
            }
            if it.label.is_empty() {
                r.fill_rect(Rect::new(PAD, (y + rh / 2.0).floor(), w - 2.0 * PAD, 1.0), look.line.with_alpha(0.5));
                continue;
            }
            let row = Rect::new(1.0, y, w - 2.0, rh);
            let hot = self.hot == Some(i);
            if hot {
                r.fill_rect(row, look.pick);
            }
            let mut x = PAD + 1.0;
            let (words, keys) = split(&it.label);
            if menu {
                if it.checked {
                    let c = Point::new(1.0 + TICK_W / 2.0, y + rh / 2.0);
                    let ink = if hot { look.on_pick } else { look.text };
                    r.polyline(&[Point::new(c.x - 4.0, c.y), Point::new(c.x - 1.0, c.y + 3.0), Point::new(c.x + 4.5, c.y - 3.5)], 1.6, ink);
                }
                x = 1.0 + TICK_W;
            }
            if has_pictures {
                if let Some((pic, pw, ph)) = &self.pictures[i] {
                    let dest = Rect::new(x, y + (rh - ph) / 2.0, *pw, *ph);
                    r.picture(pic, dest, None, 1.0, true);
                }
                x += self.pictures.iter().flatten().map(|(_, pw, _)| *pw).fold(0.0, f32::max) + PICTURE_GAP;
            }
            let color = if hot {
                look.on_pick
            } else if it.disabled {
                look.muted
            } else {
                look.text
            };
            let style = TextStyle::new(color);
            let style = if it.disabled || it.bold { style.semibold() } else { style };
            r.text(&words, Rect::new(x, y, (w - x - PAD).max(0.0), rh), &style);
            if let Some(keys) = keys {
                let keys_color = if hot { look.on_pick } else { look.muted };
                r.text(&keys, Rect::new(x, y, (w - x - PAD - 4.0).max(0.0), rh), &TextStyle::new(keys_color).align(crate::render::Align::Right));
            }
        }
        r.pop_clip();
        r.stroke_rect(Rect::new(0.5, 0.5, w - 1.0, h - 1.0), 1.0, look.line);
        let _ = r.end();
    }
}

/// Opens `list` for the window `owner` and waits: the id of the item picked, or 0 when the list was
/// closed without a pick.
pub fn show(owner: HWND, list: &DropList) -> u32 {
    register_class();
    let items = &list.items;
    if items.iter().all(|it| !pickable(it)) {
        return 0;
    }
    // SAFETY: a popup of our own, its renderer and the mouse capture, all let go before returning;
    // the structs passed live across each call.
    unsafe {
        let dpi = GetDpiForWindow(owner).max(96);
        let scale = dpi as f32 / 96.0;
        let Ok(instance) = GetModuleHandleW(None) else { return 0 };
        let Ok(hwnd) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE, w!("GlintDropList"), w!(""), WS_POPUP, 0, 0, 1, 1, Some(owner), None, Some(instance.into()), None) else {
            return 0;
        };
        let mut renderer = match Renderer::new(hwnd, dpi) {
            Ok(r) => r,
            Err(e) => {
                log::error(&format!("A drop-down list couldn't draw: {e}"));
                let _ = DestroyWindow(hwnd);
                return 0;
            }
        };
        renderer.set_font(&list.look.font, 1.0);
        let pictures: Vec<Option<(Arc<Picture>, f32, f32)>> = items
            .iter()
            .map(|it| {
                let (pw, ph, bytes) = crate::popup::picture_bytes(it.picture?, scale)?;
                let pic = Picture::new(pw, ph, bytes, false)?;
                let h = PICTURE_H.min(ph as f32 / scale);
                Some((pic, pw as f32 / scale * h / (ph as f32 / scale), h))
            })
            .collect();
        // As wide as the box, wider only when a choice wouldn't fit.
        let style = TextStyle::new(list.look.text).semibold();
        let words = items
            .iter()
            .map(|it| {
                let (words, keys) = split(&it.label);
                renderer.measure(&words, &style) + keys.map_or(0.0, |k| KEYS_GAP + renderer.measure(&k, &style))
            })
            .fold(0.0_f32, f32::max);
        let words = if list.menu { words + TICK_W - PAD + 8.0 } else { words };
        let picture_w = pictures.iter().flatten().map(|(_, pw, _)| *pw).fold(0.0_f32, f32::max);
        let picture_w = if picture_w > 0.0 { picture_w + PICTURE_GAP } else { 0.0 };
        let box_w = (list.under.right - list.under.left) as f32 / scale;
        let rows = rows(items);
        let extent = rows.last().map_or(0.0, |(y, h)| y + h);
        let most = MOST as f32 * ROW_H;
        let w = box_w.max((words + picture_w + 2.0 * PAD + 2.0 + if extent > most { 12.0 } else { 0.0 }).ceil());
        let h = extent.min(most) + 2.0;
        let (pw, ph) = ((w * scale).round() as i32, (h * scale).round() as i32);
        // Under the box, its top on the box's bottom line - or above it when the screen ends.
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let monitor = MonitorFromRect(&list.under, MONITOR_DEFAULTTONEAREST);
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() { info.rcWork } else { RECT { left: 0, top: 0, right: 1 << 15, bottom: 1 << 15 } };
        let one = scale.round().max(1.0) as i32;
        let y = if list.under.bottom - one + ph <= work.bottom || list.under.top - work.top < ph { list.under.bottom - one } else { list.under.top - ph + one };
        let x = if list.right { list.under.right - pw } else { list.under.left };
        let x = x.min(work.right - pw).max(work.left);
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER};
        let _ = SetWindowPos(hwnd, None, x, y, pw, ph, SWP_NOZORDER | SWP_NOACTIVATE);
        renderer.resize(pw as u32, ph as u32);
        let current = if list.menu { None } else { items.iter().position(|it| it.checked && pickable(it)) };
        let mut open = Open { hwnd, renderer, size: (w, h), rows, top: 0.0, hot: current, pictures };
        if let Some(c) = current {
            open.scroll_to_show(c);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        open.paint(items, &list.look, list.menu);
        // Whoever had the mouse (the window whose button was pressed) lets it go first: it finishes
        // its press then, and its ReleaseCapture can't take the mouse from the list later.
        if !GetCapture().is_invalid() {
            let _ = ReleaseCapture();
        }
        let _ = SetCapture(hwnd);
        let point = |lparam: LPARAM| {
            let x = (lparam.0 & 0xffff) as i16 as f32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
            Point::new(x / scale, y / scale)
        };
        // A press inside the list picks on release (as Windows' lists do); a press outside closes it.
        let mut picked = 0u32;
        let mut msg = MSG::default();
        loop {
            if GetCapture() != hwnd || (GetForegroundWindow() != owner && GetForegroundWindow() != owner_of(owner)) {
                break;
            }
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            let mut repaint = false;
            match msg.message {
                WM_PAINT if msg.hwnd == hwnd => {
                    open.paint(items, &list.look, list.menu);
                    let _ = ValidateRect(Some(hwnd), None);
                    continue;
                }
                WM_MOUSEMOVE if msg.hwnd == hwnd => {
                    let p = point(msg.lParam);
                    let at = open.row_at(p).filter(|i| pickable(&items[*i]));
                    if (at.is_some() || list.menu) && at != open.hot {
                        open.hot = at;
                        repaint = true;
                    }
                }
                WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN if msg.hwnd == hwnd => {
                    if !open.inside(point(msg.lParam)) {
                        break;
                    }
                }
                WM_LBUTTONUP if msg.hwnd == hwnd => {
                    if let Some(i) = open.row_at(point(msg.lParam)).filter(|i| pickable(&items[*i])) {
                        picked = items[i].id;
                        break;
                    }
                }
                WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN => break,
                WM_MOUSEWHEEL => {
                    let notches = ((msg.wParam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                    open.scroll_by(-notches * WHEEL_ROWS * ROW_H);
                    repaint = true;
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    let vk = msg.wParam.0 as u16;
                    let move_to = |open: &mut Open, to: Option<usize>| {
                        if let Some(t) = to {
                            open.hot = Some(t);
                            open.scroll_to_show(t);
                        }
                    };
                    match vk {
                        k if k == VK_ESCAPE.0 || k == VK_TAB.0 => break,
                        k if k == VK_RETURN.0 || k == VK_SPACE.0 => {
                            if let Some(i) = open.hot {
                                picked = items[i].id;
                            }
                            break;
                        }
                        k if k == VK_DOWN.0 => { let to = next_pick(items, open.hot, 1); move_to(&mut open, to) },
                        k if k == VK_UP.0 => { let to = next_pick(items, open.hot, -1); move_to(&mut open, to) },
                        k if k == VK_HOME.0 => { let to = next_pick(items, None, 1); move_to(&mut open, to) },
                        k if k == VK_END.0 => { let to = next_pick(items, None, -1); move_to(&mut open, to) },
                        k if k == VK_NEXT.0 || k == VK_PRIOR.0 => {
                            let step = if k == VK_NEXT.0 { 1 } else { -1 };
                            let mut to = open.hot;
                            for _ in 0..MOST - 1 {
                                to = next_pick(items, to, step);
                            }
                            move_to(&mut open, to);
                        }
                        // Alt+Down / Alt+Up / F4 close it again, as in Windows.
                        0x73 => break,
                        _ => {
                            let _ = TranslateMessage(&msg);
                        }
                    }
                    repaint = true;
                }
                WM_CHAR => {
                    if let Some(c) = char::from_u32(msg.wParam.0 as u32) {
                        if let Some(t) = by_letter(items, open.hot, c) {
                            open.hot = Some(t);
                            open.scroll_to_show(t);
                            repaint = true;
                        }
                    }
                }
                _ => {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if repaint {
                open.paint(items, &list.look, list.menu);
            }
        }
        if GetCapture() == hwnd {
            let _ = ReleaseCapture();
        }
        let _ = DestroyWindow(open.hwnd);
        picked
    }
}

/// The window that owns `hwnd` (the main window, for the Settings window), or `hwnd` itself.
fn owner_of(hwnd: HWND) -> HWND {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindow, GW_OWNER};
    // SAFETY: a plain query.
    unsafe { GetWindow(hwnd, GW_OWNER).unwrap_or(hwnd) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<MenuItem> {
        vec![MenuItem::new(0, "Default").disabled(true), MenuItem::new(1, "AMD Radeon"), MenuItem::new(2, "Chrome"), MenuItem::separator(), MenuItem::new(0, "Your skins").disabled(true), MenuItem::new(3, "Classic")]
    }

    #[test]
    fn the_arrows_skip_titles_and_lines() {
        let it = items();
        assert_eq!(next_pick(&it, None, 1), Some(1));
        assert_eq!(next_pick(&it, Some(2), 1), Some(5));
        assert_eq!(next_pick(&it, Some(5), 1), Some(5), "stays at the end");
        assert_eq!(next_pick(&it, Some(1), -1), Some(1), "stays at the top");
        assert_eq!(next_pick(&it, None, -1), Some(5));
    }

    #[test]
    fn a_letter_jumps_to_the_next_choice_with_it() {
        let it = items();
        assert_eq!(by_letter(&it, None, 'c'), Some(2));
        assert_eq!(by_letter(&it, Some(2), 'C'), Some(5));
        assert_eq!(by_letter(&it, Some(5), 'c'), Some(2), "round the end");
        assert_eq!(by_letter(&it, None, 'd'), None, "a group's title isn't a choice");
    }

    #[test]
    fn rows_stack_with_thin_lines() {
        let r = rows(&items());
        assert_eq!(r[3], (3.0 * ROW_H, LINE_H));
        assert_eq!(r[4].0, 3.0 * ROW_H + LINE_H);
    }
}
