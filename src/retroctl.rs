//! The controls of Glint's own windows (Settings, Sort, Skins, the questions) and the scroll bars
//! as an older Windows drew them, for a skin with an `era` (theme.rs `Era`; kit.rs and list.rs come
//! here for those):
//!
//! - check boxes in place of the switches (no Windows of the time had a switch): 2000's sunken
//!   white box with its black tick, XP's with the green tick and the orange glow under the mouse,
//!   7's with the slate blue one;
//! - push buttons: 2000's raised grey, XP's white with the blue edge and the orange glow, 7's glass;
//! - drop-downs: 2000's sunken box with its raised arrow button, XP's with the pale blue one, 7's
//!   that looks like a push button;
//! - group boxes in place of the cards, and the scroll bars.

use crate::eraicons::{c, g};
use crate::kit::Look;
use crate::render::{Align, Renderer, TextStyle};
use crate::retro::{self, Edge};
use crate::retrotool;
use crate::theme::{Color, Era, Paint, Theme};
use crate::ui::{Point, Rect};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

/// The XP colour scheme (the blue one, Silver, Royale), judged by the title bar's words.
fn royale(theme: &Theme) -> bool {
    theme.era == Era::Xp && theme.title_shadow.a > 0.0 && theme.title_shadow.lightness() < 0.02 && theme.title_text.lightness() > 0.5
}

fn silver(theme: &Theme) -> bool {
    theme.era == Era::Xp && theme.title_text.lightness() < 0.5
}

/// The 13 DIP box of a check box centred in `rect`.
fn box13(r: &Renderer, rect: Rect) -> Rect {
    Rect::new(r.snap(rect.x + (rect.w - 13.0) / 2.0), r.snap(rect.y + (rect.h - 13.0) / 2.0), 13.0, 13.0)
}

/// The tick of a check box as Windows drew it: 7 by 7 pixels, every column 3 tall, its top left at
/// (x, y).
fn pixel_tick(r: &mut Renderer, x: f32, y: f32, color: Color) {
    // (column, top row) of each 3 pixel column.
    for (col, top) in [(0.0, 2.0), (1.0, 3.0), (2.0, 4.0), (3.0, 3.0), (4.0, 2.0), (5.0, 1.0), (6.0, 0.0)] {
        r.fill_rect(Rect::new(x + col, y + top, 1.0, 3.0), color);
    }
}

/// A check box (the switches too) with `rect` around it, ticked as far as `t` (0 to 1).
pub fn checkbox(r: &mut Renderer, theme: &Theme, rect: Rect, t: f32, look: Look) {
    let b = box13(r, rect);
    let on = t > 0.5;
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            let inside = if look.enabled && !(look.pressed && look.hover) { c(0xffffff) } else { face };
            r.fill_rect(b, inside);
            retro::edge(r, theme, b, Edge::Sunken, face);
            if on {
                r.set_aliased(true);
                pixel_tick(r, b.x + 3.0, b.y + 3.0, if look.enabled { c(0x000000) } else { theme.disabled });
                r.set_aliased(false);
            }
        }
        Era::Xp => {
            if !look.enabled {
                r.fill_rect(b, c(0xffffff));
                r.stroke_rect(b.inset(0.5, 0.5), 1.0, c(0xcac8bb));
            } else {
                let paint = if look.pressed && look.hover { g(&[(0.0, 0xb0b0a7), (1.0, 0xe3e1d2)]) } else { g(&[(0.0, 0xdcdcd7), (1.0, 0xffffff)]) };
                r.fill_rect_toward(b, 0.0, &paint, Point::new(b.x, b.y), Point::new(b.right(), b.bottom()));
                if look.hover {
                    let glow = b.inset(1.0, 1.0);
                    r.fill_rect_toward(glow, 0.0, &g(&[(0.0, 0xfedf9c), (1.0, 0xf8b636)]), Point::new(glow.x, glow.y), Point::new(glow.right(), glow.bottom()));
                    let inner = b.inset(3.0, 3.0);
                    r.fill_rect_toward(inner, 0.0, &paint, Point::new(b.x, b.y), Point::new(b.right(), b.bottom()));
                }
                r.stroke_rect(b.inset(0.5, 0.5), 1.0, c(0x1c5180));
            }
            if on {
                r.set_aliased(true);
                pixel_tick(r, b.x + 3.0, b.y + 3.0, if look.enabled { c(0x21a121) } else { c(0xcac8bb) });
                r.set_aliased(false);
            }
        }
        Era::Win7 => {
            let (border, inner) = if !look.enabled {
                (0xbcbcbc, g(&[(0.0, 0xf4f4f4), (1.0, 0xf4f4f4)]))
            } else if look.pressed && look.hover {
                (0x2c628b, g(&[(0.0, 0x7fbae1), (1.0, 0xd7eefa)]))
            } else if look.hover {
                (0x3c7fb1, g(&[(0.0, 0xb1dffd), (1.0, 0xe9f7fe)]))
            } else {
                (0x8e8f8f, g(&[(0.0, 0xcbcfd5), (1.0, 0xf6f6f6)]))
            };
            r.fill_rect(b, c(0xf4f4f4));
            let i = b.inset(2.0, 2.0);
            r.fill_rect_toward(i, 0.0, &inner, Point::new(i.x, i.y), Point::new(i.right(), i.bottom()));
            r.stroke_rect(b.inset(0.5, 0.5), 1.0, c(border));
            if on {
                let p = [Point::new(b.x + 3.0, b.y + 6.5), Point::new(b.x + 5.5, b.y + 9.0), Point::new(b.x + 10.0, b.y + 3.5)];
                r.polyline(&p, 2.0, c(if look.enabled { 0x475c94 } else { 0xa0a0a0 }));
            }
        }
        Era::None => {}
    }
    if look.focused {
        dotted(r, b.inset(-2.0, -2.0), theme.text);
    }
}

/// The keyboard's dotted outline.
pub fn dotted(r: &mut Renderer, rect: Rect, color: Color) {
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    let mut x = rect.x;
    while x < rect.right() {
        r.fill_rect(Rect::new(x, rect.y, 1.0, 1.0), color);
        r.fill_rect(Rect::new(x, rect.bottom() - 1.0, 1.0, 1.0), color);
        x += 2.0;
    }
    let mut y = rect.y;
    while y < rect.bottom() {
        r.fill_rect(Rect::new(rect.x, y, 1.0, 1.0), color);
        r.fill_rect(Rect::new(rect.right() - 1.0, y, 1.0, 1.0), color);
        y += 2.0;
    }
}

/// A push button with `label`. `primary`: the window's main button (2000: a black frame round it;
/// XP: the blue glow inside; 7: the cyan glow).
pub fn push_button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look, primary: bool) {
    let down = look.enabled && look.pressed && look.hover;
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    let shift = if down { 1.0 } else { 0.0 };
    let text_rect = Rect::new(rect.x + shift, rect.y + shift, rect.w, rect.h);
    let style = TextStyle::new(theme.button_text).align(Align::Center);
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            r.fill_rect(rect, face);
            let inner = if primary || down {
                r.stroke_rect(rect.inset(0.5, 0.5), 1.0, c(0x000000));
                rect.inset(1.0, 1.0)
            } else {
                rect
            };
            if down {
                retro::edge(r, theme, inner, Edge::Frame, face);
            } else {
                retro::edge(r, theme, inner, Edge::Raised, face);
            }
            if look.enabled {
                r.text(label, text_rect, &style);
            } else {
                // Pressed into the face: a light copy under, the grey one over it.
                let (hl, ..) = retro::bevel_colours(theme, face);
                r.text(label, Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w, rect.h), &style.color(hl));
                r.text(label, rect, &style.color(theme.disabled));
            }
            if look.focused {
                dotted(r, rect.inset(4.0, 4.0), c(0x000000));
            }
            return;
        }
        Era::Xp => {
            if !look.enabled {
                r.fill_rect(rect.inset(1.0, 1.0), c(0xf5f4ea));
                r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, c(0xc9c7ba));
                r.text(label, rect, &style.color(c(0xaca899)));
                return;
            }
            let face = rect.inset(1.0, 1.0);
            let paint = if down {
                g(&[(0.0, 0xcdcac3), (0.08, 0xe3e3db), (0.94, 0xe5e5de), (1.0, 0xf2f2f1)])
            } else if royale(theme) {
                g(&[(0.0, 0xe8eff7), (0.08, 0xf8fcff), (0.5, 0xe7eff5), (0.75, 0xc6d4e6), (0.95, 0xa5b7cf), (1.0, 0x9ab0d2)])
            } else {
                g(&[(0.0, 0xffffff), (0.5, 0xf6f6f3), (0.84, 0xf0f0ea), (0.89, 0xecebe6), (0.94, 0xe2dfd6), (1.0, 0xd6d0c5)])
            };
            r.fill_paint(face, 2.5, &paint);
            if look.hover && !down {
                let glow = face.inset(1.0, 1.0);
                r.stroke_round_rect(glow.inset(0.5, 0.5), 2.0, 2.0, c(0xfbc761));
                r.fill_rect(Rect::new(glow.x + 1.0, glow.y, glow.w - 2.0, 1.0), c(0xfff0cf));
                r.fill_rect(Rect::new(glow.x + 1.0, glow.bottom() - 1.0, glow.w - 2.0, 1.0), c(0xe5a01a));
            } else if primary && !down {
                let glow = face.inset(1.0, 1.0);
                r.stroke_round_rect(glow.inset(0.5, 0.5), 2.0, 2.0, c(0xa0beec));
                r.fill_rect(Rect::new(glow.x + 1.0, glow.y, glow.w - 2.0, 1.0), c(0xcee7ff));
                r.fill_rect(Rect::new(glow.x + 1.0, glow.bottom() - 1.0, glow.w - 2.0, 1.0), c(0x6982ee));
            }
            let border = if royale(theme) { 0x2d5082 } else { 0x003c74 };
            r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, c(border));
        }
        Era::Win7 => {
            if !look.enabled {
                r.fill_rect(rect.inset(1.0, 1.0), c(0xf4f4f4));
                r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, c(0xadb2b5));
                r.text(label, rect, &style.color(c(0x838383)));
                return;
            }
            let (paint, border) = if down {
                (g(&[(0.0, 0xe5f4fc), (0.49, 0xc4e5f6), (0.5, 0x98d1ef), (1.0, 0x68b3db)]), 0x2c628b)
            } else if look.hover {
                (g(&[(0.0, 0xeaf6fd), (0.49, 0xd9f0fc), (0.5, 0xbee6fd), (1.0, 0xa7d9f5)]), 0x3c7fb1)
            } else if primary {
                (g(&[(0.0, 0xeef3f6), (0.49, 0xeaf0f4), (0.5, 0xd0e0e8), (0.85, 0xc9dae4), (1.0, 0xa3d2e2)]), 0x3c7fb1)
            } else {
                (g(&[(0.0, 0xf2f2f2), (0.49, 0xebebeb), (0.5, 0xdddddd), (1.0, 0xcfcfcf)]), 0x707070)
            };
            r.fill_paint(rect.inset(1.0, 1.0), 2.5, &paint);
            let highlight = if primary && !look.hover && !down { c(0x3cd6fd) } else { Color::rgba(255, 255, 255, 0.8) };
            r.stroke_round_rect(rect.inset(1.5, 1.5), 2.0, 1.0, highlight);
            r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, c(border));
        }
        Era::None => {}
    }
    r.text(label, text_rect, &style);
    if look.focused {
        dotted(r, rect.inset(3.0, 3.0), theme.button_text.with_alpha(0.7));
    }
}

/// A closed drop-down list showing `text`.
pub fn combo(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, look: Look) {
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    let ink = if look.enabled { theme.text } else { theme.disabled };
    match theme.era {
        Era::Win7 => {
            // 7: a drop-down list that can't be typed in looks like a push button.
            push_button(r, theme, rect, "", Look { focused: false, ..look }, false);
            r.text(text, Rect::new(rect.x + 6.0, rect.y, (rect.w - 26.0).max(0.0), rect.h), &TextStyle::new(ink));
            retrotool::triangle_down(r, Point::new(rect.right() - 10.0, rect.y + rect.h / 2.0), 7.0, if look.enabled { c(0x000000) } else { c(0x838383) });
        }
        _ => {
            r.fill_paint(rect, 0.0, &theme.input_paint);
            retrotool::field(r, theme, rect, false);
            let arrow = retrotool::drop_arrow(r, theme, rect, look.hover && look.enabled);
            if look.pressed && look.hover && theme.era.classic() {
                // Held down: a flat frame instead of the raised one.
                r.fill_rect(arrow, theme.toolbar);
                retro::edge(r, theme, arrow, Edge::Frame, theme.toolbar);
                retrotool::triangle_down(r, Point::new(arrow.x + arrow.w / 2.0 + 1.0, arrow.y + arrow.h / 2.0 + 1.0), 7.0, c(0x000000));
            }
            let text_rect = Rect::new(rect.x + 5.0, rect.y, (arrow.x - rect.x - 7.0).max(0.0), rect.h);
            r.text(text, text_rect, &TextStyle::new(ink));
        }
    }
    if look.focused {
        dotted(r, rect.inset(3.0, 3.0), theme.text.with_alpha(0.7));
    }
}

/// A group box in place of a card: 2000's etched frame, XP's thin rounded one, 7's pale one; the
/// inside left as the window is.
pub fn group_box(r: &mut Renderer, theme: &Theme, rect: Rect) {
    let rect = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    match theme.era {
        Era::Win98 | Era::Win2000 => retro::edge(r, theme, rect, Edge::Etched, theme.toolbar),
        Era::Xp => {
            r.fill_paint(rect.inset(1.0, 1.0), 3.0, &Paint::Flat(Color::rgba(255, 255, 255, 0.55)));
            r.stroke_round_rect(rect.inset(0.5, 0.5), 3.5, 1.0, c(if silver(theme) { 0xbfbfc6 } else { 0xd0d0bf }));
        }
        Era::Win7 => {
            r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, c(0xd5dfe5));
            r.stroke_round_rect(rect.inset(1.5, 1.5), 2.0, 1.0, c(0xffffff));
        }
        Era::None => {}
    }
}

/// A group box's title colour: 2000's black, XP's blue (Silver's dark grey), 7's dark blue.
pub fn group_title(theme: &Theme) -> Color {
    match theme.era {
        Era::Xp if silver(theme) => c(0x3f3d3d),
        Era::Xp if royale(theme) => c(0x1a4aa6),
        Era::Xp => c(0x0046d5),
        Era::Win7 => c(0x1e395b),
        _ => theme.text,
    }
}

/// Which arrow button of a scroll bar is held down (or which way a held track pages).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Held {
    #[default]
    None,
    /// The one at the top (or the left).
    Start,
    /// The one at the bottom (or the right).
    End,
}

impl Held {
    /// Which way it scrolls: -1 back toward the start, 1 on toward the end.
    pub fn sign(self) -> f32 {
        match self {
            Held::Start => -1.0,
            Held::End => 1.0,
            Held::None => 0.0,
        }
    }
}

/// What the mouse holds down on a scroll bar: an arrow button, or the track before (`Start`) or
/// after (`End`) the thumb, which pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hold {
    #[default]
    None,
    Arrow(Held),
    Page(Held),
}

impl Hold {
    /// The arrow button that shows pressed.
    pub fn arrow(self) -> Held {
        match self {
            Hold::Arrow(h) => h,
            _ => Held::None,
        }
    }

    /// Which way it scrolls.
    pub fn way(self) -> Held {
        match self {
            Hold::Arrow(h) | Hold::Page(h) => h,
            Hold::None => Held::None,
        }
    }
}

/// Which ways a scroll bar can still go: its arrow buttons grey out at the ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    pub start: bool,
    pub end: bool,
}

impl Default for Room {
    fn default() -> Room {
        Room { start: true, end: true }
    }
}

impl Room {
    /// A bar scrolled `offset` of the most it can, `max`.
    pub fn of(offset: f32, max: f32) -> Room {
        Room { start: offset > 0.5, end: offset < max - 0.5 }
    }

    /// Whether it can still go `way`.
    pub fn toward(self, way: Held) -> bool {
        match way {
            Held::Start => self.start,
            Held::End => self.end,
            Held::None => false,
        }
    }
}

/// How long a held arrow button or track waits before it first repeats, in ms...
pub const REPEAT_FIRST_MS: u32 = 400;
/// ...and how often it repeats after that.
pub const REPEAT_NEXT_MS: u32 = 50;

/// At most this many steps at once, catching up after a slow moment.
const REPEAT_CATCH_UP: u32 = 8;

/// A held scroll bar arrow or track repeating as in Windows: one step as the mouse goes down, the
/// next REPEAT_FIRST_MS later, then one every REPEAT_NEXT_MS until the button comes up or the bar
/// can go no further that way. The window runs a timer only meanwhile: `press` and `fire` say what
/// to set it to (`repeat_timer`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Repeat {
    /// What is held (`Hold::None` when nothing). It shows pressed until the button comes up.
    pub hold: Hold,
    /// The timer runs (it stops at the end, while the button may still be down).
    pub ticking: bool,
    /// Steps repeated since the press.
    pub repeats: u32,
    /// When the button went down, in seconds.
    since: f64,
}

impl Repeat {
    /// The mouse went down on `hold` at `now` (seconds; the window took the first step): the
    /// timer's first wait.
    pub fn press(&mut self, hold: Hold, now: f64) -> u32 {
        *self = Repeat { hold, ticking: true, repeats: 0, since: now };
        REPEAT_FIRST_MS
    }

    /// The timer fired at `now`; `room` says whether the bar can still go the held way. Returns
    /// what to step (`Hold::None`: nothing), how many times (the repeats due by now, so a timer
    /// that comes late, while a slow frame is drawn, doesn't slow the scrolling down), and the
    /// timer's next wait (`None`: stop it).
    pub fn fire(&mut self, room: bool, now: f64) -> (Hold, u32, Option<u32>) {
        if self.hold == Hold::None || !self.ticking || !room {
            self.ticking = false;
            return (Hold::None, 0, None);
        }
        let after_first = (now - self.since) * 1000.0 - REPEAT_FIRST_MS as f64;
        let due = 1 + (after_first / REPEAT_NEXT_MS as f64).floor().max(0.0) as u32;
        let steps = due.saturating_sub(self.repeats).clamp(1, REPEAT_CATCH_UP);
        self.repeats += steps;
        (self.hold, steps, Some(REPEAT_NEXT_MS))
    }

    /// The button came up: whether something was held (it shows released now; stop the timer).
    pub fn release(&mut self) -> bool {
        let was = self.hold != Hold::None;
        *self = Repeat::default();
        was
    }
}

/// Sets `hwnd`'s repeat timer `id` to fire `ms` from now, or stops it (`None`).
pub fn repeat_timer(hwnd: HWND, id: usize, ms: Option<u32>) {
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: a timer of our own window (stopping one that doesn't run is harmless).
    unsafe {
        match ms {
            Some(ms) => {
                SetTimer(Some(hwnd), id, ms, None);
            }
            None => {
                let _ = KillTimer(Some(hwnd), id);
            }
        }
    }
}

/// Whether a held track still pages `way` with the mouse at `p`: Windows stops once the thumb
/// reaches the mouse, and pauses while the mouse is off the bar (`lane`).
pub fn page_on(lane: Rect, thumb: Rect, way: Held, p: Point) -> bool {
    if !lane.contains(p) {
        return false;
    }
    let (pos, start, end) = if lane.h >= lane.w { (p.y, thumb.y, thumb.bottom()) } else { (p.x, thumb.x, thumb.right()) };
    match way {
        Held::Start => pos < start,
        Held::End => pos >= end,
        Held::None => false,
    }
}

/// How a scroll bar is: the mouse on it (`hot`), its thumb dragged, what the mouse holds, and
/// which ways it can still go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BarLook {
    pub hot: bool,
    pub dragged: bool,
    pub hold: Hold,
    pub room: Room,
}

/// How long each arrow button is along a scroll bar: as long as the bar is wide (square buttons,
/// 16 in 98 / 2000, 17 in XP and 7). 0 without an era: the skins of today have no arrows.
pub fn arrows(theme: &Theme) -> f32 {
    if theme.era.old() {
        theme.scrollbar_lane()
    } else {
        0.0
    }
}

/// Which arrow button of a scroll bar along `lane` is at `p` (the lane's two ends, `arrows` long).
pub fn arrow_at(lane: Rect, arrows: f32, p: Point) -> Option<Held> {
    if arrows <= 0.0 || !lane.contains(p) {
        return None;
    }
    let (pos, start, end) = if lane.h >= lane.w { (p.y, lane.y, lane.bottom()) } else { (p.x, lane.x, lane.right()) };
    if pos < start + arrows {
        Some(Held::Start)
    } else if pos >= end - arrows {
        Some(Held::End)
    } else {
        None
    }
}

/// Where an arrow points.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

/// A scroll bar: `lane` is its whole strip, the arrow buttons at its two ends and the thumb (the
/// part to drag) between them. A held arrow shows pressed, and an arrow greys out where the bar
/// can go no further.
pub fn scrollbar(r: &mut Renderer, theme: &Theme, lane: Rect, thumb: Rect, look: BarLook) {
    let BarLook { hot, dragged, hold, room } = look;
    let upright = lane.h >= lane.w;
    let lane = Rect::new(r.snap(lane.x), r.snap(lane.y), r.snap(lane.w), r.snap(lane.h));
    // The arrow buttons: square, or half the lane each when it is too short for both.
    let a = if upright { lane.w.min((lane.h / 2.0).floor()) } else { lane.h.min((lane.w / 2.0).floor()) };
    let (first, last, dirs) = if upright {
        (Rect::new(lane.x, lane.y, lane.w, a), Rect::new(lane.x, lane.bottom() - a, lane.w, a), (Dir::Up, Dir::Down))
    } else {
        (Rect::new(lane.x, lane.y, a, lane.h), Rect::new(lane.right() - a, lane.y, a, lane.h), (Dir::Left, Dir::Right))
    };
    // The thumb fills the lane across and stays between the arrows.
    let thumb = if upright {
        let y = r.snap(thumb.y).max(first.bottom());
        Rect::new(lane.x, y, lane.w, r.snap(thumb.h).min(last.y - y).max(0.0))
    } else {
        let x = r.snap(thumb.x).max(first.right());
        Rect::new(x, lane.y, r.snap(thumb.w).min(last.x - x).max(0.0), lane.h)
    };
    let (from, to) = if upright { (Point::new(lane.x, 0.0), Point::new(lane.right(), 0.0)) } else { (Point::new(0.0, lane.y), Point::new(0.0, lane.bottom())) };
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            retro::dither(r, theme, lane, face);
            // The part of the track held down to page darkens while the button is down.
            if let Hold::Page(way) = hold {
                let part = match (upright, way) {
                    (true, Held::Start) => Rect::new(lane.x, first.bottom(), lane.w, thumb.y - first.bottom()),
                    (true, _) => Rect::new(lane.x, thumb.bottom(), lane.w, last.y - thumb.bottom()),
                    (false, Held::Start) => Rect::new(first.right(), lane.y, thumb.x - first.right(), lane.h),
                    (false, _) => Rect::new(thumb.right(), lane.y, last.x - thumb.right(), lane.h),
                };
                if part.w > 0.0 && part.h > 0.0 {
                    let (_, _, sh, dk) = retro::bevel_colours(theme, face);
                    r.fill_checker(part, 1.0 / r.scale().max(1.0), sh, dk);
                }
            }
            if thumb.w > 0.0 && thumb.h > 0.0 {
                r.fill_rect(thumb, face);
                retro::edge(r, theme, thumb, Edge::RaisedWindow, face);
            }
        }
        Era::Xp => {
            let track = g(&[(0.0, 0xeeede5), (0.06, 0xf3f1ec), (0.5, 0xf9f9f7), (0.94, 0xfefefb), (1.0, 0xeeede5)]);
            r.fill_rect_toward(lane, 0.0, &track, from, to);
            let t = thumb.inset(1.0, 1.0);
            let (body, edge, grip) = if silver(theme) {
                (g(&[(0.0, 0xd5d5d5), (0.15, 0xe1e1e1), (0.7, 0xdfdfdf), (1.0, 0xd4d4d4)]), 0xb8b8b8, (0xf6f6f6, 0xc2c2c2))
            } else if royale(theme) {
                (g(&[(0.0, 0xffffff), (0.2, 0xe2efff), (0.6, 0xcbdff7), (1.0, 0xbed2ea)]), 0x8599b1, (0xffffff, 0x8397af))
            } else if dragged {
                (g(&[(0.0, 0x9fb4ec), (1.0, 0x93b2f6)]), 0x839ad3, (0xeef4fe, 0x8cb0f8))
            } else if hot {
                (g(&[(0.0, 0xdae9ff), (1.0, 0xcee2ff)]), 0xaccefe, (0xeef4fe, 0x8cb0f8))
            } else {
                (g(&[(0.0, 0xb4c8f6), (0.1, 0xc5d5fc), (0.6, 0xc2d5fc), (0.85, 0xb3cffc), (1.0, 0xb5c9f3)]), 0x9fb5d2, (0xeef4fe, 0x8cb0f8))
            };
            if t.w > 2.0 && t.h > 2.0 {
                r.fill_rect_toward(t, 2.0, &body, from, to);
                r.stroke_round_rect(t.inset(0.5, 0.5), 2.0, 1.0, c(0xffffff));
                r.stroke_round_rect(Rect::new(t.x, t.y, t.w + 0.5, t.h + 0.5).inset(-0.5, -0.5), 2.5, 1.0, c(edge).with_alpha(0.7));
                grip_lines(r, t, upright, c(grip.0), c(grip.1), 4, 7.0);
            }
        }
        Era::Win7 => {
            let track = g(&[(0.0, 0xe3e3e3), (0.1, 0xe7e7e7), (0.4, 0xefefef), (1.0, 0xf1f1f1)]);
            r.fill_rect_toward(lane, 0.0, &track, from, to);
            let t = thumb.inset(1.0, 1.0);
            let (body, border, grip) = if dragged {
                (g(&[(0.0, 0xcdebfc), (0.49, 0xa9dcf7), (0.5, 0x6ec2ee), (1.0, 0x64b9e6)]), 0x2c628b, (0xeef9fe, 0x3c7fb1))
            } else if hot {
                (g(&[(0.0, 0xe3f4fc), (0.49, 0xd6eefb), (0.5, 0xa9dbf6), (1.0, 0xa4d6f2)]), 0x3c7fb1, (0xeef9fe, 0x3c7fb1))
            } else {
                (g(&[(0.0, 0xf5f5f5), (0.4, 0xecedee), (0.5, 0xd9dadc), (0.8, 0xc7c7cb), (1.0, 0xc0c0c4)]), 0x979797, (0xe9e9e9, 0x9c9c9c))
            };
            if t.w > 2.0 && t.h > 2.0 {
                r.fill_rect_toward(t, 2.5, &body, from, to);
                r.stroke_round_rect(t.inset(0.5, 0.5), 2.5, 1.0, c(border));
                grip_lines(r, t, upright, c(grip.0), c(grip.1), 4, 7.0);
            }
        }
        Era::None => return,
    }
    if a >= 4.0 {
        let held = hold.arrow();
        arrow_button(r, theme, first, dirs.0, held == Held::Start, hot, room.start);
        arrow_button(r, theme, last, dirs.1, held == Held::End, hot, room.end);
    }
}

/// One arrow button of a scroll bar in `b`: 98 / 2000's raised grey box with a black triangle
/// (a flat frame while held, the triangle a pixel down and right); XP's rounded pale blue (Silver
/// grey, Royale white) button with its chevron; 7's bare dark triangle that gets a button round it
/// while the mouse is on the bar. Not `enabled` (nothing more to scroll that way): 98 / 2000's
/// triangle embossed, white a pixel down and right under grey; XP's button faded; 7's triangle
/// light grey.
#[allow(clippy::too_many_arguments)]
fn arrow_button(r: &mut Renderer, theme: &Theme, b: Rect, dir: Dir, pressed: bool, bar_hot: bool, enabled: bool) {
    let pressed = pressed && enabled;
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            r.fill_rect(b, face);
            retro::edge(r, theme, b, if pressed { Edge::Frame } else { Edge::RaisedWindow }, face);
            if !enabled {
                let (hl, _, sh, _) = retro::bevel_colours(theme, face);
                triangle(r, b, dir, 1.0, hl);
                triangle(r, b, dir, 0.0, sh);
                return;
            }
            let shift = if pressed { 1.0 } else { 0.0 };
            triangle(r, b, dir, shift, c(0x000000));
        }
        Era::Xp => {
            let t = b.inset(1.0, 1.0);
            let (top, bottom, edge, ink) = if !enabled {
                if silver(theme) {
                    (0xf7f7f7, 0xe9e9e9, 0xd2d2d2, 0xbdbdbd)
                } else if royale(theme) {
                    (0xffffff, 0xe6edf6, 0xc2ccd8, 0xb6bcc6)
                } else {
                    (0xf7f9fe, 0xe3eafb, 0xc9d5ef, 0xb9c6e6)
                }
            } else if silver(theme) {
                if pressed {
                    (0xc5c5c5, 0xc1c1c1, 0x9a9a9a, 0x696969)
                } else if bar_hot {
                    (0xf6f6f6, 0xe2e2e2, 0xb8b8b8, 0x696969)
                } else {
                    (0xf1f1f1, 0xd1d1d1, 0xb8b8b8, 0x696969)
                }
            } else if royale(theme) {
                if pressed {
                    (0xbcd0e8, 0xffffff, 0x364a62, 0x5b6473)
                } else if bar_hot {
                    (0xffffff, 0xd4e3f5, 0x52667e, 0x5b6473)
                } else {
                    (0xffffff, 0xbcd0e8, 0x8599b1, 0x5b6473)
                }
            } else if pressed {
                (0x9fb4ec, 0x93b2f6, 0x839ad3, 0x4d6185)
            } else if bar_hot {
                (0xdcedfd, 0xc5e3fc, 0x95ade5, 0x4d6185)
            } else {
                (0xe6eefc, 0xaac0f3, 0x9fb5d2, 0x4d6185)
            };
            let body = g(&[(0.0, top), (1.0, bottom)]);
            r.fill_rect_toward(t, 2.0, &body, Point::new(0.0, t.y), Point::new(0.0, t.bottom()));
            r.stroke_round_rect(t.inset(0.5, 0.5), 2.0, 1.0, c(0xffffff));
            r.stroke_round_rect(Rect::new(t.x, t.y, t.w + 0.5, t.h + 0.5).inset(-0.5, -0.5), 2.5, 1.0, c(edge).with_alpha(0.7));
            chevron(r, b, dir, c(ink));
        }
        Era::Win7 => {
            if !enabled {
                triangle(r, b, dir, 0.0, c(0xbfbfbf));
                return;
            }
            if pressed || bar_hot {
                let t = b.inset(1.0, 1.0);
                let (paint, border) = if pressed {
                    (g(&[(0.0, 0xe5f4fc), (0.49, 0xc4e5f6), (0.5, 0x98d1ef), (1.0, 0x68b3db)]), 0x2c628b)
                } else {
                    (g(&[(0.0, 0xf2f2f2), (0.49, 0xebebeb), (0.5, 0xdddddd), (1.0, 0xcfcfcf)]), 0xb7b7b7)
                };
                let (from, to) = if matches!(dir, Dir::Up | Dir::Down) {
                    (Point::new(t.x, 0.0), Point::new(t.right(), 0.0))
                } else {
                    (Point::new(0.0, t.y), Point::new(0.0, t.bottom()))
                };
                r.fill_rect_toward(t, 2.5, &paint, from, to);
                r.stroke_round_rect(t.inset(0.5, 0.5), 2.5, 1.0, c(border));
            }
            triangle(r, b, dir, 0.0, c(0x4d4d4d));
        }
        Era::None => {}
    }
}

/// A solid triangle 7 wide and 4 deep pointing `dir`, in the middle of `b` (moved `shift` down and
/// right), made of whole pixels.
fn triangle(r: &mut Renderer, b: Rect, dir: Dir, shift: f32, color: Color) {
    let upright = matches!(dir, Dir::Up | Dir::Down);
    let (w, h) = if upright { (7.0, 4.0) } else { (4.0, 7.0) };
    let x = r.snap(b.x + ((b.w - w) / 2.0).floor()) + shift;
    let y = r.snap(b.y + ((b.h - h) / 2.0).floor()) + shift;
    for k in 0..4 {
        // Row (or column) k from the point: 2k + 1 long.
        let k = k as f32;
        let (from, len) = (3.0 - k, 2.0 * k + 1.0);
        let rect = match dir {
            Dir::Up => Rect::new(x + from, y + k, len, 1.0),
            Dir::Down => Rect::new(x + from, y + 3.0 - k, len, 1.0),
            Dir::Left => Rect::new(x + k, y + from, 1.0, len),
            Dir::Right => Rect::new(x + 3.0 - k, y + from, 1.0, len),
        };
        r.fill_rect(rect, color);
    }
}

/// XP's chevron: 9 wide and 6 deep, its arms 2 pixels thick, pointing `dir`, in the middle of `b`.
fn chevron(r: &mut Renderer, b: Rect, dir: Dir, color: Color) {
    let upright = matches!(dir, Dir::Up | Dir::Down);
    let (w, h) = if upright { (9.0, 6.0) } else { (6.0, 9.0) };
    let x = r.snap(b.x + ((b.w - w) / 2.0).round());
    let y = r.snap(b.y + ((b.h - h) / 2.0).round());
    // (row from the point, first column, length) of each run, for one pointing up.
    const RUNS: [(f32, f32, f32); 9] = [
        (0.0, 4.0, 1.0),
        (1.0, 3.0, 3.0),
        (2.0, 2.0, 5.0),
        (3.0, 1.0, 3.0),
        (3.0, 5.0, 3.0),
        (4.0, 0.0, 3.0),
        (4.0, 6.0, 3.0),
        (5.0, 0.0, 1.0),
        (5.0, 8.0, 1.0),
    ];
    for (row, col, len) in RUNS {
        let rect = match dir {
            Dir::Up => Rect::new(x + col, y + row, len, 1.0),
            Dir::Down => Rect::new(x + col, y + 5.0 - row, len, 1.0),
            Dir::Left => Rect::new(x + row, y + col, 1.0, len),
            Dir::Right => Rect::new(x + 5.0 - row, y + col, 1.0, len),
        };
        r.fill_rect(rect, color);
    }
}

/// The grip in the middle of a thumb: `n` short lines `len` long, each light over dark.
#[allow(clippy::too_many_arguments)]
fn grip_lines(r: &mut Renderer, t: Rect, upright: bool, light: Color, dark: Color, n: usize, len: f32) {
    let long = if upright { t.h } else { t.w };
    if long < 2.0 * n as f32 + 10.0 {
        return;
    }
    let (cx, cy) = (r.snap(t.x + t.w / 2.0), r.snap(t.y + t.h / 2.0));
    let start = -(n as f32) + 1.0;
    for k in 0..n {
        let at = start + k as f32 * 2.0;
        if upright {
            r.fill_rect(Rect::new(cx - len / 2.0, cy + at - 1.0, len, 1.0), light);
            r.fill_rect(Rect::new(cx - len / 2.0 + 1.0, cy + at, len, 1.0), dark);
        } else {
            r.fill_rect(Rect::new(cx + at - 1.0, cy - len / 2.0, 1.0, len), light);
            r.fill_rect(Rect::new(cx + at, cy - len / 2.0 + 1.0, 1.0, len), dark);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_arrow_repeats_after_400_ms_then_every_50() {
        let mut rep = Repeat::default();
        assert_eq!(rep.press(Hold::Arrow(Held::End), 10.0), REPEAT_FIRST_MS);
        assert!(rep.ticking);
        // Each tick of the timer takes a step and keeps it going at the quicker pace.
        for n in 1..=3 {
            let now = 10.0 + 0.4 + 0.05 * (n - 1) as f64;
            assert_eq!(rep.fire(true, now), (Hold::Arrow(Held::End), 1, Some(REPEAT_NEXT_MS)));
            assert_eq!(rep.repeats, n);
        }
        assert_eq!(REPEAT_FIRST_MS, 400);
        assert_eq!(REPEAT_NEXT_MS, 50);
    }

    #[test]
    fn a_late_timer_catches_up_so_the_pace_stays() {
        let mut rep = Repeat::default();
        rep.press(Hold::Arrow(Held::End), 0.0);
        assert_eq!(rep.fire(true, 0.401).1, 1);
        // A slow frame held the timer up for 200 ms: four steps were due meanwhile.
        assert_eq!(rep.fire(true, 0.601).1, 4);
        assert_eq!(rep.repeats, 5);
        // Never less than one step a tick, and never a big jump after a long stall.
        assert_eq!(rep.fire(true, 0.602).1, 1);
        assert_eq!(rep.fire(true, 9.0).1, REPEAT_CATCH_UP);
    }

    #[test]
    fn the_repeat_stops_at_the_end_but_stays_pressed_until_release() {
        let mut rep = Repeat::default();
        rep.press(Hold::Arrow(Held::Start), 0.0);
        assert_eq!(rep.fire(false, 0.4), (Hold::None, 0, None));
        assert!(!rep.ticking);
        // Nothing more happens, even with room again, until the next press...
        assert_eq!(rep.fire(true, 0.45), (Hold::None, 0, None));
        // ...and the arrow still shows pressed while the button is down.
        assert_eq!(rep.hold.arrow(), Held::Start);
        assert!(rep.release());
        assert_eq!(rep.hold, Hold::None);
        assert!(!rep.release());
    }

    #[test]
    fn releasing_stops_the_repeat() {
        let mut rep = Repeat::default();
        rep.press(Hold::Page(Held::End), 0.0);
        assert!(rep.release());
        assert_eq!(rep.fire(true, 0.4), (Hold::None, 0, None));
        // A held track pages without pressing an arrow.
        rep.press(Hold::Page(Held::End), 1.0);
        assert_eq!(rep.hold.arrow(), Held::None);
        assert_eq!(rep.hold.way(), Held::End);
    }

    #[test]
    fn room_greys_the_arrow_at_each_end() {
        assert_eq!(Room::of(0.0, 100.0), Room { start: false, end: true });
        assert_eq!(Room::of(100.0, 100.0), Room { start: true, end: false });
        assert_eq!(Room::of(40.0, 100.0), Room { start: true, end: true });
        assert!(!Room::of(0.0, 0.0).toward(Held::Start) && !Room::of(0.0, 0.0).toward(Held::End));
    }

    #[test]
    fn a_held_track_pages_until_the_thumb_reaches_the_mouse() {
        let lane = Rect::new(100.0, 0.0, 16.0, 300.0);
        let thumb = Rect::new(100.0, 120.0, 16.0, 40.0);
        assert!(page_on(lane, thumb, Held::End, Point::new(108.0, 200.0)));
        assert!(!page_on(lane, thumb, Held::End, Point::new(108.0, 140.0)));
        assert!(page_on(lane, thumb, Held::Start, Point::new(108.0, 50.0)));
        assert!(!page_on(lane, thumb, Held::Start, Point::new(108.0, 200.0)));
        // Off the bar it waits.
        assert!(!page_on(lane, thumb, Held::End, Point::new(40.0, 200.0)));
    }
}
