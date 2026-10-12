//! The file list's column titles, Glint's Home page and the status bar as an older Windows drew
//! them, for a skin with an `era` (theme.rs `Era`):
//!
//! - column titles: 98 / 2000's raised grey buttons, XP's tray with its orange line under the
//!   mouse, 7's white titles with the light blue sorted one;
//! - Home: the fold box (2000's tree box, XP's round task pane button, 7's small arrow), the
//!   section titles with their rule, Recent and Favorites as the era's buttons or tabs;
//! - the status bar: 2000's sunken panes and grip, XP's, 7's details pane strip.

use crate::eraicons::{c, ca, g, Pen};
use crate::render::{Align, Renderer, TextStyle};
use crate::retro::{self, Edge};
use crate::retrotool;
use crate::theme::{Color, Era, Paint, Theme};
use crate::ui::{Point, Rect};

/// The XP scheme's colours for column titles: (fill, the three bottom rows, divider).
fn xp_header(theme: &Theme) -> (u32, [u32; 3], u32) {
    if theme.title_text.lightness() < 0.5 {
        (0xf3f3f7, [0xe5e5ea, 0xd9d9df, 0xcdcdd3], 0xc5c5cb)
    } else if theme.title_shadow.a > 0.0 && theme.title_shadow.lightness() < 0.02 {
        (0xfcfcfc, [0xf0f0f0, 0xe2e2e6, 0xd0d0d6], 0xd0cfd4)
    } else {
        (0xebeadb, [0xe2decd, 0xd6d2c2, 0xcbc7b8], 0xc7c5b2)
    }
}

/// The band behind the column titles (`rect` is the whole row).
pub fn header_band(r: &mut Renderer, theme: &Theme, rect: Rect) {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            r.fill_rect(rect, theme.list_header.base());
            // The empty raised item that fills to the right edge.
            retro::edge(r, theme, rect, Edge::Raised, theme.list_header.base());
        }
        Era::Xp => {
            let (fill, foot, _) = xp_header(theme);
            r.fill_rect(rect, c(fill));
            r.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1.0), c(0xfcfbf8));
            for (k, col) in foot.iter().enumerate() {
                r.fill_rect(Rect::new(rect.x, rect.bottom() - 3.0 + k as f32, rect.w, 1.0), c(*col));
            }
        }
        Era::Win7 => {
            r.fill_paint_down(rect, 0.0, &g(&[(0.0, 0xffffff), (0.4, 0xffffff), (0.42, 0xf7f8fa), (1.0, 0xf1f2f4)]), rect.y, rect.bottom() - 1.0);
            r.fill_rect(Rect::new(rect.x, rect.bottom() - 1.0, rect.w, 1.0), c(0xd5d5d5));
        }
        Era::None => {}
    }
}

/// One column title in `cell`: its frame for the state, `label` in `text` (`right`: numbers,
/// aligned right), and `sort`: Some(descending) on the column the list is sorted by.
#[allow(clippy::too_many_arguments)]
pub fn header_item(r: &mut Renderer, theme: &Theme, cell: Rect, text: Rect, label: &str, right: bool, sort: Option<bool>, hot: bool) {
    let style = TextStyle::new(match theme.era {
        Era::Win7 => c(0x4c607a),
        _ => theme.list_header_text,
    })
    .align(if right { Align::Right } else { Align::Left });
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.list_header.base();
            r.fill_rect(cell, face);
            retro::edge(r, theme, cell, Edge::Raised, face);
        }
        Era::Xp => {
            let (_, _, divider) = xp_header(theme);
            if hot {
                r.fill_rect(Rect::new(cell.x, cell.y + 1.0, cell.w, cell.h - 4.0), c(0xfaf9f4));
                for (k, col) in [0xf8a900, 0xf6c456, 0xf8b31f].iter().enumerate() {
                    r.fill_rect(Rect::new(cell.x, cell.bottom() - 3.0 + k as f32, cell.w, 1.0), c(*col));
                }
            }
            let x = (cell.right() - 2.0).round();
            r.fill_rect(Rect::new(x, cell.y + 4.0, 1.0, cell.h - 8.0), c(divider));
            r.fill_rect(Rect::new(x + 1.0, cell.y + 4.0, 1.0, cell.h - 8.0), c(0xffffff));
        }
        Era::Win7 => {
            if sort.is_some() || hot {
                let paint = if hot {
                    g(&[(0.0, 0xf3fbfe), (0.4, 0xf3fbfe), (0.42, 0xe3f4fc), (1.0, 0xd7effa)])
                } else {
                    g(&[(0.0, 0xf2f9fc), (0.4, 0xf2f9fc), (0.42, 0xe0f1f9), (1.0, 0xd8ecf6)])
                };
                r.fill_paint_down(Rect::new(cell.x, cell.y, cell.w, cell.h - 1.0), 0.0, &paint, cell.y, cell.bottom() - 1.0);
                r.fill_rect(Rect::new(cell.x, cell.bottom() - 1.0, cell.w, 1.0), c(0x96d9f9));
            }
            let x = (cell.right() - 1.0).round();
            r.fill_rect(Rect::new(x, cell.y, 1.0, cell.h - 1.0), c(if hot { 0x88cbeb } else { 0xe7e8ea }));
        }
        Era::None => {}
    }
    r.text(label, text, &style);
    let Some(descending) = sort else { return };
    match theme.era {
        Era::Win7 => {
            // 7: a light blue arrow at the top middle of the column.
            let m = Point::new(cell.x + cell.w / 2.0, cell.y + 3.5);
            let pts = if descending {
                [Point::new(m.x - 3.5, m.y - 2.0), Point::new(m.x + 3.5, m.y - 2.0), Point::new(m.x, m.y + 2.0)]
            } else {
                [Point::new(m.x - 3.5, m.y + 2.0), Point::new(m.x + 3.5, m.y + 2.0), Point::new(m.x, m.y - 2.0)]
            };
            r.fill_polygon(&pts, c(0xc4e3f4));
            r.stroke_polygon(&pts, 1.0, c(0x72abca));
        }
        _ => {
            // 98 / 2000: an outlined arrow pressed into the face; XP: a grey solid one.
            let w = r.measure(label, &style).min(text.w);
            let x = if right { text.right() - w - 12.0 } else { text.x + w + 8.0 };
            let y = cell.y + cell.h / 2.0;
            let pts = if descending {
                [Point::new(x, y - 2.0), Point::new(x + 7.0, y - 2.0), Point::new(x + 3.5, y + 2.0)]
            } else {
                [Point::new(x, y + 2.0), Point::new(x + 7.0, y + 2.0), Point::new(x + 3.5, y - 2.0)]
            };
            if theme.era == Era::Xp {
                r.fill_polygon(&pts, c(0xaca899));
            } else {
                let (hl, _, sh, _) = retro::bevel_colours(theme, theme.list_header.base());
                r.line(pts[0], pts[2], 1.0, sh);
                r.line(pts[2], pts[1], 1.0, hl);
                r.line(pts[0], pts[1], 1.0, if descending { sh } else { hl });
            }
        }
    }
}

// ---------------------------------------------------------------- Home

/// The control that folds a section away, centred on `m`: 2000's tree box with a minus or plus,
/// XP's round task pane button with two arrows, 7's small arrow.
pub fn fold(r: &mut Renderer, theme: &Theme, m: Point, open: bool, hot: bool) {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let b = Rect::new(r.snap(m.x - 4.5), r.snap(m.y - 4.5), 9.0, 9.0);
            r.fill_rect(b, c(0xffffff));
            r.stroke_rect(b.inset(0.5, 0.5), 1.0, c(0x808080));
            r.fill_rect(Rect::new(b.x + 2.0, b.y + 4.0, 5.0, 1.0), c(0x000000));
            if !open {
                r.fill_rect(Rect::new(b.x + 4.0, b.y + 2.0, 1.0, 5.0), c(0x000000));
            }
        }
        Era::Xp => {
            let p = &Pen::over(r, Rect::new(m.x - 8.5, m.y - 8.5, 17.0, 17.0), 17.0);
            let face = if hot { g(&[(0.0, 0xffffff), (1.0, 0xd6e0f7)]) } else { g(&[(0.0, 0xffffff), (1.0, 0xc6d3f7)]) };
            crate::eraicons::oval_g(r, p, 8.5, 8.5, 8.0, 8.0, &face, (8.5, 0.5), (8.5, 16.5));
            crate::eraicons::oval_line(r, p, 8.5, 8.5, 8.0, 8.0, c(0xb4c3e8));
            // The scheme's task pane blue (Silver's grey, Royale's deeper blue), lighter under the mouse.
            let ink = if hot { theme.panel_header_text.lighter(0.3) } else { theme.panel_header_text };
            for k in [0.0, 4.0] {
                let y = m.y + if open { -3.5 + k } else { -0.5 + k - 4.0 + 3.0 };
                let pts = if open {
                    [Point::new(m.x - 3.0, y + 3.0), Point::new(m.x, y), Point::new(m.x + 3.0, y + 3.0)]
                } else {
                    [Point::new(m.x - 3.0, y), Point::new(m.x, y + 3.0), Point::new(m.x + 3.0, y)]
                };
                r.polyline(&pts, 1.5, ink);
            }
        }
        Era::Win7 => {
            let ink = c(if hot { 0x3c7fb1 } else { 0x1e395b });
            if open {
                retrotool::triangle_down(r, m, 7.0, ink);
            } else {
                retrotool::triangle_right(r, m, 7.0, ink);
            }
        }
        Era::None => {}
    }
}

/// A section's title ("Quick access") at `text` with the era's rule along the row `row` (the
/// page's width): 2000's line shading from the title bar's colour to white, XP's blue line fading
/// out, 7's grey line after the words.
pub fn section_title(r: &mut Renderer, theme: &Theme, row: Rect, text: Rect, title: &str, size: f32) {
    let (color, style) = match theme.era {
        Era::Win98 | Era::Win2000 => (theme.text, TextStyle::new(theme.text).size(size).bold()),
        Era::Xp => (theme.panel_header_text, TextStyle::new(theme.panel_header_text).size(size).bold()),
        _ => (c(0x1e395b), TextStyle::new(c(0x1e395b)).size(size + 3.0)),
    };
    r.text(title, text, &style.color(color));
    let w = r.measure(title, &style).ceil();
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let y = (row.bottom() - 2.0).round();
            let line = Rect::new(row.x, y, row.w, 2.0);
            r.fill_paint_across(line, &Paint::even(&[theme.titlebar_paint.first(), theme.panel]));
        }
        Era::Xp => {
            let y = (row.bottom() - 1.0).round();
            r.fill_paint_across(Rect::new(row.x, y, row.w, 1.0), &Paint::even(&[c(0x6e8fd9), ca(0x6e8fd9, 0.0)]));
        }
        _ => {
            let x = text.x + w + 8.0;
            let y = (text.y + text.h / 2.0 + 1.0).round();
            r.fill_paint_across(Rect::new(x, y, (row.right() - x).max(0.0), 1.0), &Paint::even(&[c(0xe2e2e2), c(0xffffff)]));
        }
    }
}

/// Recent or Favorites (`b`) as the era's switch: 2000's flat toolbar button, pressed in over the
/// light checker while on; XP's tab (the one on in front with its orange top); 7's command bar
/// button, pressed in while on. `picture` draws its little picture into the 16 DIP box it's given.
#[allow(clippy::too_many_arguments)]
pub fn list_switch(r: &mut Renderer, theme: &Theme, b: Rect, label: &str, on: bool, hot: bool, picture: &dyn Fn(&mut Renderer, Rect)) {
    let b = Rect::new(r.snap(b.x), r.snap(b.y), r.snap(b.w), r.snap(b.h));
    let mut shift: f32 = 0.0;
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.toolbar;
            if on {
                if !hot {
                    retro::dither(r, theme, b.inset(1.0, 1.0), face);
                }
                retro::edge(r, theme, b, Edge::ThinSunken, face);
                shift = 1.0;
            } else if hot {
                retro::edge(r, theme, b, Edge::ThinRaised, face);
            }
        }
        Era::Xp => {
            if on {
                r.fill_rect(Rect::new(b.x, b.y, b.w, b.h + 1.0), c(0xfcfcfe));
                r.fill_rect(Rect::new(b.x + 1.0, b.y, b.w - 2.0, 1.0), c(0xe68b2c));
                r.fill_rect(Rect::new(b.x, b.y + 1.0, b.w, 2.0), c(0xffc83c));
                r.fill_rect(Rect::new(b.x, b.y + 3.0, 1.0, b.h - 2.0), c(0x919b9c));
                r.fill_rect(Rect::new(b.right() - 1.0, b.y + 3.0, 1.0, b.h - 2.0), c(0x919b9c));
            } else {
                let t = Rect::new(b.x, b.y + 2.0, b.w, b.h - 2.0);
                r.fill_paint_down(t, 0.0, &g(&[(0.0, 0xffffff), (0.6, 0xf4f4f0), (1.0, 0xecebe6)]), t.y, t.bottom());
                if hot {
                    r.fill_rect(Rect::new(t.x + 1.0, t.y, t.w - 2.0, 1.0), c(0xe68b2c));
                    r.fill_rect(Rect::new(t.x, t.y + 1.0, t.w, 1.0), c(0xffc73c));
                }
                r.stroke_round_rect(Rect::new(t.x + 0.5, t.y + 0.5, t.w - 1.0, t.h + 2.0), 3.0, 1.0, c(0x91a7b4));
                shift = 1.0;
            }
        }
        Era::Win7 => {
            if on || hot {
                retrotool::seven_box(r, b, on);
            }
        }
        Era::None => {}
    }
    let dx = if theme.era == Era::Xp { 0.0 } else { shift };
    let pic = Rect::new(b.x + 6.0 + dx, b.y + (b.h - 16.0) / 2.0 + shift, 16.0, 16.0);
    picture(r, pic);
    let ink = if theme.era == Era::Win7 { c(0x1e395b) } else { theme.text };
    r.text(label, Rect::new(pic.right() + 5.0, b.y + shift, (b.right() - pic.right() - 6.0).max(0.0), b.h), &TextStyle::new(ink));
}

/// The baseline the XP tabs stand on, or 2000's band behind its buttons (`strip` is the whole row).
pub fn list_switch_strip(r: &mut Renderer, theme: &Theme, strip: Rect) {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            r.fill_rect(strip, theme.toolbar);
            retro::etched_line(r, theme, strip.x, strip.y, strip.w, theme.toolbar);
        }
        Era::Xp => r.fill_rect(Rect::new(strip.x, strip.bottom(), strip.w, 1.0), c(0x919b9c)),
        _ => {}
    }
}

/// Recent's clock in the era's colours: a white face with a black rim and hands.
pub fn clock(r: &mut Renderer, theme: &Theme, at: Rect) {
    let p = &Pen::over(r, at, 16.0);
    let rim = if theme.era == Era::Win7 { 0x5a6e86 } else { 0x000000 };
    crate::eraicons::oval_g(r, p, 8.0, 8.0, 7.0, 7.0, &g(&[(0.0, 0xffffff), (1.0, if theme.era.classic() { 0xffffff } else { 0xdfe8f5 })]), (8.0, 1.0), (8.0, 15.0));
    crate::eraicons::oval_line(r, p, 8.0, 8.0, 7.0, 7.0, c(rim));
    if theme.era.classic() {
        r.fill_rect(Rect::new(at.x + 3.0, at.y + 8.0, 1.0, 1.0), c(0x808080));
    }
    crate::eraicons::stroke(r, p, &[(8.0, 3.5), (8.0, 8.0), (11.5, 8.0)], 1.3, c(0x000000));
    if theme.era != Era::Win98 && theme.era != Era::Win2000 {
        crate::eraicons::stroke(r, p, &[(8.0, 8.0), (5.0, 12.0)], 0.7, c(0xd02020));
    }
}

/// Favorites' picture: 2000's folder with a blue star, XP and 7's gold star.
pub fn favorite(r: &mut Renderer, theme: &Theme, at: Rect) {
    let p = &Pen::over(r, at, 16.0);
    let star: Vec<(f32, f32)> = (0..10)
        .map(|k| {
            let a = k as f32 * std::f32::consts::PI / 5.0 - std::f32::consts::FRAC_PI_2;
            let radius = if k % 2 == 0 { 7.5 } else { 3.2 };
            (8.0 + radius * a.cos(), 8.5 + radius * a.sin())
        })
        .collect();
    if theme.era.classic() {
        crate::eraicons::draw(r, crate::theme::IconSet::Classic, crate::eraicons::Role::Folder, at);
        let small: Vec<(f32, f32)> = star.iter().map(|&(x, y)| (8.0 + (x - 8.0) * 0.6, 10.0 + (y - 8.5) * 0.6)).collect();
        crate::eraicons::poly(r, p, &small, c(0x0000ff));
        crate::eraicons::oval(r, p, 8.0, 10.0, 1.0, 1.0, c(0xffff00));
    } else {
        crate::eraicons::poly_g(r, p, &star, &g(&[(0.0, 0xfff4a2), (0.5, 0xffd129), (1.0, 0xf2a51c)]), (8.0, 1.0), (8.0, 15.0));
        crate::eraicons::outline(r, p, &star, c(0xd45017));
    }
}

// ---------------------------------------------------------------- the status bar

/// The status bar's band and frames in `rect`; the text goes on top (status.rs). `split`: where the
/// right hand pane starts.
pub fn status_bar(r: &mut Renderer, theme: &Theme, rect: Rect, split: f32, zone: f32) {
    match theme.era {
        Era::Win98 | Era::Win2000 => {
            let face = theme.statusbar;
            r.fill_rect(rect, face);
            // The panes: thin sunken frames, 2 DIPs apart (the last, `zone` wide, for where the
            // folder is: My Computer), and the size grip in the corner.
            let y = rect.y + 2.0;
            let h = rect.h - 3.0;
            let zone_x = rect.right() - 16.0 - zone;
            let left = Rect::new(rect.x + 1.0, y, (split - rect.x - 3.0).max(0.0), h);
            let right = Rect::new(split, y, (zone_x - split - if zone > 0.0 { 2.0 } else { 0.0 }).max(0.0), h);
            retro::edge(r, theme, left, Edge::ThinSunken, face);
            retro::edge(r, theme, right, Edge::ThinSunken, face);
            if zone > 0.0 {
                retro::edge(r, theme, Rect::new(zone_x, y, zone, h), Edge::ThinSunken, face);
            }
            grip(r, theme, Rect::new(rect.right() - 14.0, rect.bottom() - 14.0, 13.0, 13.0), face);
        }
        Era::Xp => {
            r.fill_paint(rect, 0.0, &theme.statusbar_paint);
            r.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1.0), theme.status_line);
            let x = split.round() - 2.0;
            r.fill_rect(Rect::new(x, rect.y + 4.0, 1.0, rect.h - 7.0), c(0xc5c2b8));
            r.fill_rect(Rect::new(x + 1.0, rect.y + 4.0, 1.0, rect.h - 7.0), c(0xffffff));
            grip_dots(r, Rect::new(rect.right() - 13.0, rect.bottom() - 13.0, 12.0, 12.0), c(0xb9b7a8), c(0xffffff));
        }
        Era::Win7 => {
            r.fill_rect(rect, c(0xf1f5fb));
            r.fill_rect(Rect::new(rect.x, rect.y, rect.w, 1.0), c(0xccd9ea));
            r.fill_rect(Rect::new(rect.x, rect.y + 1.0, rect.w, 1.0), c(0xe8eef7));
            r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 1.0), c(0xf0f2f5));
            r.fill_rect(Rect::new(rect.x, rect.bottom() - 1.0, rect.w, 1.0), c(0xdfe3eb));
        }
        Era::None => {}
    }
}

/// The status bar's text colour.
pub fn status_text(theme: &Theme) -> Color {
    if theme.era == Era::Win7 {
        c(0x1e395b)
    } else {
        theme.status_text
    }
}

/// 98 / 2000's size grip: three pairs of diagonal lines, light and shadow.
fn grip(r: &mut Renderer, theme: &Theme, b: Rect, face: Color) {
    let (hl, _, sh, _) = retro::bevel_colours(theme, face);
    let (x, y) = (r.snap(b.right()), r.snap(b.bottom()));
    for k in 0..3 {
        let d = 3.0 + k as f32 * 4.0;
        r.line(Point::new(x - d, y), Point::new(x, y - d), 1.0, hl);
        r.line(Point::new(x - d + 1.0, y), Point::new(x, y - d + 1.0), 1.0, sh);
        r.line(Point::new(x - d + 2.0, y), Point::new(x, y - d + 2.0), 1.0, sh);
    }
}

/// XP's size grip: a triangle of square dots, each with a white one under it.
fn grip_dots(r: &mut Renderer, b: Rect, dot: Color, light: Color) {
    let (x, y) = (r.snap(b.right()) - 12.0, r.snap(b.bottom()) - 12.0);
    for i in 0..3 {
        for j in 0..3 {
            if i + j < 2 {
                continue;
            }
            let (dx, dy) = (x + i as f32 * 4.0, y + j as f32 * 4.0);
            r.fill_rect(Rect::new(dx + 1.0, dy + 1.0, 2.0, 2.0), light);
            r.fill_rect(Rect::new(dx, dy, 2.0, 2.0), dot);
        }
    }
}
