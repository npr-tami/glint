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
//! - Tab goes to the next box;
//! - the mouse wheel scrolls the preview's list (renaming many shows every new name, a few at a
//!   time), and so does its scroll bar: drag the thumb, or click beside it for a page.

use windows::Win32::Foundation::HWND;

use crate::app::{Form, Preview};
use crate::ask::Answer;
use crate::kit::{self, Look};
use crate::pathbar::{box_contains_beside, face, fill_box_beside, Beside};
use crate::render::{Align, Renderer, TextStyle};
use crate::retroctl::Hold;
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
/// A field's note line under its box.
const NOTE_LINE: f32 = 18.0;
/// How many lines of the preview's list show at once (the rest scroll into view).
const LIST_ROWS: usize = 6;
/// How many lines one notch of the mouse wheel scrolls the list.
const WHEEL_ROWS: f32 = 3.0;
/// Between an old name and its new one in the list.
const ARROW: &str = "  \u{2192}  ";
/// How much of a list line the new name may take when both names don't fit (the old one keeps
/// the rest): the new name is what the list is for.
const NEW_SHARE: f32 = 0.62;
/// The gap between the list's lines and its scroll bar.
const BAR_GAP: f32 = 4.0;
const BUTTON_H: f32 = 30.0;
const LEAN: f32 = 8.0;
const BUTTON_GAP: f32 = 6.0;

/// The SHOW / HIDE button at the right end of a password box: its width, and the gap between it and
/// the typing.
const EYE_W: f32 = 52.0;
const EYE_GAP: f32 = 4.0;
/// The slider (Split into): its thumb's width, its line's thickness, and the room right of it for the
/// number picked.
const THUMB_W: f32 = 12.0;
const TRACK_H: f32 = 4.0;
const SLIDER_NUMBER_W: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Yes,
    No,
    /// The SHOW / HIDE button of box `n` (a password box).
    Eye(usize),
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
    /// Where the preview's list is, and how far it's scrolled.
    list: Rect,
    scroll: kit::Scroller,
    /// The skin's scroll bar, as the list's was last drawn.
    bar: kit::Bar,
    /// The list's scroll bar thumb is held, this far down it.
    grab: Option<f32>,
    /// The mouse is over that thumb.
    over_thumb: bool,
    /// The form's two-way switch (ZIP / 7Z), when it has one: which side is on, and where each is.
    choice: Option<usize>,
    sides: [Rect; 2],
    /// Each password box's SHOW / HIDE button, where it was drawn (None: not a password box).
    eyes: [Option<Rect>; MAX_FIELDS],
    /// Each password box shows what's typed (its SHOW was pressed) instead of dots.
    shown: [bool; MAX_FIELDS],
    /// The form's slider (Split into), when it has one: the number picked, the lowest and highest, and
    /// where its line was drawn.
    slider: Option<(u32, u32, u32)>,
    slider_track: Rect,
    /// The slider's thumb is held (it follows the mouse until the button comes up).
    sliding: bool,
    /// The slider was drawn (a form that splits by size hides it while SIZE is picked).
    slider_shown: bool,
    /// The form's second switch (Split by: FILES / SIZE), when it has one: which side is on, and
    /// where each is.
    by: Option<usize>,
    by_sides: [Rect; 2],
    /// The form's size choices (Each file: 25 MB ... and a box for your own), when it has them and
    /// they show: which is picked (the last number is the own box), and where each was drawn.
    chip: Option<usize>,
    chips: Vec<Rect>,
    chips_shown: bool,
    /// The own size box, and where it was drawn.
    own: Option<TextBox>,
    own_rect: Rect,
}

/// The own size box's id (after the form's boxes).
const OWN_ID: u16 = FIRST_ID + MAX_FIELDS as u16;
/// The own size box's width, and the room for its unit ("MB") inside it on the right.
const OWN_W: f32 = 96.0;
const UNIT_W: f32 = 28.0;
const CHIP_GAP: f32 = 6.0;

impl FormBox {
    /// What is typed in the form's boxes now, and after them, for a form with a switch, which side
    /// of it is on ("0" or "1").
    pub fn values(&self) -> Vec<String> {
        let mut values: Vec<String> = self.boxes.iter().take(self.used).map(TextBox::text).collect();
        values.extend(self.choice.map(|c| c.to_string()));
        values.extend(self.slider.map(|(n, ..)| n.to_string()));
        values.extend(self.by.map(|b| b.to_string()));
        if let Some(c) = self.chip {
            values.push(c.to_string());
            values.push(self.own.as_ref().map(TextBox::text).unwrap_or_default());
        }
        values
    }

    /// Whether form `id` has been drawn (input before that isn't meant for it; see ask.rs).
    pub fn shown(&self, id: u64) -> bool {
        self.seen == id
    }

    /// The form closed: its boxes go away and the keyboard goes back to the window.
    pub fn close(&mut self, window: HWND) {
        for b in &mut self.boxes {
            b.hide();
            // (A password typed in it doesn't stay behind.)
            b.set_text("");
            b.set_secret(false);
        }
        self.used = 0;
        self.choice = None;
        (self.slider, self.sliding) = (None, false);
        (self.by, self.chip, self.chips_shown, self.slider_shown) = (None, None, false, false);
        if let Some(own) = &mut self.own {
            own.hide();
            own.set_text("");
        }
        (self.eyes, self.shown) = Default::default();
        self.seen = 0;
        (self.grab, self.over_thumb) = (None, false);
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
            self.scroll = kit::Scroller::default();
            (self.grab, self.over_thumb) = (None, false);
            while self.boxes.len() < form.fields.len().min(MAX_FIELDS) {
                let id = FIRST_ID + self.boxes.len() as u16;
                self.boxes.push(TextBox::new(window, id));
            }
            self.used = form.fields.len().min(MAX_FIELDS);
            // (A password starts as dots, every time.)
            (self.eyes, self.shown) = Default::default();
            for (b, f) in self.boxes.iter().zip(&form.fields) {
                b.set_secret(f.secret);
                b.set_text(&f.start);
                b.set_hint(f.hint);
            }
            self.choice = form.choice.as_ref().map(|c| c.start);
            self.slider = form.slider.as_ref().map(|s| (s.start.clamp(s.min, s.max), s.min, s.max));
            self.sliding = false;
            self.by = form.by.as_ref().map(|c| c.start);
            self.chip = form.sizes.as_ref().map(|z| z.start);
            if form.sizes.is_some() && self.own.is_none() {
                self.own = Some(TextBox::new(window, OWN_ID));
            }
            if let Some(own) = &mut self.own {
                own.set_text("");
                own.hide();
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
        let lines = preview.lines.len() + preview.list.len().min(LIST_ROWS) + preview.problem.is_some() as usize + preview.alert.is_some() as usize;
        let switch_h = if form.choice.is_some() { FIELD_H + FIELD_GAP } else { 0.0 };
        let notes = form.fields.iter().take(self.used).filter(|f| !f.note.is_empty()).count();
        // (A form that splits by count or size shows the slider or the sizes, after its switch.)
        let by_size = self.by == Some(1);
        let by_h = if form.by.is_some() { FIELD_H + FIELD_GAP } else { 0.0 };
        let slider_h = if form.slider.is_some() && !by_size { FIELD_H + FIELD_GAP } else { 0.0 };
        let chip_style = TextStyle::new(theme.text).semibold().align(Align::Center);
        let (chip_x, chip_right) = (LABEL_W, WIDTH.min(area.w - 32.0).max(260.0) - 2.0 * PAD);
        let sizes_h = match (&form.sizes, by_size) {
            (Some(z), true) => lay_chips(r, &chip_style, z.chips, chip_x, chip_right, 0.0).2 + FIELD_GAP,
            _ => 0.0,
        };
        let fields_h = self.used as f32 * (FIELD_H + FIELD_GAP) + notes as f32 * NOTE_LINE + by_h + slider_h + sizes_h;
        let h = 20.0 + TITLE_LINE + 12.0 + switch_h + fields_h + 6.0 + lines as f32 * LINE + 22.0 + BUTTON_H + 20.0;
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
        }

        let title_style = TextStyle::new(theme.text_strong).size(TITLE_SIZE).semibold();
        r.text(&form.title, Rect::new(x + PAD, y + 20.0, inner, TITLE_LINE), &title_style);
        let label_style = TextStyle::new(theme.text);
        let mut line_y = y + 20.0 + TITLE_LINE + 12.0;
        // The switch (ZIP / 7Z): two halves side by side, the one that's on in the accent colour.
        if let (Some(c), Some(on)) = (&form.choice, self.choice) {
            self.sides = switch(r, theme, c.label, c.sides, on, x + PAD, line_y);
            line_y += FIELD_H + FIELD_GAP;
        }
        for (i, f) in form.fields.iter().take(self.used).enumerate() {
            r.text(f.label, Rect::new(x + PAD, line_y + (FIELD_H - 20.0) / 2.0, LABEL_W - 8.0, 20.0), &label_style);
            let field = Rect::new(x + PAD + LABEL_W, line_y, inner - LABEL_W, FIELD_H);
            r.fill_rect(field, theme.input);
            r.stroke_rect(field.inset(0.5, 0.5), 1.0, theme.input_border);
            // A password box: SHOW (or HIDE) at its right end, and the typing left of it.
            let mut typing = field.inset(6.0, 4.0);
            self.eyes[i] = None;
            if f.secret {
                let eye = Rect::new(field.right() - EYE_W - 3.0, field.y + 3.0, EYE_W, field.h - 6.0);
                let lit = self.hover == Some(Button::Eye(i));
                r.fill_rect(eye, if lit { theme.chip_hover } else { theme.chip });
                let word = if self.shown[i] { "HIDE" } else { "SHOW" };
                r.text(word, eye, &TextStyle::new(theme.text).semibold().align(Align::Center));
                typing.w = (eye.x - EYE_GAP - typing.x).max(10.0);
                self.eyes[i] = Some(eye);
            }
            let b = &mut self.boxes[i];
            b.place(typing, 0.0, scale);
            b.show();
            line_y += FIELD_H + FIELD_GAP;
            // How to fill it in, faint, under the box.
            // (One too long to fit under the box starts under the label instead, rather than be cut.)
            if !f.note.is_empty() {
                let note_style = TextStyle::new(theme.muted);
                let from = if r.measure(f.note, &note_style) > field.w { x + PAD } else { field.x };
                r.text(f.note, Rect::new(from, line_y - FIELD_GAP + 2.0, field.right() - from, NOTE_LINE), &note_style);
                line_y += NOTE_LINE;
            }
        }
        if let (Some(c), Some(on)) = (&form.by, self.by) {
            self.by_sides = switch(r, theme, c.label, c.sides, on, x + PAD, line_y);
            line_y += FIELD_H + FIELD_GAP;
        }
        // The sizes (Each file): a chip each, the picked one in the accent colour, and a box to type
        // your own (picked while it's typed in).
        self.chips_shown = false;
        if let (Some(z), Some(picked), true) = (&form.sizes, self.chip, by_size) {
            r.text(z.label, Rect::new(x + PAD, line_y + (FIELD_H - 20.0) / 2.0, LABEL_W - 8.0, 20.0), &label_style);
            let (rects, own, rows_h) = lay_chips(r, &chip_style, z.chips, x + PAD + LABEL_W, x + PAD + inner, line_y);
            for (i, (rect, name)) in rects.iter().zip(z.chips).enumerate() {
                if i == picked {
                    r.fill_rect(*rect, theme.accent);
                    r.text(name, *rect, &chip_style.color(theme.on_accent));
                } else {
                    r.fill_rect(*rect, theme.chip);
                    r.stroke_rect(rect.inset(0.5, 0.5), 1.0, theme.input_border);
                    r.text(name, *rect, &chip_style);
                }
            }
            r.fill_rect(own, theme.input);
            let own_on = picked == z.chips.len();
            r.stroke_rect(own.inset(0.5, 0.5), if own_on { 2.0 } else { 1.0 }, if own_on { theme.accent } else { theme.input_border });
            r.text(z.unit, Rect::new(own.right() - UNIT_W, own.y, UNIT_W - 6.0, own.h), &TextStyle::new(theme.muted).align(Align::Right));
            if let Some(b) = &mut self.own {
                b.set_hint(z.own_hint);
                b.place(Rect::new(own.x + 6.0, own.y + 4.0, own.w - UNIT_W - 8.0, own.h - 8.0), 0.0, scale);
                b.show();
            }
            (self.chips, self.own_rect, self.chips_shown) = (rects, own, true);
            line_y += rows_h + FIELD_GAP;
        } else if let Some(b) = &mut self.own {
            b.hide();
        }
        // The slider (Split into): a line with a mark for each number, the thumb on the one picked, and
        // the number right of it.
        self.slider_shown = false;
        if let (Some(sl), Some((n, lo, hi)), false) = (&form.slider, self.slider, by_size) {
            self.slider_shown = true;
            r.text(sl.label, Rect::new(x + PAD, line_y + (FIELD_H - 20.0) / 2.0, LABEL_W - 8.0, 20.0), &label_style);
            let left = x + PAD + LABEL_W + THUMB_W / 2.0;
            let right = x + PAD + inner - SLIDER_NUMBER_W - THUMB_W / 2.0;
            self.slider_track = Rect::new(left, line_y, (right - left).max(10.0), FIELD_H);
            let mid = line_y + FIELD_H / 2.0;
            let at = |v: u32| left + self.slider_track.w * (v - lo) as f32 / (hi - lo).max(1) as f32;
            r.fill_rect(Rect::new(left, mid - TRACK_H / 2.0, self.slider_track.w, TRACK_H), theme.chip);
            r.fill_rect(Rect::new(left, mid - TRACK_H / 2.0, at(n) - left, TRACK_H), theme.accent);
            for v in lo..=hi {
                r.fill_rect(Rect::new(at(v) - 0.5, mid + TRACK_H / 2.0 + 2.0, 1.0, 4.0), theme.muted);
            }
            let thumb = Rect::new(at(n) - THUMB_W / 2.0, line_y + 4.0, THUMB_W, FIELD_H - 8.0);
            r.fill_rect(thumb, theme.accent);
            let count = format!("{n} {}", sl.units[usize::from(n != 1)]);
            r.text(&count, Rect::new(x + PAD + inner - SLIDER_NUMBER_W + 8.0, line_y, SLIDER_NUMBER_W - 8.0, FIELD_H), &TextStyle::new(theme.text_strong).semibold());
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
        // The list: only the lines in view are drawn, however many items there are.
        let shown = preview.list.len().min(LIST_ROWS);
        self.list = Rect::new(x + PAD, line_y, inner, shown as f32 * LINE);
        self.scroll.set_extent(preview.list.len() as f32 * LINE, self.list.h);
        self.bar = kit::Bar::of(theme);
        if shown > 0 {
            let scrolls = self.scroll.max() > 0.0;
            // (Room for the scroll bar, as wide as the skin's is.)
            let row_w = inner - if scrolls { self.bar.place(self.list).lane.w + BAR_GAP } else { 0.0 };
            r.push_clip(self.list);
            for i in rows_in_view(preview.list.len(), self.scroll.offset, self.list.h) {
                let row_y = self.list.y + i as f32 * LINE - self.scroll.offset;
                let (old, new) = &preview.list[i];
                let line = fit_row(old, new, row_w, &mut |t| r.measure(t, &faint));
                r.text(&line, Rect::new(self.list.x, row_y, row_w, LINE), &faint);
            }
            r.pop_clip();
            if scrolls {
                kit::scrollbar(r, theme, self.bar.track(self.list), &self.scroll, self.over_thumb || self.grab.is_some(), Hold::None);
            }
            line_y += self.list.h;
        } else {
            (self.grab, self.over_thumb) = (None, false);
        }
        if let Some(p) = &preview.problem {
            r.text(p, Rect::new(x + PAD, line_y, inner, LINE), &TextStyle::new(theme.accent).semibold());
            line_y += LINE;
        }
        if let Some(a) = &preview.alert {
            r.text(a, Rect::new(x + PAD, line_y, inner, LINE), &TextStyle::new(theme.accent).semibold());
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
            kit::primary_button_beside(r, theme, self.yes, form.yes, look(Button::Yes, self.can), Beside::RIGHT);
            kit::button_beside(r, theme, self.no, "CANCEL", look(Button::No, true), Beside::LEFT);
            return;
        }
        let yes_color = match (self.can, self.pressed, self.hover) {
            (false, _, _) => theme.accent.with_alpha(0.4),
            (_, Some(Button::Yes), Some(Button::Yes)) => theme.accent.with_alpha(0.75),
            (_, _, Some(Button::Yes)) => theme.accent.with_alpha(0.88),
            _ => theme.accent,
        };
        fill_box_beside(r, theme, self.yes, self.lean, Beside::RIGHT, yes_color);
        r.text(form.yes, face(self.yes, self.lean, Beside::RIGHT), &label.color(theme.on_accent));
        let no_color = if self.hover == Some(Button::No) { theme.chip_hover } else { theme.chip };
        fill_box_beside(r, theme, self.no, self.lean, Beside::LEFT, if self.pressed == Some(Button::No) && self.hover == Some(Button::No) { theme.button_pressed } else { no_color });
        r.text("CANCEL", face(self.no, self.lean, Beside::LEFT), &label);
    }

    fn button_at(&self, p: Point) -> Option<Button> {
        if box_contains_beside(self.yes, self.lean, Beside::RIGHT, p) {
            Some(Button::Yes)
        } else if box_contains_beside(self.no, self.lean, Beside::LEFT, p) {
            Some(Button::No)
        } else {
            self.eyes.iter().take(self.used).position(|e| e.is_some_and(|e| e.contains(p))).map(Button::Eye)
        }
    }

    /// SHOW / HIDE pressed on password box `i`: it shows what's typed, or dots again. The cursor
    /// goes back into the box, so typing carries on.
    fn flip_eye(&mut self, i: usize) -> Answer {
        let Some(b) = self.boxes.get(i) else { return Answer::Nothing };
        self.shown[i] = !self.shown[i];
        b.set_secret(!self.shown[i]);
        b.focus();
        Answer::Redraw
    }

    fn yes(&self) -> Answer {
        if self.can { Answer::Yes } else { Answer::Nothing }
    }

    /// The list's scroll bar thumb, while the list scrolls.
    fn thumb(&self) -> Option<Rect> {
        self.scroll.thumb(self.bar.track(self.list))
    }

    /// Whether the list's scroll bar thumb is being dragged (the window then keeps the mouse, so
    /// the drag goes on outside it and ends wherever the button comes up).
    pub fn dragging(&self) -> bool {
        self.grab.is_some() || self.sliding
    }

    /// The drag ended without a mouse-up here (something else took the mouse).
    pub fn let_go(&mut self) {
        self.grab = None;
        self.sliding = false;
    }

    /// Puts the slider on the number nearest `x` (as far as it goes). Redraw when that's another.
    fn slide_to(&mut self, x: f32) -> Answer {
        let Some((n, lo, hi)) = self.slider else { return Answer::Nothing };
        let t = self.slider_track;
        let share = ((x - t.x) / t.w.max(1.0)).clamp(0.0, 1.0);
        let v = lo + (share * (hi - lo) as f32).round() as u32;
        self.slider = Some((v, lo, hi));
        if v == n { Answer::Nothing } else { Answer::Redraw }
    }

    /// The slider a number up or down (Right / Left arrow).
    fn slide_by(&mut self, up: bool) -> Answer {
        let Some((n, lo, hi)) = self.slider.filter(|_| self.slider_shown) else { return Answer::Nothing };
        let v = if up { (n + 1).min(hi) } else { n.saturating_sub(1).max(lo) };
        self.slider = Some((v, lo, hi));
        if v == n { Answer::Nothing } else { Answer::Redraw }
    }

    /// Whether `p` is on the slider (its line, a thumb's width either side, the row's height).
    fn on_slider(&self, p: Point) -> bool {
        self.slider_shown && self.slider.is_some() && self.slider_track.inset(-THUMB_W, 0.0).contains(p)
    }

    pub fn mouse_move(&mut self, p: Point) -> Answer {
        if self.sliding {
            return self.slide_to(p.x);
        }
        if let Some(grab) = self.grab {
            // The thumb follows the mouse (as far as the list goes).
            let track = self.bar.track(self.list);
            let before = self.scroll.offset;
            if let Some(thumb) = self.thumb() {
                self.scroll.jump_to(self.scroll.offset_for_thumb(track, thumb.h, p.y - grab));
            }
            return if self.scroll.offset == before { Answer::Nothing } else { Answer::Redraw };
        }
        let hover = self.button_at(p);
        // (The thumb is thin: a little either side of it counts too.)
        let over_thumb = self.thumb().is_some_and(|t| t.inset(-3.0, 0.0).contains(p));
        if hover != self.hover || over_thumb != self.over_thumb {
            (self.hover, self.over_thumb) = (hover, over_thumb);
            return Answer::Redraw;
        }
        Answer::Nothing
    }

    pub fn mouse_down(&mut self, p: Point) -> Answer {
        if let Some(thumb) = self.thumb() {
            if thumb.inset(-3.0, 0.0).contains(p) {
                self.grab = Some(p.y - thumb.y);
                return Answer::Redraw;
            }
            let place = self.bar.place(self.list);
            if place.lane.contains(p) {
                // Beside the thumb: a page that way (a line, on an older Windows' arrow buttons).
                let step = if p.y < place.track.y || p.y >= place.track.bottom() { LINE } else { (self.list.h - LINE).max(LINE) };
                let way = if p.y < thumb.y { -1.0 } else { 1.0 };
                self.scroll.jump_to(self.scroll.offset + way * step);
                return Answer::Redraw;
            }
        }
        if self.on_slider(p) {
            self.sliding = true;
            self.slide_to(p.x);
            return Answer::Redraw;
        }
        if let Some(on) = self.choice {
            if let Some(i) = self.sides.iter().position(|s| s.contains(p)) {
                self.choice = Some(i);
                return if i == on { Answer::Nothing } else { Answer::Redraw };
            }
        }
        if let Some(on) = self.by {
            if let Some(i) = self.by_sides.iter().position(|s| s.contains(p)) {
                self.by = Some(i);
                return if i == on { Answer::Nothing } else { Answer::Redraw };
            }
        }
        if self.chips_shown {
            if let Some(i) = self.chips.iter().position(|c| c.contains(p)) {
                self.chip = Some(i);
                return Answer::Redraw;
            }
            if self.own_rect.contains(p) {
                self.chip = Some(self.chips.len());
                if let Some(b) = &self.own {
                    b.focus();
                }
                return Answer::Redraw;
            }
        }
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
        if std::mem::take(&mut self.sliding) {
            return self.slide_to(p.x);
        }
        if self.grab.take().is_some() {
            // (Let go anywhere, a button or outside the panel too: only the drag ends.)
            self.over_thumb = self.thumb().is_some_and(|t| t.inset(-3.0, 0.0).contains(p));
            return Answer::Redraw;
        }
        match (self.pressed.take(), self.button_at(p)) {
            (Some(Button::Yes), Some(Button::Yes)) => self.yes(),
            (Some(Button::No), Some(Button::No)) => Answer::No,
            (Some(Button::Eye(i)), Some(Button::Eye(j))) if i == j => self.flip_eye(i),
            (Some(_), _) => Answer::Redraw,
            _ => Answer::Nothing,
        }
    }

    /// The mouse wheel over the panel: the preview's list scrolls, three lines a notch.
    pub fn wheel(&mut self, p: Point, notches: f32) -> Answer {
        if !self.panel.contains(p) {
            return Answer::Nothing;
        }
        let before = self.scroll.offset;
        self.scroll.jump_to(before - notches * WHEEL_ROWS * LINE);
        if self.scroll.offset == before { Answer::Nothing } else { Answer::Redraw }
    }

    /// A key that reached the window itself while the form is open (after a click on the panel).
    pub fn key(&mut self, vk: u16) -> Answer {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT};
        match vk {
            v if v == VK_RETURN.0 => self.yes(),
            v if v == VK_LEFT.0 => self.slide_by(false),
            v if v == VK_RIGHT.0 => self.slide_by(true),
            v if v == VK_ESCAPE.0 => Answer::No,
            _ => Answer::Nothing,
        }
    }

    /// News from one of the form's boxes. None: not one of them.
    pub fn text_event(&mut self, ev: &TextEvent) -> Option<Answer> {
        if ev.id == OWN_ID && self.own.is_some() {
            return Some(match ev.kind {
                // (Typing your own size picks it.)
                TextEventKind::Changed => {
                    self.chip = Some(self.chips.len());
                    Answer::Redraw
                }
                TextEventKind::Enter => self.yes(),
                TextEventKind::Escape => Answer::No,
                TextEventKind::Tab => {
                    if let Some(b) = self.boxes.first().filter(|_| self.used > 0) {
                        b.focus();
                        b.select_all();
                    }
                    Answer::Nothing
                }
                TextEventKind::LostFocus | TextEventKind::Down => Answer::Nothing,
            });
        }
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

/// A two-way switch (ZIP / 7Z, FILES / SIZE) with its label left of it, at `x`, `y`: two halves side
/// by side, the one that's on in the accent colour. Returns where each half is.
fn switch(r: &mut Renderer, theme: &Theme, label: &str, sides: [&str; 2], on: usize, x: f32, y: f32) -> [Rect; 2] {
    r.text(label, Rect::new(x, y + (FIELD_H - 20.0) / 2.0, LABEL_W - 8.0, 20.0), &TextStyle::new(theme.text));
    let side_style = TextStyle::new(theme.text).semibold().align(Align::Center);
    let mut rects = [Rect::default(); 2];
    let mut sx = x + LABEL_W;
    for (i, name) in sides.iter().enumerate() {
        let side_w = (r.measure(name, &side_style) + 32.0).max(64.0);
        let side = Rect::new(sx, y, side_w, FIELD_H);
        rects[i] = side;
        if i == on {
            r.fill_rect(side, theme.accent);
            r.text(name, side, &side_style.color(theme.on_accent));
        } else {
            r.fill_rect(side, theme.chip);
            r.stroke_rect(side.inset(0.5, 0.5), 1.0, theme.input_border);
            r.text(name, side, &side_style);
        }
        sx += side_w;
    }
    rects
}

/// Where the size chips and the own size box go, from `left` to `right`, starting at `y`: a chip
/// each, then the box, on as many rows as they need. Returns the chips, the box, and the rows'
/// height.
fn lay_chips(r: &mut Renderer, style: &TextStyle, chips: &[&str], left: f32, right: f32, y: f32) -> (Vec<Rect>, Rect, f32) {
    let (mut cx, mut cy) = (left, y);
    let mut place = |w: f32| {
        if cx > left && cx + w > right {
            (cx, cy) = (left, cy + FIELD_H + CHIP_GAP);
        }
        let rect = Rect::new(cx, cy, w, FIELD_H);
        cx += w + CHIP_GAP;
        rect
    };
    let rects: Vec<Rect> = chips.iter().map(|c| r.measure(c, style) + 20.0).collect::<Vec<_>>().into_iter().map(&mut place).collect();
    let own = place(OWN_W);
    (rects, own, own.bottom() - y)
}

/// One line of the list, "old  →  new", no wider than `width` (`measure`: how wide a text is).
/// Where the two don't fit, the new name comes first: it keeps what it needs, up to `NEW_SHARE`
/// of the line (more when the old name is short), and the old name gets the rest. A name too
/// long for its part is cut in the middle, so its start and its end - the number, the
/// extension - both still show.
fn fit_row(old: &str, new: &str, width: f32, measure: &mut dyn FnMut(&str) -> f32) -> String {
    let whole = format!("{old}{ARROW}{new}");
    if measure(&whole) <= width {
        return whole;
    }
    // (A pixel spare: the parts measured one by one can come out a hair narrower than together.)
    let room = (width - measure(ARROW) - 1.0).max(0.0);
    let (old_w, new_w) = (measure(old), measure(new));
    let for_new = new_w.min((room - old_w).max(room * NEW_SHARE));
    let for_old = (room - for_new).max(0.0);
    format!("{}{ARROW}{}", fit_middle(old, for_old, measure), fit_middle(new, for_new, measure))
}

/// `text` no wider than `width`: as it is when it fits, else its start and its end with "…"
/// between them, as much of both as fits.
fn fit_middle(text: &str, width: f32, measure: &mut dyn FnMut(&str) -> f32) -> String {
    if measure(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // The end keeps one letter more than the start where they can't be the same.
    let cut = |n: usize| {
        let (head, tail) = (n / 2, n - n / 2);
        format!("{}\u{2026}{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
    };
    let (mut lo, mut hi) = (0usize, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&cut(mid)) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// Which of `len` lines (each `LINE` high) show in a list `view` high scrolled down by `offset`:
/// the ones at least partly in view.
fn rows_in_view(len: usize, offset: f32, view: f32) -> std::ops::Range<usize> {
    let first = ((offset / LINE).floor().max(0.0) as usize).min(len);
    let end = (((offset + view) / LINE).ceil().max(0.0) as usize).min(len);
    first..end.max(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A form box whose list of `len` lines was laid out as `paint` lays it out.
    fn laid_out(len: usize) -> FormBox {
        let mut f = FormBox { panel: Rect::new(0.0, 0.0, 520.0, 400.0), ..Default::default() };
        f.list = Rect::new(22.0, 200.0, 476.0, len.min(LIST_ROWS) as f32 * LINE);
        f.scroll.set_extent(len as f32 * LINE, f.list.h);
        f
    }

    #[test]
    fn only_the_lines_in_view_are_drawn_however_many_there_are() {
        let view = LIST_ROWS as f32 * LINE;
        assert_eq!(rows_in_view(5000, 0.0, view), 0..6);
        // Half a line down: the first is half hidden and the seventh half shows.
        assert_eq!(rows_in_view(5000, LINE / 2.0, view), 0..7);
        assert_eq!(rows_in_view(5000, 4994.0 * LINE, view), 4994..5000);
        assert_eq!(rows_in_view(3, 0.0, 3.0 * LINE), 0..3);
        assert_eq!(rows_in_view(0, 0.0, 0.0), 0..0);
        // (Past the end, as it can be for a moment while the list shrinks: nothing out of range.)
        assert_eq!(rows_in_view(4, 99.0 * LINE, view), 4..4);
    }

    #[test]
    fn the_wheel_scrolls_the_list_to_its_last_line_and_back() {
        let mut f = laid_out(5000);
        assert_eq!(f.wheel(Point::new(100.0, 210.0), -1.0), Answer::Redraw);
        assert_eq!(f.scroll.offset, WHEEL_ROWS * LINE);
        // A big spin ends at the last line, which then shows in full.
        for _ in 0..2000 {
            f.wheel(Point::new(100.0, 210.0), -1.0);
        }
        assert_eq!(f.scroll.offset, (5000 - LIST_ROWS) as f32 * LINE);
        assert_eq!(rows_in_view(5000, f.scroll.offset, f.list.h).last(), Some(4999));
        assert_eq!(f.wheel(Point::new(100.0, 210.0), -1.0), Answer::Nothing, "nothing further to go");
        f.wheel(Point::new(100.0, 210.0), 1.0e6);
        assert_eq!(f.scroll.offset, 0.0);
        // Outside the panel the wheel does nothing.
        f.wheel(Point::new(900.0, 900.0), -1.0);
        assert_eq!(f.scroll.offset, 0.0);
    }

    /// Every letter one unit wide (and the arrow five), as the tests' ruler.
    fn chars(t: &str) -> f32 {
        t.chars().count() as f32
    }

    #[test]
    fn a_line_too_long_shows_the_new_name_first() {
        let fit = |old: &str, new: &str, w: f32| fit_row(old, new, w, &mut chars);
        // Both fit: as they are.
        assert_eq!(fit("a.jpg", "Photo 1.jpg", 40.0), "a.jpg  \u{2192}  Photo 1.jpg");
        assert_eq!(chars("a.jpg  \u{2192}  Photo 1.jpg"), 21.0);
        assert_eq!(fit("a.jpg", "Photo 1.jpg", 21.0), "a.jpg  \u{2192}  Photo 1.jpg");
        // A long old name: the new one stays whole, the old one keeps its start and its end.
        let old = format!("IMG_{}_0042.jpg", "x".repeat(80));
        let line = fit(&old, "Holiday 12.jpg", 60.0);
        assert!(line.ends_with("  \u{2192}  Holiday 12.jpg"), "{line}");
        assert!(line.starts_with("IMG_") && line.contains("\u{2026}") && line.contains("42.jpg  "), "{line}");
        assert!(chars(&line) <= 60.0, "{line}");
        // Both long: the new one gets most of the line, cut in the middle, so its number shows.
        let new = format!("Holiday {} 12.jpg", "y".repeat(80));
        let line = fit(&old, &new, 60.0);
        let (o, n) = line.split_once("  \u{2192}  ").unwrap();
        assert!(chars(n) > chars(o) && n.starts_with("Holiday ") && n.ends_with(" 12.jpg"), "{line}");
        assert!(o.starts_with("IMG_") && o.ends_with(".jpg") && chars(&line) <= 60.0, "{line}");
        // A short old name gives the new one the rest of the line.
        let line = fit("a.jpg", &new, 60.0);
        assert!(line.starts_with("a.jpg  \u{2192}  Holiday ") && line.ends_with(" 12.jpg") && chars(&line) <= 60.0, "{line}");
        assert!(chars(&line) >= 58.0, "the room is used: {line}");
        // Odd letters are cut whole, and no width is too small.
        let odd = "\u{1f389}\u{130}\u{f1}#".repeat(30);
        for w in [0.0, 1.0, 5.0, 6.0, 7.0, 12.0, 33.0, 1.0e9, -4.0, f32::NAN] {
            let line = fit(&odd, &odd, w);
            assert!(line.contains("\u{2192}"), "{w}: {line}");
            assert!(w.is_nan() || w < 7.0 || chars(&line) <= w, "{w}: {line}");
        }
        assert_eq!(fit("", "", 0.0), "  \u{2192}  ");
        assert_eq!(fit_middle("abcdefghij", 6.0, &mut chars), "ab\u{2026}hij");
        assert_eq!(fit_middle("abcdefghij", 10.0, &mut chars), "abcdefghij");
        assert_eq!(fit_middle("abcdefghij", 0.0, &mut chars), "\u{2026}");
        assert_eq!(fit_middle("", 0.0, &mut chars), "");
    }

    #[test]
    fn the_scroll_bar_is_dragged_and_clicked() {
        let mut f = laid_out(5000);
        let thumb = f.thumb().expect("a list this long scrolls");
        let mid = Point::new(thumb.x + thumb.w / 2.0, thumb.y + thumb.h / 2.0);
        // Over the thumb it lights up; pressed there it is held.
        assert_eq!(f.mouse_move(mid), Answer::Redraw);
        assert!(f.over_thumb);
        assert_eq!(f.mouse_down(mid), Answer::Redraw);
        assert!(f.dragging());
        // Dragged down to the end of the list, and past it: the last line shows.
        assert_eq!(f.mouse_move(Point::new(mid.x, mid.y + 40.0)), Answer::Redraw);
        assert!(f.scroll.offset > 0.0 && f.scroll.offset < f.scroll.max());
        f.mouse_move(Point::new(-500.0, 9000.0));
        assert_eq!(f.scroll.offset, f.scroll.max());
        assert_eq!(rows_in_view(5000, f.scroll.offset, f.list.h).last(), Some(4999));
        // Let go outside the panel: the drag ends and the form stays open.
        assert_eq!(f.mouse_up(Point::new(-500.0, 9000.0)), Answer::Redraw);
        assert!(!f.dragging() && !f.over_thumb);
        assert_eq!(f.mouse_move(Point::new(100.0, 100.0)), Answer::Nothing);
        assert_eq!(f.scroll.offset, f.scroll.max(), "it no longer follows the mouse");
        // Back up to the top.
        let thumb = f.thumb().unwrap();
        f.mouse_down(Point::new(thumb.x + 1.0, thumb.y + 1.0));
        f.mouse_move(Point::new(thumb.x, -9000.0));
        assert_eq!(f.scroll.offset, 0.0);
        f.let_go();
        assert!(!f.dragging());
        // A click beside the thumb goes a page that way.
        let lane = f.bar.place(f.list).lane;
        assert_eq!(f.mouse_down(Point::new(lane.x + lane.w / 2.0, lane.bottom() - 8.0)), Answer::Redraw);
        assert_eq!(f.scroll.offset, (LIST_ROWS - 1) as f32 * LINE);
        assert!(!f.dragging());
        f.mouse_up(Point::new(lane.x + lane.w / 2.0, lane.bottom() - 8.0));
        // A list that fits has no bar to hold: a press there is a press on the panel.
        let mut f = laid_out(LIST_ROWS);
        assert!(f.thumb().is_none());
        assert_eq!(f.mouse_down(Point::new(f.list.right() - 4.0, f.list.y + 4.0)), Answer::Nothing);
        assert!(!f.dragging());
    }

    #[test]
    fn a_password_boxs_show_button_is_found_under_the_mouse() {
        let mut f = laid_out(0);
        f.used = 2;
        f.eyes[1] = Some(Rect::new(400.0, 100.0, EYE_W, 22.0));
        assert_eq!(f.button_at(Point::new(410.0, 110.0)), Some(Button::Eye(1)));
        assert_eq!(f.button_at(Point::new(300.0, 110.0)), None);
        // Over it it lights up; pressed and let go there it flips (no real box in a test: nothing).
        assert_eq!(f.mouse_move(Point::new(410.0, 110.0)), Answer::Redraw);
        assert_eq!(f.mouse_down(Point::new(410.0, 110.0)), Answer::Redraw);
        assert_eq!(f.mouse_up(Point::new(410.0, 110.0)), Answer::Nothing);
        assert!(!f.shown[1]);
        // Let go somewhere else: no flip.
        f.mouse_down(Point::new(410.0, 110.0));
        assert_eq!(f.mouse_up(Point::new(300.0, 110.0)), Answer::Redraw);
    }

    #[test]
    fn the_slider_picks_the_number_nearest_the_mouse() {
        let mut f = laid_out(0);
        f.slider = Some((1, 1, 10));
        f.slider_shown = true;
        f.slider_track = Rect::new(150.0, 100.0, 270.0, FIELD_H);
        // Pressed at the middle: 5.5 rounds to 6 (each number is 30 apart).
        assert_eq!(f.mouse_down(Point::new(285.0, 110.0)), Answer::Redraw);
        assert!(f.dragging());
        assert_eq!(f.values(), vec!["6".to_string()]);
        // Dragged past either end it stops at 10 and 1.
        f.mouse_move(Point::new(900.0, 110.0));
        assert_eq!(f.slider.unwrap().0, 10);
        f.mouse_move(Point::new(-50.0, 300.0));
        assert_eq!(f.slider.unwrap().0, 1);
        f.mouse_up(Point::new(180.0, 300.0));
        assert_eq!(f.slider.unwrap().0, 2);
        assert!(!f.dragging());
        // The arrows: one up, one down, never past the ends.
        assert_eq!(f.key(windows::Win32::UI::Input::KeyboardAndMouse::VK_RIGHT.0), Answer::Redraw);
        assert_eq!(f.slider.unwrap().0, 3);
        for _ in 0..5 {
            f.key(windows::Win32::UI::Input::KeyboardAndMouse::VK_LEFT.0);
        }
        assert_eq!(f.slider.unwrap().0, 1);
        assert_eq!(f.key(windows::Win32::UI::Input::KeyboardAndMouse::VK_LEFT.0), Answer::Nothing);
    }

    #[test]
    fn a_list_that_fits_doesnt_scroll() {
        let mut f = laid_out(LIST_ROWS);
        assert_eq!(f.wheel(Point::new(100.0, 210.0), -1.0), Answer::Nothing);
        assert_eq!(f.scroll.offset, 0.0);
        let mut f = laid_out(0);
        assert_eq!(f.wheel(Point::new(100.0, 210.0), -1.0), Answer::Nothing);
    }
}
