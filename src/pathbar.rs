//! The path bar in the toolbar: the folder you're in as clickable steps,
//! This PC › Local Disk (C:) › Users › Downloads.
//!
//! Earlier steps are muted text that gets a box when the mouse is over it; the folder you're in sits
//! in a slanted box (a parallelogram, like the GLINT logo's cut) in lighter text. (With `buttons:
//! liquid` the steps are in the normal text colour and the folder you're in is bold, without a box.)
//! When the steps don't fit, the earliest ones make way for "…", which lists them in a popup. The
//! empty part on the right is for typing: a click there (or Ctrl+L / F4) swaps the steps for a text
//! box (see toolbar.rs).
//!
//! The path is only split as text - the disk is never asked - so this is instant. Measuring the
//! names is done only when the steps or the width change, not on every frame.
//!
//! This file also holds two small drawing helpers the toolbar shares: the slanted box and the
//! chevron.

use crate::app::AppState;
use crate::render::{Renderer, TextStyle};
use crate::theme::{size, ButtonStyle, Color, Paint, Stop, Theme};
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
/// folder-you're-in box leans, the font, whether the folder you're in is bold and how the steps
/// look (the last four are the skin's).
type LaidOut = (Vec<(String, String)>, Rect, f32, u32, bool, Crumbs);

/// How the steps look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Crumbs {
    /// Glint's own: muted steps with chevrons between, the folder you're in in a box.
    #[default]
    Modern,
    /// 2000 and XP's Address box: the path as plain text ("C:\Users\Me"), each folder in it still
    /// a step to click (underlined under the mouse), the place's icon before it, a drop arrow
    /// after it.
    Plain,
    /// 7's breadcrumbs: the place's icon, then the steps as words with solid black arrows between,
    /// a box only round the one under the mouse.
    Seven,
}

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
    style: Crumbs,
}

impl PathBar {
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    pub fn set_style(&mut self, style: Crumbs) {
        self.style = style;
    }

    /// Room kept before the steps (for the place's icon) and after them (the drop arrow and some
    /// room to click to type).
    fn margins(&self) -> (f32, f32) {
        match self.style {
            Crumbs::Modern => (4.0, EDIT_ROOM),
            Crumbs::Plain => (24.0, 18.0 + EDIT_ROOM),
            Crumbs::Seven => (37.0, 17.0 + EDIT_ROOM),
        }
    }

    /// Where the place's icon goes (Plain and Seven).
    pub fn icon_rect(&self) -> Rect {
        let x = if self.style == Crumbs::Seven { 4.0 } else { 5.0 };
        Rect::new(self.rect.x + x, (self.rect.y + (self.rect.h - 16.0) / 2.0).round(), 16.0, 16.0)
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
        let mut steps = app.path_steps(&app.tab().path);
        let style_now = self.style;
        if style_now == Crumbs::Plain {
            // The path as Windows wrote it: no "This PC" in front, a drive as its letter ("C:").
            if steps.len() > 1 && steps[0].1.is_empty() {
                steps.remove(0);
            }
            for (name, path) in &mut steps {
                let root = path.trim_end_matches('\\');
                if root.len() == 2 && root.ends_with(':') {
                    *name = root.to_string();
                }
            }
        }
        let modern = style_now == Crumbs::Modern;
        let lean = if modern { app.theme.path_lean() } else { 0.0 };
        let font = r.font_generation();
        let bold = app.theme.buttons == ButtonStyle::Liquid && modern;
        if let Some((s, at, l, f, b, st)) = &self.laid_out {
            if *s == steps && *at == self.rect && *l == lean && *f == font && *b == bold && *st == style_now {
                return;
            }
        }
        let (pad, sep_w) = match style_now {
            Crumbs::Modern => (PAD, SEP_W),
            Crumbs::Plain => (0.0, r.measure("\\", &TextStyle::new(Color::hex(0))).ceil() + 1.0),
            Crumbs::Seven => (6.0, 15.0),
        };
        self.crumbs.clear();
        self.hidden.clear();

        let style = TextStyle::new(Color::hex(0));
        let inset = if style_now == Crumbs::Seven { 2.0 } else { BOX_INSET };
        let box_h = (self.rect.h - 2.0 * inset).max(size::FONT + 4.0);
        let top = self.rect.y + (self.rect.h - box_h) / 2.0;
        let (lead, trail) = self.margins();
        let left = self.rect.x + lead;
        let room = self.rect.right() - trail - left;

        let last = steps.len().saturating_sub(1);
        let mut widths: Vec<f32> = steps
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let style = if i == last && bold { style.bold() } else { style };
                r.measure(name, &style).min(MAX_NAME).ceil() + 2.0 * pad + if i == last { 2.0 * lean } else { 0.0 }
            })
            .collect();
        let more_w = if modern { MORE_W } else { r.measure("...", &style).ceil() + 2.0 * pad + 2.0 };
        let total = |from: usize, widths: &[f32]| -> f32 {
            let mut t = if from > 0 { more_w + sep_w } else { 0.0 };
            for (i, w) in widths.iter().enumerate().skip(from) {
                t += w + if i < last { sep_w } else { 0.0 };
            }
            t
        };
        let mut first = 0;
        while first < last && total(first, &widths) > room {
            first += 1;
        }
        if !widths.is_empty() && total(first, &widths) > room {
            let rest = room - if first > 0 { more_w + sep_w } else { 0.0 };
            widths[last] = rest.max(2.0 * pad + 2.0 * lean + 20.0).min(widths[last]);
        }

        let mut x = left;
        if first > 0 {
            self.hidden.extend(steps[..first].iter().cloned());
            self.crumbs.push(Crumb { path: String::new(), name: "\u{2026}".into(), rect: Rect::new(x, top, more_w, box_h), current: false, more: true });
            x += more_w + sep_w;
        }
        for (i, (name, path)) in steps.iter().enumerate().skip(first) {
            self.crumbs.push(Crumb { path: path.clone(), name: name.clone(), rect: Rect::new(x, top, widths[i], box_h), current: i == last, more: false });
            x += widths[i] + if i < last { sep_w } else { 0.0 };
        }
        self.end = x;
        self.laid_out = Some((steps, self.rect, lean, font, bold, style_now));
    }

    /// Draws the box and (unless `editing`, when a text box sits on top) the steps.
    /// `mouse` is where the mouse is, if it is over the bar.
    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, editing: bool, mouse: Option<Point>) {
        if self.style != Crumbs::Modern {
            // (The toolbar drew the era's box.)
            crate::retrotool::place_icon(r, theme, &app.tab().path, self.icon_rect());
            if self.style == Crumbs::Seven {
                let at = self.icon_rect();
                crate::retrotool::triangle_right(r, Point::new(at.right() + 9.0, at.y + at.h / 2.0), 7.0, Color::hex(0));
            }
            if editing {
                return;
            }
            self.layout(r, app);
            return self.paint_era(r, theme, mouse);
        }
        fill_field(r, theme, self.rect);
        if editing {
            return;
        }
        self.layout(r, app);
        let hover = match mouse.map(|p| self.hit(p)) {
            Some(PathHit::Step(i)) => Some(i),
            _ => None,
        };
        // With liquid buttons the folder you're in is bold text rather than a button of glass, and
        // the steps before it are in the normal text colour.
        let liquid = theme.buttons == ButtonStyle::Liquid;
        let step_color = if liquid { theme.text } else { theme.muted };
        let style = TextStyle::new(step_color);
        let lean = theme.path_lean();
        r.push_clip(self.rect);
        for (i, c) in self.crumbs.iter().enumerate() {
            let dropping = !c.more && self.drop_path.as_deref() == Some(c.path.as_str());
            let hot = hover == Some(i) || dropping;
            let mut color = step_color;
            let mut inset = PAD;
            let mut style = style;
            if c.current && liquid {
                if hot {
                    fill_box(r, theme, c.rect, lean, theme.crumb_hover);
                }
                color = theme.text_strong;
                style = style.bold();
                inset = PAD + lean;
            } else if c.current {
                fill_box_paint(r, theme, c.rect, lean, if hot { &theme.chip_hover_paint } else { &theme.chip_paint });
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

impl PathBar {
    /// The steps as 2000 / XP's plain path or 7's breadcrumbs.
    fn paint_era(&mut self, r: &mut Renderer, theme: &Theme, mouse: Option<Point>) {
        let hover = match mouse.map(|p| self.hit(p)) {
            Some(PathHit::Step(i)) => Some(i),
            _ => None,
        };
        let seven = self.style == Crumbs::Seven;
        // The colour a step under the mouse takes (Windows' "hot" link colour).
        let hot_color = match theme.era {
            crate::theme::Era::Xp => Color::hex(0x000080),
            _ => theme.accent,
        };
        let style = TextStyle::new(theme.text);
        r.push_clip(self.rect);
        let count = self.crumbs.len();
        for (i, c) in self.crumbs.iter().enumerate() {
            let dropping = !c.more && self.drop_path.as_deref() == Some(c.path.as_str());
            let hot = hover == Some(i) || dropping;
            let name = if c.more { "..." } else { c.name.as_str() };
            if seven {
                if hot {
                    // The step and the arrow after it light up as one split button.
                    let w = c.rect.w + if i + 1 < count { 15.0 } else { 0.0 };
                    crate::retrotool::seven_crumb_box(r, Rect::new(c.rect.x, c.rect.y, w, c.rect.h), false);
                    if i + 1 < count {
                        r.fill_rect(Rect::new(c.rect.right(), c.rect.y + 1.0, 1.0, c.rect.h - 2.0), Color::hex(0x3c7fb1));
                    }
                }
                r.text(name, Rect::new(c.rect.x + 6.0, c.rect.y, (c.rect.w - 12.0).max(0.0), c.rect.h), &style);
                if i + 1 < count {
                    crate::retrotool::triangle_right(r, Point::new(c.rect.right() + 8.0, c.rect.y + c.rect.h / 2.0), 7.0, Color::hex(0));
                }
            } else {
                let color = if hot { hot_color } else { theme.text };
                r.text(name, c.rect, &style.color(color));
                if hot {
                    let w = r.measure(name, &style).min(c.rect.w);
                    let base = (c.rect.y + c.rect.h / 2.0 + size::FONT * 0.55).round();
                    r.fill_rect(Rect::new(c.rect.x, base, w, 1.0), color);
                }
                if i + 1 < count {
                    r.text("\\", Rect::new(c.rect.right(), c.rect.y, 12.0, c.rect.h), &style);
                }
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

/// Fills a box in the skin's shape, as `fill_box`, with a paint (a gradient runs across the box's
/// short side).
pub fn fill_box_paint(r: &mut Renderer, theme: &Theme, rect: Rect, lean: f32, paint: &Paint) {
    if lean > 0.0 {
        let (top, bottom) = (r.snap(rect.y), r.snap(rect.bottom()));
        r.fill_polygon_paint(&slanted_points(Rect::new(rect.x, top, rect.w, bottom - top), lean), paint);
    } else {
        r.fill_paint(rect, theme.corner(rect.h), paint);
    }
}

/// Windows 2000's 3D edge just inside `rect` (`bevels: true`), two DIPs deep: light on the top and
/// left and dark on the bottom and right when `raised`, the other way round when sunken. `face` is
/// the surface it sits on, which the shades are made from.
pub fn bevel(r: &mut Renderer, theme: &Theme, rect: Rect, raised: bool, face: Color) {
    if rect.w < 4.0 || rect.h < 4.0 {
        return;
    }
    let kind = if raised { crate::retro::Edge::RaisedWindow } else { crate::retro::Edge::Sunken };
    crate::retro::edge(r, theme, rect, kind, face);
}

/// A selected row (or one under the mouse, `hover`) in `paint`: with the skin's `selection-border`
/// it has round corners and that edge (fainter under the mouse), else it is a plain band.
pub fn fill_row(r: &mut Renderer, theme: &Theme, rect: Rect, paint: &Paint, hover: bool) {
    if theme.selection_border.a <= 0.0 {
        return r.fill_paint(rect, 0.0, paint);
    }
    let rect = rect.inset(1.0, 0.5);
    r.fill_paint(rect, 3.0, paint);
    let edge = if hover { theme.selection_border.with_alpha(theme.selection_border.a * 0.55) } else { theme.selection_border };
    r.stroke_round_rect(rect.inset(0.5, 0.5), 2.5, 1.0, edge);
}

/// A text field of the main window (the path, the filter box): the skin's `input` paint, round
/// corners in a rounded skin, and its `input-border` edge if it has one.
pub fn fill_field(r: &mut Renderer, theme: &Theme, rect: Rect) {
    let radius = theme.corner(rect.h);
    r.fill_paint(rect, radius, &field_paint(&theme.input_paint));
    if theme.input_border.a > 0.0 {
        // A faint shade along the inside of the top edge: the field sits below the surface.
        r.fill_rect(Rect::new(rect.x + radius * 0.7, rect.y + 1.0, (rect.w - 1.4 * radius).max(0.0), 1.0), Color::rgba(0, 0, 0, 0.07));
        r.stroke_round_rect(rect.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, theme.input_border);
    }
    if theme.bevels {
        bevel(r, theme, rect, false, theme.toolbar);
    }
}

/// A field's paint: a gradient keeps its first and last colours at the top and bottom but holds
/// the field's one colour (`Paint::base`, which Windows' text box in it is painted with) across the
/// middle, where the text box sits, so the box doesn't show as a patch.
fn field_paint(paint: &Paint) -> Paint {
    if paint.gradient().is_none() {
        return *paint;
    }
    let mid = paint.base();
    Paint::from_stops(&[
        Stop { at: 0.0, color: paint.first() },
        Stop { at: 0.2, color: mid },
        Stop { at: 0.8, color: mid },
        Stop { at: 1.0, color: paint.last() },
    ])
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

/// A ring right round the outside of a box drawn in `rect` (no gap), `width` DIPs thick, in the
/// box's own shape: sides leaning by `lean` as the box's do, or corners as round as its `radius`
/// (and a little rounder, so the ring keeps the same distance all the way round).
pub fn ring_around(r: &mut Renderer, rect: Rect, lean: f32, radius: f32, width: f32, color: Color) {
    let d = width / 2.0; // the line's middle: its inside edge lies on the box's edge
    if lean > 0.0 && rect.h > 0.0 {
        // Each side moved out by d, the slanted ones along their slant.
        let k = lean / rect.h;
        let side = d * (1.0 + k * k).sqrt();
        let (top, bottom) = (rect.y - d, rect.bottom() + d);
        let mut ring = vec![
            Point::new(rect.x - side + k * (rect.h + d), top),
            Point::new(rect.right() + side + k * d, top),
            Point::new(rect.right() + side - k * (rect.h + d), bottom),
            Point::new(rect.x - side - k * d, bottom),
        ];
        ring.push(ring[0]);
        ring.push(ring[1]); // round the first corner too
        r.polyline(&ring, width, color);
    } else if radius > 0.0 {
        r.stroke_round_rect(rect.inset(-d, -d), radius + d, width, color);
    } else {
        r.stroke_rect(rect.inset(-width, -width), width, color);
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
