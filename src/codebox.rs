//! Text-box helpers shared by the windows that edit text themselves, like the sort order's rules
//! box (sortwin.rs): a scrollbar ([`Track`]), the clipboard, the held keys, and moving by
//! characters and lines in a string.

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
#[cfg(not(test))]
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;

use crate::render::Renderer;
use crate::theme::{Color, Theme};
use crate::ui::{Point, Rect};

/// The red of something Glint doesn't understand.
pub const PROBLEM_RED: Color = Color::hex(0xe5484d);

/// A scrollbar as last drawn: where it runs, its thumb, and how far its page scrolls.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Track {
    rect: Rect,
    thumb: Rect,
    max: f32,
    /// Along the bottom (scrolling sideways).
    across: bool,
}

impl Track {
    /// The bar for a page `content` long with `view` of it showing, scrolled `offset`; None when
    /// it all shows.
    pub(crate) fn new(rect: Rect, content: f32, view: f32, offset: f32, across: bool) -> Option<Track> {
        let max = (content - view).max(0.0);
        let len = if across { rect.w } else { rect.h };
        if max <= 0.0 || view <= 0.0 || len <= 0.0 {
            return None;
        }
        let t = (len * view / content).clamp(24.0_f32.min(len), len);
        let at = (len - t) * (offset / max).clamp(0.0, 1.0);
        let thumb = if across { Rect::new(rect.x + at, rect.y, t, rect.h) } else { Rect::new(rect.x, rect.y + at, rect.w, t) };
        Some(Track { rect, thumb, max, across })
    }

    /// Whether `p` is on it (a little wider than drawn, to be easy to hit).
    pub(crate) fn hit(&self, p: Point) -> bool {
        if self.across { self.rect.inset(0.0, -3.0) } else { self.rect.inset(-3.0, 0.0) }.contains(p)
    }

    /// Where on the thumb it's held: where it was pressed, or its middle when the track was.
    pub(crate) fn grab(&self, p: Point) -> f32 {
        match (self.across, self.thumb.inset(-3.0, -3.0).contains(p)) {
            (true, true) => p.x - self.thumb.x,
            (false, true) => p.y - self.thumb.y,
            (true, false) => self.thumb.w / 2.0,
            (false, false) => self.thumb.h / 2.0,
        }
    }

    /// The scroll that puts the thumb, held at `grab`, under `p`.
    pub(crate) fn offset_at(&self, p: Point, grab: f32) -> f32 {
        let (len, t, start, at) = if self.across { (self.rect.w, self.thumb.w, self.rect.x, p.x) } else { (self.rect.h, self.thumb.h, self.rect.y, p.y) };
        ((at - grab - start) / (len - t).max(1.0)).clamp(0.0, 1.0) * self.max
    }

    pub(crate) fn paint(&self, r: &mut Renderer, theme: &Theme, hot: bool) {
        let radius = self.thumb.w.min(self.thumb.h) / 2.0;
        r.fill_paint(self.thumb, radius, if hot { &theme.scroll_thumb_hover_paint } else { &theme.scroll_thumb_paint });
    }
}

#[cfg(not(test))]
pub(crate) fn key_down(vk: u16) -> bool {
    // SAFETY: a plain key-state query.
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// In tests no key is held: the real keyboard (someone's Ctrl, a tester's automation) mustn't
/// change what typing does.
#[cfg(test)]
pub(crate) fn key_down(_vk: u16) -> bool {
    false
}

/// The text on the clipboard, if any.
pub(crate) fn clipboard_text(owner: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and closed here; the memory is locked only while copied.
    unsafe {
        // Another program may have it open for a moment (a clipboard manager, or Remote Desktop,
        // reading what was just copied): it's tried a few times.
        let mut tries = 0;
        while OpenClipboard(Some(owner)).is_err() {
            tries += 1;
            if tries == 5 {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let text = GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(|h: HANDLE| {
            let mem = HGLOBAL(h.0);
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                return None;
            }
            let max = GlobalSize(mem) / 2;
            let len = (0..max).find(|&i| *p.add(i) == 0).unwrap_or(max);
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
}

/// The byte offset of the start of the line `pos` is on, and the line's number (from 0).
pub(crate) fn line_of(text: &str, pos: usize) -> (usize, usize) {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    (start, text[..start].matches('\n').count())
}

pub(crate) fn line_end(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |i| pos + i)
}

/// The byte offset of column `col` (in characters) of the line starting at `start`.
pub(crate) fn at_col(text: &str, start: usize, col: usize) -> usize {
    let end = line_end(text, start);
    text[start..end].char_indices().nth(col).map_or(end, |(i, _)| start + i)
}

pub(crate) fn col_of(text: &str, pos: usize) -> usize {
    let (start, _) = line_of(text, pos);
    text[start..pos].chars().count()
}

pub(crate) fn prev_char(text: &str, pos: usize) -> usize {
    text[..pos].char_indices().next_back().map_or(0, |(i, _)| i)
}

pub(crate) fn next_char(text: &str, pos: usize) -> usize {
    text[pos..].chars().next().map_or(pos, |c| pos + c.len_utf8())
}

/// The whole lines the selection `a..b` is on: from the start of the first to the end of the
/// last (one ending at the very start of a line leaves that line out).
pub(crate) fn whole_lines(text: &str, a: usize, b: usize) -> (usize, usize) {
    let first = line_of(text, a).0;
    let b = if b > a && b > first && line_of(text, b).0 == b { b - 1 } else { b };
    (first, line_end(text, b))
}

/// Alt+Up / Alt+Down: the whole lines `first..last` (from [`whole_lines`]) swapped with the line
/// above or below. The part of the text that changes, what it becomes, and how far the lines went
/// (bytes); None at the top or the bottom.
pub(crate) fn swap_lines(text: &str, first: usize, last: usize, up: bool) -> Option<((usize, usize), String, isize)> {
    let lines = &text[first..last];
    if up {
        let above = line_of(text, first.checked_sub(1)?).0;
        let other = &text[above..first - 1];
        Some(((above, last), format!("{lines}\n{other}"), -((other.len() + 1) as isize)))
    } else {
        if last >= text.len() {
            return None;
        }
        let end = line_end(text, last + 1);
        let other = &text[last + 1..end];
        Some(((first, end), format!("{other}\n{lines}"), (other.len() + 1) as isize))
    }
}
