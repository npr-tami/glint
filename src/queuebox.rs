//! The copy queue's list (copyqueue.rs): a small panel at the bottom right of the file list while
//! copies are waiting or running. Each copy has a line saying what it is, a bar for how far it is,
//! its speed and time left (or "Waiting"), and PAUSE / RESUME and CANCEL. It goes away by itself
//! when the last copy is done.

use crate::copyqueue;
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::Theme;
use crate::ui::{Point, Rect};

const WIDTH: f32 = 380.0;
const ROW_H: f32 = 62.0;
const PAD: f32 = 12.0;
const MARGIN: f32 = 14.0;
const BUTTON_W: f32 = 72.0;
const BUTTON_H: f32 = 22.0;
/// Copies shown at most; the others are counted on one line under them.
const MAX_ROWS: usize = 4;
const MORE_H: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Button {
    Pause(u64),
    Cancel(u64),
}

#[derive(Default)]
pub struct QueueBox {
    panel: Rect,
    buttons: Vec<(Button, Rect)>,
    hover: Option<Button>,
    /// It was on screen at the last paint (so the window paints once more when it goes).
    pub shown: bool,
}

impl QueueBox {
    /// Draws the list at the bottom right of `area` (the file list), if anything is queued.
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, area: Rect) {
        let tasks = copyqueue::shown();
        self.buttons.clear();
        self.shown = !tasks.is_empty();
        if tasks.is_empty() {
            self.panel = Rect::default();
            return;
        }
        let rows = tasks.len().min(MAX_ROWS);
        let more = tasks.len() - rows;
        let w = WIDTH.min(area.w - 2.0 * MARGIN).max(200.0);
        let h = rows as f32 * ROW_H + if more > 0 { MORE_H } else { 0.0 } + 4.0;
        let x = area.right() - w - MARGIN;
        let y = (area.bottom() - h - MARGIN).max(area.y);
        self.panel = Rect::new(x, y, w, h);
        r.fill_round_rect(self.panel, 5.0, theme.toolbar);
        r.stroke_round_rect(self.panel.inset(0.5, 0.5), 4.5, 1.0, theme.line);

        let label_style = TextStyle::new(theme.text_strong).semibold();
        let detail_style = TextStyle::new(theme.muted);
        let button_style = TextStyle::new(theme.text).semibold().size(11.0).align(Align::Center);
        for (i, t) in tasks.iter().take(rows).enumerate() {
            let ry = y + 4.0 + i as f32 * ROW_H;
            if i > 0 {
                r.fill_rect(Rect::new(x + PAD, ry, w - 2.0 * PAD, 1.0), theme.line);
            }
            let inner = w - 2.0 * PAD;
            let percent = t.fraction.map(|f| format!("{}%", (f * 100.0).floor() as u32)).unwrap_or_default();
            let pw = if percent.is_empty() { 0.0 } else { 44.0 };
            r.text(&t.label, Rect::new(x + PAD, ry + 6.0, inner - pw, 20.0), &label_style);
            if !percent.is_empty() {
                r.text(&percent, Rect::new(x + w - PAD - pw, ry + 6.0, pw, 20.0), &detail_style.clone().align(Align::Right));
            }
            // The bar (empty while it waits).
            let bar = Rect::new(x + PAD, ry + 29.0, inner, 4.0);
            r.fill_rect(bar, theme.line);
            if let Some(f) = t.fraction {
                r.fill_rect(Rect::new(bar.x, bar.y, (bar.w * f as f32).max(1.0), bar.h), if t.paused { theme.muted } else { theme.accent });
            }
            let by = ry + 37.0;
            let cancel = Rect::new(x + w - PAD - BUTTON_W, by, BUTTON_W, BUTTON_H);
            let pause = Rect::new(cancel.x - 6.0 - BUTTON_W, by, BUTTON_W, BUTTON_H);
            r.text(&t.detail, Rect::new(x + PAD, by, (pause.x - x - PAD - 6.0).max(0.0), BUTTON_H), &detail_style);
            let mut button = |r: &mut Renderer, b: Button, rect: Rect, text: &str| {
                let fill = if self.hover == Some(b) { theme.chip_hover } else { theme.chip };
                r.fill_round_rect(rect, 3.0, fill);
                r.text(text, rect, &button_style);
                self.buttons.push((b, rect));
            };
            if t.running {
                button(r, Button::Pause(t.id), pause, if t.paused { "RESUME" } else { "PAUSE" });
            }
            button(r, Button::Cancel(t.id), cancel, "CANCEL");
        }
        if more > 0 {
            let text = if more == 1 { "1 more waiting".to_string() } else { format!("{more} more waiting") };
            r.text(&text, Rect::new(x + PAD, y + 4.0 + rows as f32 * ROW_H, w - 2.0 * PAD, MORE_H - 4.0), &detail_style);
        }
    }

    /// Whether `p` is on the list (it takes the mouse there).
    pub fn contains(&self, p: Point) -> bool {
        self.shown && self.panel.contains(p)
    }

    fn button_at(&self, p: Point) -> Option<Button> {
        self.buttons.iter().find(|(_, r)| r.contains(p)).map(|(b, _)| *b)
    }

    /// The mouse moved: true when the hover changed (paint again).
    pub fn mouse_move(&mut self, p: Point) -> bool {
        let hover = if self.contains(p) { self.button_at(p) } else { None };
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// A click on the list: PAUSE / RESUME or CANCEL.
    pub fn click(&mut self, p: Point) {
        match self.button_at(p) {
            Some(Button::Pause(id)) => copyqueue::toggle_pause(id),
            Some(Button::Cancel(id)) => copyqueue::cancel(id),
            None => {}
        }
    }
}
