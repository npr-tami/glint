//! The Settings window drawn as its approved mockup: flat and small, like File Explorer's Folder
//! Options in the skin's colours - a plain title, page names with a thin accent line under the
//! one you're on, group boxes with small grey titles, square ticks, round buttons, small boxed
//! drop-downs and plain buttons in sentence case.
//!
//! Only for Glint's own skins: the older Windows skins (98 to 7) and glass keep their own
//! controls (kit.rs), which already are File Explorer's of their time.

use crate::kit::{self, Look};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{Color, Era, Theme};
use crate::ui::{Point, Rect};

/// The title bar's height.
pub const TITLE_H: f32 = 30.0;
/// A page name's height (the line under them is at its bottom).
pub const TAB_H: f32 = 28.0;
/// Room each side of a page name.
pub const TAB_PAD: f32 = 10.0;
/// Between two page names.
pub const TAB_GAP: f32 = 2.0;
/// A group's title.
pub const GROUP_FONT: f32 = 11.0;
/// A button's words.
pub const BUTTON_FONT: f32 = 11.0;
/// Room each side of a button's words.
pub const BUTTON_PAD: f32 = 12.0;

/// Whether the window is drawn this way in `theme`.
pub fn on(theme: &Theme) -> bool {
    theme.era == Era::None && !kit::glass(theme) && !theme.bevels && !kit::old_windows(theme)
}

fn faded(c: Color, look: Look) -> Color {
    if look.enabled {
        c
    } else {
        c.with_alpha(c.a * 0.38)
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    crate::details::blend(a, b, t)
}

/// The edge of a box you type in or pick from.
fn control_edge(theme: &Theme) -> Color {
    mix(theme.line, theme.muted, 0.35)
}

/// The edge of an empty tick or round button.
fn mark_edge(theme: &Theme) -> Color {
    mix(theme.line, theme.muted, 0.75)
}

/// The title bar: "Settings" in small grey words (the close button is drawn by the window).
pub fn title(r: &mut Renderer, theme: &Theme, bar: Rect, active: bool) {
    r.fill_rect(bar, theme.window);
    let color = if active { theme.muted } else { theme.muted.with_alpha(0.7) };
    r.text("Settings", Rect::new(bar.x + 10.0, bar.y, (bar.w - 60.0).max(0.0), bar.h), &TextStyle::new(color));
}

/// How wide a page's name is with its room.
pub fn tab_width(r: &mut Renderer, text: &str) -> f32 {
    r.measure(text, &TextStyle::new(Color::hex(0))).ceil() + 2.0 * TAB_PAD
}

/// A page's name along the top: the one you're on in the text colour with an accent line under it.
pub fn tab(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, selected: bool, look: Look) {
    let color = if selected || look.hover { theme.text } else { theme.muted };
    r.text(text, rect, &TextStyle::new(color).align(Align::Center));
    if selected {
        r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 2.0), theme.accent);
    }
    if look.focused {
        kit::focus_ring_round(r, theme, rect.inset(1.0, 3.0), 2.0, 0.0);
    }
}

/// The line under the page names.
pub fn tab_line(r: &mut Renderer, theme: &Theme, y: f32, w: f32) {
    r.fill_rect(Rect::new(0.0, y - 1.0, w, 1.0), theme.line);
}

/// A group box: a hairline with soft corners, its small grey title sitting on the top line.
pub fn group(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, back: Color) {
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), rect.w.round(), rect.h.round());
    r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, theme.line);
    if text.is_empty() {
        return;
    }
    let style = TextStyle::new(theme.muted).size(GROUP_FONT);
    let w = r.measure(text, &style).ceil();
    let x = rect.x + 8.0;
    r.fill_rect(Rect::new(x, rect.y - 1.0, w + 8.0, 3.0), back);
    r.text(text, Rect::new(x + 4.0, rect.y - 9.0, w + 2.0, 18.0), &style);
}

/// A square tick box: an empty box, or filled with the accent and a white tick. `on` fades
/// between them (0.0 to 1.0) while it flips.
pub fn check(r: &mut Renderer, theme: &Theme, rect: Rect, on: f32, look: Look) {
    let t = on.clamp(0.0, 1.0);
    let lit = look.enabled && (look.hover || look.pressed);
    let edge = if lit { theme.text.with_alpha(0.8) } else { mark_edge(theme) };
    r.fill_round_rect(rect, 2.0, faded(theme.input, look));
    r.stroke_round_rect(rect.inset(0.5, 0.5), 2.0, 1.0, faded(mix(edge, theme.accent, t), look));
    if t > 0.0 {
        r.fill_round_rect(rect, 2.0, faded(theme.accent.with_alpha(theme.accent.a * t), look));
        let s = rect.h / 13.0;
        let (x, y) = (rect.x, rect.y);
        let tick = [Point::new(x + 3.0 * s, y + 6.8 * s), Point::new(x + 5.4 * s, y + 9.2 * s), Point::new(x + 10.0 * s, y + 3.8 * s)];
        r.polyline(&tick, 1.6, faded(theme.on_accent.with_alpha(theme.on_accent.a * t), look));
    }
    if look.focused {
        kit::focus_ring_round(r, theme, rect.inset(-2.0, -2.0), 3.0, 0.0);
    }
}

/// A round button: a ring, with the accent's dot in it when it's the one picked.
pub fn radio(r: &mut Renderer, theme: &Theme, rect: Rect, on: f32, look: Look) {
    let t = on.clamp(0.0, 1.0);
    let c = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
    let radius = rect.w.min(rect.h) / 2.0;
    let lit = look.enabled && (look.hover || look.pressed);
    let edge = if lit { theme.text.with_alpha(0.8) } else { mark_edge(theme) };
    r.fill_ellipse(c, radius, radius, faded(theme.input, look));
    r.stroke_ellipse(c, radius - 0.5, radius - 0.5, 1.0, faded(mix(edge, theme.accent, t), look));
    if t > 0.0 {
        let d = radius - 3.0;
        r.fill_ellipse(c, d, d, faded(theme.accent.with_alpha(theme.accent.a * t), look));
    }
    if look.focused {
        kit::focus_ring_round(r, theme, rect.inset(-2.0, -2.0), radius + 2.0, 0.0);
    }
}

/// How wide a drop-down is for the longest of `labels`, between `min` and `max`.
pub fn dropdown_width<'a>(r: &mut Renderer, labels: impl IntoIterator<Item = &'a str>, min: f32, max: f32) -> f32 {
    let style = TextStyle::new(Color::hex(0));
    let longest = labels.into_iter().map(|l| r.measure(l, &style)).fold(0.0_f32, f32::max);
    (longest + 7.0 + 24.0).ceil().clamp(min, max)
}

/// A closed drop-down: a small box in the text boxes' colour, the choice in it, a small arrow.
pub fn dropdown(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, look: Look) {
    let lit = look.enabled && (look.hover || look.pressed);
    r.fill_round_rect(rect, 3.0, faded(if look.pressed && look.enabled { mix(theme.input, theme.text, 0.06) } else { theme.input }, look));
    r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, faded(if lit { mix(control_edge(theme), theme.text, 0.35) } else { control_edge(theme) }, look));
    r.text(text, Rect::new(rect.x + 7.0, rect.y, (rect.w - 7.0 - 22.0).max(0.0), rect.h), &TextStyle::new(faded(theme.text, look)));
    let c = Point::new(r.snap(rect.right() - 11.0), r.snap(rect.y + rect.h / 2.0));
    let arrow = [Point::new(c.x - 4.0, c.y - 2.0), Point::new(c.x + 4.0, c.y - 2.0), Point::new(c.x, c.y + 2.5)];
    r.fill_polygon(&arrow, faded(if lit { theme.text } else { theme.muted }, look));
    if look.focused {
        kit::focus_ring_round(r, theme, rect, 3.0, 0.0);
    }
}

fn button_style(color: Color) -> TextStyle {
    TextStyle::new(color).size(BUTTON_FONT).align(Align::Center)
}

/// A button's words: "RESET PAGE" as "Reset page".
pub fn button_text(label: &str) -> String {
    kit::sentence_case(label)
}

pub fn button_width(r: &mut Renderer, label: &str) -> f32 {
    r.measure(&button_text(label), &button_style(Color::hex(0))).ceil() + 2.0 * BUTTON_PAD
}

/// A plain button: a small box with a hairline, its words in sentence case.
pub fn button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look) {
    let fill = match (look.enabled, look.pressed, look.hover) {
        (true, true, true) => theme.button_pressed,
        (true, _, true) => theme.chip_hover,
        _ => theme.chip,
    };
    r.fill_round_rect(rect, 3.0, faded(fill, look));
    r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, faded(control_edge(theme), look));
    r.text(&button_text(label), rect, &button_style(faded(theme.text, look)));
    if look.focused {
        kit::focus_ring_round(r, theme, rect, 3.0, 0.0);
    }
}
