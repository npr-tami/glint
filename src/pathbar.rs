//! The path bar in the toolbar: the folder you're in as clickable steps,
//! This PC › Local Disk (C:) › Users › Downloads.
//!
//! Earlier steps are muted text that gets a box when the mouse is over it; the folder you're in sits
//! in a slanted box (a parallelogram, like the GLINT logo's cut) in lighter text. When the steps don't
//! fit, the earliest ones make way for "…", which lists them in a popup. The empty part on the right
//! is for typing: a click there (or Ctrl+L / F4) swaps the steps for a text box (see toolbar.rs).
//!
//! The path is only split as text - the disk is never asked - so this is instant. Measuring the
//! names is done only when the steps or the width change, not on every frame.
//!
//! This file also holds two small drawing helpers the toolbar shares: the slanted box and the
//! chevron.

use crate::app::AppState;
use crate::render::{Renderer, TextStyle};
use crate::theme::{size, Color, Theme};
use crate::ui::{Point, Rect};

/// Room around a step's name. (How far the folder-you're-in box leans comes from the skin:
/// `Theme::path_lean`, 6 DIPs in AMD Radeon.)
const PAD: f32 = 8.0;
/// The most a name may take before it is shortened with "…".
const MAX_NAME: f32 = 240.0;
/// The chevron between two steps, with its breathing room.
const SEP_W: f32 = 16.0;
/// The "…" step that stands in for the hidden earlier steps.
const MORE_W: f32 = 30.0;
/// Empty room always kept on the right, so there is somewhere to click to type.
const EDIT_ROOM: f32 = 24.0;
/// Step boxes are this much shorter than the path box, top and bottom.
const BOX_INSET: f32 = 4.0;

/// One step as laid out.
#[derive(Clone, Debug)]
pub struct Crumb {
    /// Where it goes ("" = This PC). Empty for the "…" step.
    pub path: String,
    pub name: String,
    /// Its box. A click anywhere in this column (top to bottom of the bar) picks it.
    pub rect: Rect,
    /// The folder you're in (the last step).
    pub current: bool,
    /// The "…" step.
    pub more: bool,
}

/// What a layout of the steps was made for: the steps (name, path), where the bar sits, how far the
/// folder-you're-in box leans and the font (the last two are the skin's).
type LaidOut = (Vec<(String, String)>, Rect, f32, u32);

/// What is under a point of the path bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathHit {
    /// A step (index into the laid-out steps).
    Step(usize),
    /// The empty part on the right: click to type.
    Empty,
    /// Between steps (on a chevron), or outside the bar.
    Nothing,
}

#[derive(Default)]
pub struct PathBar {
    rect: Rect,
    crumbs: Vec<Crumb>,
    /// The steps behind "…", earliest first: (name, path).
    hidden: Vec<(String, String)>,
    /// Where the steps end; right of it is "click to type".
    end: f32,
    /// What the current layout was made for. Same again = no work.
    laid_out: Option<LaidOut>,
    /// Something dragged would be dropped into this step's folder (toolbar.rs): a red line marks it.
    pub drop_path: Option<String>,
}

impl PathBar {
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The steps hidden behind "…", nearest first (for its popup): (name, path).
    pub fn hidden_nearest_first(&self) -> Vec<(String, String)> {
        self.hidden.iter().rev().cloned().collect()
    }

    pub fn crumb(&self, i: usize) -> Option<&Crumb> {
        self.crumbs.get(i)
    }

    /// What is under `p`, judged by the last layout.
    pub fn hit(&self, p: Point) -> PathHit {
        if !self.rect.contains(p) {
            return PathHit::Nothing;
        }
        if let Some(i) = self.crumbs.iter().position(|c| p.x >= c.rect.x && p.x < c.rect.right()) {
            return PathHit::Step(i);
        }
        if p.x >= self.end {
            PathHit::Empty
        } else {
            PathHit::Nothing
        }
    }

    /// Places the steps for the tab's current path (only measures when something changed).
    /// When they don't fit, the earliest go behind "…" (the folder you're in always stays); when
    /// even that one doesn't fit, its name is shortened.
    fn layout(&mut self, r: &mut Renderer, app: &AppState) {
        let steps = app.path_steps(&app.tab().path);
        let lean = app.theme.path_lean();
        let font = r.font_generation();
        if let Some((s, at, l, f)) = &self.laid_out {
            if *s == steps && *at == self.rect && *l == lean && *f == font {
                return;
            }
        }
        self.crumbs.clear();
        self.hidden.clear();

        let style = TextStyle::new(Color::hex(0));
        let box_h = (self.rect.h - 2.0 * BOX_INSET).max(size::FONT + 4.0);
        let top = self.rect.y + (self.rect.h - box_h) / 2.0;
        let left = self.rect.x + 4.0;
        let room = self.rect.right() - EDIT_ROOM - left;

        let last = steps.len().saturating_sub(1);
        let mut widths: Vec<f32> = steps
            .iter()
            .enumerate()
            .map(|(i, (name, _))| r.measure(name, &style).min(MAX_NAME).ceil() + 2.0 * PAD + if i == last { 2.0 * lean } else { 0.0 })
            .collect();
        let total = |from: usize, widths: &[f32]| -> f32 {
            let mut t = if from > 0 { MORE_W + SEP_W } else { 0.0 };
            for (i, w) in widths.iter().enumerate().skip(from) {
                t += w + if i < last { SEP_W } else { 0.0 };
            }
            t
        };
        let mut first = 0;
        while first < last && total(first, &widths) > room {
            first += 1;
        }
        if !widths.is_empty() && total(first, &widths) > room {
            let rest = room - if first > 0 { MORE_W + SEP_W } else { 0.0 };
            widths[last] = rest.max(2.0 * PAD + 2.0 * lean + 20.0).min(widths[last]);
        }

        let mut x = left;
        if first > 0 {
            self.hidden.extend(steps[..first].iter().cloned());
            self.crumbs.push(Crumb { path: String::new(), name: "\u{2026}".into(), rect: Rect::new(x, top, MORE_W, box_h), current: false, more: true });
            x += MORE_W + SEP_W;
        }
        for (i, (name, path)) in steps.iter().enumerate().skip(first) {
            self.crumbs.push(Crumb { path: path.clone(), name: name.clone(), rect: Rect::new(x, top, widths[i], box_h), current: i == last, more: false });
            x += widths[i] + if i < last { SEP_W } else { 0.0 };
        }
        self.end = x;
        self.laid_out = Some((steps, self.rect, lean, font));
    }

    /// Draws the box and (unless `editing`, when a text box sits on top) the steps.
    /// `mouse` is where the mouse is, if it is over the bar.
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, editing: bool, mouse: Option<Point>) {
        r.fill_rect(self.rect, theme.input);
        if editing {
            return;
        }
        self.layout(r, app);
        let hover = match mouse.map(|p| self.hit(p)) {
            Some(PathHit::Step(i)) => Some(i),
            _ => None,
        };
        let style = TextStyle::new(theme.muted);
        let lean = theme.path_lean();
        r.push_clip(self.rect);
        for (i, c) in self.crumbs.iter().enumerate() {
            let dropping = !c.more && self.drop_path.as_deref() == Some(c.path.as_str());
            let hot = hover == Some(i) || dropping;
            let mut color = theme.muted;
            let mut inset = PAD;
            if c.current {
                fill_box(r, theme, c.rect, lean, if hot { theme.chip_hover } else { theme.chip });
                color = theme.text_strong;
                inset = PAD + lean;
            } else if hot {
                fill_box(r, theme, c.rect, lean, theme.crumb_hover);
                color = theme.text;
            }
            if dropping {
                // The step something dragged would go into: a red line under it.
                r.fill_rect(Rect::new(c.rect.x + lean / 2.0, c.rect.bottom() - 2.0, c.rect.w - lean, 2.0), theme.accent);
            }
            if c.more {
                // Three dots, drawn (not a font's "…", so it looks the same everywhere).
                let cy = c.rect.y + c.rect.h / 2.0;
                let cx = c.rect.x + c.rect.w / 2.0;
                for k in -1..=1 {
                    r.fill_round_rect(Rect::new(cx + k as f32 * 4.5 - 1.1, cy - 1.1, 2.2, 2.2), 1.1, color);
                }
            } else {
                let text = Rect::new(c.rect.x + inset, c.rect.y, (c.rect.w - 2.0 * inset).max(0.0), c.rect.h);
                r.text(&c.name, text, &style.color(color));
            }
            if i + 1 < self.crumbs.len() {
                let cx = c.rect.right() + SEP_W / 2.0;
                chevron_right(r, Point::new(cx, c.rect.y + c.rect.h / 2.0), 3.5, theme.muted.with_alpha(0.8));
            }
        }
        r.pop_clip();
    }
}

/// The four corners of `rect` slanted like "/": the top edge shifted `lean` to the right of the
/// bottom edge (both side edges lean the same way - a parallelogram).
pub fn slanted_points(rect: Rect, lean: f32) -> [Point; 4] {
    [
        Point::new(rect.x + lean, rect.y),
        Point::new(rect.right(), rect.y),
        Point::new(rect.right() - lean, rect.bottom()),
        Point::new(rect.x, rect.bottom()),
    ]
}

/// Fills a slanted box. Top and bottom land on whole pixels so they stay sharp; only the slanted
/// sides are smoothed.
pub fn fill_slanted(r: &mut Renderer, rect: Rect, lean: f32, color: Color) {
    let (top, bottom) = (r.snap(rect.y), r.snap(rect.bottom()));
    let snapped = Rect::new(rect.x, top, rect.w, bottom - top);
    r.fill_polygon(&slanted_points(snapped, lean), color);
}

/// Fills a box in the skin's shape (theme.rs `Shape`): slanted by `lean` (0: straight), straight,
/// or with round corners. `lean` is what the box leans in this skin (`Theme::lean`).
pub fn fill_box(r: &mut Renderer, theme: &Theme, rect: Rect, lean: f32, color: Color) {
    let radius = theme.corner(rect.h);
    if lean > 0.0 {
        fill_slanted(r, rect, lean, color);
    } else if radius > 0.0 {
        r.fill_round_rect(rect, radius, color);
    } else {
        r.fill_rect(rect, color);
    }
}

/// The outline of such a box, `width` DIPs thick (focus rings, frames).
pub fn outline_box(r: &mut Renderer, theme: &Theme, rect: Rect, lean: f32, width: f32, color: Color) {
    let radius = theme.corner(rect.h);
    if lean > 0.0 {
        let mut ring = slanted_points(rect, lean).to_vec();
        ring.push(ring[0]);
        ring.push(ring[1]); // round the first corner too
        r.polyline(&ring, width, color);
    } else if radius > 0.0 {
        r.stroke_round_rect(rect, radius, width, color);
    } else {
        r.stroke_rect(rect, width, color);
    }
}

/// Whether `p` is inside such a box (the exact slanted shape, so two slanted boxes side by side
/// never overlap; the whole rectangle otherwise).
pub fn box_contains(rect: Rect, lean: f32, p: Point) -> bool {
    if lean > 0.0 {
        slanted_contains(rect, lean, p)
    } else {
        rect.contains(p)
    }
}

/// Whether `p` is inside the slanted box (the exact shape, so two boxes side by side never overlap).
pub fn slanted_contains(rect: Rect, lean: f32, p: Point) -> bool {
    if p.y < rect.y || p.y >= rect.bottom() || rect.h <= 0.0 {
        return false;
    }
    let down = (p.y - rect.y) / rect.h; // 0 at the top, 1 at the bottom
    let left = rect.x + lean * (1.0 - down);
    let right = rect.right() - lean * down;
    p.x >= left && p.x < right
}

/// A small "›" drawn as two lines, `half` DIPs from its middle to its ends.
pub fn chevron_right(r: &mut Renderer, c: Point, half: f32, color: Color) {
    r.polyline(&[Point::new(c.x - half * 0.55, c.y - half), Point::new(c.x + half * 0.55, c.y), Point::new(c.x - half * 0.55, c.y + half)], 1.2, color);
}
