//! The question box: Glint's own small "are you sure?", drawn like the rest of Glint instead of a white
//! Windows message box. Glint asks before deleting for good (Shift+Del) and before a rename changes a
//! file's extension; what is asked lives in `AppState::question` (app/files.rs).
//!
//! While a question is open the window hands this box all mouse and keyboard input (window.rs):
//! - Enter, or a click on the red button, says yes;
//! - Esc, CANCEL, or a click anywhere outside the box says no;
//! - Tab and the arrow keys move between the two buttons.
//!
//! It dims the part of the window below the toolbar (the files the question is about) and sits in
//! the middle of it. In a skin with glass buttons (kit.rs `glass`) it is a rounded panel with
//! glass buttons. The toolbar is left as it is, because its filter box is a real Windows text box
//! that Glint's drawing can't cover.

use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB};

use crate::app::Question;
use crate::kit::{self, Look};
use crate::pathbar::{box_contains, fill_box, outline_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{Color, Theme};
use crate::ui::{Point, Rect};

/// What the window should do after the box handled something.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Nothing,
    Redraw,
    Yes,
    No,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Yes,
    No,
}

const WIDTH: f32 = 440.0;
const PAD: f32 = 22.0;
const TITLE_SIZE: f32 = 14.0;
const TITLE_LINE: f32 = 22.0;
const TEXT_LINE: f32 = 19.0;
const BUTTON_H: f32 = 30.0;
/// The buttons lean like SORT and SETTINGS (this much in AMD Radeon; as the skin says, `lean`).
const LEAN: f32 = 8.0;
const BUTTON_GAP: f32 = 6.0;

pub struct AskBox {
    /// The question last shown (a new one starts with the yes button in focus again).
    seen: u64,
    focus: Button,
    hover: Option<Button>,
    pressed: Option<Button>,
    panel: Rect,
    /// How far the buttons lean in the skin they were last drawn in.
    lean: f32,
    yes: Rect,
    no: Rect,
}

impl Default for AskBox {
    fn default() -> Self {
        AskBox { seen: 0, focus: Button::Yes, hover: None, pressed: None, panel: Rect::default(), lean: LEAN, yes: Rect::default(), no: Rect::default() }
    }
}

/// Splits `text` into lines that fit `width`, at spaces; at most `max` lines (the last one is cut
/// with "…" when drawn).
fn wrap(r: &mut Renderer, text: &str, style: &TextStyle, width: f32, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let longer = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
        if line.is_empty() || r.measure(&longer, style) <= width {
            line = longer;
        } else {
            lines.push(std::mem::replace(&mut line, word.to_string()));
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.len() > max {
        let rest = lines.split_off(max - 1).join(" ");
        lines.push(rest);
    }
    lines
}

impl AskBox {
    /// Whether question `id` has been drawn. Until then the box ignores the mouse and the keys: its
    /// buttons' places (and which one has the keyboard) are still the previous question's, and a
    /// click or key meant for something else (the second click of a double-click on a menu item,
    /// say) must not answer a question nobody has seen yet.
    pub fn shown(&self, id: u64) -> bool {
        self.seen == id
    }

    fn button_at(&self, p: Point) -> Option<Button> {
        if box_contains(self.yes, self.lean, p) {
            Some(Button::Yes)
        } else if box_contains(self.no, self.lean, p) {
            Some(Button::No)
        } else {
            None
        }
    }

    fn answer(b: Button) -> Answer {
        match b {
            Button::Yes => Answer::Yes,
            Button::No => Answer::No,
        }
    }

    /// Draws the question over `area` (the window below the toolbar).
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, q: &Question, area: Rect) {
        self.paint_question(r, theme, q.id, &q.title, &q.text, q.yes, area);
    }

    /// Draws a question over `area`: `id` tells questions apart, `yes` is the yes button's label
    /// ("DELETE"). The skin customiser asks this way too.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_question(&mut self, r: &mut Renderer, theme: &Theme, id: u64, title: &str, text: &str, yes: &str, area: Rect) {
        if id != self.seen {
            *self = AskBox { seen: id, ..AskBox::default() };
        }
        self.lean = theme.lean(LEAN);
        r.fill_rect(area, Color::rgba(0, 0, 0, if theme.dark { 0.5 } else { 0.28 }));

        let w = WIDTH.min(area.w - 32.0).max(220.0);
        let inner = w - 2.0 * PAD;
        let title_style = TextStyle::new(theme.text_strong).size(TITLE_SIZE).semibold();
        let text_style = TextStyle::new(theme.text);
        let title = wrap(r, title, &title_style, inner, 2);
        let text = wrap(r, text, &text_style, inner, 3);
        let h = 20.0 + title.len() as f32 * TITLE_LINE + 4.0 + text.len() as f32 * TEXT_LINE + 22.0 + BUTTON_H + 20.0;
        let x = area.x + ((area.w - w) / 2.0).max(0.0);
        let y = area.y + ((area.h - h) / 2.0 - 24.0).max(12.0);
        self.panel = Rect::new(x, y, w, h);

        let glass = kit::glass(theme);
        if glass {
            // A rounded panel on a soft shadow, with the shine along its top.
            let radius = theme.corner(h).max(6.0);
            r.fill_round_rect(Rect::new(x - 1.0, y + 2.0, w + 2.0, h + 3.0), radius + 1.0, Color::rgba(0, 0, 0, 0.22));
            r.fill_paint(self.panel, radius, &theme.toolbar_paint);
            r.stroke_round_rect(self.panel.inset(0.5, 0.5), radius - 0.5, 1.0, theme.line);
            if theme.highlight.a > 0.0 {
                r.fill_rect(Rect::new(x + radius, y + 1.0, (w - 2.0 * radius).max(0.0), 1.0), theme.highlight);
            }
        } else {
            // A hairline frame, the panel, and the accent's line along its top like the toolbar's.
            r.fill_rect(self.panel.inset(-1.0, -1.0), theme.line);
            r.fill_rect(self.panel, theme.toolbar);
            r.fill_rect(Rect::new(x, y, w, 2.0), theme.accent);
        }

        let mut line_y = y + 20.0;
        for t in &title {
            r.text(t, Rect::new(x + PAD, line_y, inner, TITLE_LINE), &title_style);
            line_y += TITLE_LINE;
        }
        line_y += 4.0;
        for t in &text {
            r.text(t, Rect::new(x + PAD, line_y, inner, TEXT_LINE), &text_style);
            line_y += TEXT_LINE;
        }

        // CANCEL at the right edge, the yes button left of it (Windows' order: yes first).
        let label_style = TextStyle::new(theme.text).semibold().align(Align::Center);
        let width_of = |r: &mut Renderer, label: &str| (r.measure(label, &label_style) + 40.0).max(96.0);
        let (yes_w, no_w) = (width_of(r, yes), width_of(r, "CANCEL"));
        let by = y + h - 20.0 - BUTTON_H;
        self.no = Rect::new(x + w - PAD - no_w, by, no_w, BUTTON_H);
        // Leaning the same way, the gap across between them is BUTTON_GAP.
        self.yes = Rect::new(self.no.x + self.lean - BUTTON_GAP - yes_w, by, yes_w, BUTTON_H);

        if glass {
            let look = |b: Button| Look { hover: self.hover == Some(b), pressed: self.pressed == Some(b) && self.hover == Some(b), focused: self.focus == b, enabled: true };
            kit::primary_button(r, theme, self.yes, yes, look(Button::Yes));
            kit::button(r, theme, self.no, "CANCEL", look(Button::No));
            return;
        }
        let yes_color = match (self.pressed, self.hover) {
            (Some(Button::Yes), Some(Button::Yes)) => theme.accent.with_alpha(0.75),
            (_, Some(Button::Yes)) => theme.accent.with_alpha(0.88),
            _ => theme.accent,
        };
        fill_box(r, theme, self.yes, self.lean, yes_color);
        r.text(yes, self.yes, &label_style.color(theme.on_accent));
        let no_color = if self.hover == Some(Button::No) { theme.chip_hover } else { theme.chip };
        fill_box(r, theme, self.no, self.lean, if self.pressed == Some(Button::No) && self.hover == Some(Button::No) { theme.button_pressed } else { no_color });
        r.text("CANCEL", self.no, &label_style);

        // The keyboard's button gets a thin outline (Enter or Space presses it).
        let focused = if self.focus == Button::Yes { self.yes } else { self.no };
        outline_box(r, theme, focused.inset(-2.5, -2.5), self.lean, 1.0, theme.text.with_alpha(0.65));
    }

    pub fn mouse_move(&mut self, p: Point) -> Answer {
        let hover = self.button_at(p);
        if hover != self.hover {
            self.hover = hover;
            return Answer::Redraw;
        }
        Answer::Nothing
    }

    pub fn mouse_down(&mut self, p: Point) -> Answer {
        match self.button_at(p) {
            Some(b) => {
                self.pressed = Some(b);
                self.focus = b;
                Answer::Redraw
            }
            // A click beside the box: the question is dismissed (and the click goes no further).
            None if !self.panel.contains(p) => Answer::No,
            None => Answer::Nothing,
        }
    }

    pub fn mouse_up(&mut self, p: Point) -> Answer {
        match (self.pressed.take(), self.button_at(p)) {
            (Some(a), Some(b)) if a == b => Self::answer(a),
            (Some(_), _) => Answer::Redraw,
            _ => Answer::Nothing,
        }
    }

    pub fn key(&mut self, vk: u16) -> Answer {
        match vk {
            v if v == VK_RETURN.0 || v == VK_SPACE.0 => Self::answer(self.focus),
            v if v == VK_ESCAPE.0 => Answer::No,
            v if v == VK_TAB.0 || v == VK_LEFT.0 || v == VK_RIGHT.0 => {
                self.focus = if self.focus == Button::Yes { Button::No } else { Button::Yes };
                Answer::Redraw
            }
            _ => Answer::Nothing,
        }
    }
}
