//! The Space preview: a wide pane on the right of the file list (about half the window, resizable),
//! like File Pilot's - not a pop-up window.
//!
//! - A top bar: close ×, fit / 1:1 / fill, the zoom in %, the file's name, and ‹ › to step to the
//!   item before or after it in the list.
//! - Pictures are decoded at full size with WIC on a background thread (peek.rs) and drawn on a
//!   checkerboard. Until the full picture is in, its thumbnail (thumbs.rs) is drawn in the same
//!   place, so it only gets sharper. Ctrl+wheel or + / - zoom, dragging pans, and fit (the whole
//!   picture, never blown up), 1:1 (one pixel of the picture on one pixel of the screen) and fill
//!   (the pane filled, cut where it must) are one click. The zoom mode stays while stepping
//!   through pictures.
//! - Text files show their first 200 KB as monospaced text with line numbers; the wheel scrolls.
//! - Anything else shows its big thumbnail (a video's frame, a PDF's first page, or its icon) and
//!   its details, as the details panel says them (details.rs).
//!
//! The arrow keys keep moving the list's selection while the pane is open (the keyboard stays with
//! the list), and the pane follows. Esc or Space closes it. It shows the item with the keyboard
//! cursor (when it's selected), else the first selected one. Only the picture on screen is kept at
//! full size; closing the pane or going to another file lets it go.

use std::sync::Arc;

use crate::app::AppState;
use crate::details::{self, One, Sel};
use crate::folder;
use crate::pathbar::{chevron_right, fill_box, fill_box_paint};
use crate::peek::{self, Answer, FactsCache, Peeker};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{Color, Theme};
use crate::thumbs::{self, Picture};
use crate::ui::{Command, Ctx, CursorKind, Effect, MouseButton, MouseEvent, Point, Rect};

/// The top bar's height.
pub const TOP_H: f32 = 40.0;
/// A picture decoded for the preview has at most this many pixels (36 million: a big camera's
/// photo); bigger ones are shrunk to it, which keeps the memory sane (4 bytes a pixel).
const MAX_PIXELS: u64 = 36_000_000;
/// The zoom stays between these (as a share of the picture's own pixels).
const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.0;
/// Each wheel notch or + / - zooms by this much.
const ZOOM_STEP: f32 = 1.25;
/// Text files.
const MONO_FONT: f32 = 12.5;
const MONO_LINE: f32 = 18.0;
/// Text lines scrolled per wheel notch.
const WHEEL_LINES: f32 = 3.0;

/// How a picture is sized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// The whole picture in the pane, never bigger than its own size.
    Fit,
    /// One pixel of the picture on one pixel of the screen.
    Actual,
    /// The pane filled (the picture cut where it's longer than the pane).
    Fill,
    /// Zoomed by hand (Ctrl+wheel, + / -), as a share of the picture's own pixels.
    Custom(f32),
}

/// The zoom (picture pixels to screen pixels) a mode means for a picture of `natural` pixels in a
/// view of `view_px` screen pixels.
pub fn zoom_value(zoom: Zoom, natural: (f32, f32), view_px: (f32, f32)) -> f32 {
    let (nw, nh) = (natural.0.max(1.0), natural.1.max(1.0));
    let (sx, sy) = (view_px.0 / nw, view_px.1 / nh);
    match zoom {
        Zoom::Fit => sx.min(sy).min(1.0),
        Zoom::Actual => 1.0,
        Zoom::Fill => sx.max(sy),
        Zoom::Custom(z) => z,
    }
    .max(MIN_ZOOM.min(sx.min(sy)))
}

/// Keeps a picture of `draw` size (DIPs) from being panned out of a `view` (DIPs): centred along a
/// side where it's smaller than the view, and never showing past its edges where it's bigger.
pub fn clamp_pan(pan: Point, draw: (f32, f32), view: (f32, f32)) -> Point {
    let limit = |d: f32, v: f32| ((d - v) / 2.0).max(0.0);
    let (lx, ly) = (limit(draw.0, view.0), limit(draw.1, view.1));
    Point::new(pan.x.clamp(-lx, lx), pan.y.clamp(-ly, ly))
}

/// The pan that keeps the point `at` (relative to the view's centre) on the same spot of the
/// picture when the zoom goes from `old` to `new`.
pub fn pan_for_zoom(pan: Point, at: Point, old: f32, new: f32) -> Point {
    let k = new / old.max(f32::MIN_POSITIVE);
    Point::new(at.x - (at.x - pan.x) * k, at.y - (at.y - pan.y) * k)
}

/// What's under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    None,
    Close,
    Fit,
    Actual,
    Fill,
    Prev,
    Next,
    /// The picture (or text, or thumbnail) area.
    View,
}

/// The picture at full size, for one item.
struct Full {
    key: String,
    picture: Option<Arc<Picture>>,
    /// Its size on disk (it may have been decoded smaller).
    original: (u32, u32),
}

/// What the pane showed at the last frame, for the mouse and the keys.
#[derive(Clone, Copy, Debug)]
struct Shown {
    view: Rect,
    /// A picture: its own size in pixels, and the zoom it was drawn at.
    picture: Option<((f32, f32), f32)>,
    /// Text: how many lines it has and how many fit.
    text: Option<(usize, usize)>,
}

/// Where the top bar's buttons are.
struct Bar {
    close: Rect,
    fit: Rect,
    actual: Rect,
    fill: Rect,
    zoom: Rect,
    name: Rect,
    prev: Rect,
    next: Rect,
}

pub struct PreviewPane {
    rect: Rect,
    peeker: Peeker,
    facts: FactsCache,
    /// The key last asked of peek.rs.
    asked: String,
    full: Option<Full>,
    zoom: Zoom,
    /// Where the picture's centre sits, from the view's centre (DIPs).
    pan: Point,
    /// The item zoom, pan and scroll belong to.
    view_key: String,
    /// The first text line shown (fractions while scrolling).
    scroll: f32,
    hover: Hit,
    pressed: Hit,
    /// Dragging the picture: where the mouse went down and the pan then.
    panning: Option<(Point, Point)>,
    scale: f32,
    shown: Option<Shown>,
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewPane {
    pub fn new() -> PreviewPane {
        PreviewPane {
            rect: Rect::default(),
            peeker: Peeker::new("space preview"),
            facts: FactsCache::new(32),
            asked: String::new(),
            full: None,
            zoom: Zoom::Fit,
            pan: Point::default(),
            view_key: String::new(),
            scroll: 0.0,
            hover: Hit::None,
            pressed: Hit::None,
            panning: None,
            scale: 1.0,
            shown: None,
        }
    }

    pub fn layout(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The pane was closed: the full-size picture and whatever was waiting go.
    pub fn closed(&mut self) {
        self.peeker.cancel();
        self.full = None;
        self.asked.clear();
        self.view_key.clear();
        self.facts.clear();
        self.panning = None;
        self.hover = Hit::None;
        self.pressed = Hit::None;
        self.shown = None;
        if !matches!(self.zoom, Zoom::Custom(_)) {
            return;
        }
        self.zoom = Zoom::Fit;
    }

    fn bar(&self) -> Bar {
        let r = self.rect;
        let y = r.y + 4.0;
        let b = 32.0;
        let close = Rect::new(r.x + 6.0, y, b, b);
        let fit = Rect::new(close.right() + 10.0, y, b, b);
        let actual = Rect::new(fit.right() + 2.0, y, b, b);
        let fill = Rect::new(actual.right() + 2.0, y, b, b);
        let zoom = Rect::new(fill.right() + 6.0, r.y, 52.0, TOP_H);
        let next = Rect::new(r.right() - 6.0 - b, y, b, b);
        let prev = Rect::new(next.x - 2.0 - b, y, b, b);
        let name_x = zoom.right() + 8.0;
        let name = Rect::new(name_x, r.y, (prev.x - 8.0 - name_x).max(0.0), TOP_H);
        Bar { close, fit, actual, fill, zoom, name, prev, next }
    }

    fn view_rect(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y + TOP_H + 1.0, self.rect.w, (self.rect.h - TOP_H - 1.0).max(0.0))
    }

    fn hit(&self, p: Point) -> Hit {
        let b = self.bar();
        let picture = self.shown.is_some_and(|s| s.picture.is_some());
        if b.close.contains(p) {
            Hit::Close
        } else if picture && b.fit.contains(p) {
            Hit::Fit
        } else if picture && b.actual.contains(p) {
            Hit::Actual
        } else if picture && b.fill.contains(p) {
            Hit::Fill
        } else if b.prev.contains(p) {
            Hit::Prev
        } else if b.next.contains(p) {
            Hit::Next
        } else if self.view_rect().contains(p) {
            Hit::View
        } else {
            Hit::None
        }
    }

    fn take_answers(&mut self) {
        for a in self.peeker.take() {
            match a {
                Answer::Facts { key, facts } => self.facts.insert(key, facts),
                // Only the item on screen keeps its picture at full size.
                Answer::Picture { key, picture, original } if key == self.view_key => self.full = Some(Full { key, picture, original }),
                Answer::Picture { .. } => {}
            }
        }
    }

    /// A new item came on screen: start from its top, at the same zoom mode, nothing panned.
    fn new_item(&mut self, key: &str) {
        self.view_key = key.to_string();
        self.full = None;
        self.pan = Point::default();
        self.scroll = 0.0;
        self.panning = None;
        if matches!(self.zoom, Zoom::Custom(_)) {
            self.zoom = Zoom::Fit;
        }
    }

    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        let rect = self.rect;
        if rect.is_empty() {
            return;
        }
        self.scale = ctx.scale;
        self.take_answers();
        r.fill_paint(rect, 0.0, &theme.panel_paint);
        r.push_clip(rect);
        let one = details::focused(app);
        if let Some(o) = &one {
            if o.key != self.view_key {
                self.new_item(&o.key);
            }
            // The full picture was lost with the graphics device: decode it again.
            if self.full.as_ref().is_some_and(|f| f.picture.as_ref().is_some_and(|p| p.is_lost())) {
                self.full = None;
                self.asked.clear();
            }
            let picture = o.is_file() && thumbs::is_picture(&o.entry.ext);
            let need_facts = self.facts.get(&o.key).is_none();
            let need_full = picture && self.full.is_none();
            if (need_facts || need_full) && self.asked != o.key {
                let pic = picture.then(|| (r.max_bitmap_size().min(16384), MAX_PIXELS));
                self.peeker.ask(o.ask(Some(peek::PREVIEW_TEXT), pic, app.settings.show_hidden, app.settings.show_system), ctx.hwnd);
                self.asked = o.key.clone();
            }
        } else {
            self.view_key.clear();
            self.full = None;
        }
        let view = self.view_rect();
        self.shown = Some(Shown { view, picture: None, text: None });
        match &one {
            None => {
                r.text("Select a file to preview it", view, &TextStyle::new(theme.muted).align(Align::Center));
            }
            Some(o) => self.paint_item(r, theme, app, o, view),
        }
        self.paint_bar(r, theme, app, one.as_ref());
        r.pop_clip();
    }

    fn paint_item(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, one: &One, view: Rect) {
        let s = self.scale;
        let facts = self.facts.get(&one.key).cloned();
        let is_picture = one.is_file() && thumbs::is_picture(&one.entry.ext);
        let full_failed = self.full.as_ref().is_some_and(|f| f.key == one.key && f.picture.is_none());
        // The text of a text file (by name, or sniffed when there's no real thumbnail).
        let by_name = one.is_file() && peek::is_text_file(&one.entry.ext);
        let thumb_px = ((view.w.max(view.h) * s) as u32).min(1024);
        let waiting_for_text = by_name && facts.as_ref().is_none_or(|f| f.text.is_some());
        let thumb = if waiting_for_text { None } else { thumbs::request_versioned(&one.shell_path, one.version, thumb_px) };
        let real_thumb = thumb.as_ref().filter(|t| !t.is_icon()).cloned();
        if is_picture && !(full_failed && real_thumb.is_none()) {
            self.paint_picture(r, theme, view, facts.as_ref().and_then(|f| f.dims), real_thumb.or(thumb));
            return;
        }
        let text = facts.as_ref().and_then(|f| f.text.clone()).filter(|_| by_name || real_thumb.is_none());
        if let Some(text) = text {
            self.paint_text(r, theme, view, &text);
            return;
        }
        if waiting_for_text {
            return; // its lines are on their way
        }
        // Anything else: the big thumbnail, and the details under it.
        let pad = 18.0;
        let img = Rect::new(view.x + pad, view.y + pad, (view.w - 2.0 * pad).max(0.0), (view.h * 0.55).max(80.0).min(view.h - 2.0 * pad).max(0.0));
        if let Some(t) = &thumb {
            details::draw_fitted(r, t, img, s, 256.0);
        }
        let info = details::info(&Sel::One(Box::new(one.clone())), facts.as_ref(), app);
        let area = Rect::new(view.x + pad, img.bottom() + 16.0, (view.w - 2.0 * pad).max(0.0), (view.bottom() - img.bottom() - 16.0 - pad).max(0.0));
        details::draw_info(r, theme, area, &info);
    }

    /// A picture on the checkerboard, zoomed and panned.
    fn paint_picture(&mut self, r: &mut Renderer, theme: &Theme, view: Rect, dims: Option<(u32, u32)>, thumb: Option<Arc<Picture>>) {
        let s = self.scale;
        let (a, b) = if theme.dark { (Color::hex(0x1c1c1f), Color::hex(0x242428)) } else { (Color::hex(0xf4f4f6), Color::hex(0xe4e4e8)) };
        r.fill_checker(view, 8.0, a, b);
        let full = self.full.as_ref().filter(|f| f.key == self.view_key).and_then(|f| f.picture.clone().map(|p| (p, f.original)));
        // The picture's own size: from the full decode, the shell, or (for now) the thumbnail.
        let natural = match (&full, dims, &thumb) {
            (Some((_, (w, h))), _, _) if *w > 0 => (*w as f32, *h as f32),
            (_, Some((w, h)), _) => (w as f32, h as f32),
            (_, _, Some(t)) if !t.is_icon() => (t.width() as f32, t.height() as f32),
            _ => {
                if let Some(t) = &thumb {
                    details::draw_fitted(r, t, view.inset(24.0, 24.0), s, 256.0);
                }
                return;
            }
        };
        let z = zoom_value(self.zoom, natural, (view.w * s, view.h * s));
        let draw = (natural.0 * z / s, natural.1 * z / s);
        self.pan = clamp_pan(self.pan, draw, (view.w, view.h));
        let c = Point::new(view.x + view.w / 2.0 + self.pan.x, view.y + view.h / 2.0 + self.pan.y);
        let dest = Rect::new(c.x - draw.0 / 2.0, c.y - draw.1 / 2.0, draw.0, draw.1);
        r.push_clip(view);
        let mut drawn = false;
        if let Some((pic, _)) = &full {
            // Zoomed in a lot, each pixel stays a sharp square.
            let per_pixel = draw.0 * s / pic.width().max(1) as f32;
            drawn = r.picture(pic, dest, None, 1.0, per_pixel < 2.0);
        }
        if !drawn {
            if let Some(t) = thumb.filter(|t| !t.is_icon()) {
                r.picture(&t, dest, None, 1.0, true);
            }
        }
        r.pop_clip();
        if let Some(sh) = self.shown.as_mut() {
            sh.picture = Some((natural, z));
        }
    }

    /// The first 200 KB of a text file, with line numbers.
    fn paint_text(&mut self, r: &mut Renderer, theme: &Theme, view: Rect, text: &peek::Text) {
        let inner = view.inset(12.0, 10.0);
        let fits = (inner.h / MONO_LINE).floor().max(1.0) as usize;
        let extra = usize::from(text.cut);
        let total = text.lines.len() + extra;
        if let Some(sh) = self.shown.as_mut() {
            sh.text = Some((total, fits));
        }
        if text.lines.is_empty() && !text.cut {
            r.text("Empty file", view, &TextStyle::new(theme.muted).align(Align::Center));
            return;
        }
        let max_scroll = total.saturating_sub(fits) as f32;
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        let digits = text.lines.len().max(1).to_string().len();
        let cw = r.mono_char_width(MONO_FONT);
        let gutter = digits as f32 * cw + 14.0;
        let first = self.scroll.floor() as usize;
        let mut y = inner.y - self.scroll.fract() * MONO_LINE;
        let color = details::blend(theme.text, theme.panel, 0.08);
        r.push_clip(view);
        r.fill_rect(Rect::new(view.x, view.y, inner.x - view.x + gutter - 6.0, view.h), theme.window.with_alpha(0.35));
        for i in first..total {
            if y > inner.bottom() {
                break;
            }
            let line_rect = Rect::new(inner.x + gutter, y, (inner.w - gutter - 8.0).max(0.0), MONO_LINE);
            match text.lines.get(i) {
                Some(line) => {
                    let number = format!("{:>digits$}", i + 1);
                    r.mono_text(&number, Rect::new(inner.x, y, gutter, MONO_LINE), MONO_FONT, theme.muted);
                    if !line.is_empty() {
                        r.mono_text(line, line_rect, MONO_FONT, color);
                    }
                }
                None => r.text("Only the first 200 KB is shown", line_rect, &TextStyle::new(theme.muted).size(11.5)),
            }
            y += MONO_LINE;
        }
        // A thin scrollbar when it's longer than the pane.
        if total > fits {
            let track = Rect::new(view.right() - 6.0 - 3.0, view.y + 4.0, 6.0, view.h - 8.0);
            let h = (track.h * fits as f32 / total as f32).max(24.0).min(track.h);
            let top = track.y + (track.h - h) * (self.scroll / max_scroll.max(1.0));
            r.fill_paint(Rect::new(track.x, top, track.w, h), 3.0, &theme.scroll_thumb_paint);
        }
        r.pop_clip();
    }

    fn paint_bar(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, one: Option<&One>) {
        let bar = self.bar();
        // The bar takes the look of the file list's column titles.
        r.fill_paint(Rect::new(self.rect.x, self.rect.y, self.rect.w, TOP_H), 0.0, &theme.list_header);
        r.fill_rect(Rect::new(self.rect.x, self.rect.y + TOP_H, self.rect.w, 1.0), theme.line);
        let picture = self.shown.and_then(|s| s.picture);
        let button_bg = |r: &mut Renderer, rect: Rect, hit: Hit, on: bool, enabled: bool| {
            if on {
                fill_box_paint(r, theme, rect, 0.0, &theme.accent_tint_paint);
                r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 2.0), theme.accent);
            } else if enabled && self.hover == hit {
                fill_box(r, theme, rect, 0.0, if self.pressed == hit { theme.button_pressed } else { theme.button_hover });
            }
        };
        // ×
        button_bg(r, bar.close, Hit::Close, false, true);
        let c = Point::new(bar.close.x + bar.close.w / 2.0, bar.close.y + bar.close.h / 2.0);
        let glyph = if self.hover == Hit::Close { theme.text_strong } else { theme.text };
        r.line(Point::new(c.x - 5.0, c.y - 5.0), Point::new(c.x + 5.0, c.y + 5.0), 1.3, glyph);
        r.line(Point::new(c.x + 5.0, c.y - 5.0), Point::new(c.x - 5.0, c.y + 5.0), 1.3, glyph);
        // Fit, 1:1, fill (only for pictures).
        let zoom_color = if picture.is_some() { theme.text } else { theme.disabled };
        for (rect, hit, mode) in [(bar.fit, Hit::Fit, Zoom::Fit), (bar.actual, Hit::Actual, Zoom::Actual), (bar.fill, Hit::Fill, Zoom::Fill)] {
            button_bg(r, rect, hit, picture.is_some() && self.zoom == mode, picture.is_some());
            let c = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            match mode {
                Zoom::Fit => {
                    // Two arrows pointing in.
                    r.polyline(&[Point::new(c.x + 7.0, c.y - 7.0), Point::new(c.x + 1.5, c.y - 1.5)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x + 1.5, c.y - 5.5), Point::new(c.x + 1.5, c.y - 1.5), Point::new(c.x + 5.5, c.y - 1.5)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 7.0, c.y + 7.0), Point::new(c.x - 1.5, c.y + 1.5)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 1.5, c.y + 5.5), Point::new(c.x - 1.5, c.y + 1.5), Point::new(c.x - 5.5, c.y + 1.5)], 1.3, zoom_color);
                }
                Zoom::Fill => {
                    // Two arrows pointing out.
                    r.polyline(&[Point::new(c.x + 1.5, c.y - 1.5), Point::new(c.x + 7.0, c.y - 7.0)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x + 3.0, c.y - 7.0), Point::new(c.x + 7.0, c.y - 7.0), Point::new(c.x + 7.0, c.y - 3.0)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 1.5, c.y + 1.5), Point::new(c.x - 7.0, c.y + 7.0)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 3.0, c.y + 7.0), Point::new(c.x - 7.0, c.y + 7.0), Point::new(c.x - 7.0, c.y + 3.0)], 1.3, zoom_color);
                }
                _ => r.text("1:1", rect, &TextStyle::new(zoom_color).size(11.0).semibold().align(Align::Center)),
            }
        }
        if let Some((_, z)) = picture {
            r.text(&zoom_text(z), bar.zoom, &TextStyle::new(theme.muted).align(Align::Center));
        }
        // The name, then ‹ ›.
        if let Some(o) = one {
            r.text(&o.entry.name, bar.name, &TextStyle::new(theme.text).align(Align::Right));
        }
        let tab = app.tab();
        let row = one.and_then(|o| tab.row_of(o.index));
        let can_prev = row.is_some_and(|r| r > 0);
        let can_next = row.is_some_and(|r| r + 1 < tab.view.len());
        for (rect, hit, enabled, right) in [(bar.prev, Hit::Prev, can_prev, false), (bar.next, Hit::Next, can_next, true)] {
            button_bg(r, rect, hit, false, enabled);
            let c = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            let color = if enabled { theme.text } else { theme.disabled };
            if right {
                chevron_right(r, c, 5.0, color);
            } else {
                r.polyline(&[Point::new(c.x + 2.75, c.y - 5.0), Point::new(c.x - 2.75, c.y), Point::new(c.x + 2.75, c.y + 5.0)], 1.2, color);
            }
        }
    }

    // ------------------------------------------------------------ zooming, panning, stepping

    /// Zooms by `factor` around `at` (a point in the window), or the view's centre.
    fn zoom_by(&mut self, factor: f32, at: Option<Point>) -> Effect {
        let Some(sh) = self.shown else { return Effect::None };
        let Some((natural, old)) = sh.picture else { return Effect::None };
        let new = (old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let centre = Point::new(sh.view.x + sh.view.w / 2.0, sh.view.y + sh.view.h / 2.0);
        let at = at.map_or(Point::default(), |p| Point::new(p.x - centre.x, p.y - centre.y));
        let s = self.scale;
        let pan = pan_for_zoom(self.pan, at, old, new);
        self.pan = clamp_pan(pan, (natural.0 * new / s, natural.1 * new / s), (sh.view.w, sh.view.h));
        self.zoom = Zoom::Custom(new);
        Effect::Redraw
    }

    fn set_zoom(&mut self, z: Zoom) -> Effect {
        self.zoom = z;
        self.pan = Point::default();
        Effect::Redraw
    }

    /// Selects the item `delta` rows away from the one shown (‹ ›, Left and Right).
    pub fn step(&mut self, app: &mut AppState, delta: isize) -> Effect {
        let one = details::focused(app);
        let tab = app.tab_mut();
        let n = tab.view.len();
        let Some(row) = one.and_then(|o| tab.row_of(o.index)).or_else(|| tab.focus_row()) else { return Effect::None };
        if n == 0 {
            return Effect::None;
        }
        let to = (row as isize + delta).clamp(0, n as isize - 1) as usize;
        if to == row {
            return Effect::None;
        }
        tab.select_only_row(to);
        tab.reveal_focus = true;
        Effect::Redraw
    }

    /// Keys while the pane is open (the window asks before the file list): Esc closes, Left and
    /// Right step through the files.
    pub fn key_down(&mut self, vk: u16, mods: crate::ui::Mods, app: &mut AppState) -> Effect {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RIGHT};
        if mods.ctrl || mods.alt || mods.shift {
            return Effect::None;
        }
        match windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk) {
            VK_ESCAPE => Effect::Command(Command::ClosePreview),
            VK_LEFT => self.step(app, -1),
            VK_RIGHT => self.step(app, 1),
            _ => Effect::None,
        }
    }

    /// Typed characters: + and - zoom a picture. None of the others.
    pub fn char_input(&mut self, ch: char) -> Effect {
        match ch {
            '+' | '=' => self.zoom_by(ZOOM_STEP, None),
            '-' | '_' => self.zoom_by(1.0 / ZOOM_STEP, None),
            _ => Effect::None,
        }
    }

    // ------------------------------------------------------------ the mouse

    pub fn cursor(&self, p: Point) -> CursorKind {
        if self.hit(p) == Hit::View && self.can_pan() {
            CursorKind::Hand
        } else {
            CursorKind::Arrow
        }
    }

    /// Whether the picture is bigger than the view (so dragging moves it).
    fn can_pan(&self) -> bool {
        let Some(sh) = self.shown else { return false };
        let Some((natural, z)) = sh.picture else { return false };
        natural.0 * z / self.scale > sh.view.w + 0.5 || natural.1 * z / self.scale > sh.view.h + 0.5
    }

    pub fn mouse_move(&mut self, e: &MouseEvent) -> Effect {
        if let Some((start, pan0)) = self.panning {
            let Some(sh) = self.shown else { return Effect::None };
            let Some((natural, z)) = sh.picture else { return Effect::None };
            let pan = Point::new(pan0.x + e.pos.x - start.x, pan0.y + e.pos.y - start.y);
            self.pan = clamp_pan(pan, (natural.0 * z / self.scale, natural.1 * z / self.scale), (sh.view.w, sh.view.h));
            return Effect::Redraw;
        }
        let h = self.hit(e.pos);
        if h != self.hover {
            self.hover = h;
            return Effect::Redraw;
        }
        Effect::None
    }

    pub fn mouse_leave(&mut self) -> Effect {
        if self.hover != Hit::None && self.panning.is_none() {
            self.hover = Hit::None;
            return Effect::Redraw;
        }
        Effect::None
    }

    pub fn mouse_down(&mut self, e: &MouseEvent) -> Effect {
        if e.button != MouseButton::Left {
            return Effect::None;
        }
        let h = self.hit(e.pos);
        self.hover = h;
        if h == Hit::View {
            if self.can_pan() {
                self.panning = Some((e.pos, self.pan));
            }
            return Effect::None;
        }
        self.pressed = h;
        Effect::Redraw
    }

    pub fn mouse_up(&mut self, e: &MouseEvent, app: &mut AppState) -> Effect {
        if e.button != MouseButton::Left {
            return Effect::None;
        }
        if self.panning.take().is_some() {
            return Effect::Redraw;
        }
        let was = std::mem::replace(&mut self.pressed, Hit::None);
        if was == Hit::None || self.hit(e.pos) != was {
            return Effect::Redraw;
        }
        match was {
            Hit::Close => Effect::Command(Command::ClosePreview),
            Hit::Fit => self.set_zoom(Zoom::Fit),
            Hit::Actual => self.set_zoom(Zoom::Actual),
            Hit::Fill => self.set_zoom(Zoom::Fill),
            Hit::Prev => self.step(app, -1),
            Hit::Next => self.step(app, 1),
            _ => Effect::Redraw,
        }
    }

    /// A double-click on the picture (or text, or thumbnail) opens the file.
    pub fn double_click(&mut self, e: &MouseEvent) -> Effect {
        if e.button == MouseButton::Left && self.hit(e.pos) == Hit::View && self.shown.is_some() && !self.view_key.is_empty() {
            self.panning = None;
            return Effect::Command(Command::OpenSelected);
        }
        Effect::None
    }

    /// Ctrl+wheel zooms a picture around the mouse; the plain wheel scrolls text, and moves a
    /// picture that is bigger than the pane (Shift: sideways).
    pub fn wheel(&mut self, e: &MouseEvent) -> Effect {
        let Some(sh) = self.shown else { return Effect::None };
        if sh.picture.is_some() {
            if e.mods.ctrl {
                return self.zoom_by(ZOOM_STEP.powf(e.wheel), Some(e.pos));
            }
            if !self.can_pan() {
                return Effect::None;
            }
            let (natural, z) = sh.picture.unwrap_or_default();
            let step = e.wheel * 80.0;
            let pan = if e.mods.shift { Point::new(self.pan.x + step, self.pan.y) } else { Point::new(self.pan.x, self.pan.y + step) };
            self.pan = clamp_pan(pan, (natural.0 * z / self.scale, natural.1 * z / self.scale), (sh.view.w, sh.view.h));
            return Effect::Redraw;
        }
        if let Some((total, fits)) = sh.text {
            let max = total.saturating_sub(fits) as f32;
            let to = (self.scroll - e.wheel * WHEEL_LINES).clamp(0.0, max);
            if to != self.scroll {
                self.scroll = to;
                return Effect::Redraw;
            }
        }
        Effect::None
    }
}

/// How the zoom reads in the top bar ("93%").
pub fn zoom_text(z: f32) -> String {
    format!("{}%", folder::format_count((z * 100.0).round().max(0.0) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_blows_up_and_fill_fills() {
        // A 4000 x 3000 photo in an 800 x 800 pane.
        assert_eq!(zoom_value(Zoom::Fit, (4000.0, 3000.0), (800.0, 800.0)), 0.2);
        assert!((zoom_value(Zoom::Fill, (4000.0, 3000.0), (800.0, 800.0)) - 800.0 / 3000.0).abs() < 1e-6);
        assert_eq!(zoom_value(Zoom::Actual, (4000.0, 3000.0), (800.0, 800.0)), 1.0);
        // A small icon-sized picture stays its own size with fit.
        assert_eq!(zoom_value(Zoom::Fit, (64.0, 48.0), (800.0, 800.0)), 1.0);
        assert_eq!(zoom_value(Zoom::Custom(2.5), (64.0, 48.0), (800.0, 800.0)), 2.5);
    }

    #[test]
    fn panning_stays_inside_the_picture() {
        // Smaller than the view: centred.
        assert_eq!(clamp_pan(Point::new(50.0, -20.0), (100.0, 100.0), (400.0, 300.0)), Point::new(0.0, 0.0));
        // Bigger: at most half the difference either way.
        assert_eq!(clamp_pan(Point::new(500.0, -500.0), (800.0, 600.0), (400.0, 300.0)), Point::new(200.0, -150.0));
        assert_eq!(clamp_pan(Point::new(10.0, 5.0), (800.0, 600.0), (400.0, 300.0)), Point::new(10.0, 5.0));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_mouse() {
        // The picture's centre is 40 DIPs left of the view's; the mouse 100 right of the view's
        // centre is 140 right of the picture's centre. Twice the zoom: 280 right, so the picture's
        // centre moves to 100 - 280 = -180.
        let pan = pan_for_zoom(Point::new(-40.0, 0.0), Point::new(100.0, 0.0), 1.0, 2.0);
        assert_eq!(pan, Point::new(-180.0, 0.0));
        // Around the centre with nothing panned, nothing moves.
        assert_eq!(pan_for_zoom(Point::default(), Point::default(), 1.0, 3.0), Point::default());
    }

    #[test]
    fn zoom_reads_as_a_percentage() {
        assert_eq!(zoom_text(0.93), "93%");
        assert_eq!(zoom_text(1.0), "100%");
    }
}
