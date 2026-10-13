//! A box for typing code, drawn by Glint in a monospaced font, like the sort order's rules box
//! (sortwin.rs): line numbers down its left side (a red dot by a line with a problem), words
//! coloured by what they are, a red line under what Glint couldn't understand, and suggestions like
//! a code editor's - the first one as grey text after the cursor, up to eight in a list under the
//! line (Tab, or Enter while the list is open, takes one; Up / Down pick; Esc closes it). Faint
//! words after the cursor's line say what it does (or what's wrong with it).
//!
//! It isn't a window: the window it's in (the skin customiser) gives it its place, paints it, and
//! passes it the mouse and the keys while it has the keyboard. What it suggests and how it colours
//! the words come from a [`Lang`]. Keys: the usual (arrows, Home / End, Ctrl for whole words, Shift
//! to select, Page Up / Down), Ctrl+A / C / X / V, Ctrl+Z / Y (typing is taken back a word at a
//! time), Ctrl+/ makes lines notes and back, Alt+Up / Alt+Down move them.

use std::time::Instant;

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_OEM_2,
    VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP, VK_V, VK_X, VK_Y, VK_Z,
};

use crate::complete::Completion;
use crate::kit::{self, size as ksize, Bar, Look};
use crate::render::{Align, Renderer, TextStyle};
use crate::shell;
use crate::theme::{Color, Theme};
use crate::ui::{Point, Rect};

/// The text's size and line height.
pub const MONO: f32 = 12.5;
pub const LINE_H: f32 = 19.0;
const BOX_PAD: f32 = 8.0;
/// The line numbers' column: room for a problem's mark, and the gap after the numbers.
const GUTTER_MARK: f32 = 12.0;
const GUTTER_GAP: f32 = 10.0;
const POPUP_ROW: f32 = 22.0;
const POPUP_MAX: usize = 8;
const POPUP_W: f32 = 380.0;
/// Typing that goes on within this long (seconds) is taken back by one Ctrl+Z.
const GROUP_SECS: f64 = 2.0;
/// The red of something Glint doesn't understand.
pub const PROBLEM_RED: Color = Color::hex(0xe5484d);

/// What a language knows: what fits where the cursor is, what a line does, how its words look.
pub trait Lang {
    /// Suggestions for the cursor's place.
    fn complete(&self, text: &str, caret: usize) -> Completion;
    /// What line `row` does, in a few words (None: nothing to say).
    fn explain(&self, row: &str) -> Option<String>;
    /// The line's pieces and their colours: (from, to) in bytes of `row`. What's left out is in
    /// the text colour.
    fn ink(&self, row: &str, theme: &Theme) -> Vec<(usize, usize, Color)>;
    /// A colour to show as a little square at the end of the line (a colour's value).
    fn swatch(&self, _row: &str) -> Option<Color> {
        None
    }
    /// Whether a line is a note (Ctrl+/ makes lines notes and back with this mark).
    fn note_mark(&self) -> &'static str {
        "//"
    }
}

/// A problem on a line (from 1), shown as a mark by its number, a red line under it and, when the
/// cursor is on that line, its words after it.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub line: usize,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Edit {
    Typing,
    Deleting,
    Other,
}

/// What a key or the mouse did, for the window round the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Done {
    /// Not the box's (Tab with no suggestions, Esc with nothing to close): the window's.
    NotMine,
    /// Only the look changed (the cursor moved).
    Moved,
    /// The text changed.
    Changed,
}

/// A scrollbar as last drawn.
#[derive(Clone, Copy, Debug)]
struct Track {
    rect: Rect,
    thumb: Rect,
    max: f32,
    across: bool,
}

impl Track {
    fn new(rect: Rect, content: f32, view: f32, offset: f32, across: bool) -> Option<Track> {
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

    fn hit(&self, p: Point) -> bool {
        if self.across { self.rect.inset(0.0, -3.0) } else { self.rect.inset(-3.0, 0.0) }.contains(p)
    }

    fn grab(&self, p: Point) -> f32 {
        match (self.across, self.thumb.inset(-3.0, -3.0).contains(p)) {
            (true, true) => p.x - self.thumb.x,
            (false, true) => p.y - self.thumb.y,
            (true, false) => self.thumb.w / 2.0,
            (false, false) => self.thumb.h / 2.0,
        }
    }

    fn offset_at(&self, p: Point, grab: f32) -> f32 {
        let (len, t, start, at) = if self.across { (self.rect.w, self.thumb.w, self.rect.x, p.x) } else { (self.rect.h, self.thumb.h, self.rect.y, p.y) };
        ((at - grab - start) / (len - t).max(1.0)).clamp(0.0, 1.0) * self.max
    }

    fn paint(&self, r: &mut Renderer, theme: &Theme, hot: bool) {
        let radius = self.thumb.w.min(self.thumb.h) / 2.0;
        r.fill_paint(self.thumb, radius, if hot { &theme.scroll_thumb_hover_paint } else { &theme.scroll_thumb_paint });
    }
}

#[derive(Default)]
pub struct CodeBox {
    pub text: String,
    caret: usize,
    anchor: usize,
    want_col: Option<usize>,
    undo: Vec<(String, usize)>,
    redo: Vec<(String, usize)>,
    last_edit: Option<(Edit, usize, Option<Instant>)>,
    /// Across, down (DIPs).
    scroll: (f32, f32),
    char_w: f32,
    /// Where it is (the window's DIPs), and where the window's room ends (for the list).
    pub rect: Rect,
    room: Rect,
    /// Built-in skins: shown, not changed.
    pub read_only: bool,
    /// It has the keyboard.
    pub focused: bool,
    completion: Completion,
    pick: usize,
    list_closed: bool,
    marks: Vec<Mark>,
    selecting: bool,
    /// A scrollbar held: down (false) or across (true), and where its thumb is held.
    drag: Option<(bool, f32)>,
    vtrack: Option<Track>,
    htrack: Option<Track>,
    hover_bar: Option<bool>,
    hover_pick: Option<usize>,
    suggestion_rects: Vec<Rect>,
}

impl CodeBox {
    /// New text (another skin, or the skin changed elsewhere): the cursor at the top, no undo.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.caret = 0;
        self.anchor = 0;
        self.undo.clear();
        self.redo.clear();
        self.last_edit = None;
        self.scroll = (0.0, 0.0);
        self.list_closed = true;
        self.completion = Completion::default();
    }

    pub fn set_marks(&mut self, marks: Vec<Mark>) {
        self.marks = marks;
    }

    /// The cursor to the start of line `n` (from 1), with the keyboard.
    pub fn go_to_line(&mut self, n: usize) {
        let mut start = 0;
        for _ in 1..n {
            match self.text[start..].find('\n') {
                Some(k) => start += k + 1,
                None => break,
            }
        }
        (self.caret, self.anchor) = (start, start);
        self.list_closed = true;
        self.reveal_caret();
    }

    /// The font's width changed (another DPI, another skin).
    pub fn forget_font(&mut self) {
        self.char_w = 0.0;
    }

    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    fn edit(&mut self, a: usize, b: usize, with: &str, kind: Edit, lang: &dyn Lang) -> Done {
        if self.read_only {
            return Done::Moved;
        }
        let goes_on = kind != Edit::Other
            && self.caret == self.anchor
            && !(kind == Edit::Typing && with.starts_with(char::is_whitespace))
            && self.last_edit.is_some_and(|(k, at, when)| k == kind && at == self.caret && when.is_some_and(|w| w.elapsed().as_secs_f64() < GROUP_SECS));
        if !goes_on {
            self.undo.push((self.text.clone(), self.caret));
            if self.undo.len() > 300 {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.text.replace_range(a..b, with);
        self.caret = a + with.len();
        self.anchor = self.caret;
        self.last_edit = Some((kind, self.caret, Some(Instant::now())));
        self.list_closed = false;
        self.want_col = None;
        self.suggest(lang);
        self.reveal_caret();
        Done::Changed
    }

    fn insert(&mut self, s: &str, lang: &dyn Lang) -> Done {
        let (a, b) = self.selection();
        self.edit(a, b, s, Edit::Other, lang)
    }

    fn undo_redo(&mut self, redo: bool, lang: &dyn Lang) -> Done {
        if self.read_only {
            return Done::Moved;
        }
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some((text, caret)) = from.pop() else { return Done::Moved };
        to.push((std::mem::replace(&mut self.text, text), self.caret));
        self.caret = caret.min(self.text.len());
        while !self.text.is_char_boundary(self.caret) {
            self.caret -= 1;
        }
        self.anchor = self.caret;
        self.last_edit = None;
        self.list_closed = true;
        self.suggest(lang);
        self.reveal_caret();
        Done::Changed
    }

    /// Works out the suggestions for the cursor's place.
    fn suggest(&mut self, lang: &dyn Lang) {
        let c = lang.complete(&self.text, self.caret);
        if c.items.len() != self.completion.items.len() || c.start != self.completion.start {
            self.pick = 0;
        }
        self.pick = self.pick.min(c.items.len().saturating_sub(1));
        self.completion = c;
    }

    fn list_open(&self) -> bool {
        self.focused && !self.read_only && !self.list_closed && !self.completion.items.is_empty() && self.caret == self.anchor
    }

    fn accept(&mut self, i: usize, lang: &dyn Lang) -> Done {
        let Some(s) = self.completion.items.get(i).cloned() else { return Done::Moved };
        let (a, b) = (self.completion.start, self.completion.end.min(self.text.len()));
        self.anchor = a;
        self.caret = b;
        let done = self.insert(&s.insert, lang);
        // A name taken ("accent: ") goes on to its values; a finished value closes the list.
        if !s.insert.ends_with(' ') {
            self.list_closed = true;
        }
        done
    }

    fn move_to(&mut self, pos: usize, keep_col: bool) {
        self.caret = pos.min(self.text.len());
        self.last_edit = None;
        if !key_down(VK_SHIFT.0) {
            self.anchor = self.caret;
        }
        if !keep_col {
            self.want_col = None;
        }
        self.reveal_caret();
    }

    fn line_move(&mut self, lines: i32) {
        let col = *self.want_col.get_or_insert(col_of(&self.text, self.caret));
        let (start, _) = line_of(&self.text, self.caret);
        let mut target = start;
        if lines < 0 {
            for _ in 0..-lines {
                if target == 0 {
                    break;
                }
                target = line_of(&self.text, target - 1).0;
            }
        } else {
            for _ in 0..lines {
                let end = line_end(&self.text, target);
                if end >= self.text.len() {
                    break;
                }
                target = end + 1;
            }
        }
        if target == start && (lines < 0 && start == 0 || lines > 0 && line_end(&self.text, start) == self.text.len()) {
            let to = if lines < 0 { 0 } else { self.text.len() };
            self.move_to(to, true);
            return;
        }
        let pos = at_col(&self.text, target, col);
        self.move_to(pos, true);
    }

    fn gutter_w(&self) -> f32 {
        let digits = (self.text.matches('\n').count() + 1).to_string().len().max(2);
        GUTTER_MARK + digits as f32 * self.char_w + GUTTER_GAP
    }

    fn reveal_caret(&mut self) {
        let e = self.rect;
        if e.w <= 0.0 || self.char_w <= 0.0 {
            return;
        }
        let (_, line) = line_of(&self.text, self.caret);
        let (vh, vw) = (e.h - 2.0 * BOX_PAD, e.w - 2.0 * BOX_PAD - self.gutter_w());
        let y = line as f32 * LINE_H;
        if y < self.scroll.1 {
            self.scroll.1 = y;
        } else if y + LINE_H > self.scroll.1 + vh {
            self.scroll.1 = y + LINE_H - vh;
        }
        let x = col_of(&self.text, self.caret) as f32 * self.char_w;
        if x < self.scroll.0 {
            self.scroll.0 = (x - 40.0).max(0.0);
        } else if x + 2.0 > self.scroll.0 + vw {
            self.scroll.0 = x + 40.0 - vw;
        }
    }

    fn pos_at(&self, p: Point) -> usize {
        let e = self.rect;
        let line = ((p.y - e.y - BOX_PAD + self.scroll.1) / LINE_H).floor().max(0.0) as usize;
        let col = ((p.x - e.x - BOX_PAD - self.gutter_w() + self.scroll.0) / self.char_w.max(1.0)).round().max(0.0) as usize;
        let mut start = 0;
        for _ in 0..line {
            let end = line_end(&self.text, start);
            if end >= self.text.len() {
                return self.text.len();
            }
            start = end + 1;
        }
        at_col(&self.text, start, col)
    }

    fn caret_point(&self, pos: usize) -> Point {
        let e = self.rect;
        let (_, line) = line_of(&self.text, pos);
        Point::new(e.x + BOX_PAD + self.gutter_w() + col_of(&self.text, pos) as f32 * self.char_w - self.scroll.0, e.y + BOX_PAD + line as f32 * LINE_H - self.scroll.1)
    }

    fn widest(&self) -> f32 {
        let chars = self.text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0);
        chars as f32 * self.char_w + 48.0
    }

    // ------------------------------------------------------------ painting

    /// Draws the box at `rect`; `room` is where the suggestions' list may go (the window).
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, lang: &dyn Lang, rect: Rect, room: Rect) {
        if self.char_w <= 0.0 {
            self.char_w = r.mono_char_width(MONO);
        }
        if rect != self.rect {
            self.rect = rect;
            self.reveal_caret();
        }
        self.room = room;
        let e = rect;
        let cw = self.char_w;
        let list_open = self.list_open();
        let ghost = if list_open { self.completion.ghost(&self.text) } else { None };
        let gutter = self.gutter_w();
        let (caret_start, caret_line) = line_of(&self.text, self.caret);
        let caret_row = &self.text[caret_start..line_end(&self.text, self.caret)];
        let caret_row_chars = caret_row.chars().count();
        let (sel_a, sel_b) = self.selection();
        // What the cursor's line does (or what's wrong with it), in faint words after it.
        let problem = self.marks.iter().find(|m| m.line == caret_line + 1).map(|m| m.text.clone());
        let explained = if self.focused && ghost.is_none() && sel_a == sel_b && !list_open { problem.clone().or_else(|| lang.explain(caret_row)) } else { None };
        let bar = Bar::of(theme);
        let (view_h, view_w) = (e.h - 2.0 * BOX_PAD, e.w - 2.0 * BOX_PAD - gutter);
        let lines = self.text.matches('\n').count() + 1;
        self.vtrack = Track::new(bar.track(e), lines as f32 * LINE_H, view_h, self.scroll.1, false);
        let across = Rect::new(e.x + BOX_PAD + gutter, e.bottom() - bar.w - bar.margin, (view_w - bar.w - bar.margin).max(0.0), bar.w);
        self.htrack = Track::new(across, self.widest(), view_w, self.scroll.0, true);
        kit::field(r, theme, e, None, self.focused, Look::enabled());
        let inner = e.inset(BOX_PAD, BOX_PAD);
        r.push_clip(inner);
        let number_w = gutter - GUTTER_MARK - GUTTER_GAP;
        let text_area = Rect::new(inner.x + gutter, inner.y, (inner.w - gutter).max(0.0), inner.h);
        let mut start = 0;
        let mut line = 0;
        loop {
            let end = line_end(&self.text, start);
            let y = inner.y + line as f32 * LINE_H - self.scroll.1;
            if y > inner.bottom() {
                break;
            }
            if y + LINE_H >= inner.y {
                let marked = self.marks.iter().any(|m| m.line == line + 1);
                if line == caret_line && self.focused && sel_a == sel_b {
                    r.fill_rect(Rect::new(text_area.x - 4.0, y, text_area.w + 4.0, LINE_H), theme.accent.with_alpha(0.06));
                }
                let number = (line + 1).to_string();
                let nx = inner.x + GUTTER_MARK + number_w - number.len() as f32 * cw;
                r.mono_text(&number, Rect::new(nx, y, number.len() as f32 * cw + cw, LINE_H), MONO, if line == caret_line { theme.text } else { theme.muted.with_alpha(0.7) });
                if marked {
                    r.text("\u{25cf}", Rect::new(inner.x, y, GUTTER_MARK, LINE_H), &TextStyle::new(PROBLEM_RED).size(8.0));
                }
                r.push_clip(text_area);
                let row = &self.text[start..end];
                let x0 = text_area.x - self.scroll.0;
                let col = |pos: usize| row[..pos.min(row.len())].chars().count() as f32;
                if sel_a != sel_b && sel_a <= end && sel_b >= start {
                    let (a, b) = (col(sel_a.max(start) - start), col(sel_b.min(end) - start) + if sel_b > end { 0.5 } else { 0.0 });
                    let wash = if theme.era.old() { theme.accent_tint.with_alpha(0.3) } else { theme.accent_tint };
                    r.fill_rect(Rect::new(x0 + a * cw, y, ((b - a) * cw).max(2.0), LINE_H), wash);
                }
                // The words, coloured as the language says (one run of each colour at a time).
                let ink = lang.ink(row, theme);
                let mut at = 0;
                let mut pieces: Vec<(usize, usize, Color)> = Vec::new();
                for &(a, b, c) in &ink {
                    let (a, b) = (a.clamp(at, row.len()), b.min(row.len()));
                    if a > at {
                        pieces.push((at, a, theme.text));
                    }
                    if b > a {
                        pieces.push((a, b, c));
                        at = b;
                    }
                }
                if at < row.len() {
                    pieces.push((at, row.len(), theme.text));
                }
                for (a, b, c) in pieces {
                    let (a, b) = (fix_boundary(row, a), fix_boundary(row, b));
                    let piece = &row[a..b];
                    if piece.trim().is_empty() {
                        continue;
                    }
                    r.mono_text(piece, Rect::new(x0 + col(a) * cw, y, piece.chars().count() as f32 * cw + cw, LINE_H), MONO, c);
                }
                // A colour's little square after the line.
                if let Some(c) = lang.swatch(row) {
                    let x = x0 + (row.trim_end().chars().count() as f32 + 1.0) * cw;
                    let sq = Rect::new(x, y + 4.0, LINE_H - 8.0, LINE_H - 8.0);
                    r.fill_rect(sq, c);
                    r.stroke_rect(sq, 1.0, theme.muted.with_alpha(0.6));
                }
                // A red line under a line Glint couldn't understand.
                if marked {
                    let code = row.trim_end();
                    let lead = (code.len() - code.trim_start().len()) as f32;
                    let n = code.chars().count() as f32 - lead;
                    if n > 0.0 {
                        r.fill_rect(Rect::new(x0 + lead * cw, y + LINE_H - 3.0, n * cw, 1.5), PROBLEM_RED);
                    }
                }
                r.pop_clip();
            }
            if end >= self.text.len() {
                break;
            }
            start = end + 1;
            line += 1;
        }
        r.push_clip(text_area);
        if self.focused {
            let c = self.caret_point(self.caret);
            if let Some(ghost) = ghost {
                let ghost: String = ghost.chars().take(80).collect();
                r.mono_text(&ghost, Rect::new(c.x, c.y, ghost.chars().count() as f32 * cw + cw, LINE_H), MONO, theme.muted.with_alpha(0.75));
            } else if let Some(said) = explained {
                let x = text_area.x - self.scroll.0 + (caret_row_chars + 3) as f32 * cw;
                let room = text_area.right() - x - if self.vtrack.is_some() { bar.w + 2.0 * bar.margin } else { 0.0 };
                if room >= 90.0 {
                    let color = if problem.is_some() { PROBLEM_RED.with_alpha(0.85) } else { theme.muted.with_alpha(0.7) };
                    r.text(&said, Rect::new(x, c.y, room, LINE_H), &TextStyle::new(color).size(ksize::NOTE_FONT));
                }
            }
            if !self.read_only {
                r.fill_rect(Rect::new(c.x.round() - 0.5, c.y + 2.0, 1.5, LINE_H - 4.0), theme.text);
            }
        }
        r.pop_clip();
        r.pop_clip();
        if let Some(t) = self.vtrack {
            t.paint(r, theme, self.hover_bar == Some(false) || self.drag.is_some_and(|(a, _)| !a));
        }
        if let Some(t) = self.htrack {
            t.paint(r, theme, self.hover_bar == Some(true) || self.drag.is_some_and(|(a, _)| a));
        }
    }

    /// The suggestions' list under the cursor's line (drawn last, over everything).
    pub fn paint_list(&mut self, r: &mut Renderer, theme: &Theme) {
        self.suggestion_rects.clear();
        if !self.list_open() {
            return;
        }
        let room = self.room;
        let at = self.caret_point(self.completion.start);
        let n = self.completion.items.len().min(POPUP_MAX);
        let first = self.pick.saturating_sub(POPUP_MAX - 1).min(self.completion.items.len().saturating_sub(n));
        let w = POPUP_W.min(room.w - 16.0);
        let x = at.x.clamp(room.x + 8.0, (room.right() - 8.0 - w).max(room.x + 8.0));
        let mut y = at.y + LINE_H + 2.0;
        let h = n as f32 * POPUP_ROW + 6.0;
        if y + h > room.bottom() - 4.0 {
            y = at.y - h - 2.0;
        }
        let bg = Rect::new(x, y, w, h);
        let rounded = kit::panels(theme);
        let radius = if rounded { theme.corner(h).min(8.0) } else { 0.0 };
        if rounded {
            r.fill_round_rect(Rect::new(bg.x + 1.0, bg.y + 3.0, bg.w, bg.h), radius, Color::hex(0).with_alpha(0.22));
            r.fill_paint(bg, radius, &theme.sidebar_panel);
            let edge = if theme.input_border.a > 0.0 { theme.input_border } else { theme.line };
            r.stroke_round_rect(bg.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, edge);
        } else {
            r.fill_rect(Rect::new(bg.x + 2.0, bg.y + 3.0, bg.w, bg.h), Color::hex(0).with_alpha(0.18));
            r.fill_rect(bg, theme.panel);
            r.stroke_rect(bg, 1.0, theme.line);
        }
        for (k, s) in self.completion.items.iter().enumerate().skip(first).take(n) {
            let row = Rect::new(x + 3.0, y + 3.0 + (k - first) as f32 * POPUP_ROW, w - 6.0, POPUP_ROW);
            let picked = k == self.pick;
            if rounded {
                if picked {
                    r.fill_paint(row, (radius - 2.0).max(0.0), &theme.accent_tint_paint);
                } else if self.hover_pick == Some(k) {
                    r.fill_paint(row, (radius - 2.0).max(0.0), &theme.hover_paint);
                }
            } else if picked {
                r.fill_rect(row, theme.accent_tint);
                r.fill_rect(Rect::new(row.x, row.y, 2.0, row.h), theme.select_edge);
            } else if self.hover_pick == Some(k) {
                r.fill_rect(row, theme.hover);
            }
            let label_w = (s.label.chars().count() as f32 * self.char_w + 12.0).min(row.w * 0.5);
            let (label_color, hint_color) = match (picked, rounded) {
                (true, true) => (theme.selection_text, theme.selection_details),
                (true, false) if theme.era.old() => (theme.selection_text, theme.selection_details),
                (true, false) => (theme.text_strong, theme.muted),
                _ => (theme.text, theme.muted),
            };
            r.mono_text(&s.label, Rect::new(row.x + 8.0, row.y, label_w, row.h), MONO, label_color);
            r.text(&s.hint, Rect::new(row.x + 14.0 + label_w, row.y, (row.w - label_w - 20.0).max(0.0), row.h), &TextStyle::new(hint_color).size(11.0));
            self.suggestion_rects.push(row);
        }
        if first > 0 || first + n < self.completion.items.len() {
            let more = format!("{} of {}", self.pick + 1, self.completion.items.len());
            r.text(&more, Rect::new(x, y + h - 14.0, w - 8.0, 12.0), &TextStyle::new(theme.muted).size(9.5).align(Align::Right));
        }
    }

    // ------------------------------------------------------------ the mouse

    /// Whether `p` is the box's (the box, or its open list).
    pub fn contains(&self, p: Point) -> bool {
        self.rect.contains(p) || self.suggestion_rects.iter().any(|r| r.contains(p))
    }

    /// Selecting with the mouse, or dragging a scrollbar: the box wants every move.
    pub fn busy(&self) -> bool {
        self.selecting || self.drag.is_some()
    }

    /// A press in the box (the window captures the mouse when this says `true`).
    pub fn mouse_down(&mut self, p: Point, double: bool, lang: &dyn Lang) -> (Done, bool) {
        if let Some(k) = self.suggestion_rects.iter().position(|r| r.contains(p)) {
            return (self.accept(k, lang), false);
        }
        for (across, t) in [(false, self.vtrack), (true, self.htrack)] {
            if let Some(t) = t.filter(|t| t.hit(p)) {
                let grab = t.grab(p);
                self.drag = Some((across, grab));
                self.set_scroll(across, t.offset_at(p, grab));
                return (Done::Moved, true);
            }
        }
        let pos = self.pos_at(p);
        let mut capture = false;
        if double {
            let mut a = pos;
            while a > 0 && is_word(self.text[..a].chars().next_back().unwrap_or(' ')) {
                a = prev_char(&self.text, a);
            }
            let mut b = pos;
            while b < self.text.len() && is_word(self.text[b..].chars().next().unwrap_or(' ')) {
                b = next_char(&self.text, b);
            }
            (self.anchor, self.caret) = (a, b);
        } else {
            self.move_to(pos, false);
            self.selecting = true;
            capture = true;
        }
        self.list_closed = true;
        self.suggest(lang);
        (Done::Moved, capture)
    }

    fn set_scroll(&mut self, across: bool, to: f32) {
        if across {
            self.scroll.0 = to;
        } else {
            self.scroll.1 = to;
        }
    }

    /// The mouse moved: true when the look changed.
    pub fn mouse_move(&mut self, p: Point) -> bool {
        if let Some((across, grab)) = self.drag {
            if let Some(t) = if across { self.htrack } else { self.vtrack } {
                self.set_scroll(across, t.offset_at(p, grab));
            }
            return true;
        }
        if self.selecting {
            let pos = self.pos_at(p);
            if pos != self.caret {
                self.caret = pos;
                self.reveal_caret();
                return true;
            }
            return false;
        }
        let pick = self.suggestion_rects.iter().position(|r| r.contains(p));
        let bar = if self.vtrack.is_some_and(|t| t.hit(p)) {
            Some(false)
        } else if self.htrack.is_some_and(|t| t.hit(p)) {
            Some(true)
        } else {
            None
        };
        let changed = pick != self.hover_pick || bar != self.hover_bar;
        (self.hover_pick, self.hover_bar) = (pick, bar);
        changed
    }

    pub fn mouse_up(&mut self) {
        self.selecting = false;
        self.drag = None;
    }

    pub fn mouse_leave(&mut self) {
        self.hover_pick = None;
        self.hover_bar = None;
    }

    /// The wheel over the box (`notches` up; with `across`, sideways).
    pub fn wheel(&mut self, notches: f32, across: bool) {
        if across {
            let max = (self.widest() - (self.rect.w - 2.0 * BOX_PAD - self.gutter_w())).max(0.0);
            self.scroll.0 = (self.scroll.0 - notches * 6.0 * self.char_w.max(4.0)).clamp(0.0, max);
        } else {
            let lines = self.text.matches('\n').count() as f32 + 1.0;
            let max = (lines * LINE_H - (self.rect.h - 2.0 * BOX_PAD)).max(0.0);
            self.scroll.1 = (self.scroll.1 - notches * 3.0 * LINE_H).clamp(0.0, max);
        }
    }

    // ------------------------------------------------------------ the keys

    /// A typed character.
    pub fn char(&mut self, c: char, lang: &dyn Lang) -> Done {
        if c.is_control() || (key_down(VK_CONTROL.0) && !key_down(VK_MENU.0)) {
            return Done::Moved;
        }
        let (a, b) = self.selection();
        self.edit(a, b, c.encode_utf8(&mut [0; 4]), Edit::Typing, lang)
    }

    /// A key while the box has the keyboard (`owner`: for the clipboard).
    pub fn key(&mut self, vk: u16, owner: HWND, lang: &dyn Lang) -> Done {
        let ctrl = key_down(VK_CONTROL.0);
        let alt = key_down(VK_MENU.0);
        let open = self.list_open();
        let text_len = self.text.len();
        let mut done = Done::Moved;
        match vk {
            _ if vk == VK_ESCAPE.0 => {
                if open {
                    self.list_closed = true;
                } else if self.caret != self.anchor {
                    self.anchor = self.caret;
                } else {
                    return Done::NotMine;
                }
            }
            _ if alt && !ctrl && (vk == VK_UP.0 || vk == VK_DOWN.0) => done = self.move_line(vk == VK_UP.0, lang),
            _ if alt && !ctrl => return Done::NotMine,
            _ if vk == VK_TAB.0 => {
                if !open {
                    return Done::NotMine;
                }
                done = self.accept(self.pick, lang);
            }
            _ if vk == VK_RETURN.0 => {
                if open {
                    done = self.accept(self.pick, lang);
                } else {
                    done = self.insert("\n", lang);
                    self.list_closed = true;
                }
            }
            _ if vk == VK_UP.0 || vk == VK_DOWN.0 => {
                let down = vk == VK_DOWN.0;
                if open {
                    let n = self.completion.items.len();
                    self.pick = if down { (self.pick + 1) % n } else { (self.pick + n - 1) % n };
                } else {
                    // Moving between lines never opens a list; only typing does.
                    self.line_move(if down { 1 } else { -1 });
                    self.list_closed = true;
                    self.suggest(lang);
                }
            }
            _ if vk == VK_PRIOR.0 || vk == VK_NEXT.0 => {
                let lines = ((self.rect.h - 2.0 * BOX_PAD) / LINE_H) as i32;
                self.line_move(if vk == VK_NEXT.0 { lines } else { -lines });
                self.list_closed = true;
            }
            _ if vk == VK_LEFT.0 || vk == VK_RIGHT.0 => {
                let right = vk == VK_RIGHT.0;
                let (a, b) = self.selection();
                let pos = if a != b && !key_down(VK_SHIFT.0) {
                    if right {
                        b
                    } else {
                        a
                    }
                } else if ctrl {
                    if right {
                        word_right(&self.text, self.caret)
                    } else {
                        word_left(&self.text, self.caret)
                    }
                } else if right {
                    next_char(&self.text, self.caret)
                } else {
                    prev_char(&self.text, self.caret)
                };
                self.move_to(pos, false);
                self.list_closed = true;
            }
            _ if vk == VK_HOME.0 || vk == VK_END.0 => {
                let pos = match (vk == VK_HOME.0, ctrl) {
                    (true, true) => 0,
                    (false, true) => text_len,
                    (true, false) => line_of(&self.text, self.caret).0,
                    (false, false) => line_end(&self.text, self.caret),
                };
                self.move_to(pos, false);
                self.list_closed = true;
            }
            _ if vk == VK_BACK.0 || vk == VK_DELETE.0 => {
                let (a, b) = self.selection();
                let kind = if a == b && !ctrl { Edit::Deleting } else { Edit::Other };
                let (a, b) = if a != b {
                    (a, b)
                } else if vk == VK_BACK.0 {
                    (if ctrl { word_left(&self.text, a) } else { prev_char(&self.text, a) }, a)
                } else {
                    (a, if ctrl { word_right(&self.text, a) } else { next_char(&self.text, a) })
                };
                if a != b {
                    done = self.edit(a, b, "", kind, lang);
                }
            }
            _ if ctrl && vk == VK_A.0 => (self.anchor, self.caret) = (0, text_len),
            _ if ctrl && (vk == VK_C.0 || vk == VK_X.0) => {
                let (a, b) = self.selection();
                if a != b {
                    let _ = shell::copy_text(owner, &self.text[a..b].replace('\n', "\r\n"));
                    if vk == VK_X.0 {
                        done = self.insert("", lang);
                    }
                }
            }
            _ if ctrl && vk == VK_V.0 => {
                if let Some(t) = clipboard_text(owner) {
                    let t = t.replace("\r\n", "\n").replace('\r', "\n").replace('\t', " ");
                    done = self.insert(&t, lang);
                    self.list_closed = true;
                }
            }
            _ if ctrl && vk == VK_Z.0 => done = self.undo_redo(key_down(VK_SHIFT.0), lang),
            _ if ctrl && vk == VK_Y.0 => done = self.undo_redo(true, lang),
            _ if ctrl && !alt && (vk == VK_OEM_2.0 || vk == VK_DIVIDE.0) => done = self.toggle_note(lang),
            _ => return Done::NotMine,
        }
        done
    }

    /// Ctrl+/: the cursor's lines become notes, or settings again.
    fn toggle_note(&mut self, lang: &dyn Lang) -> Done {
        if self.read_only {
            return Done::Moved;
        }
        let (a, b) = self.selection();
        let (first, last) = whole_lines(&self.text, a, b);
        let old = self.text[first..last].to_string();
        let new = toggle_notes(&old, lang.note_mark());
        if new == old {
            return Done::Moved;
        }
        let done = self.edit(first, last, &new, Edit::Other, lang);
        (self.anchor, self.caret) = (first, first + new.len());
        self.list_closed = true;
        done
    }

    /// Alt+Up / Alt+Down: the cursor's lines go up or down one.
    fn move_line(&mut self, up: bool, lang: &dyn Lang) -> Done {
        if self.read_only {
            return Done::Moved;
        }
        let (a, b) = self.selection();
        let (first, last) = whole_lines(&self.text, a, b);
        let Some(((from, to), new, shift)) = swap_lines(&self.text, first, last, up) else { return Done::Moved };
        let (caret, anchor) = (self.caret, self.anchor);
        let done = self.edit(from, to, &new, Edit::Other, lang);
        self.caret = (caret as isize + shift) as usize;
        self.anchor = (anchor as isize + shift) as usize;
        self.list_closed = true;
        self.reveal_caret();
        done
    }
}

fn key_down(vk: u16) -> bool {
    // SAFETY: a plain key-state query.
    unsafe { GetKeyState(vk as i32) < 0 }
}

/// The text on the clipboard, if any.
fn clipboard_text(owner: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and closed here; the memory is locked only while copied.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
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

fn fix_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The byte offset of the start of the line `pos` is on, and the line's number (from 0).
fn line_of(text: &str, pos: usize) -> (usize, usize) {
    let start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    (start, text[..start].matches('\n').count())
}

fn line_end(text: &str, pos: usize) -> usize {
    text[pos..].find('\n').map_or(text.len(), |i| pos + i)
}

fn at_col(text: &str, start: usize, col: usize) -> usize {
    let end = line_end(text, start);
    text[start..end].char_indices().nth(col).map_or(end, |(i, _)| start + i)
}

fn col_of(text: &str, pos: usize) -> usize {
    let (start, _) = line_of(text, pos);
    text[start..pos].chars().count()
}

fn prev_char(text: &str, pos: usize) -> usize {
    text[..pos].char_indices().next_back().map_or(0, |(i, _)| i)
}

fn next_char(text: &str, pos: usize) -> usize {
    text[pos..].chars().next().map_or(pos, |c| pos + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '#'
}

fn word_left(text: &str, pos: usize) -> usize {
    let mut p = pos;
    while p > 0 && !is_word(text[..p].chars().next_back().unwrap_or(' ')) {
        p = prev_char(text, p);
    }
    while p > 0 && is_word(text[..p].chars().next_back().unwrap_or(' ')) {
        p = prev_char(text, p);
    }
    p
}

fn word_right(text: &str, pos: usize) -> usize {
    let mut p = pos;
    while p < text.len() && is_word(text[p..].chars().next().unwrap_or(' ')) {
        p = next_char(text, p);
    }
    while p < text.len() && !is_word(text[p..].chars().next().unwrap_or('a')) && !text[p..].starts_with('\n') {
        p = next_char(text, p);
    }
    p
}

fn whole_lines(text: &str, a: usize, b: usize) -> (usize, usize) {
    let first = line_of(text, a).0;
    let b = if b > a && b > first && line_of(text, b).0 == b { b - 1 } else { b };
    (first, line_end(text, b))
}

/// Ctrl+/: the lines made notes (`mark` and a space after their indent), or settings again when
/// they all are notes. Blank lines stay as they are.
fn toggle_notes(lines: &str, mark: &str) -> String {
    let all = lines.split('\n').map(str::trim_start).filter(|l| !l.is_empty()).all(|l| l.starts_with(mark));
    let line = |l: &str| {
        let body = l.trim_start();
        let indent = &l[..l.len() - body.len()];
        if body.is_empty() {
            l.to_string()
        } else if all {
            let rest = body.strip_prefix(mark).unwrap_or(body);
            format!("{indent}{}", rest.strip_prefix(' ').unwrap_or(rest))
        } else {
            format!("{indent}{mark} {body}")
        }
    };
    lines.split('\n').map(line).collect::<Vec<_>>().join("\n")
}

fn swap_lines(text: &str, first: usize, last: usize, up: bool) -> Option<((usize, usize), String, isize)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_go_on_and_off() {
        assert_eq!(toggle_notes("accent: #fff\n\n  text: #000", "//"), "// accent: #fff\n\n  // text: #000");
        assert_eq!(toggle_notes("// accent: #fff\n  // text: #000", "//"), "accent: #fff\n  text: #000");
    }
}
