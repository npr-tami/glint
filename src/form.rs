//! The form box: Glint's own small window for typing a few things in before something happens
//! (the name of a set of tabs, how to rename many files at once). It's drawn like the question box
//! (ask.rs) - the files dimmed behind it, a panel in the middle - with real Windows text boxes for
//! the typing (textbox.rs), and a preview of what will happen under them.
//!
//! What the form is about lives in `AppState::form` (app/forms.rs), which also says what the
//! preview shows and does the work. While a form is open the window hands this box the mouse and
//! the text boxes' news (window.rs):
//! - Enter in a box, or a click on the coloured button, says yes (when the preview says it can);
//! - Esc, CANCEL, or a click outside the panel closes it;
//! - Tab goes to the next box.

use windows::Win32::Foundation::HWND;

use crate::app::{Form, Preview};
use crate::ask::Answer;
use crate::kit::{self, Look};
use crate::pathbar::{box_contains, fill_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::textbox::{TextBox, TextEvent, TextEventKind};
use crate::theme::{Color, Theme};
use crate::ui::{Point, Rect};

/// The text boxes' ids (textbox.rs: 400-499 are the form's).
pub const FIRST_ID: u16 = 400;
/// The most boxes a form has.
const MAX_FIELDS: usize = 4;

const WIDTH: f32 = 520.0;
const PAD: f32 = 22.0;
const TITLE_SIZE: f32 = 14.0;
const TITLE_LINE: f32 = 22.0;
const LABEL_W: f32 = 120.0;
const FIELD_H: f32 = 28.0;
const FIELD_GAP: f32 = 8.0;
const LINE: f32 = 19.0;
const BUTTON_H: f32 = 30.0;
const LEAN: f32 = 8.0;
const BUTTON_GAP: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Yes,
    No,
}

#[derive(Default)]
pub struct FormBox {
    /// The form last shown (a new one fills its boxes and puts the cursor in the first).
    seen: u64,
    boxes: Vec<TextBox>,
    /// How many of the boxes the form uses.
    used: usize,
    hover: Option<Button>,
    pressed: Option<Button>,
    panel: Rect,
    yes: Rect,
    no: Rect,
    lean: f32,
    /// The preview allows yes (nothing in it is a problem).
    can: bool,
}

impl FormBox {
    /// What is typed in the form's boxes now.
    pub fn values(&self) -> Vec<String> {
        self.boxes.iter().take(self.used).map(TextBox::text).collect()
    }

    /// Whether form `id` has been drawn (input before that isn't meant for it; see ask.rs).
    pub fn shown(&self, id: u64) -> bool {
        self.seen == id
    }

    /// The form closed: its boxes go away and the keyboard goes back to the window.
    pub fn close(&mut self, window: HWND) {
        for b in &mut self.boxes {
            b.hide();
        }
        self.used = 0;
        self.seen = 0;
        // SAFETY: handing the keyboard to our own window.
        unsafe {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(window));
        }
    }

    /// Draws the form over `area` (the window below the toolbar) and puts its boxes in place.
    /// `preview`: what app/forms.rs says the typing would do.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, window: HWND, scale: f32, form: &Form, preview: &Preview, area: Rect) {
        let fresh = form.id != self.seen;
        if fresh {
            self.seen = form.id;
            (self.hover, self.pressed) = (None, None);
            while self.boxes.len() < form.fields.len().min(MAX_FIELDS) {
                let id = FIRST_ID + self.boxes.len() as u16;
                self.boxes.push(TextBox::new(window, id));
            }
            self.used = form.fields.len().min(MAX_FIELDS);
            for (b, f) in self.boxes.iter().zip(&form.fields) {
                b.set_text(&f.start);
                b.set_hint(f.hint);
            }
            for b in self.boxes.iter_mut().skip(self.used) {
                b.hide();
            }
        }
        self.lean = theme.lean(LEAN);
        self.can = preview.problem.is_none() && preview.ready;
        r.fill_rect(area, Color::rgba(0, 0, 0, if theme.dark { 0.5 } else { 0.28 }));

        let w = WIDTH.min(area.w - 32.0).max(260.0);
        let inner = w - 2.0 * PAD;
        let lines = preview.lines.len() + preview.problem.is_some() as usize;
        let fields_h = self.used as f32 * (FIELD_H + FIELD_GAP);
        let h = 20.0 + TITLE_LINE + 12.0 + fields_h + 6.0 + lines as f32 * LINE + 22.0 + BUTTON_H + 20.0;
        let x = area.x + ((area.w - w) / 2.0).max(0.0);
        let y = area.y + ((area.h - h) / 2.0 - 24.0).max(12.0);
        self.panel = Rect::new(x, y, w, h);

        let glass = kit::glass(theme);
        if glass {
            let radius = theme.corner(h).max(6.0);
            r.fill_round_rect(Rect::new(x - 1.0, y + 2.0, w + 2.0, h + 3.0), radius + 1.0, Color::rgba(0, 0, 0, 0.22));
            r.fill_paint(self.panel, radius, &theme.toolbar_paint);
            r.stroke_round_rect(self.panel.inset(0.5, 0.5), radius - 0.5, 1.0, theme.line);
        } else {
            r.fill_rect(self.panel.inset(-1.0, -1.0), theme.line);
            r.fill_rect(self.panel, theme.toolbar);
            r.fill_rect(Rect::new(x, y, w, 2.0), theme.accent);
        }

        let title_style = TextStyle::new(theme.text_strong).size(TITLE_SIZE).semibold();
        r.text(&form.title, Rect::new(x + PAD, y + 20.0, inner, TITLE_LINE), &title_style);
        let label_style = TextStyle::new(theme.text);
        let mut line_y = y + 20.0 + TITLE_LINE + 12.0;
        for (i, f) in form.fields.iter().take(self.used).enumerate() {
            r.text(f.label, Rect::new(x + PAD, line_y + (FIELD_H - 20.0) / 2.0, LABEL_W - 8.0, 20.0), &label_style);
            let field = Rect::new(x + PAD + LABEL_W, line_y, inner - LABEL_W, FIELD_H);
            r.fill_rect(field, theme.input);
            r.stroke_rect(field.inset(0.5, 0.5), 1.0, theme.input_border);
            let b = &mut self.boxes[i];
            b.place(field.inset(6.0, 4.0), 0.0, scale);
            b.show();
            line_y += FIELD_H + FIELD_GAP;
        }
        if fresh {
            if let Some(first) = self.boxes.first() {
                first.focus();
                first.select_all();
            }
        }
        line_y += 6.0;
        let faint = TextStyle::new(theme.muted);
        for l in &preview.lines {
            r.text(l, Rect::new(x + PAD, line_y, inner, LINE), &faint);
            line_y += LINE;
        }
        if let Some(p) = &preview.problem {
            r.text(p, Rect::new(x + PAD, line_y, inner, LINE), &TextStyle::new(theme.accent).semibold());
        }

        // CANCEL at the right, the yes button left of it (as in the question box).
        let label = TextStyle::new(theme.text).semibold().align(Align::Center);
        let width_of = |r: &mut Renderer, l: &str| (r.measure(l, &label) + 40.0).max(96.0);
        let (yes_w, no_w) = (width_of(r, form.yes), width_of(r, "CANCEL"));
        let by = y + h - 20.0 - BUTTON_H;
        self.no = Rect::new(x + w - PAD - no_w, by, no_w, BUTTON_H);
        self.yes = Rect::new(self.no.x + self.lean - BUTTON_GAP - yes_w, by, yes_w, BUTTON_H);
        if glass {
            let look = |b: Button, enabled: bool| Look { hover: self.hover == Some(b), pressed: self.pressed == Some(b) && self.hover == Some(b), focused: false, enabled };
            kit::primary_button(r, theme, self.yes, form.yes, look(Button::Yes, self.can));
            kit::button(r, theme, self.no, "CANCEL", look(Button::No, true));
            return;
        }
        let yes_color = match (self.can, self.pressed, self.hover) {
            (false, _, _) => theme.accent.with_alpha(0.4),
            (_, Some(Button::Yes), Some(Button::Yes)) => theme.accent.with_alpha(0.75),
            (_, _, Some(Button::Yes)) => theme.accent.with_alpha(0.88),
            _ => theme.accent,
        };
        fill_box(r, theme, self.yes, self.lean, yes_color);
        r.text(form.yes, self.yes, &label.color(theme.on_accent));
        let no_color = if self.hover == Some(Button::No) { theme.chip_hover } else { theme.chip };
        fill_box(r, theme, self.no, self.lean, if self.pressed == Some(Button::No) && self.hover == Some(Button::No) { theme.button_pressed } else { no_color });
        r.text("CANCEL", self.no, &label);
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

    fn yes(&self) -> Answer {
        if self.can { Answer::Yes } else { Answer::Nothing }
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
                Answer::Redraw
            }
            None if !self.panel.contains(p) => Answer::No,
            None => Answer::Nothing,
        }
    }

    pub fn mouse_up(&mut self, p: Point) -> Answer {
        match (self.pressed.take(), self.button_at(p)) {
            (Some(Button::Yes), Some(Button::Yes)) => self.yes(),
            (Some(Button::No), Some(Button::No)) => Answer::No,
            (Some(_), _) => Answer::Redraw,
            _ => Answer::Nothing,
        }
    }

    /// A key that reached the window itself while the form is open (after a click on the panel).
    pub fn key(&mut self, vk: u16) -> Answer {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};
        match vk {
            v if v == VK_RETURN.0 => self.yes(),
            v if v == VK_ESCAPE.0 => Answer::No,
            _ => Answer::Nothing,
        }
    }

    /// News from one of the form's boxes. None: not one of them.
    pub fn text_event(&mut self, ev: &TextEvent) -> Option<Answer> {
        let i = ev.id.checked_sub(FIRST_ID).map(usize::from).filter(|&i| i < self.used)?;
        Some(match ev.kind {
            TextEventKind::Changed => Answer::Redraw,
            TextEventKind::Enter => self.yes(),
            TextEventKind::Escape => Answer::No,
            TextEventKind::Tab => {
                let next = (i + 1) % self.used.max(1);
                if let Some(b) = self.boxes.get(next) {
                    b.focus();
                    b.select_all();
                }
                Answer::Nothing
            }
            TextEventKind::LostFocus | TextEventKind::Down => Answer::Nothing,
        })
    }
}
