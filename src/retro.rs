//! The parts that older Windows drew its own way, for a skin with an `era` (theme.rs `Era`):
//! Windows 98 and 2000's 3D edges, the window titles and the minimise / maximise / close buttons
//! of 98 and 2000, XP and 7. Skins without an era never come here.
//!
//! - [`edge`]: one of Windows 2000's 3D edges (raised, sunken, etched...) in the skin's four edge
//!   colours (`bevel-highlight`, `bevel-light`, `bevel-shadow`, `bevel-dark`).
//! - [`dither`]: the 50 % checker of the edge's light colour and the face (a pressed-in toolbar
//!   button, the scroll bar's track).
//! - [`title`]: a window's title on its title bar: white bold on 98 / 2000 / XP's coloured bars
//!   (with XP's shadow), black with a white glow on 7's glass.
//! - [`caption_slots`], [`caption_button`] and [`caption_group`]: the window buttons, as big as each
//!   Windows had them and where it put them.
//!
//! Sizes are DIPs; lines are one DIP, on whole pixels.

use crate::render::{Renderer, TextStyle};
use crate::theme::{Color, Era, Paint, Stop, Theme, MAX_STOPS};
use crate::ui::{Point, Rect};

// ---------------------------------------------------------------- 3D edges

/// Windows 2000's edges (its DrawEdge), outside in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// Push buttons, caption buttons, column titles: light, face / shadow, dark.
    Raised,
    /// Window frames, menus, scroll bar arrows and thumbs: face, light / shadow, dark.
    RaisedWindow,
    /// Text boxes, lists, check boxes: shadow, dark / face, light.
    Sunken,
    /// A push button held down: dark, shadow / face, light.
    Pressed,
    /// A toolbar button under the mouse: one light line top and left, one shadow line under.
    ThinRaised,
    /// A toolbar button held down or switched on, the status bar's panes.
    ThinSunken,
    /// Group boxes and separators: a shadow line with a light one beside it.
    Etched,
    /// One shadow line all round.
    Frame,
}

/// The four colours of a 3D edge: (highlight, light, shadow, dark). The skin's own, or worked out
/// from `face` (white, the face a little lighter, darker, much darker).
pub fn bevel_colours(theme: &Theme, face: Color) -> (Color, Color, Color, Color) {
    if theme.bevel_highlight.a > 0.0 {
        let light = if theme.bevel_light.a > 0.0 { theme.bevel_light } else { face };
        let shadow = if theme.bevel_shadow.a > 0.0 { theme.bevel_shadow } else { face.darker(0.4) };
        let dark = if theme.bevel_dark.a > 0.0 { theme.bevel_dark } else { face.darker(0.75) };
        (theme.bevel_highlight, light, shadow, dark)
    } else {
        (Color::hex(0xffffff), face.lighter(0.35), face.darker(0.4), face.darker(0.75))
    }
}

/// One ring of an edge: `tl` along the top and left, `br` along the bottom and right (which owns
/// the top-right and bottom-left corners, as in Windows).
fn ring(r: &mut Renderer, rect: Rect, tl: Color, br: Color) {
    if rect.w < 2.0 || rect.h < 2.0 {
        return;
    }
    r.fill_rect(Rect::new(rect.x, rect.y, rect.w - 1.0, 1.0), tl);
    r.fill_rect(Rect::new(rect.x, rect.y, 1.0, rect.h - 1.0), tl);
    r.fill_rect(Rect::new(rect.x, rect.bottom() - 1.0, rect.w, 1.0), br);
    r.fill_rect(Rect::new(rect.right() - 1.0, rect.y, 1.0, rect.h), br);
}

/// Draws `kind` just inside `rect` (on whole pixels), in the colours for a face of `face`.
pub fn edge(r: &mut Renderer, theme: &Theme, rect: Rect, kind: Edge, face: Color) {
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    let (hl, lt, sh, dk) = bevel_colours(theme, face);
    let inner = rect.inset(1.0, 1.0);
    match kind {
        Edge::Raised => {
            ring(r, rect, hl, dk);
            ring(r, inner, lt, sh);
        }
        Edge::RaisedWindow => {
            ring(r, rect, lt, dk);
            ring(r, inner, hl, sh);
        }
        Edge::Sunken => {
            ring(r, rect, sh, hl);
            ring(r, inner, dk, lt);
        }
        Edge::Pressed => {
            ring(r, rect, dk, hl);
            ring(r, inner, sh, lt);
        }
        Edge::ThinRaised => ring(r, rect, hl, sh),
        Edge::ThinSunken => ring(r, rect, sh, hl),
        Edge::Etched => {
            r.stroke_rect(Rect::new(rect.x, rect.y, rect.w - 1.0, rect.h - 1.0), 1.0, sh);
            r.stroke_rect(Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w - 1.0, rect.h - 1.0), 1.0, hl);
        }
        Edge::Frame => r.stroke_rect(rect, 1.0, sh),
    }
}

/// An etched line across (`w` long at `x`, `y`): shadow, then light under it.
pub fn etched_line(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32, face: Color) {
    let (hl, _, sh, _) = bevel_colours(theme, face);
    let (x, y) = (r.snap(x), r.snap(y));
    r.fill_rect(Rect::new(x, y, w, 1.0), sh);
    r.fill_rect(Rect::new(x, y + 1.0, w, 1.0), hl);
}

/// An etched line down (`h` long at `x`, `y`): shadow, then light to its right.
pub fn etched_upright(r: &mut Renderer, theme: &Theme, x: f32, y: f32, h: f32, face: Color) {
    let (hl, _, sh, _) = bevel_colours(theme, face);
    let (x, y) = (r.snap(x), r.snap(y));
    r.fill_rect(Rect::new(x, y, 1.0, h), sh);
    r.fill_rect(Rect::new(x + 1.0, y, 1.0, h), hl);
}

/// The 50 % checker of the edge's light colour and `face` over `rect`.
pub fn dither(r: &mut Renderer, theme: &Theme, rect: Rect, face: Color) {
    let (hl, ..) = bevel_colours(theme, face);
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    r.fill_checker(rect, 1.0 / r.scale().max(1.0), hl, face);
}

// ---------------------------------------------------------------- window titles

/// A window's title on its title bar `rect` (left aligned, centred down): in the skin's title
/// colours and font. 98 and 2000: bold; XP: Trebuchet bold with its shadow; 7: regular black with
/// a white glow behind it. Skins of today: semibold, as Glint always drew it.
pub fn title(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, active: bool) {
    let color = if active { theme.title_text } else { theme.title_text_inactive };
    let style = match theme.era {
        Era::None => TextStyle::new(color).size(13.0).semibold(),
        Era::Win98 | Era::Win2000 => TextStyle::new(color).size(12.0).bold().title_font(),
        Era::Xp => TextStyle::new(color).size(13.0).bold().title_font(),
        Era::Win7 => TextStyle::new(color).size(12.0).title_font(),
    };
    if theme.era == Era::Win7 {
        // The glow: a soft white cloud behind the words, then a white rim round each letter.
        let w = r.measure(text, &style).min(rect.w);
        let mid = rect.y + rect.h / 2.0;
        for (grow, alpha) in [(9.0, 0.16), (6.0, 0.22), (3.0, 0.3)] {
            let cloud = Rect::new(rect.x - grow, mid - 8.0 - grow * 0.4, w + 2.0 * grow, 16.0 + grow * 0.8);
            r.fill_round_rect(cloud, 8.0, Color::rgba(255, 255, 255, alpha));
        }
        let rim = style.color(Color::rgba(255, 255, 255, 0.55));
        for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
            r.text(text, Rect::new(rect.x + dx, rect.y + dy, rect.w, rect.h), &rim);
        }
    }
    let shadow = if active { theme.title_shadow } else { theme.title_shadow.with_alpha(theme.title_shadow.a * 0.5) };
    if shadow.a > 0.0 {
        r.text(text, Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w, rect.h), &style.color(shadow));
    }
    r.text(text, rect, &style);
}

/// A window's own title bar `bar` in the skin's paint (the inactive one while the window isn't
/// the active one), with Windows 7's glass streaks.
pub fn title_band(r: &mut Renderer, theme: &Theme, bar: Rect, active: bool) {
    let paint = if active { theme.titlebar_paint } else { theme.titlebar_inactive_paint };
    if theme.era == Era::Win7 && !theme.title_across {
        ask_glass(r, theme, bar);
        fill_glass(r, theme, bar, bar, &paint);
    } else if theme.title_across {
        r.fill_paint_across(bar, &paint);
    } else {
        r.fill_paint(bar, 0.0, &paint);
    }
    if theme.era == Era::Win7 {
        glass_streaks(r, bar, if active { 1.0 } else { 0.5 });
    }
}

/// Windows 7's glass over `area` (the main window's title bar and toolbar together, so the two
/// run on with no seam), drawn only inside `clip`: the skin's title bar paint stretched over the
/// whole area, a soft white glow along the top, the streaks. The part at the top of the area (the
/// title bar's) can be see-through (`see-through`); the toolbar's stays solid, as Windows draws
/// its text boxes there and those can't be.
pub fn glass(r: &mut Renderer, theme: &Theme, area: Rect, clip: Rect, active: bool) {
    let paint = if active { theme.titlebar_paint } else { theme.titlebar_inactive_paint };
    if clip.y <= area.y {
        ask_glass(r, theme, clip);
    }
    r.push_clip(clip);
    fill_glass(r, theme, clip, area, &paint);
    let glow = Rect::new(area.x, area.y, area.w, (area.h / 3.0).min(28.0));
    r.fill_paint_down(glow, 0.0, &Paint::even(&[Color::rgba(255, 255, 255, 0.25), Color::rgba(255, 255, 255, 0.0)]), glow.y, glow.bottom());
    glass_streaks(r, area, if active { 1.0 } else { 0.5 });
    r.pop_clip();
}

/// Asks for the window to be see-through down to the bottom of `bar` (the title bar at its top),
/// when the skin has `see-through`.
fn ask_glass(r: &mut Renderer, theme: &Theme, bar: Rect) {
    if theme.see_through > 0.0 && bar.y <= 0.0 {
        r.ask_glass(bar.bottom());
    }
}

/// Fills `rect` (part of the glass `area`) with the glass `paint`, its gradient running down the
/// whole area. Where the window is see-through now (`Renderer::glass_bottom`), what is behind it
/// shows through as much as the skin's `see-through` says, firming up to solid by the bottom of
/// the see-through part, so the glass runs on into what is below it with no seam.
fn fill_glass(r: &mut Renderer, theme: &Theme, rect: Rect, area: Rect, paint: &Paint) {
    let bottom = r.glass_bottom().min(rect.bottom());
    if theme.see_through <= 0.0 || bottom <= rect.y {
        r.fill_paint_down(rect, 0.0, paint, area.y, area.bottom());
        return;
    }
    let see = Rect::new(rect.x, rect.y, rect.w, bottom - rect.y);
    r.clear_glass(see);
    let mut stops = [Stop { at: 0.0, color: Color::rgba(0, 0, 0, 0.0) }; MAX_STOPS];
    for (i, stop) in stops.iter_mut().enumerate() {
        let at = i as f32 / (MAX_STOPS - 1) as f32;
        let color = paint.color_at(((see.y + at * see.h - area.y) / area.h.max(1.0)).clamp(0.0, 1.0));
        // As see-through as the skin says over the top half, then smoothly solid.
        let firm = ((at - 0.5) / 0.5).clamp(0.0, 1.0);
        let firm = firm * firm * (3.0 - 2.0 * firm);
        *stop = Stop { at, color: color.with_alpha(color.a * (1.0 - theme.see_through * (1.0 - firm))) };
    }
    r.fill_paint_down(see, 0.0, &Paint::from_stops(&stops), see.y, see.bottom());
    if let Some(buttons) = r.glass_solid() {
        // Windows keeps its own window buttons under the glass at the top right (Glint's are
        // drawn over them): solid from their left edge on, fading in just before it.
        let x = buttons.x.clamp(see.x, see.right());
        let (fade, steps) = (24.0, 12);
        for i in 0..steps {
            let w = fade / steps as f32;
            let left = x - fade + i as f32 * w;
            if left + w > see.x {
                let slice = Rect::new(left.max(see.x), see.y, left + w - left.max(see.x), see.h);
                r.fill_paint_down(slice, 0.0, &faded(paint, (i + 1) as f32 / (steps + 1) as f32), area.y, area.bottom());
            }
        }
        r.fill_paint_down(Rect::new(x, see.y, see.right() - x, see.h), 0.0, paint, area.y, area.bottom());
    }
    if bottom < rect.bottom() {
        r.fill_paint_down(Rect::new(rect.x, bottom, rect.w, rect.bottom() - bottom), 0.0, paint, area.y, area.bottom());
    }
}

/// `paint` with every colour `k` times as solid.
fn faded(paint: &Paint, k: f32) -> Paint {
    match paint {
        Paint::Flat(c) => Paint::Flat(c.with_alpha(c.a * k)),
        Paint::Gradient(g) => {
            let mut stops = [Stop { at: 0.0, color: Color::rgba(0, 0, 0, 0.0) }; MAX_STOPS];
            let n = g.stops().len().min(MAX_STOPS);
            for (out, s) in stops.iter_mut().zip(g.stops()) {
                *out = Stop { at: s.at, color: s.color.with_alpha(s.color.a * k) };
            }
            Paint::from_stops(&stops[..n])
        }
    }
}

/// Windows 7's glass: a few soft white bands leaning right, unevenly spaced, across `area`.
pub fn glass_streaks(r: &mut Renderer, area: Rect, strength: f32) {
    r.push_clip(area);
    let lean = area.h * 0.58;
    let mut x = area.x + 120.0;
    let mut k = 0;
    while x < area.right() + lean {
        let w = [46.0, 30.0, 58.0, 38.0][k % 4];
        let a = [0.16, 0.11, 0.13, 0.09][k % 4] * strength;
        let band = [Point::new(x, area.bottom()), Point::new(x + lean, area.y), Point::new(x + lean + w, area.y), Point::new(x + w, area.bottom())];
        let paint = Paint::from_stops(&[
            Stop { at: 0.0, color: Color::rgba(255, 255, 255, 0.0) },
            Stop { at: 0.5, color: Color::rgba(255, 255, 255, a) },
            Stop { at: 1.0, color: Color::rgba(255, 255, 255, 0.0) },
        ]);
        r.fill_polygon_toward(&band, &paint, Point::new(x, area.bottom()), Point::new(x + w, area.bottom()));
        x += [210.0, 250.0, 185.0, 260.0][k % 4];
        k += 1;
    }
    r.pop_clip();
}

// ---------------------------------------------------------------- window buttons

/// A window button's sign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    Close,
    Maximize,
    Restore,
    Minimize,
}

/// How a window button is right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub hot: bool,
    pub down: bool,
    /// Its window is the active one.
    pub active: bool,
}

/// Where close, maximise and minimise sit (in that order) at the right of a title bar `bar`, as
/// big as the era had them: 2000's raised boxes 25 x 22 (its 16 x 14 grown to the taller bar),
/// XP's tiles 21 x 21, 7's joined group hanging from the top (26, 27 and 47 wide, 20 high).
pub fn caption_slots(era: Era, bar: Rect) -> [Rect; 3] {
    match era {
        Era::Win98 | Era::Win2000 => {
            let y = bar.y + ((bar.h - 22.0) / 2.0).floor();
            let close = Rect::new(bar.right() - 4.0 - 25.0, y, 25.0, 22.0);
            let max = Rect::new(close.x - 3.0 - 25.0, y, 25.0, 22.0);
            let min = Rect::new(max.x - 25.0, y, 25.0, 22.0);
            [close, max, min]
        }
        Era::Xp => {
            let y = bar.y + ((bar.h - 21.0) / 2.0).floor();
            let close = Rect::new(bar.right() - 6.0 - 21.0, y, 21.0, 21.0);
            let max = Rect::new(close.x - 2.0 - 21.0, y, 21.0, 21.0);
            let min = Rect::new(max.x - 2.0 - 21.0, y, 21.0, 21.0);
            [close, max, min]
        }
        Era::Win7 | Era::None => {
            let close = Rect::new(bar.right() - 6.0 - 47.0, bar.y, 47.0, 20.0);
            let max = Rect::new(close.x - 27.0, bar.y, 27.0, 20.0);
            let min = Rect::new(max.x - 26.0, bar.y, 26.0, 20.0);
            [close, max, min]
        }
    }
}

/// Where a window's lone close button sits in `b` (its spot at the right of the title bar).
pub fn close_slot(era: Era, b: Rect) -> Rect {
    match era {
        Era::Win7 => Rect::new(b.right() - 3.0 - 43.0, b.y, 43.0, 20.0),
        _ => caption_slots(era, b)[0],
    }
}

/// One window button in `face` (a slot from `caption_slots`). In 7 a lone button (a window with
/// only close) has both bottom corners round.
pub fn caption_button(r: &mut Renderer, theme: &Theme, face: Rect, sign: Sign, s: State) {
    match theme.era {
        Era::Win98 | Era::Win2000 => classic_button(r, theme, face, sign, s),
        Era::Xp => xp_button(r, theme, face, sign, s),
        Era::Win7 => {
            seven_face(r, theme, face, sign, s, (true, true));
            r.stroke_round_rect(face.inset(0.5, 0.5), 3.5, 1.0, Color::hex(0x585a5c));
            r.fill_rect(Rect::new(face.x, face.y, face.w, 1.0), Color::hex(0x585a5c));
            seven_sign(r, face, sign);
        }
        Era::None => {}
    }
}

/// Close, maximise and minimise together (the slots from `caption_slots`): in 7 one joined group
/// with round bottom corners at its two ends and lines between the buttons.
pub fn caption_group(r: &mut Renderer, theme: &Theme, slots: [Rect; 3], signs: [Sign; 3], states: [State; 3]) {
    if theme.era != Era::Win7 {
        for i in 0..3 {
            caption_button(r, theme, slots[i], signs[i], states[i]);
        }
        return;
    }
    let [close, _, min] = slots;
    let group = Rect::new(min.x, min.y, close.right() - min.x, close.h);
    for i in 0..3 {
        let ends = (i == 2, i == 0);
        seven_face(r, theme, slots[i], signs[i], states[i], ends);
    }
    let line = Color::hex(0x585a5c);
    r.stroke_round_rect(group.inset(0.5, 0.5), 3.5, 1.0, line);
    // Square top corners: the group hangs from the window's top edge.
    r.fill_rect(Rect::new(group.x, group.y, group.w, 1.0), line);
    r.fill_rect(Rect::new(group.x, group.y, 1.0, 4.0), line);
    r.fill_rect(Rect::new(group.right() - 1.0, group.y, 1.0, 4.0), line);
    for s in [slots[1], slots[2]] {
        r.fill_rect(Rect::new(r.snap(s.right()) - 1.0, s.y, 1.0, s.h), line);
    }
    for i in 0..3 {
        seven_sign(r, slots[i], signs[i]);
    }
}

/// 98 / 2000: a raised grey box (sunk while pressed) with a black sign, made of whole pixels.
fn classic_button(r: &mut Renderer, theme: &Theme, face: Rect, sign: Sign, s: State) {
    let face = Rect::new(r.snap(face.x), r.snap(face.y), face.w, face.h);
    let fill = theme.chip_paint.base();
    r.fill_rect(face, fill);
    let pressed = s.down && s.hot;
    edge(r, theme, face, if pressed { Edge::Pressed } else { Edge::Raised }, fill);
    let ink = theme.button_text;
    let (x, y) = if pressed { (face.x + 1.0, face.y + 1.0) } else { (face.x, face.y) };
    let px = |r: &mut Renderer, dx: f32, dy: f32, w: f32, h: f32| r.fill_rect(Rect::new(x + dx, y + dy, w, h), ink);
    match sign {
        Sign::Minimize => px(r, 6.0, 14.0, 9.0, 3.0),
        Sign::Maximize => {
            px(r, 5.0, 3.0, 14.0, 3.0);
            px(r, 5.0, 6.0, 2.0, 11.0);
            px(r, 17.0, 6.0, 2.0, 11.0);
            px(r, 5.0, 15.0, 14.0, 2.0);
        }
        Sign::Restore => {
            // The window behind (its top bar and right side), then the one in front.
            px(r, 8.0, 3.0, 10.0, 3.0);
            px(r, 16.0, 6.0, 2.0, 7.0);
            px(r, 8.0, 6.0, 2.0, 2.0);
            px(r, 4.0, 8.0, 10.0, 3.0);
            px(r, 4.0, 11.0, 2.0, 7.0);
            px(r, 12.0, 11.0, 2.0, 7.0);
            px(r, 4.0, 16.0, 10.0, 2.0);
        }
        Sign::Close => {
            r.set_aliased(true);
            let (l, t, rr, b) = (x + 7.0, y + 5.0, x + 18.0, y + 16.0);
            let w = 1.6;
            r.fill_polygon(&[Point::new(l, t), Point::new(l + 2.0 * w, t), Point::new(rr, b), Point::new(rr - 2.0 * w, b)], ink);
            r.fill_polygon(&[Point::new(rr - 2.0 * w, t), Point::new(rr, t), Point::new(l + 2.0 * w, b), Point::new(l, b)], ink);
            r.set_aliased(false);
        }
    }
}

/// XP: a rounded glossy tile (the skin's `marble`, close in `marble-close`) lit from the top left,
/// a white edge, and a white sign (a dark one on a light tile, as in Silver).
fn xp_button(r: &mut Renderer, theme: &Theme, face: Rect, sign: Sign, s: State) {
    let face = Rect::new(r.snap(face.x), r.snap(face.y), face.w, face.h);
    let base = if sign == Sign::Close { theme.marble_close } else { theme.marble };
    let light = base.color_at(0.5).lightness() > 0.62;
    let pressed = s.down && s.hot;
    let paint = if pressed {
        // Dark and turned over: darkest at the top.
        let g = base.darker(0.42);
        Paint::from_stops(&[Stop { at: 0.0, color: g.last() }, Stop { at: 0.6, color: g.color_at(0.5) }, Stop { at: 1.0, color: g.first() }])
    } else if s.hot {
        base.lighter(0.16)
    } else {
        base
    };
    let paint = if s.active { paint } else { paint.map(|c| c.mix(c.grey(), 0.55).lighter(0.25)) };
    r.fill_paint(face, 3.0, &paint);
    if !pressed {
        // The light spot at the top left.
        let spot = paint.first().lighter(0.35);
        r.push_clip(face.inset(1.0, 1.0));
        for (k, a) in [(7.0, 0.25), (5.0, 0.3), (3.0, 0.35)] {
            r.fill_ellipse(Point::new(face.x + 5.0, face.y + 5.0), k, k, spot.with_alpha(a));
        }
        r.pop_clip();
        let dark = paint.last();
        r.fill_rect(Rect::new(face.right() - 2.0, face.y + 2.0, 1.0, face.h - 4.0), dark.with_alpha(0.7));
        r.fill_rect(Rect::new(face.x + 2.0, face.bottom() - 2.0, face.w - 4.0, 1.0), dark.with_alpha(0.7));
    }
    let border = if light { base.last().darker(0.25) } else { Color::hex(0xffffff) };
    let border = if s.active { border } else { border.with_alpha(0.7) };
    r.stroke_round_rect(face.inset(0.5, 0.5), 2.5, 1.0, border);
    let ink = if light { Color::hex(0x252536) } else { Color::hex(0xffffff) };
    let (x, y) = (face.x, face.y);
    let emboss = light.then_some(Color::hex(0xffffff));
    let block = |r: &mut Renderer, dx: f32, dy: f32, w: f32, h: f32| {
        if let Some(e) = emboss {
            r.fill_rect(Rect::new(x + dx - 1.0, y + dy - 1.0, w, h), e);
        }
        r.fill_rect(Rect::new(x + dx, y + dy, w, h), ink);
    };
    match sign {
        Sign::Minimize => block(r, 5.0, 13.0, 7.0, 3.0),
        Sign::Maximize => {
            block(r, 5.0, 5.0, 11.0, 3.0);
            block(r, 5.0, 8.0, 1.0, 8.0);
            block(r, 15.0, 8.0, 1.0, 8.0);
            block(r, 5.0, 15.0, 11.0, 1.0);
        }
        Sign::Restore => {
            block(r, 7.0, 5.0, 9.0, 2.0);
            block(r, 15.0, 7.0, 1.0, 6.0);
            block(r, 5.0, 9.0, 9.0, 2.0);
            block(r, 5.0, 11.0, 1.0, 5.0);
            block(r, 13.0, 11.0, 1.0, 5.0);
            block(r, 5.0, 15.0, 9.0, 1.0);
        }
        Sign::Close => {
            if let Some(e) = emboss {
                r.line(Point::new(x + 5.5, y + 5.0), Point::new(x + 15.0, y + 14.5), 2.2, e);
            }
            r.line(Point::new(x + 6.0, y + 6.0), Point::new(x + 15.0, y + 15.0), 2.2, ink);
            r.line(Point::new(x + 15.0, y + 6.0), Point::new(x + 6.0, y + 15.0), 2.2, ink);
        }
    }
}

/// 7: one button's glass face (its bottom left / bottom right corner round when `ends` says).
fn seven_face(r: &mut Renderer, theme: &Theme, face: Rect, sign: Sign, s: State, ends: (bool, bool)) {
    let face = Rect::new(r.snap(face.x), r.snap(face.y), r.snap(face.w), face.h);
    let pressed = s.down && s.hot;
    let close = sign == Sign::Close && s.active;
    let stops: &[(f32, u32)] = match (close, s.hot, pressed) {
        (true, _, true) => &[(0.0, 0xd79c8c), (0.49, 0xc46a55), (0.5, 0xa82a12), (1.0, 0x8c1e0a)],
        (true, true, false) => &[(0.0, 0xf9c9be), (0.49, 0xee8c74), (0.5, 0xe1462a), (1.0, 0xd53a1d)],
        (false, _, true) => &[(0.0, 0x9ec2e8), (0.49, 0x6c9fd6), (0.5, 0x386eb8), (1.0, 0x2758a8)],
        (false, true, false) => &[(0.0, 0xe3f1fd), (0.49, 0xbddbf7), (0.5, 0x7fb6ee), (1.0, 0x5a9ee6)],
        _ => &[],
    };
    let paint = if stops.is_empty() {
        let p = if close { theme.marble_close } else { theme.marble };
        if s.active {
            p
        } else {
            p.map(|c| c.mix(c.grey(), 0.6).lighter(0.2))
        }
    } else {
        Paint::from_stops(&stops.iter().map(|&(at, c)| Stop { at, color: Color::hex(c) }).collect::<Vec<_>>())
    };
    // The bottom corners: round where the group ends, square where it joins the next button.
    r.push_clip(face);
    r.fill_paint(Rect::new(face.x, face.y - 5.0, face.w, face.h + 5.0), 4.0, &paint);
    r.pop_clip();
    let (left, right) = ends;
    if !left {
        r.fill_paint(Rect::new(face.x, face.bottom() - 5.0, 5.0, 5.0), 0.0, &Paint::Flat(paint.last()));
    }
    if !right {
        r.fill_paint(Rect::new(face.right() - 5.0, face.bottom() - 5.0, 5.0, 5.0), 0.0, &Paint::Flat(paint.last()));
    }
    if s.hot {
        // The glow rising from the bottom middle.
        let glow = if sign == Sign::Close && s.active { Color::hex(0xffb27a) } else { Color::hex(0x6ce0ff) };
        let c = Point::new(face.x + face.w / 2.0, face.bottom());
        r.push_clip(face.inset(1.0, 1.0));
        for (k, a) in [(1.0, 0.25), (0.75, 0.3), (0.5, 0.35)] {
            r.fill_ellipse(c, face.w * 0.36 * k, face.h * 0.62 * k, glow.with_alpha(a));
        }
        r.pop_clip();
    }
    // The inner light edge.
    let shine = Color::rgba(255, 255, 255, if close && !s.hot { 0.5 } else { 0.6 });
    r.fill_rect(Rect::new(face.x + 1.0, face.y + 1.0, face.w - 2.0, 1.0), shine);
    r.fill_rect(Rect::new(face.x + 1.0, face.y + 1.0, 1.0, face.h - 4.0), shine);
    r.fill_rect(Rect::new(face.right() - 2.0, face.y + 1.0, 1.0, face.h - 4.0), shine);
}

/// 7: a white sign with a dark edge round it, in the middle of `face` (a pixel higher).
fn seven_sign(r: &mut Renderer, face: Rect, sign: Sign) {
    let rim = Color::hex(0x535666);
    let white = Color::hex(0xffffff);
    let c = Point::new(r.snap(face.x + face.w / 2.0), r.snap(face.y + face.h / 2.0) - 1.0);
    let boxed = |r: &mut Renderer, rect: Rect| {
        r.fill_rect(rect.inset(-1.0, -1.0), rim);
        r.fill_rect(rect, white);
    };
    match sign {
        Sign::Minimize => boxed(r, Rect::new(c.x - 5.0, c.y + 3.0, 10.0, 3.0)),
        Sign::Maximize => {
            let w = Rect::new(c.x - 5.0, c.y - 4.0, 10.0, 9.0);
            r.fill_rect(w.inset(-1.0, -1.0), rim);
            r.fill_rect(w, white);
            let hole = Rect::new(w.x + 2.0, w.y + 3.0, w.w - 4.0, w.h - 5.0);
            r.fill_rect(hole.inset(-1.0, -1.0), rim);
            r.fill_rect(hole, Color::hex(0xc9cfd8));
        }
        Sign::Restore => {
            let back = Rect::new(c.x - 3.0, c.y - 6.0, 9.0, 7.0);
            r.fill_rect(back.inset(-1.0, -1.0), rim);
            r.fill_rect(back, white);
            let front = Rect::new(c.x - 6.0, c.y - 2.0, 9.0, 8.0);
            r.fill_rect(front.inset(-1.0, -1.0), rim);
            r.fill_rect(front, white);
            let hole = Rect::new(front.x + 2.0, front.y + 3.0, front.w - 4.0, front.h - 5.0);
            r.fill_rect(hole.inset(-1.0, -1.0), rim);
            r.fill_rect(hole, Color::hex(0xc9cfd8));
        }
        Sign::Close => {
            let (h, w) = (4.5, 2.6);
            for color_w in [(rim, w + 2.0), (white, w)] {
                r.line(Point::new(c.x - h, c.y - h + 0.5), Point::new(c.x + h, c.y + h + 0.5), color_w.1, color_w.0);
                r.line(Point::new(c.x + h, c.y - h + 0.5), Point::new(c.x - h, c.y + h + 0.5), color_w.1, color_w.0);
            }
        }
    }
}
