//! The main window's toolbar as an older Windows drew it, for a skin with an `era` (theme.rs
//! `Era`; toolbar.rs comes here for those): the buttons for the sidebar, Back, Forward, Up and
//! Refresh with each Windows' own pictures, the Address band (2000 and XP) or the breadcrumb box
//! (7), and SORT / VIEW / SETTINGS as that Windows' buttons.
//!
//! - 98 and 2000: flat toolbar buttons that rise (a thin raised edge) under the mouse and sink when
//!   pressed or switched on; Internet Explorer 5's pictures (in grey until the mouse is over them
//!   on 98); "Address", a sunken white box, a Go button.
//! - XP: the soft hover frame, the big green Back and Forward, the folder with the green arrow.
//! - 7: the blue round Back, Forward and Up, the breadcrumb box with Refresh joined to its right.
//!
//! The pictures are drawn here shape by shape on a small grid (`Pen` from eraicons.rs).

use crate::eraicons::{self as ei, c, ca, fill, g, oval, oval_g, poly, poly_g, stroke, DriveKind, Pen, Role};
use crate::render::{Align, Renderer, TextStyle};
use crate::retro::{self, Edge};
use crate::theme::{Color, Era, IconSet, Paint, Theme};
use crate::ui::{Point, Rect};

/// A toolbar button's job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Show or hide the sidebar (Explorer's "Folders").
    Folders,
    Back,
    Forward,
    Up,
    Refresh,
    /// The Address band's Go.
    Go,
}

/// A button's state.
#[derive(Clone, Copy, Debug, Default)]
pub struct Look {
    pub enabled: bool,
    pub hot: bool,
    pub pressed: bool,
    /// Switched on (Folders while the sidebar shows).
    pub checked: bool,
}

/// The XP colour schemes, which differ in a few frames and buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Blue,
    Silver,
    Royale,
}

/// Which XP scheme a skin is, judged by its title bar's words: black on Silver, white with a black
/// shadow on Royale, white with a blue shadow on the blue one.
fn scheme(theme: &Theme) -> Scheme {
    if theme.title_text.lightness() < 0.5 {
        Scheme::Silver
    } else if theme.title_shadow.a > 0.0 && theme.title_shadow.lightness() < 0.02 {
        Scheme::Royale
    } else {
        Scheme::Blue
    }
}

/// How a 98 / 2000 picture is coloured: its own colours, in greys (98 at rest), or all one colour
/// (the two layers of a disabled picture).
#[derive(Clone, Copy)]
enum Ink {
    Colour,
    Grey,
    Flat(Color),
}

impl Ink {
    fn of(self, hex: u32) -> Color {
        match self {
            Ink::Colour => c(hex),
            Ink::Grey => {
                let k = Color::hex(hex);
                let l = (0.30 * k.r + 0.59 * k.g + 0.11 * k.b).clamp(0.0, 1.0);
                Color { r: l, g: l, b: l, a: 1.0 }
            }
            Ink::Flat(color) => color,
        }
    }

    fn paint(self, stops: &[(f32, u32)]) -> Paint {
        match self {
            Ink::Colour => g(stops),
            _ => {
                let stops: Vec<crate::theme::Stop> = stops.iter().map(|&(at, hex)| crate::theme::Stop { at, color: self.of(hex) }).collect();
                Paint::from_stops(&stops)
            }
        }
    }
}

// ---------------------------------------------------------------- the nav buttons

/// One of the sidebar switch, Back, Forward, Up, Refresh (or Go) in the cell `b`.
pub fn nav_button(r: &mut Renderer, theme: &Theme, b: Rect, tool: Tool, look: Look) {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            let frame = b.center(28.0, 26.0);
            classic_frame(r, theme, frame, look, face);
            let shift = if look.pressed || look.checked { 1.0 } else { 0.0 };
            let at = b.center(20.0, 20.0);
            let at = Rect::new(at.x + shift, at.y + shift, at.w, at.h);
            classic_picture(r, theme, tool, at, look);
        }
        Era::Xp => {
            // Back and Folders carry their word after the picture when given the room (toolbar.rs).
            let word = match tool {
                Tool::Back if b.w > b.h + 8.0 => Some("Back"),
                Tool::Folders if b.w > b.h + 8.0 => Some("Folders"),
                _ => None,
            };
            if word.is_some() || !matches!(tool, Tool::Back | Tool::Forward) {
                xp_frame(r, theme, b.inset(1.0, 1.0), look);
            }
            let shift = if look.pressed { 1.0 } else { 0.0 };
            let at = if word.is_some() { Rect::new(b.x + 4.0, b.y + (b.h - 24.0) / 2.0, 24.0, 24.0) } else { b.center(24.0, 24.0) };
            xp_picture(r, tool, Rect::new(at.x + shift, at.y + shift, at.w, at.h), look);
            if let Some(word) = word {
                let ink = if look.enabled { theme.button_text } else { theme.disabled };
                let text = Rect::new(at.right() + 4.0 + shift, b.y + shift, (b.right() - at.right() - 4.0).max(0.0), b.h);
                r.text(word, text, &TextStyle::new(ink).align(Align::Left));
            }
        }
        Era::Win7 => match tool {
            Tool::Back | Tool::Forward | Tool::Up => seven_orb(r, b.center(24.0, 24.0), tool, look),
            _ => {
                if look.hot || look.pressed {
                    seven_box(r, b.center(28.0, 24.0), look.pressed);
                }
                seven_picture(r, tool, b.center(16.0, 16.0), look);
            }
        },
        Era::None => {}
    }
}

/// 98 / 2000's flat toolbar button: nothing at rest, a thin raised edge under the mouse, a thin
/// sunken one while pressed, and switched on, sunken over the light checker (plain face while the
/// mouse is over it).
fn classic_frame(r: &mut Renderer, theme: &Theme, frame: Rect, look: Look, face: Color) {
    if look.checked {
        if !look.hot {
            retro::dither(r, theme, frame.inset(1.0, 1.0), face);
        }
        retro::edge(r, theme, frame, Edge::ThinSunken, face);
    } else if look.pressed {
        retro::edge(r, theme, frame, Edge::ThinSunken, face);
    } else if look.hot {
        retro::edge(r, theme, frame, Edge::ThinRaised, face);
    }
}

/// XP's toolbar button frame: round cornered, a light gradient with a darker foot under the mouse,
/// pressed in when pressed, white when switched on.
pub fn xp_frame(r: &mut Renderer, theme: &Theme, rect: Rect, look: Look) {
    let s = scheme(theme);
    let (fill_paint, border) = if look.pressed {
        (g(&[(0.0, 0xdcdad1), (0.15, 0xe7e5e0), (0.5, 0xe2e1d9), (0.8, 0xdeddd4), (1.0, 0xebeae5)]), match s {
            Scheme::Blue => 0x9d9d92,
            Scheme::Silver => 0x8c8c96,
            Scheme::Royale => 0x85878c,
        })
    } else if look.checked {
        (g(&[(0.0, 0xffffff), (1.0, 0xf7f8f8)]), match s {
            Scheme::Royale => 0x7f93b5,
            _ => 0x8097a9,
        })
    } else if look.hot {
        (g(&[(0.0, 0xfefefd), (0.3, 0xf6f6f1), (0.5, 0xf4f4ee), (0.85, 0xf1f1ea), (0.9, 0xeeede4), (0.95, 0xe3e0d5), (1.0, 0xd7d2c6)]), match s {
            Scheme::Blue => 0xcecec3,
            Scheme::Silver => 0xb6b6be,
            Scheme::Royale => 0xa7a6aa,
        })
    } else {
        return;
    };
    r.fill_paint(rect, 3.0, &fill_paint);
    r.stroke_round_rect(rect.inset(0.5, 0.5), 2.5, 1.0, c(border));
}

/// Windows 7's command bar button under the mouse: a pale blue box with a step at its middle.
pub fn seven_box(r: &mut Renderer, rect: Rect, pressed: bool) {
    let paint = if pressed {
        g(&[(0.0, 0xc9d4e4), (0.12, 0xdde8f1), (0.15, 0xe1ebf5), (0.49, 0xd8e4f1), (0.5, 0xcfdbec), (1.0, 0xcfdbec)])
    } else {
        g(&[(0.0, 0xfdfeff), (0.49, 0xedf1fa), (0.5, 0xd6e4f4), (1.0, 0xc1d1e8)])
    };
    r.fill_paint(rect, 2.0, &paint);
    if !pressed {
        r.stroke_round_rect(rect.inset(1.5, 1.5), 1.5, 1.0, ca(0xf4f7fb, 0.9));
    }
    r.stroke_round_rect(rect.inset(0.5, 0.5), 2.0, 1.0, c(0xaabcd3));
}

/// Windows 7's breadcrumb button under the mouse (or pressed): bluer, with a darker blue edge.
pub fn seven_crumb_box(r: &mut Renderer, rect: Rect, pressed: bool) {
    let (paint, border) = if pressed {
        (g(&[(0.0, 0xe5f4fc), (0.49, 0xc4e5f6), (0.5, 0x98d1ef), (1.0, 0x68b3db)]), 0x2c628b)
    } else {
        (g(&[(0.0, 0xeaf6fd), (0.49, 0xd9f0fc), (0.5, 0xbee6fd), (1.0, 0xa7d9f5)]), 0x3c7fb1)
    };
    r.fill_paint(rect, 2.0, &paint);
    r.stroke_round_rect(rect.inset(0.5, 0.5), 2.0, 1.0, c(border));
}

/// 7's Refresh, joined to the right end of the breadcrumb box (`b` is its square, inside the box's
/// edge): a blue box under the mouse, the two blue arrows.
pub fn seven_refresh(r: &mut Renderer, b: Rect, look: Look) {
    if look.hot || look.pressed {
        seven_crumb_box(r, b, look.pressed);
    }
    seven_picture(r, Tool::Refresh, b.center(16.0, 16.0), look);
}

/// 7's soft round socket behind Back and Forward (`both` covers the two).
pub fn seven_socket(r: &mut Renderer, both: Rect) {
    let s = both.center(both.w + 2.0, 28.0);
    r.fill_round_rect(s, 14.0, ca(0xffffff, 0.19));
    r.stroke_round_rect(s.inset(0.5, 0.5), 13.5, 1.0, ca(0xffffff, 0.38));
}

// ---------------------------------------------------------------- 98 / 2000 pictures

/// Internet Explorer 5's toolbar picture for `tool` in the 20 DIP box `at`: in colour, in grey (98
/// at rest) or, while it can't be used, pressed into the face (a light copy one pixel down and
/// right, a shadow copy over it).
fn classic_picture(r: &mut Renderer, theme: &Theme, tool: Tool, at: Rect, look: Look) {
    r.set_aliased(true);
    if !look.enabled {
        let (hl, _, sh, _) = retro::bevel_colours(theme, theme.toolbar);
        classic_glyph(r, tool, Rect::new(at.x + 1.0, at.y + 1.0, at.w, at.h), Ink::Flat(hl), false);
        classic_glyph(r, tool, at, Ink::Flat(sh), false);
    } else {
        let lit = look.hot || look.pressed || look.checked;
        let ink = if theme.era == Era::Win98 && !lit { Ink::Grey } else { Ink::Colour };
        classic_glyph(r, tool, at, ink, look.hot);
    }
    r.set_aliased(false);
}

/// A yellow folder seen flat, `w` by `h` units with its top left at (x, y): tab, body, olive outline,
/// a black edge along the right and the bottom.
#[allow(clippy::too_many_arguments)]
fn classic_folder(r: &mut Renderer, p: &Pen, ink: Ink, x: f32, y: f32, w: f32, h: f32) {
    let olive = ink.of(0x666600);
    fill(r, p, x + 1.0, y, w * 0.4, 2.0, ink.of(0xffff99));
    fill(r, p, x, y + 2.0, w, h - 2.0, ink.of(0xffff99));
    poly(r, p, &[(x + w, y + 4.0), (x + w, y + h), (x + w * 0.4, y + h)], ink.of(0xffcc00));
    fill(r, p, x + 1.0, y + 3.0, w - 2.0, 1.0, ink.of(0xffffcc));
    ei::frame(r, p, x, y + 2.0, w, h - 2.0, olive);
    ei::hline(r, p, x + 1.0, y, w * 0.4 - 1.0, olive);
    ei::vline(r, p, x + 1.0, y, 2.0, olive);
    ei::vline(r, p, x + w * 0.4, y, 2.0, olive);
    ei::vline(r, p, x + w, y + 3.0, h - 2.0, ink.of(0x000000));
    ei::hline(r, p, x + 1.0, y + h, w, ink.of(0x000000));
}

fn classic_glyph(r: &mut Renderer, tool: Tool, at: Rect, ink: Ink, hot: bool) {
    let p = &Pen::over(r, at, 20.0);
    let black = ink.of(0x000000);
    match tool {
        Tool::Back | Tool::Forward => {
            let s = |x: f32| if tool == Tool::Back { x } else { 20.0 - x };
            let arrow: Vec<(f32, f32)> = [(2.0, 10.0), (8.0, 4.0), (8.0, 8.0), (17.0, 8.0), (17.0, 12.0), (8.0, 12.0), (8.0, 16.0)].iter().map(|&(x, y)| (s(x), y)).collect();
            poly_g(r, p, &arrow, &ink.paint(&[(0.0, 0xccffff), (0.4, 0x99ffff), (0.5, 0x33ccff), (1.0, 0x3399ff)]), (0.0, 4.0), (0.0, 16.0));
            ei::outline(r, p, &arrow, black);
            let blue = ink.of(0x3366ff);
            stroke(r, p, &[(s(2.0), 10.0), (s(8.0), 4.0)], 0.0, blue);
            stroke(r, p, &[(s(8.0), 8.0), (s(17.0), 8.0)], 0.0, blue);
        }
        Tool::Up => {
            classic_folder(r, p, ink, 1.0, 7.0, 17.0, 11.0);
            // The bent arrow: up out of the folder, the head at the top left.
            poly(r, p, &[(3.0, 7.0), (7.0, 2.0), (11.0, 7.0)], black);
            fill(r, p, 6.0, 7.0, 2.0, 7.0, black);
            fill(r, p, 6.0, 12.0, 9.0, 2.0, black);
        }
        Tool::Refresh => {
            // A white page with two green arrows chasing round.
            fill(r, p, 4.0, 1.0, 12.0, 18.0, ink.of(0xffffff));
            fill(r, p, 14.0, 2.0, 1.0, 16.0, ink.of(0xcccccc));
            ei::hline(r, p, 4.0, 1.0, 12.0, ink.of(0x868686));
            ei::vline(r, p, 4.0, 1.0, 18.0, ink.of(0x868686));
            ei::vline(r, p, 16.0, 1.0, 19.0, black);
            ei::hline(r, p, 4.0, 19.0, 12.5, black);
            let green = ink.of(0x339900);
            arc_arrow(r, p, Point::new(10.0, 10.0), 4.2, 200.0, 340.0, 1.8, green);
            arc_arrow(r, p, Point::new(10.0, 10.0), 4.2, 20.0, 160.0, 1.8, green);
        }
        Tool::Folders => {
            classic_folder(r, p, ink, 1.0, 2.0, 12.0, 9.0);
            classic_folder(r, p, ink, 5.0, 8.0, 13.0, 10.0);
        }
        Tool::Go => {
            // Internet Explorer 5's Go: a green arrow pointing right, lit along its top.
            let _ = hot;
            let arrow = [(2.0, 7.0), (10.0, 7.0), (10.0, 3.0), (18.0, 10.0), (10.0, 17.0), (10.0, 13.0), (2.0, 13.0)];
            poly_g(r, p, &arrow, &ink.paint(&[(0.0, 0x99ff99), (0.35, 0x33cc33), (0.7, 0x009900), (1.0, 0x006600)]), (0.0, 3.0), (0.0, 17.0));
            stroke(r, p, &[(3.0, 8.0), (10.5, 8.0)], 0.0, ink.of(0xccffcc));
            stroke(r, p, &[(11.0, 5.0), (16.0, 9.5)], 0.0, ink.of(0xccffcc));
            ei::outline(r, p, &arrow, ink.of(0x003300));
        }
    }
}

/// An arc round `centre` (units) of radius `rad` from `from` to `to` degrees (clockwise on screen,
/// 0 = right), `w` units thick, with an arrow head at its end.
#[allow(clippy::too_many_arguments)]
fn arc_arrow(r: &mut Renderer, p: &Pen, centre: Point, rad: f32, from: f32, to: f32, w: f32, color: Color) {
    let n = 12;
    let pts: Vec<(f32, f32)> = (0..=n)
        .map(|i| {
            let a = (from + (to - from) * i as f32 / n as f32).to_radians();
            (centre.x + rad * a.cos(), centre.y + rad * a.sin())
        })
        .collect();
    stroke(r, p, &pts[..n], w, color);
    // The head: a triangle at the end, pointing along the arc.
    let a = to.to_radians();
    let dir = if to > from { 1.0 } else { -1.0 };
    let (tx, ty) = (-a.sin() * dir, a.cos() * dir);
    let (ex, ey) = (centre.x + rad * a.cos(), centre.y + rad * a.sin());
    let (nx, ny) = (a.cos(), a.sin());
    let head = w * 1.6;
    poly(r, p, &[(ex + tx * head, ey + ty * head), (ex + nx * head - tx * 0.4, ey + ny * head - ty * 0.4), (ex - nx * head - tx * 0.4, ey - ny * head - ty * 0.4)], color);
}

// ---------------------------------------------------------------- XP pictures

/// XP's toolbar picture for `tool` in the 24 DIP box `at`.
fn xp_picture(r: &mut Renderer, tool: Tool, at: Rect, look: Look) {
    let p = &Pen::over(r, at, 24.0);
    match tool {
        Tool::Back | Tool::Forward => xp_orb(r, p, tool, look),
        Tool::Up => {
            ei::draw(r, IconSet::Xp, Role::Folder, Rect::new(at.x, at.y + 6.0, 18.0, 18.0));
            let arrow = [(8.0, 21.0), (8.5, 16.0), (10.5, 12.0), (13.0, 8.5), (11.0, 8.5), (15.5, 2.0), (20.0, 8.5), (17.5, 8.5), (15.0, 12.5), (13.0, 16.5), (12.5, 21.0)];
            let paint = if look.enabled { g(&[(0.0, 0x93d253), (0.5, 0x6eb632), (1.0, 0x518824)]) } else { g(&[(0.0, 0xd0d0d0), (1.0, 0xa0a0a0)]) };
            poly_g(r, p, &arrow, &paint, (8.0, 2.0), (20.0, 21.0));
            ei::outline(r, p, &arrow, c(if look.enabled { 0x3a6b12 } else { 0x8a8a8a }));
            stroke(r, p, &[(9.6, 20.0), (10.0, 16.0), (12.0, 12.0)], 0.0, ca(0xffffff, 0.7));
        }
        Tool::Refresh => {
            let page = [(4.0, 1.0), (16.0, 1.0), (20.0, 5.0), (20.0, 23.0), (4.0, 23.0)];
            poly_g(r, p, &page, &g(&[(0.0, 0xf1f8ff), (1.0, 0xcbe0ff)]), (4.0, 1.0), (20.0, 23.0));
            ei::outline(r, p, &page, c(0x8d9cc2));
            ei::vline(r, p, 19.6, 5.0, 18.0, c(0x58588b));
            let green = c(0x4e9317);
            arc_arrow(r, p, Point::new(12.0, 13.0), 5.0, 200.0, 340.0, 2.2, green);
            arc_arrow(r, p, Point::new(12.0, 13.0), 5.0, 20.0, 160.0, 2.2, green);
            arc_arrow(r, p, Point::new(12.0, 12.6), 5.0, 205.0, 335.0, 1.0, c(0x77ac56));
        }
        Tool::Folders => {
            ei::draw(r, IconSet::Xp, Role::Folder, Rect::new(at.x + 6.0, at.y + 1.0, 17.0, 17.0));
            ei::draw(r, IconSet::Xp, Role::Folder, Rect::new(at.x + 1.0, at.y + 6.0, 17.0, 17.0));
        }
        Tool::Go => xp_go(r, p),
    }
}

/// XP's big round Back or Forward: a green glass ball with a white arrow (grey while it can't be
/// used).
fn xp_orb(r: &mut Renderer, p: &Pen, tool: Tool, look: Look) {
    let (ring, body) = if look.enabled {
        (g(&[(0.0, 0x2c9a16), (1.0, 0x0b5e05)]), g(&[(0.0, 0xa9e38a), (0.3, 0x78cc48), (0.6, 0x43b800), (1.0, 0x22a80c)]))
    } else {
        (g(&[(0.0, 0x9d9d9d), (1.0, 0x7a7a7a)]), g(&[(0.0, 0xe8e8e8), (1.0, 0xb4b4b4)]))
    };
    let body = if look.pressed { body.darker(0.12) } else if look.hot { body.lighter(0.12) } else { body };
    oval_g(r, p, 12.0, 12.0, 11.0, 11.0, &ring, (12.0, 1.0), (12.0, 23.0));
    oval_g(r, p, 12.0, 12.0, 10.0, 10.0, &body, (12.0, 2.0), (12.0, 22.0));
    oval_g(r, p, 12.0, 7.5, 8.0, 5.0, &Paint::even(&[ca(0xffffff, 0.55), ca(0xffffff, 0.0)]), (12.0, 2.5), (12.0, 12.5));
    let s = |x: f32| if tool == Tool::Back { x } else { 24.0 - x };
    let arrow: Vec<(f32, f32)> = [(5.0, 12.0), (11.0, 5.5), (11.0, 10.0), (19.0, 10.0), (19.0, 14.0), (11.0, 14.0), (11.0, 18.5)].iter().map(|&(x, y)| (s(x), y)).collect();
    let shadow: Vec<(f32, f32)> = arrow.iter().map(|&(x, y)| (x + 0.8, y + 0.8)).collect();
    poly(r, p, &shadow, ca(if look.enabled { 0x2e7a20 } else { 0x7a7a7a }, 0.5));
    poly(r, p, &arrow, c(0xffffff));
}

/// XP's Go: a green rounded square with a white arrow pointing right.
fn xp_go(r: &mut Renderer, p: &Pen) {
    ei::fill_g(r, p, 2.0, 2.0, 20.0, 20.0, 2.0, &g(&[(0.0, 0x5cc46a), (0.45, 0x37a13d), (0.85, 0x149b15), (1.0, 0x0f6e10)]), (2.0, 2.0), (2.0, 22.0));
    ei::fill_g(r, p, 3.0, 3.0, 18.0, 8.0, 2.0, &Paint::even(&[ca(0xffffff, 0.35), ca(0xffffff, 0.05)]), (3.0, 3.0), (3.0, 11.0));
    r.stroke_round_rect(p.rect(2.0, 2.0, 20.0, 20.0).inset(0.5, 0.5), 2.0 * p.u, 1.0, c(0x0f6e10));
    poly(r, p, &[(5.0, 10.0), (12.0, 10.0), (12.0, 5.0), (19.0, 12.0), (12.0, 19.0), (12.0, 14.0), (5.0, 14.0)], c(0xffffff));
}

// ---------------------------------------------------------------- 7 pictures

/// Windows 7's round blue Back, Forward or Up in the 24 DIP box `at`: a glass ball with a white
/// arrow; brighter with a glow under the mouse, deeper pressed, grey-white while it can't be used.
fn seven_orb(r: &mut Renderer, at: Rect, tool: Tool, look: Look) {
    let p = &Pen::over(r, at, 24.0);
    let (ring, body, outline) = if !look.enabled {
        (0x9aa7b6, g(&[(0.0, 0xf7f9fb), (0.49, 0xe1e6ec), (0.5, 0xced6df), (1.0, 0xe4e9ef)]), 0xa5b1bf)
    } else if look.pressed {
        (0x1b3d73, g(&[(0.0, 0x6fa3d9), (0.45, 0x2e6fbf), (0.5, 0x0a3d8c), (0.6, 0x0a3d8c), (0.85, 0x135fb8), (1.0, 0x2a9ad8)]), 0x0d3c82)
    } else if look.hot {
        (0x1b3d73, g(&[(0.0, 0xa9d4fb), (0.45, 0x5fa6ee), (0.5, 0x1863c6), (0.6, 0x1863c6), (0.85, 0x2b8de6), (1.0, 0x55d2ff)]), 0x0d3c82)
    } else {
        (0x1b3d73, g(&[(0.0, 0x8ec3f5), (0.45, 0x4d93e0), (0.5, 0x0e4fb0), (0.6, 0x0a49a6), (0.85, 0x1e78d8), (1.0, 0x3ab4f2)]), 0x0d3c82)
    };
    if look.hot && look.enabled && !look.pressed {
        oval(r, p, 12.0, 12.0, 12.6, 12.6, ca(0x9ad8ff, 0.5));
    }
    oval(r, p, 12.0, 12.0, 11.5, 11.5, c(ring));
    oval_g(r, p, 12.0, 12.0, 10.5, 10.5, &body, (12.0, 1.5), (12.0, 22.5));
    let gloss = if look.pressed { 0.44 } else { 0.69 };
    oval_g(r, p, 12.0, 7.0, 8.5, 5.2, &Paint::even(&[ca(0xffffff, gloss), ca(0xffffff, 0.12)]), (12.0, 2.0), (12.0, 12.0));
    // The arrow, 12 by 10: a head 5 deep and a shaft 4 thick, drawn pointing left then turned.
    let left = [(6.0, 12.0), (11.0, 7.0), (11.0, 10.0), (18.0, 10.0), (18.0, 14.0), (11.0, 14.0), (11.0, 17.0)];
    let turn = |&(x, y): &(f32, f32)| match tool {
        Tool::Forward => (24.0 - x, y),
        Tool::Up => (y, x),
        _ => (x, y),
    };
    let arrow: Vec<(f32, f32)> = left.iter().map(turn).collect();
    let shadow: Vec<(f32, f32)> = arrow.iter().map(|&(x, y)| (x + 0.7, y + 0.7)).collect();
    poly(r, p, &shadow, ca(outline, 0.5));
    poly_g(r, p, &arrow, &g(&[(0.0, 0xffffff), (1.0, 0xd9dee6)]), (12.0, 6.0), (12.0, 18.0));
}

/// Windows 7's small pictures (16 DIPs): the navigation pane layout, Refresh, Go.
fn seven_picture(r: &mut Renderer, tool: Tool, at: Rect, look: Look) {
    let p = &Pen::over(r, at, 16.0);
    match tool {
        Tool::Folders => {
            fill(r, p, 0.0, 1.0, 16.0, 14.0, c(0xffffff));
            if look.checked {
                ei::fill_g(r, p, 1.0, 2.0, 5.0, 12.0, 0.0, &g(&[(0.0, 0xbee0f7), (1.0, 0x86c3ec)]), (1.0, 2.0), (1.0, 14.0));
            }
            ei::hline(r, p, 1.0, 2.0, 14.0, ca(0xffffff, 0.9));
            ei::frame(r, p, 0.0, 1.0, 16.0, 14.0, c(0x5a6e86));
            ei::vline(r, p, 6.0, 1.0, 14.0, c(0x5a6e86));
        }
        Tool::Refresh => {
            let (dark, light) = if look.enabled { (c(0x1a5fb8), c(0x5fb2f0)) } else { (c(0x9aa7b6), c(0xc8d0da)) };
            arc_arrow(r, p, Point::new(8.0, 8.0), 5.0, 330.0, 195.0, 2.2, dark);
            arc_arrow(r, p, Point::new(8.0, 8.0), 5.0, 150.0, 15.0, 2.2, dark);
            arc_arrow(r, p, Point::new(8.0, 7.7), 5.0, 325.0, 205.0, 0.9, light);
            arc_arrow(r, p, Point::new(8.0, 7.7), 5.0, 145.0, 25.0, 0.9, light);
        }
        Tool::Go | Tool::Back | Tool::Forward | Tool::Up => {
            let arrow = [(2.0, 6.5), (8.5, 6.5), (8.5, 3.0), (14.0, 8.0), (8.5, 13.0), (8.5, 9.5), (2.0, 9.5)];
            poly_g(r, p, &arrow, &g(&[(0.0, 0x4da3ec), (1.0, 0x1658b4)]), (8.0, 3.0), (8.0, 13.0));
            ei::outline(r, p, &arrow, c(0x0e3c7e));
        }
    }
}

// ---------------------------------------------------------------- the band and its boxes

/// The toolbar's band behind everything in `rect`: 98 / 2000's grey with etched lines along the top
/// and bottom and a grip at the start; XP's band lighter on the left than the right; 7's glass,
/// running on from the title bar (`glass` is the whole glass area, title bar and toolbar).
pub fn band(r: &mut Renderer, theme: &Theme, rect: Rect, glass: Rect, active: bool) {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            r.fill_rect(rect, theme.toolbar);
            retro::etched_line(r, theme, rect.x, rect.y, rect.w, theme.toolbar);
            retro::etched_line(r, theme, rect.x, rect.bottom() - 2.0, rect.w, theme.toolbar);
            // The band's grip.
            retro::edge(r, theme, Rect::new(rect.x + 2.0, rect.y + 6.0, 3.0, rect.h - 12.0), Edge::ThinRaised, theme.toolbar);
        }
        Era::Xp => {
            r.fill_paint_across(rect, &theme.toolbar_paint);
            let (dark, light) = match scheme(theme) {
                Scheme::Blue => (0xd4d1c8, 0xfffff6),
                Scheme::Silver => (0xc6c5cb, 0xffffff),
                Scheme::Royale => (0xd4d0d7, 0xffffff),
            };
            r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 1.0), c(dark));
            r.fill_rect(Rect::new(rect.x, rect.bottom() - 1.0, rect.w, 1.0), c(light));
        }
        Era::Win7 => {
            retro::glass(r, theme, glass, rect, active);
            // The client area's dark edge under the glass.
            r.fill_rect(Rect::new(rect.x, rect.bottom() - 1.0, rect.w, 1.0), c(0x8ea2bb));
        }
        Era::None => {}
    }
}

/// A text box on the toolbar (the address, the filter): 98 / 2000's sunken white box, XP's thin
/// blue edge, 7's glass box with its dark top edge (brighter under the mouse).
pub fn field(r: &mut Renderer, theme: &Theme, rect: Rect, hot: bool) {
    let paint = theme.input_paint;
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            r.fill_paint(rect, 0.0, &paint);
            retro::edge(r, theme, rect, Edge::Sunken, theme.toolbar);
        }
        Era::Xp => {
            r.fill_paint(rect, 0.0, &paint);
            r.stroke_rect(rect.inset(0.5, 0.5), 1.0, c(0x7f9db9));
        }
        Era::Win7 => {
            r.fill_paint(rect, 1.0, &paint);
            if hot {
                r.fill_rect(rect.inset(1.0, 1.0), ca(0xffffff, 0.6));
            }
            let x = rect.inset(0.5, 0.5);
            r.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1.0), if hot { c(0x3d7bad) } else { ca(0x555b62, 0.8) });
            r.fill_rect(Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w - 2.0, 1.0), c(0xfbfdff));
            r.fill_rect(Rect::new(rect.x, x.y, 1.0, rect.h - 1.0), c(0x8e9baa));
            r.fill_rect(Rect::new(rect.right() - 1.0, x.y, 1.0, rect.h - 1.0), c(0x8e9baa));
            r.fill_rect(Rect::new(rect.x + 1.0, rect.bottom() - 1.0, rect.w - 2.0, 1.0), c(0xaab7c4));
        }
        Era::None => {}
    }
}

/// The drop arrow at the right end of a box (`rect` is the box; the arrow's own rectangle comes
/// back): 98 / 2000's raised grey button, XP's pale blue tile, 7's plain arrow behind a divider.
pub fn drop_arrow(r: &mut Renderer, theme: &Theme, rect: Rect, hot: bool) -> Rect {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let b = Rect::new(rect.right() - 2.0 - 16.0, rect.y + 2.0, 16.0, rect.h - 4.0);
            r.fill_rect(b, theme.toolbar);
            retro::edge(r, theme, b, Edge::RaisedWindow, theme.toolbar);
            triangle_down(r, Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0), 7.0, Color::hex(0));
            b
        }
        Era::Xp => {
            let b = Rect::new(rect.right() - 2.0 - 15.0, rect.y + 2.0, 15.0, rect.h - 4.0);
            let (paint, glyph) = match scheme(theme) {
                Scheme::Blue if hot => (g(&[(0.0, 0xdcedfd), (1.0, 0xc4dafd)]), 0x4d6185),
                Scheme::Blue => (g(&[(0.0, 0xcedbfd), (0.5, 0xc8d6fb), (0.6, 0xbbcefd), (1.0, 0xb9cffb)]), 0x4d6185),
                Scheme::Silver => (g(&[(0.0, 0xf1f1f1), (1.0, 0xd1d1d1)]), 0x696969),
                Scheme::Royale => (g(&[(0.0, 0xffffff), (1.0, 0xbcd0e8)]), 0x5b6473),
            };
            r.fill_paint(b, 1.5, &paint);
            r.stroke_round_rect(b.inset(0.5, 0.5), 1.5, 1.0, c(0xbaccf4).mix(c(0xffffff), 0.2));
            let m = Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0 + 0.5);
            r.polyline(&[Point::new(m.x - 3.5, m.y - 2.0), Point::new(m.x, m.y + 1.5), Point::new(m.x + 3.5, m.y - 2.0)], 1.8, c(glyph));
            b
        }
        Era::Win7 => {
            let b = Rect::new(rect.right() - 17.0, rect.y + 1.0, 17.0, rect.h - 2.0);
            if hot {
                seven_crumb_box(r, b, false);
            } else {
                r.fill_rect(Rect::new(b.x, b.y + 1.0, 1.0, b.h - 2.0), c(0xa8b3bf));
            }
            triangle_down(r, Point::new(b.x + b.w / 2.0 + 0.5, b.y + b.h / 2.0), 7.0, Color::hex(0));
            b
        }
        Era::None => rect,
    }
}

/// A solid triangle pointing down, `w` wide (its height half that, rounded up), centred on `m`.
pub fn triangle_down(r: &mut Renderer, m: Point, w: f32, color: Color) {
    let h = (w / 2.0).ceil();
    let (x, y) = (r.snap(m.x - w / 2.0), r.snap(m.y - h / 2.0));
    r.fill_polygon(&[Point::new(x, y), Point::new(x + w, y), Point::new(x + w / 2.0, y + h)], color);
}

/// A solid triangle pointing right, `h` tall, centred on `m` (7's breadcrumb chevrons).
pub fn triangle_right(r: &mut Renderer, m: Point, h: f32, color: Color) {
    let w = (h / 2.0).ceil();
    let (x, y) = (r.snap(m.x - w / 2.0), r.snap(m.y - h / 2.0));
    r.fill_polygon(&[Point::new(x, y), Point::new(x + w, y + h / 2.0), Point::new(x, y + h)], color);
}

/// The word "Address" before the address box, in the band's text colour (XP: its grey).
pub fn address_label(r: &mut Renderer, theme: &Theme, rect: Rect) {
    let color = if theme.era == Era::Xp { c(0x716f64) } else { theme.text };
    r.text("Address", Rect::new(rect.x + 4.0, rect.y, rect.w - 4.0, rect.h), &TextStyle::new(color));
}

/// The Address band's Go button: the green or white arrow and the word "Go", flat until the mouse
/// is over it.
pub fn go_button(r: &mut Renderer, theme: &Theme, b: Rect, look: Look) {
    let shift = if look.pressed { 1.0 } else { 0.0 };
    let pic = Rect::new(b.x + 4.0 + shift, b.y + (b.h - 16.0) / 2.0 + shift, 16.0, 16.0);
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            classic_frame(r, theme, b, look, theme.toolbar);
            r.set_aliased(true);
            classic_glyph(r, Tool::Go, pic.inset(-2.0, -2.0), if theme.era == Era::Win98 && !look.hot { Ink::Grey } else { Ink::Colour }, look.hot);
            r.set_aliased(false);
        }
        Era::Xp => {
            xp_frame(r, theme, b, look);
            let p = &Pen::over(r, pic.inset(-1.0, -1.0), 24.0);
            xp_go(r, p);
        }
        _ => {}
    }
    let text = Rect::new(pic.right() + 3.0, b.y + shift, (b.right() - pic.right() - 3.0).max(0.0), b.h);
    r.text("Go", text, &TextStyle::new(theme.text));
}

/// SORT, VIEW or SETTINGS as the era's button, its word in title case ("Sort"): 98 / 2000's raised
/// push button; XP's flat toolbar button with a picture; 7's command bar word. `menu`: it opens a
/// menu (a small down arrow after the word).
pub fn word_button(r: &mut Renderer, theme: &Theme, b: Rect, word: &str, hot: bool, menu: bool) {
    let label = title_case(word);
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            let b = b.inset(1.0, 1.0);
            r.fill_rect(b, face);
            retro::edge(r, theme, b, Edge::Raised, face);
            // Its small picture, in colour (98: grey until the mouse is over it, as its toolbars).
            let pic = Rect::new(b.x + 5.0, b.y + (b.h - 16.0) / 2.0, 16.0, 16.0);
            let ink = if theme.era == Era::Win98 && !hot { Ink::Grey } else { Ink::Colour };
            r.set_aliased(true);
            classic_word_picture(r, word, pic, ink);
            r.set_aliased(false);
            let rest = Rect::new(pic.right(), b.y, b.right() - pic.right(), b.h);
            text_and_arrow(r, rest, &label, theme.button_text, menu, 5.0, 0.0);
        }
        Era::Xp => {
            let b = b.inset(1.0, 1.0);
            xp_frame(r, theme, b, Look { enabled: true, hot, ..Look::default() });
            let pic = Rect::new(b.x + 6.0, b.y + (b.h - 16.0) / 2.0, 16.0, 16.0);
            xp_word_picture(r, word, pic);
            let rest = Rect::new(pic.right(), b.y, b.right() - pic.right(), b.h);
            text_and_arrow(r, rest, &label, theme.button_text, menu, 5.0, 0.0);
        }
        Era::Win7 => {
            let b = b.center(b.w - 4.0, 24.0);
            if hot {
                seven_box(r, b, false);
            }
            text_and_arrow(r, b, &label, c(0x1e395b), menu, 7.0, 0.0);
        }
        Era::None => {}
    }
}

/// `label` centred in `b` with, when `arrow`, a solid down triangle `w` wide 5 DIPs after it.
fn text_and_arrow(r: &mut Renderer, b: Rect, label: &str, color: Color, arrow: bool, w: f32, dy: f32) {
    let style = TextStyle::new(color);
    let tw = r.measure(label, &style).ceil();
    let total = tw + if arrow { 5.0 + w } else { 0.0 };
    let x = (b.x + (b.w - total) / 2.0).round();
    r.text(label, Rect::new(x, b.y + dy, tw + 2.0, b.h), &style.align(Align::Left));
    if arrow {
        triangle_down(r, Point::new(x + tw + 5.0 + w / 2.0, b.y + b.h / 2.0 + 1.0 + dy), w, color);
    }
}

/// 98 / 2000's small pictures for Sort (bars getting shorter, an arrow down), View (2000's Views:
/// a window of little icons) and Settings (Folder Options: a folder with a ticked sheet), in their
/// sixteen colours.
fn classic_word_picture(r: &mut Renderer, word: &str, at: Rect, ink: Ink) {
    let p = &Pen::over(r, at, 16.0);
    let black = ink.of(0x000000);
    match word.to_ascii_lowercase().as_str() {
        "sort" => {
            for (k, w) in [10.0, 7.0, 4.0].iter().enumerate() {
                fill(r, p, 1.0, 3.0 + k as f32 * 4.0, *w, 2.0, ink.of(0x000080));
            }
            fill(r, p, 12.0, 1.0, 2.0, 9.0, black);
            poly(r, p, &[(9.5, 9.0), (16.5, 9.0), (13.0, 14.5)], black);
        }
        "view" => {
            fill(r, p, 1.0, 1.0, 14.0, 14.0, ink.of(0xffffff));
            fill(r, p, 1.0, 1.0, 14.0, 3.0, ink.of(0x000080));
            // Four little icons: two folders and two pages.
            for (x, y, folder) in [(3.0, 6.0, true), (9.0, 6.0, false), (3.0, 11.0, false), (9.0, 11.0, true)] {
                if folder {
                    fill(r, p, x, y, 4.0, 3.0, ink.of(0xffff00));
                    ei::frame(r, p, x, y, 4.0, 3.0, ink.of(0x808000));
                } else {
                    fill(r, p, x + 0.5, y - 0.5, 3.0, 4.0, ink.of(0xffffff));
                    ei::frame(r, p, x + 0.5, y - 0.5, 3.0, 4.0, ink.of(0x0000ff));
                }
            }
            ei::frame(r, p, 1.0, 1.0, 14.0, 14.0, ink.of(0x808080));
            ei::vline(r, p, 15.0, 1.0, 15.0, black);
            ei::hline(r, p, 1.0, 15.0, 15.0, black);
        }
        _ => {
            classic_folder(r, p, ink, 0.0, 5.0, 11.0, 10.0);
            fill(r, p, 8.0, 1.0, 7.0, 9.0, ink.of(0xffffff));
            ei::frame(r, p, 8.0, 1.0, 7.0, 9.0, black);
            stroke(r, p, &[(9.5, 5.5), (11.0, 7.5), (14.0, 2.5)], 1.4, ink.of(0x008000));
        }
    }
}

/// XP's small pictures for Sort (bars getting shorter with an arrow), View (a little window with
/// rows) and Settings (a folder with a ticked sheet).
fn xp_word_picture(r: &mut Renderer, word: &str, at: Rect) {
    let p = &Pen::over(r, at, 16.0);
    match word.to_ascii_lowercase().as_str() {
        "sort" => {
            for (k, w) in [11.0, 8.0, 5.0].iter().enumerate() {
                fill(r, p, 1.0, 3.0 + k as f32 * 4.0, *w, 2.0, c(0x4a6fbf));
            }
            fill(r, p, 12.5, 2.0, 2.0, 8.0, c(0x2150bc));
            poly(r, p, &[(10.5, 9.0), (16.0, 9.0), (13.5, 14.0)], c(0x2150bc));
        }
        "view" => {
            fill(r, p, 1.0, 2.0, 14.0, 12.0, c(0xffffff));
            ei::fill_g(r, p, 1.0, 2.0, 14.0, 3.0, 0.0, &g(&[(0.0, 0x3c69c9), (1.0, 0x1148bc)]), (1.0, 2.0), (1.0, 5.0));
            for k in 0..3 {
                let y = 6.5 + k as f32 * 2.5;
                fill(r, p, 3.0, y, 1.5, 1.5, c(0x3a7bd5));
                fill(r, p, 5.5, y + 0.3, 7.5, 1.0, c(0xb9b7b2));
            }
            ei::frame(r, p, 1.0, 2.0, 14.0, 12.0, c(0x2150bc));
        }
        _ => {
            ei::draw(r, IconSet::Xp, Role::Folder, at);
            fill(r, p, 8.0, 1.0, 7.0, 8.0, c(0xffffff));
            ei::frame(r, p, 8.0, 1.0, 7.0, 8.0, c(0x8a9bc6));
            stroke(r, p, &[(9.5, 5.0), (11.0, 6.8), (14.0, 2.5)], 1.4, c(0x2150bc));
        }
    }
}

/// "SORT" -> "Sort".
fn title_case(word: &str) -> String {
    let lower = word.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |f| f.to_uppercase().chain(chars).collect())
}

/// The filter box's magnifier in the era's style, 16 DIPs, centred on `m`.
pub fn magnifier(r: &mut Renderer, theme: &Theme, m: Point, active: bool) {
    let at = Rect::new(m.x - 8.0, m.y - 8.0, 16.0, 16.0);
    let p = &Pen::over(r, at, 16.0);
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            r.set_aliased(true);
            // A round lens with a grey rim and a red handle going down to the right.
            poly(r, p, &[(9.0, 10.0), (11.0, 8.0), (16.0, 13.0), (14.0, 15.0)], c(0xcc0000));
            ei::outline(r, p, &[(9.0, 10.0), (11.0, 8.0), (16.0, 13.0), (14.0, 15.0)], c(0x000000));
            oval(r, p, 6.5, 6.5, 5.5, 5.5, c(0x868686));
            oval_g(r, p, 6.5, 6.5, 4.5, 4.5, &g(&[(0.0, 0xffffff), (0.5, 0xccffff), (1.0, 0x33ccff)]), (3.0, 3.0), (10.0, 10.0));
            r.set_aliased(false);
        }
        Era::Xp => {
            poly_g(r, p, &[(8.5, 10.0), (10.0, 8.5), (15.5, 14.0), (14.0, 15.5)], &g(&[(0.0, 0xc87137), (1.0, 0x7a3a12)]), (9.0, 9.0), (15.0, 15.0));
            oval_g(r, p, 6.5, 6.5, 5.5, 5.5, &g(&[(0.0, 0x8892bb), (1.0, 0x3e4580)]), (2.0, 2.0), (11.0, 11.0));
            oval_g(r, p, 6.5, 6.5, 4.2, 4.2, &g(&[(0.0, 0xfaffff), (1.0, 0xc4e4fc)]), (3.0, 3.0), (10.0, 10.0));
            oval(r, p, 5.0, 5.0, 1.5, 1.2, ca(0xffffff, 0.9));
        }
        _ => {
            // 7: a blue ring with a pale lens and a dark handle going down to the left.
            let blue = if active { 0x1a4f9c } else { 0x3d8fda };
            stroke(r, p, &[(6.0, 10.0), (2.0, 14.0)], 2.0, c(0x1a3f78));
            oval_g(r, p, 9.5, 6.5, 5.0, 5.0, &g(&[(0.0, 0x3d8fda), (1.0, blue)]), (5.0, 2.0), (14.0, 11.0));
            oval(r, p, 9.5, 6.5, 3.2, 3.2, c(0xe8f3fd));
            oval(r, p, 8.3, 5.3, 1.0, 1.0, c(0xffffff));
        }
    }
}

/// The address box's picture of the place (the folder, a drive, This PC...), 16 DIPs at `at`.
pub fn place_icon(r: &mut Renderer, theme: &Theme, path: &str, at: Rect) {
    let set = if theme.icons == IconSet::Windows {
        match theme.era {
            Era::Win98 | Era::Win2000 => IconSet::Classic,
            Era::Xp => IconSet::Xp,
            _ => IconSet::Aero,
        }
    } else {
        theme.icons
    };
    let role = ei::role_of_path(path);
    // A drive shows as the drive it is, not as a folder.
    let role = match role {
        Role::Drive(_) if path.trim_end_matches('\\').len() > 2 => Role::Drive(DriveKind::Fixed),
        // 98, 2000 and XP showed the folder you are in as an open one.
        Role::Folder if set != IconSet::Aero => Role::FolderOpen,
        other => other,
    };
    ei::draw(r, set, role, at);
}
