//! The pane on the right of the file list: the details panel (Alt+P, details.rs) or the Space
//! preview (preview.rs). Space and Alt+P are independent; when both would show, the preview takes
//! the details panel's place (and the details panel comes back when the preview closes).
//!
//! It is one part of the window (ui::Component). Its rectangle starts with the gap between the list
//! and the pane (theme `size::GAP`, the window's colour, like the gaps between the other panels):
//! that gap is the splitter. Dragging it changes the pane's width, which settings.json remembers -
//! one width for the details panel ("DetailsWidth") and one for the preview ("PreviewWidth"); a
//! double-click puts it back to the standard width. The window lays the list out around the pane
//! (`pane_width`), and the pane asks for a new layout with `Command::SideWidth`.

use crate::app::AppState;
use crate::details::DetailsPanel;
use crate::preview::PreviewPane;
use crate::render::Renderer;
use crate::theme::{size, Theme};
use crate::ui::{Command, Component, Ctx, CursorKind, Effect, MouseButton, MouseEvent, Mods, Point, Rect};

/// The details panel's standard width, and the narrowest it gets.
pub const DETAILS_W: f32 = 280.0;
const DETAILS_MIN: f32 = 220.0;
/// The narrowest the preview gets (its standard width is half of the room).
const PREVIEW_MIN: f32 = 300.0;
/// The list keeps at least this much room; when there isn't room for both, the pane doesn't show.
const LIST_MIN: f32 = 280.0;
/// The splitter can be grabbed this far into the pane too (the gap alone is thin).
const GRAB: f32 = 3.0;

/// What the pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Showing {
    Nothing,
    Details,
    Preview,
}

/// Dragging the splitter: where the mouse went down and the pane's width then.
#[derive(Clone, Copy, Debug)]
struct Split {
    start_x: f32,
    start_w: f32,
}

pub struct Side {
    /// The gap (the splitter) and the pane.
    rect: Rect,
    preview_open: bool,
    details: DetailsPanel,
    preview: PreviewPane,
    split: Option<Split>,
    /// Which part the mouse went down on (so the button coming up goes there too).
    down_on_pane: bool,
    /// What the pane showed at the last layout (to tell the panels when they're hidden).
    showing: Showing,
}

impl Default for Side {
    fn default() -> Self {
        Self::new()
    }
}

impl Side {
    pub fn new() -> Side {
        Side { rect: Rect::default(), preview_open: false, details: DetailsPanel::new(), preview: PreviewPane::new(), split: None, down_on_pane: false, showing: Showing::Nothing }
    }

    pub fn showing(&self, app: &AppState) -> Showing {
        if self.preview_open {
            Showing::Preview
        } else if app.settings.show_details {
            Showing::Details
        } else {
            Showing::Nothing
        }
    }

    pub fn preview_open(&self) -> bool {
        self.preview_open
    }

    /// Opens or closes the Space preview.
    pub fn set_preview(&mut self, open: bool) {
        if self.preview_open && !open {
            self.preview.closed();
        }
        self.preview_open = open;
        self.split = None;
    }

    /// The pane's width (without the gap) when the list and the pane share `room` DIPs; 0 when
    /// nothing shows or there isn't room for it next to the list.
    pub fn pane_width(&self, app: &AppState, room: f32) -> f32 {
        let (wanted, min) = match self.showing(app) {
            Showing::Nothing => return 0.0,
            Showing::Details => (if app.settings.details_width > 0.0 { app.settings.details_width } else { DETAILS_W }, DETAILS_MIN),
            Showing::Preview => (if app.settings.preview_width > 0.0 { app.settings.preview_width } else { ((room - size::GAP) / 2.0).round() }, PREVIEW_MIN),
        };
        pane_width(wanted, min, room)
    }

    /// The splitter was dragged (or double-clicked: `w` 0 means the standard width): remember the
    /// width the person chose for what shows now.
    pub fn set_width(&self, app: &mut AppState, w: f32) {
        let w = if w > 0.0 { w.round().clamp(DETAILS_MIN.min(PREVIEW_MIN), 4000.0) } else { 0.0 };
        match self.showing(app) {
            Showing::Details => app.settings.details_width = w,
            Showing::Preview => app.settings.preview_width = w,
            Showing::Nothing => {}
        }
    }

    /// The pane itself, right of the gap.
    fn pane(&self) -> Rect {
        Rect::new(self.rect.x + size::GAP, self.rect.y, (self.rect.w - size::GAP).max(0.0), self.rect.h)
    }

    fn on_splitter(&self, p: Point) -> bool {
        !self.rect.is_empty() && p.x >= self.rect.x && p.x < self.rect.x + size::GAP + GRAB && p.y >= self.rect.y && p.y < self.rect.bottom()
    }

    /// Keys while the preview is open, asked before the file list's (Esc, Left, Right).
    pub fn preview_key(&mut self, vk: u16, mods: Mods, app: &mut AppState) -> Effect {
        if !self.preview_open || self.rect.is_empty() {
            return Effect::None;
        }
        self.preview.key_down(vk, mods, app)
    }

    /// Typed characters while the preview is open (+ and - zoom).
    pub fn preview_char(&mut self, ch: char) -> Effect {
        if !self.preview_open || self.rect.is_empty() {
            return Effect::None;
        }
        self.preview.char_input(ch)
    }
}

/// `wanted`, but at least `min`, leaving the list LIST_MIN of `room`; 0 if that can't be had.
fn pane_width(wanted: f32, min: f32, room: f32) -> f32 {
    let max = room - LIST_MIN - size::GAP;
    if max < min {
        return 0.0;
    }
    wanted.clamp(min, max)
}

impl Component for Side {
    fn layout(&mut self, rect: Rect, _ctx: &Ctx, app: &AppState) {
        self.rect = rect;
        let pane = self.pane();
        let showing = if rect.is_empty() { Showing::Nothing } else { self.showing(app) };
        if showing != self.showing && self.showing == Showing::Details {
            self.details.hidden();
        }
        self.showing = showing;
        self.details.layout(if showing == Showing::Details { pane } else { Rect::default() });
        self.preview.layout(if showing == Showing::Preview { pane } else { Rect::default() });
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        if self.rect.is_empty() {
            return;
        }
        // The gap is the window's own colour (the renderer cleared it); while dragging it shows red.
        if self.split.is_some() {
            r.fill_rect(Rect::new(self.rect.x + size::GAP / 2.0 - 1.0, self.rect.y, 2.0, self.rect.h), theme.accent);
        }
        match self.showing {
            Showing::Details => self.details.paint(r, theme, app, ctx),
            Showing::Preview => self.preview.paint(r, theme, app, ctx),
            Showing::Nothing => {}
        }
    }

    fn cursor(&self, p: Point, _app: &AppState) -> CursorKind {
        if self.split.is_some() || self.on_splitter(p) {
            CursorKind::SizeWE
        } else if self.showing == Showing::Preview {
            self.preview.cursor(p)
        } else {
            CursorKind::Arrow
        }
    }

    fn mouse_move(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if let Some(s) = self.split {
            let w = s.start_w + (s.start_x - e.pos.x);
            let now = self.pane().w;
            return if (w - now).abs() >= 0.5 { Effect::Command(Command::SideWidth(w)) } else { Effect::None };
        }
        match self.showing {
            Showing::Details => self.details.mouse_move(e.pos, app),
            Showing::Preview => self.preview.mouse_move(e),
            Showing::Nothing => Effect::None,
        }
    }

    fn mouse_leave(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        match self.showing {
            Showing::Details => self.details.mouse_leave(),
            Showing::Preview => self.preview.mouse_leave(),
            Showing::Nothing => Effect::None,
        }
    }

    fn mouse_down(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.down_on_pane = false;
        if self.on_splitter(e.pos) {
            if e.button == MouseButton::Left {
                self.split = Some(Split { start_x: e.pos.x, start_w: self.pane().w });
                return Effect::Redraw;
            }
            return Effect::None;
        }
        self.down_on_pane = true;
        match self.showing {
            Showing::Details => self.details.mouse_down(e, app),
            Showing::Preview => self.preview.mouse_down(e),
            Showing::Nothing => Effect::None,
        }
    }

    fn mouse_up(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if self.split.take().is_some() {
            return Effect::Redraw;
        }
        if !std::mem::take(&mut self.down_on_pane) {
            return Effect::None;
        }
        match self.showing {
            Showing::Details => self.details.mouse_up(ctx, e, app),
            Showing::Preview => self.preview.mouse_up(e, app),
            Showing::Nothing => Effect::None,
        }
    }

    fn double_click(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if self.on_splitter(e.pos) {
            self.split = None;
            return Effect::Command(Command::SideWidth(0.0));
        }
        match self.showing {
            Showing::Details => self.details.double_click(e, app),
            Showing::Preview => self.preview.double_click(e),
            Showing::Nothing => Effect::None,
        }
    }

    fn wheel(&mut self, _ctx: &Ctx, e: &MouseEvent, _app: &mut AppState) -> Effect {
        match self.showing {
            Showing::Preview => self.preview.wheel(e),
            _ => Effect::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_leaves_the_list_room() {
        // Room for both: as wanted.
        assert_eq!(pane_width(280.0, 220.0, 1000.0), 280.0);
        // Narrower than its smallest: its smallest.
        assert_eq!(pane_width(100.0, 220.0, 1000.0), 220.0);
        // Wider than the list allows: what the list leaves.
        assert_eq!(pane_width(900.0, 220.0, 1000.0), 1000.0 - LIST_MIN - size::GAP);
        // No room for both: no pane.
        assert_eq!(pane_width(280.0, 220.0, 400.0), 0.0);
    }

    #[test]
    fn widths_are_remembered_per_pane() {
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        let mut side = Side::new();
        assert_eq!(side.showing(&app), Showing::Nothing, "off by default");
        assert_eq!(side.pane_width(&app, 1000.0), 0.0);
        app.settings.show_details = true;
        assert_eq!(side.pane_width(&app, 1000.0), DETAILS_W);
        side.set_width(&mut app, 333.4);
        assert_eq!(app.settings.details_width, 333.0);
        // The preview replaces the details panel, about half the room, with its own width.
        side.set_preview(true);
        assert_eq!(side.showing(&app), Showing::Preview);
        assert_eq!(side.pane_width(&app, 1006.0), 500.0);
        side.set_width(&mut app, 600.0);
        assert_eq!((app.settings.preview_width, app.settings.details_width), (600.0, 333.0));
        side.set_preview(false);
        assert_eq!(side.showing(&app), Showing::Details);
        side.set_width(&mut app, 0.0);
        assert_eq!(app.settings.details_width, 0.0, "a double-click: the standard width again");
    }
}
