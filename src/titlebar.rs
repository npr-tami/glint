//! The title bar: the logo (the chrome wordmark, or a skin's own word in a box: logo.rs), the tabs,
//! a "+" for a new tab, and the minimise / maximise / close buttons. There is no Windows title bar -
//! we draw our own (frame.rs keeps the resizing, snapping and shadow working).
//!
//! What Windows is told about each point (WM_NCHITTEST, through `hit_test`):
//! - empty space (and the logo) is the caption: drag to move, double-click to maximise, right-click
//!   for the window menu - all done by Windows itself;
//! - the three buttons are HTMINBUTTON / HTMAXBUTTON / HTCLOSE, so Windows 11 shows its snap
//!   layouts when the mouse rests on maximise; their clicks still come here (window.rs routes them);
//! - tabs, their × and "+" are ordinary client points.
//!
//! Tabs: the active one has the toolbar's colour (so it flows into the toolbar) with the skin's line
//! on top (3 DIPs of red in AMD Radeon); the others are muted text that lights up under the mouse.
//! A skin with `tabs: pills` makes every tab a rounded box of its own with an edge, sitting inside
//! the title bar, and `caption-buttons: marbles` draws minimise, maximise and close as three glass
//! marbles (their buttons stay as wide as ever, so they are as easy to hit). The × shows on the
//! active tab and the one under the mouse (never when there is only one tab: the last tab can't be
//! closed).
//! Click to switch, middle-click or × to close, "+" (or Ctrl+T) for a new one. When there are too
//! many to fit, they all get narrower. Right-click a tab for Duplicate tab, Close tab, Close other
//! tabs and Reopen closed tab.

use std::collections::HashMap;

use crate::app::AppState;
use crate::render::{Renderer, TextStyle};
use crate::retro;
use crate::theme::{size, CaptionStyle, Color, Era, Paint, TabStyle, Theme};
use crate::ui::{Command, Component, Ctx, DropTarget, Effect, Hit, Menu, MenuItem, MouseButton, MouseEvent, Point, Rect};

/// Tabs start this far below the top of the title bar and reach down to the toolbar.
const TAB_TOP: f32 = 6.0;
/// Room on the left of a tab's icon and on the right of its ×.
const TAB_PAD: f32 = 12.0;
const TAB_ICON_GAP: f32 = 8.0;
/// The ×'s hover box.
const TAB_CLOSE: f32 = 18.0;
const TAB_MIN: f32 = 96.0;
const TAB_MAX: f32 = 220.0;
/// Narrowest a tab gets when many share the bar.
const TAB_SQUEEZED: f32 = 36.0;
const PLUS: f32 = 28.0;
/// Empty title bar always left to grab the window by.
const DRAG_ROOM: f32 = 46.0;
/// `tabs: pills`: how far a tab sits below the top of the title bar and above its bottom, and the
/// space between two tabs.
const PILL_TOP: f32 = 8.0;
const PILL_BOTTOM: f32 = 4.0;
const PILL_GAP: f32 = 4.0;
/// XP's tabs start this far below the top of the title bar.
const XP_TAB_TOP: f32 = 9.0;
/// 98 / 2000: how tall the current tab is (the others are 2 lower), standing on the toolbar.
const CLASSIC_TAB_H: f32 = 24.0;
/// `caption-buttons: marbles`: how big the marbles are, and how far the outer two sit in from the
/// middle of their buttons, toward the middle one, so the three sit together as a group.
const MARBLE: f32 = 16.0;
const MARBLE_PULL: f32 = 10.0;

/// The right-click menu of a tab, and its items.
const MENU_TAB: u32 = 1;
const PICK_DUPLICATE: u32 = 1;
const PICK_CLOSE: u32 = 2;
const PICK_CLOSE_OTHERS: u32 = 3;
const PICK_REOPEN: u32 = 4;
const PICK_PIN: u32 = 5;
const PICK_SAVE_SET: u32 = 6;
/// "Tab sets ›": open set n (PICK_OPEN_SET + n), or remove it (PICK_REMOVE_SET + n).
const PICK_OPEN_SET: u32 = 100;
const PICK_REMOVE_SET: u32 = 200;

/// One clickable thing in the title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Tab(usize),
    TabClose(usize),
    NewTab,
    Minimize,
    Maximize,
    Close,
}

/// The name on Settings' tab, when it opens as a tab.
const SETTINGS_TAB: &str = "Settings";

/// Where a tab was drawn (filled in by `paint`, used for the mouse).
#[derive(Clone, Copy, Debug, Default)]
struct TabSlot {
    rect: Rect,
    /// The ×'s box; empty when the tab shows no ×.
    close: Rect,
}

#[derive(Default)]
pub struct TitleBar {
    rect: Rect,
    tabs: Vec<TabSlot>,
    plus: Rect,
    hover: Option<Part>,
    /// The part a button went down on (it acts if the button comes up on the same part).
    pressed: Option<(Part, MouseButton)>,
    /// Tab title widths, measured once per title (and font: `Renderer::font_generation`; and in
    /// bold with pills, where the current tab's name is bold).
    widths: HashMap<String, f32>,
    widths_font: u32,
    widths_bold: bool,
    /// The tab the open right-click menu is about, by its id: while the menu is open a tab can open
    /// in front of it (a folder from another program), and the pick must not hit its neighbour.
    menu_tab: Option<u64>,
    /// Something dragged rests on this tab (window/drag.rs switches to it after a moment): it's
    /// outlined in red.
    drop_tab: Option<usize>,
    /// A tab held down with the left button, by its id, and where the mouse went down: dragged
    /// sideways, it moves along the tab row.
    held_tab: Option<(u64, f32)>,
    /// The skin's older Windows (its window buttons are that Windows' size, where it had them).
    era: Era,
}

impl TitleBar {
    pub fn new() -> TitleBar {
        TitleBar::default()
    }

    /// The caption buttons: 0 = close (rightmost), 1 = maximise, 2 = minimise. With an era, what
    /// the mouse can hit: the button and the space beside it, the whole height of the bar (close
    /// reaching to the window's edge).
    fn button(&self, i: usize) -> Rect {
        if self.era.old() {
            let [close, max, min] = retro::caption_slots(self.era, self.rect);
            let (left, right) = match i {
                0 => (close.x - 1.0, self.rect.right()),
                1 => (max.x, close.x - 1.0),
                _ => (min.x, max.x),
            };
            return Rect::new(left, self.rect.y, right - left, self.rect.h);
        }
        Rect::new(self.rect.right() - size::CAPTION_BUTTON_W * (i + 1) as f32, self.rect.y, size::CAPTION_BUTTON_W, self.rect.h)
    }

    fn part_at(&self, p: Point) -> Option<Part> {
        if !self.rect.contains(p) {
            return None;
        }
        for (i, part) in [Part::Close, Part::Maximize, Part::Minimize].into_iter().enumerate() {
            if self.button(i).contains(p) {
                return Some(part);
            }
        }
        // Only where it's drawn: with many tabs it has no room left of the caption buttons.
        if self.plus.right() <= self.button(2).x && self.plus.contains(p) {
            return Some(Part::NewTab);
        }
        for (i, t) in self.tabs.iter().enumerate() {
            if t.close.contains(p) {
                return Some(Part::TabClose(i));
            }
            if t.rect.contains(p) {
                return Some(Part::Tab(i));
            }
        }
        None
    }

    fn measure(&mut self, r: &mut Renderer, title: &str, bold: bool) -> f32 {
        // Another font (the skin changed): measure again.
        if self.widths_font != r.font_generation() || self.widths_bold != bold {
            self.widths_font = r.font_generation();
            self.widths_bold = bold;
            self.widths.clear();
        }
        if let Some(&w) = self.widths.get(title) {
            return w;
        }
        if self.widths.len() > 256 {
            self.widths.clear();
        }
        let style = TextStyle::new(Color::hex(0));
        let w = r.measure(title, &if bold { style.bold() } else { style }).ceil();
        self.widths.insert(title.to_string(), w);
        w
    }

    /// Places the tabs and "+" for this frame.
    fn layout_tabs(&mut self, r: &mut Renderer, app: &AppState) {
        // 98 / 2000: property sheet tabs standing on the toolbar, their names in regular weight.
        let classic = app.theme.era.classic();
        let pills = app.theme.tabs == TabStyle::Pills && !classic;
        let (y, h, gap) = if classic {
            (self.rect.bottom() - CLASSIC_TAB_H, CLASSIC_TAB_H, 0.0)
        } else if pills {
            (self.rect.y + PILL_TOP, self.rect.h - PILL_TOP - PILL_BOTTOM, PILL_GAP)
        } else if self.era == Era::Xp {
            // XP: property-sheet tabs, as tall as XP's own, standing on the toolbar band.
            (self.rect.y + XP_TAB_TOP, self.rect.h - XP_TAB_TOP, 1.0)
        } else {
            (self.rect.y + TAB_TOP, self.rect.h - TAB_TOP, 2.0)
        };
        let left = self.rect.x + crate::logo::width(&app.theme) + 4.0;
        let room = (self.button(2).x - DRAG_ROOM - PLUS - 4.0 - left).max(0.0);
        let mut widths: Vec<f32> = app
            .tabs
            .iter()
            .map(|t| {
                let title = app.display_name(&t.path);
                let text = self.measure(r, &title, pills);
                (TAB_PAD + size::ICON + TAB_ICON_GAP + text + 6.0 + TAB_CLOSE + TAB_PAD * 0.5).clamp(TAB_MIN, TAB_MAX)
            })
            .collect();
        // Settings open as a tab: its tab after the folders' (no picture, the same ×).
        if app.settings_tab {
            let text = self.measure(r, SETTINGS_TAB, pills);
            widths.push((TAB_PAD + text + 6.0 + TAB_CLOSE + TAB_PAD * 0.5).clamp(TAB_MIN, TAB_MAX));
        }
        let n = widths.len().max(1) as f32;
        let total: f32 = widths.iter().sum::<f32>() + gap * (n - 1.0);
        if total > room {
            let each = ((room - gap * (n - 1.0)) / n).max(TAB_SQUEEZED);
            widths.iter_mut().for_each(|w| *w = w.min(each));
        }
        self.tabs.clear();
        let mut x = left;
        for w in widths {
            let rect = Rect::new(x, y, w, h);
            let close = if classic {
                // A small flat box round 2000's little ×, in the middle of the tab's lower part.
                Rect::new((rect.right() - 6.0 - 14.0).round(), (y + 2.0 + (h - 2.0 - 12.0) / 2.0).round(), 14.0, 12.0)
            } else {
                Rect::new(rect.right() - TAB_PAD * 0.5 - TAB_CLOSE, y + (h - TAB_CLOSE) / 2.0, TAB_CLOSE, TAB_CLOSE)
            };
            self.tabs.push(TabSlot { rect, close });
            x += w + gap;
        }
        self.plus = if classic {
            Rect::new((x + 6.0).round(), (y + 2.0 + (h - 2.0 - 20.0) / 2.0).round(), 20.0, 20.0)
        } else {
            Rect::new(x + 2.0, y + (h - PLUS) / 2.0, PLUS, PLUS)
        };
    }

    fn paint_tabs(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState) {
        self.layout_tabs(r, app);
        if theme.era.classic() {
            return self.paint_classic_tabs(r, theme, app);
        }
        let pills = theme.tabs == TabStyle::Pills;
        // Tabs never run under the caption buttons.
        r.push_clip(Rect::new(self.rect.x, self.rect.y, self.button(2).x - self.rect.x, self.rect.h));
        for i in 0..app.tabs.len() + app.settings_tab as usize {
            let Some(slot) = self.tabs.get_mut(i) else { break };
            let tab = app.tabs.get(i);
            let active = if app.settings_tab { tab.is_none() } else { i == app.active };
            let dropping = self.drop_tab == Some(i);
            let hot = matches!(self.hover, Some(Part::Tab(t) | Part::TabClose(t)) if t == i) || dropping;
            let rect = slot.rect;
            if pills {
                paint_pill(r, theme, rect, active, hot);
            } else if theme.era == Era::Xp {
                xp_tab(r, theme, self.rect, rect, active, hot);
            } else if theme.era == Era::Win7 {
                seven_tab(r, theme, rect, active, hot);
            } else if active {
                r.fill_paint(rect, 0.0, &theme.tab_active);
                r.fill_rect(Rect::new(rect.x, rect.y, rect.w, theme.tab_line_w), theme.tab_line);
            } else if hot {
                // Part of the way to the current tab's look.
                r.fill_paint(rect, 0.0, &theme.tab);
                r.fill_paint(rect, 0.0, &theme.tab_active.with_strength(0.7));
            } else {
                r.fill_paint(rect, 0.0, &theme.tab);
            }
            if dropping && pills {
                let radius = theme.corner(rect.h);
                r.stroke_round_rect(rect.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, theme.accent);
            } else if dropping {
                r.stroke_rect(rect, 1.0, theme.accent);
            }

            // The last tab has its × too: closing it closes the window, as in File Explorer. A
            // pinned tab has a pin there instead (it closes from its right-click menu).
            let pinned = tab.is_some_and(|t| t.pinned);
            let show_close = (active || hot) && rect.w >= 52.0 && !pinned;
            let pin_at = slot.close;
            if !show_close {
                slot.close = Rect::default();
            }
            let close = slot.close;
            let mid = rect.y + rect.h / 2.0 + if active && !pills { 1.0 } else { 0.0 };
            let mut x = rect.x + TAB_PAD;
            // The folder's icon, when there is room for it and a name.
            if let Some(tab) = tab.filter(|_| rect.w >= 80.0) {
                let at = Rect::new(x, (mid - size::ICON / 2.0).round(), size::ICON, size::ICON);
                tab_icon(r, theme, app, &tab.path, at);
                x += size::ICON + TAB_ICON_GAP;
            }
            let right = if show_close || pinned { pin_at.x - 4.0 } else { rect.right() - TAB_PAD * 0.75 };
            let color = if active {
                theme.tab_active_text
            } else if hot && theme.era != Era::Xp {
                theme.text
            } else {
                theme.tab_text
            };
            let title = tab.map_or_else(|| SETTINGS_TAB.to_string(), |t| app.display_name(&t.path));
            // The current pill's name is bold.
            let style = if pills && active { TextStyle::new(color).bold() } else { TextStyle::new(color) };
            let words = Rect::new(x, mid - 10.0, (right - x).max(0.0), 20.0);
            if theme.era == Era::Xp && !active && theme.title_shadow.a > 0.0 {
                // XP's taskbar buttons: their words carry the caption's shadow.
                r.text(&title, Rect::new(words.x + 1.0, words.y + 1.0, words.w, words.h), &style.color(theme.title_shadow));
            }
            r.text(&title, words, &style);
            if pinned && rect.w >= 52.0 {
                pin_mark(r, Point::new(pin_at.x + pin_at.w / 2.0, pin_at.y + pin_at.h / 2.0), if active { color } else { theme.tab_text });
            }

            if show_close {
                let close_hot = self.hover == Some(Part::TabClose(i));
                let down = self.pressed.is_some_and(|(p, _)| p == Part::TabClose(i));
                let xp = theme.era == Era::Xp;
                if close_hot && xp && !active {
                    // On XP's blue taskbar-button tabs: the caption's own hover.
                    r.fill_paint(close, theme.corner(close.h), &caption_hover(theme, down).into());
                } else if close_hot {
                    let fill = if down { theme.button_pressed } else { theme.button_hover };
                    r.fill_paint(close, theme.corner(close.h), &fill.into());
                }
                let c = Point::new(close.x + close.w / 2.0, close.y + close.h / 2.0);
                let ink = if xp {
                    color
                } else if close_hot {
                    theme.text
                } else {
                    theme.tab_text
                };
                cross(r, c, 3.5, 1.2, ink);
            }
        }
        r.pop_clip();

        // "+" for a new tab.
        if self.plus.right() <= self.button(2).x {
            let hot = self.hover == Some(Part::NewTab);
            if hot {
                let fill = caption_hover(theme, self.pressed.is_some_and(|(p, _)| p == Part::NewTab));
                r.fill_paint(self.plus, theme.corner(self.plus.h), &fill.into());
            }
            let c = Point::new(self.plus.x + self.plus.w / 2.0, self.plus.y + self.plus.h / 2.0);
            let color = if hot && theme.era != Era::Xp { theme.text } else { theme.tab_text };
            // A bolder "+" beside pills, as their names are (and on XP, as its caption signs are).
            let w = if pills || theme.era == Era::Xp { 1.8 } else { 1.2 };
            r.line(Point::new(c.x - 5.5, c.y), Point::new(c.x + 5.5, c.y), w, color);
            r.line(Point::new(c.x, c.y - 5.5), Point::new(c.x, c.y + 5.5), w, color);
        }
    }

    /// The right-click menu of tab `i`, at the mouse.
    fn tab_menu(&mut self, i: usize, at: Point, app: &AppState) -> Effect {
        self.menu_tab = app.tabs.get(i).map(|t| t.id);
        // "Close tab" on the last tab closes the window (window.rs); there are no others to close.
        let one = app.tabs.len() <= 1;
        let pinned = app.tabs.get(i).is_some_and(|t| t.pinned);
        // Saved sets: each opens with a click; "Remove" lists them again to forget one.
        let sets = &app.settings.tab_sets;
        let name = |s: &crate::settings::TabSet| s.name.replace('&', "&&");
        let mut set_items: Vec<MenuItem> = sets.iter().enumerate().take(99).map(|(n, s)| MenuItem::new(PICK_OPEN_SET + n as u32, &name(s))).collect();
        if !sets.is_empty() {
            set_items.push(MenuItem::separator());
            set_items.push(MenuItem::submenu(
                "Remove a set",
                sets.iter().enumerate().take(99).map(|(n, s)| MenuItem::new(PICK_REMOVE_SET + n as u32, &name(s))).collect(),
            ));
        }
        let mut items = vec![
            MenuItem::new(PICK_PIN, if pinned { "Unpin tab" } else { "Pin tab" }),
            MenuItem::new(PICK_DUPLICATE, "Duplicate tab"),
            MenuItem::separator(),
            MenuItem::new(PICK_SAVE_SET, "Save tabs as a set\u{2026}"),
        ];
        if !sets.is_empty() {
            items.push(MenuItem::submenu("Tab sets", set_items));
        }
        items.extend([
            MenuItem::separator(),
            MenuItem::new(PICK_CLOSE, "Close tab\tCtrl+W"),
            MenuItem::new(PICK_CLOSE_OTHERS, "Close other tabs").disabled(one),
            MenuItem::separator(),
            MenuItem::new(PICK_REOPEN, "Reopen closed tab\tCtrl+Shift+T").disabled(!app.can_reopen_tab()),
        ]);
        Effect::Menu(Menu { tag: MENU_TAB, at, items, right_aligned: false })
    }

    fn paint_caption_buttons(&self, r: &mut Renderer, theme: &Theme, ctx: &Ctx) {
        if self.era.old() {
            let slots = retro::caption_slots(self.era, self.rect);
            let state = |part: Part| {
                let hot = self.hover == Some(part);
                retro::State { hot, down: hot && self.pressed.is_some_and(|(p, _)| p == part), active: ctx.active }
            };
            let max = if ctx.maximized { retro::Sign::Restore } else { retro::Sign::Maximize };
            retro::caption_group(r, theme, slots, [retro::Sign::Close, max, retro::Sign::Minimize], [state(Part::Close), state(Part::Maximize), state(Part::Minimize)]);
            return;
        }
        for (i, part) in [Part::Close, Part::Maximize, Part::Minimize].into_iter().enumerate() {
            let b = self.button(i);
            let hot = self.hover == Some(part);
            let down = hot && self.pressed.is_some_and(|(p, _)| p == part);
            if theme.caption_buttons == CaptionStyle::Marbles {
                // Close and minimise lean toward maximise, the three as one group; each stays well
                // inside its own button.
                let pull = [-MARBLE_PULL, 0.0, MARBLE_PULL][i];
                let c = Point::new(b.x + b.w / 2.0 + pull, b.y + b.h / 2.0);
                let glass = if part == Part::Close { theme.marble_close } else { theme.marble };
                let glass = if down {
                    glass.darker(0.12)
                } else if hot {
                    glass.lighter(0.3)
                } else if ctx.active {
                    glass
                } else {
                    // Like the signs: a little faded while another window is active.
                    glass.map(|c| c.mix(c.grey(), 0.5))
                };
                r.marble(c, MARBLE / 2.0, &glass);
                continue;
            }
            let sign = match part {
                Part::Close => crate::kit::Sign::Close,
                Part::Maximize => crate::kit::Sign::Maximize,
                _ => crate::kit::Sign::Minimize,
            };
            if crate::kit::caption_face(r, theme, b, sign, hot, down, ctx.active) {
                continue;
            }
            // Like Windows: the signs fade a little while another window is active.
            let mut glyph = if ctx.active { theme.muted } else { theme.muted.with_alpha(0.6) };
            if hot && part == Part::Close {
                r.fill_rect(b, if down { theme.close_hover.with_alpha(theme.close_hover.a * 0.75) } else { theme.close_hover });
                glyph = theme.on_accent;
            } else if hot {
                r.fill_rect(b, caption_hover(theme, down));
                glyph = theme.text;
            }
            // 10 x 10 DIPs, on whole pixels so the 1-DIP lines stay sharp.
            let g = b.center(size::CAPTION_GLYPH, size::CAPTION_GLYPH);
            let g = Rect::new(r.snap(g.x), r.snap(g.y), size::CAPTION_GLYPH, size::CAPTION_GLYPH);
            match part {
                // Ends on pixel centres: a crisper diagonal at 100 %.
                Part::Close => cross(r, Point::new(g.x + g.w / 2.0, g.y + g.h / 2.0), g.w / 2.0 - 0.5, 1.0, glyph),
                Part::Maximize if ctx.maximized => {
                    // Restore: a square with another peeking out behind it, up and to the right.
                    r.stroke_rect(Rect::new(g.x, g.y + 2.0, 8.0, 8.0), 1.0, glyph);
                    r.fill_rect(Rect::new(g.x + 2.0, g.y, 8.0, 1.0), glyph);
                    r.fill_rect(Rect::new(g.right() - 1.0, g.y, 1.0, 8.0), glyph);
                }
                Part::Maximize => r.stroke_rect(g, 1.0, glyph),
                _ => r.fill_rect(Rect::new(g.x, g.y + (g.h / 2.0).floor(), g.w, 1.0), glyph),
            }
        }
    }
}

/// XP's tabs in the caption (`tab` is one tab, `bar` the whole title bar). The current one is a
/// property-sheet tab joined to the toolbar under it: the toolbar band's paint (across the whole
/// window, so the two meet with no seam), round top corners, a grey edge and the orange edge on
/// top. The others are taskbar buttons: the skin's `tab` paint, a darker edge, standing on the
/// band a little above it.
fn xp_tab(r: &mut Renderer, theme: &Theme, bar: Rect, tab: Rect, active: bool, hot: bool) {
    const RADIUS: f32 = 3.0;
    if active {
        r.push_clip(tab);
        let tall = Rect::new(tab.x, tab.y, tab.w, tab.h + 2.0 * RADIUS);
        let y = tab.y + tab.h / 2.0;
        r.fill_rect_toward(tall, RADIUS, &theme.toolbar_paint, Point::new(bar.x, y), Point::new(bar.right(), y));
        r.stroke_round_rect(tall.inset(0.5, 0.5), RADIUS - 0.5, 1.0, theme.tab_border);
        // The orange edge: a dark orange line over two light ones, inside the round corners.
        r.push_clip(Rect::new(tab.x, tab.y, tab.w, theme.tab_line_w.max(1.0)));
        r.fill_round_rect(Rect::new(tab.x, tab.y, tab.w, 2.0 * RADIUS + 2.0), RADIUS, Color::hex(0xe68b2c));
        r.pop_clip();
        if theme.tab_line_w > 1.0 {
            r.fill_rect(Rect::new(tab.x + 1.0, tab.y + 1.0, tab.w - 2.0, theme.tab_line_w - 1.0), theme.tab_line);
        }
        r.pop_clip();
        return;
    }
    let button = Rect::new(tab.x, tab.y + 2.0, tab.w, tab.h - 5.0);
    let paint = if hot { theme.tab.lighter(0.12) } else { theme.tab };
    r.fill_paint(button, RADIUS, &paint);
    r.fill_rect(Rect::new(button.x + 2.0, button.y + 1.0, button.w - 4.0, 1.0), theme.highlight.with_alpha(theme.highlight.a * 0.7));
    r.stroke_round_rect(button.inset(0.5, 0.5), RADIUS - 0.5, 1.0, paint.last().darker(0.35));
}

/// 7's tabs on its glass, as Internet Explorer's of the time: round-topped. The current one a
/// pale shine fading down into the glass toolbar it joins, with the skin's tab edge and a white
/// line inside it; the others plain glass, a little lower, with fainter edges, brighter under the
/// mouse.
fn seven_tab(r: &mut Renderer, theme: &Theme, tab: Rect, active: bool, hot: bool) {
    const RADIUS: f32 = 4.0;
    let white = |a: f32| Color::rgba(255, 255, 255, a);
    let (t, shine, edge) = if active {
        (tab, [white(0.92), white(0.62), white(0.0)], theme.tab_border)
    } else {
        let shine = if hot { [white(0.6), white(0.32), white(0.1)] } else { [white(0.32), white(0.14), white(0.0)] };
        (Rect::new(tab.x, tab.y + 3.0, tab.w, tab.h - 3.0), shine, theme.tab_border.with_alpha(theme.tab_border.a * 0.7))
    };
    r.push_clip(t);
    let tall = Rect::new(t.x, t.y, t.w, t.h + 2.0 * RADIUS);
    r.fill_paint_down(tall, RADIUS, &Paint::even(&shine), t.y, t.bottom());
    r.stroke_round_rect(tall.inset(0.5, 0.5), RADIUS - 0.5, 1.0, edge);
    r.stroke_round_rect(tall.inset(1.5, 1.5), RADIUS - 1.5, 1.0, white(if active { 0.75 } else { 0.35 }));
    r.pop_clip();
}

/// The hover fill of a button sitting right on the title bar ("+", minimise, maximise), from the
/// skin (AMD Radeon: #232327, pressed #2c2c31; its light version #dcdce1, as the lighter hover
/// would vanish on the light title bar).
fn caption_hover(theme: &Theme, pressed: bool) -> Color {
    if pressed {
        theme.caption_pressed
    } else {
        theme.caption_hover
    }
}

/// A tab's picture in `at`: the folder's, a drive's, This PC's (the era's own where the skin has one).
fn tab_icon(r: &mut Renderer, theme: &Theme, app: &AppState, path: &str, at: Rect) {
    let icon = if crate::eraicons::draw_place(r, theme.icons, path, at) {
        None
    } else if path.is_empty() {
        Some(app.icons.for_path("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"))
    } else if path.len() <= 3 {
        // A drive: never ask the drive itself from here (see Icons::drive).
        let kind = app.drives.iter().find(|d| d.root.eq_ignore_ascii_case(path)).map_or("Local Disk", |d| d.kind_name);
        Some(app.icons.drive(path, kind))
    } else {
        Some(app.icons.folder())
    };
    if let Some(icon) = icon {
        r.icon(&app.icons, icon, at);
    }
}

/// The little pin a pinned tab shows where its × would be: a round head with its needle slanting
/// down to the left.
fn pin_mark(r: &mut Renderer, c: Point, color: Color) {
    let head = Point::new(c.x + 1.5, c.y - 1.5);
    r.line(Point::new(c.x - 3.5, c.y + 3.5), head, 1.2, color);
    r.line(Point::new(head.x - 3.0, head.y - 1.0), Point::new(head.x + 1.0, head.y + 3.0), 1.4, color);
    r.fill_ellipse(Point::new(head.x + 1.0, head.y - 1.0), 2.2, 2.2, color);
}

impl TitleBar {
    /// 98 / 2000: the tabs as a property sheet's, standing on the toolbar: grey, raised, the top
    /// corners cut; the current one 2 taller and 2 wider each side, joined to the toolbar under it
    /// (`join_toolbar`). Then "+" on the caption in its white, a raised button under the mouse.
    fn paint_classic_tabs(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState) {
        r.push_clip(Rect::new(self.rect.x, self.rect.y, self.button(2).x - self.rect.x, self.rect.h));
        // The current tab last: it overlaps its neighbours.
        let current = if app.settings_tab { app.tabs.len() } else { app.active };
        let count = app.tabs.len() + app.settings_tab as usize;
        let order: Vec<usize> = (0..count).filter(|&i| i != current).chain((current < count).then_some(current)).collect();
        for i in order {
            let Some(slot) = self.tabs.get_mut(i) else { continue };
            let tab = app.tabs.get(i);
            let active = i == current;
            let dropping = self.drop_tab == Some(i);
            let hot = matches!(self.hover, Some(Part::Tab(t) | Part::TabClose(t)) if t == i) || dropping;
            let rect = slot.rect;
            let shape = classic_tab_shape(rect, active);
            classic_tab(r, theme, shape);
            if dropping {
                crate::retroctl::dotted(r, shape.inset(3.0, 3.0), theme.text);
            }
            let pinned = tab.is_some_and(|t| t.pinned);
            let show_close = (active || hot) && rect.w >= 52.0 && !pinned;
            let pin_at = slot.close;
            if !show_close {
                slot.close = Rect::default();
            }
            let close = slot.close;
            // The words and picture sit in the middle of the part below the top edge.
            let mid = (shape.y + 2.0 + (shape.h - 2.0) / 2.0).round();
            let mut x = rect.x + 8.0;
            if let Some(tab) = tab.filter(|_| rect.w >= 80.0) {
                tab_icon(r, theme, app, &tab.path, Rect::new(x, mid - size::ICON / 2.0, size::ICON, size::ICON));
                x += size::ICON + 6.0;
            }
            let right = if show_close || pinned { pin_at.x - 3.0 } else { rect.right() - 6.0 };
            let color = if active { theme.tab_active_text } else { theme.tab_text };
            let title = tab.map_or_else(|| SETTINGS_TAB.to_string(), |t| app.display_name(&t.path));
            r.text(&title, Rect::new(x, mid - 10.0, (right - x).max(0.0), 20.0), &TextStyle::new(color));
            if pinned && rect.w >= 52.0 {
                pin_mark(r, Point::new(pin_at.x + pin_at.w / 2.0, mid), color);
            }
            if show_close {
                let close_hot = self.hover == Some(Part::TabClose(i));
                let down = close_hot && self.pressed.is_some_and(|(p, _)| p == Part::TabClose(i));
                let face = theme.toolbar;
                if close_hot {
                    retro::edge(r, theme, close, if down { retro::Edge::ThinSunken } else { retro::Edge::ThinRaised }, face);
                }
                let shift = if down { 1.0 } else { 0.0 };
                pixel_cross(r, close.x + 3.0 + shift, close.y + 2.0 + shift, theme.tab_active_text);
            }
        }
        r.pop_clip();

        // "+" for a new tab: white on the caption; a raised grey button under the mouse.
        if self.plus.right() <= self.button(2).x {
            let hot = self.hover == Some(Part::NewTab);
            let b = Rect::new(r.snap(self.plus.x), r.snap(self.plus.y), self.plus.w, self.plus.h);
            let mut ink = theme.title_text;
            let mut shift = 0.0;
            if hot {
                let down = self.pressed.is_some_and(|(p, _)| p == Part::NewTab);
                let face = theme.toolbar;
                r.fill_rect(b, face);
                retro::edge(r, theme, b, if down { retro::Edge::Pressed } else { retro::Edge::Raised }, face);
                ink = theme.button_text;
                shift = if down { 1.0 } else { 0.0 };
            }
            let (cx, cy) = (b.x + (b.w / 2.0).floor() + shift, b.y + (b.h / 2.0).floor() + shift);
            r.fill_rect(Rect::new(cx - 5.0, cy - 1.0, 10.0, 2.0), ink);
            r.fill_rect(Rect::new(cx - 1.0, cy - 5.0, 2.0, 10.0), ink);
        }
    }

    /// 98 / 2000: after the toolbar is drawn, the current tab runs on into it (the toolbar's top
    /// line is broken under the tab, the tab's sides carried down).
    pub fn join_toolbar(&self, r: &mut Renderer, theme: &Theme, app: &AppState) {
        if !theme.era.classic() {
            return;
        }
        // The same current tab as paint_classic_tabs: Settings' own when it is open as a tab.
        let current = if app.settings_tab { app.tabs.len() } else { app.active };
        let Some(slot) = self.tabs.get(current) else { return };
        let shape = classic_tab_shape(slot.rect, true);
        let clip_right = self.button(2).x;
        if shape.x >= clip_right {
            return;
        }
        let (hl, _, sh, dk) = retro::bevel_colours(theme, theme.toolbar);
        let y = r.snap(self.rect.bottom());
        let (x, right) = (r.snap(shape.x), r.snap(shape.right().min(clip_right)));
        r.fill_rect(Rect::new(x + 1.0, y, (right - x - 1.0).max(0.0), 2.0), theme.toolbar);
        r.fill_rect(Rect::new(x, y, 1.0, 2.0), hl);
        if shape.right() <= clip_right {
            r.fill_rect(Rect::new(right - 2.0, y, 1.0, 1.0), sh);
            r.fill_rect(Rect::new(right - 1.0, y, 1.0, 1.0), dk);
        }
    }
}

/// Where a 98 / 2000 tab is drawn from its slot: the current one 2 wider each side, the others 2
/// lower.
fn classic_tab_shape(rect: Rect, active: bool) -> Rect {
    if active {
        Rect::new(rect.x - 2.0, rect.y, rect.w + 4.0, rect.h)
    } else {
        Rect::new(rect.x, rect.y + 2.0, rect.w, rect.h - 2.0)
    }
}

/// A 98 / 2000 property sheet tab in `t`: the face, the highlight down the left and along the top
/// (the top corners cut by a pixel), the shadow and dark down the right; no bottom edge.
fn classic_tab(r: &mut Renderer, theme: &Theme, t: Rect) {
    let face = theme.toolbar;
    let (hl, _, sh, dk) = retro::bevel_colours(theme, face);
    let t = Rect::new(r.snap(t.x), r.snap(t.y), r.snap(t.w), r.snap(t.h));
    if t.w < 6.0 || t.h < 4.0 {
        return;
    }
    r.fill_rect(Rect::new(t.x + 2.0, t.y + 1.0, t.w - 4.0, 1.0), face);
    r.fill_rect(Rect::new(t.x + 1.0, t.y + 2.0, t.w - 2.0, t.h - 2.0), face);
    r.fill_rect(Rect::new(t.x, t.y + 2.0, 1.0, t.h - 2.0), hl);
    r.fill_rect(Rect::new(t.x + 1.0, t.y + 1.0, 1.0, 1.0), hl);
    r.fill_rect(Rect::new(t.x + 2.0, t.y, t.w - 4.0, 1.0), hl);
    r.fill_rect(Rect::new(t.right() - 2.0, t.y + 1.0, 1.0, 1.0), dk);
    r.fill_rect(Rect::new(t.right() - 2.0, t.y + 2.0, 1.0, t.h - 2.0), sh);
    r.fill_rect(Rect::new(t.right() - 1.0, t.y + 2.0, 1.0, t.h - 2.0), dk);
}

/// 2000's little ×, 8 by 7 pixels (each row two pixels of each stroke), its top left at (x, y).
fn pixel_cross(r: &mut Renderer, x: f32, y: f32, color: Color) {
    for k in 0..7 {
        let k = k as f32;
        r.fill_rect(Rect::new(x + k, y + k, 2.0, 1.0), color);
        r.fill_rect(Rect::new(x + 6.0 - k, y + k, 2.0, 1.0), color);
    }
}

/// A tab as a pill (`tabs: pills`): a rounded box of the skin's `tab` paint (`tab-active` for the
/// current one, lighter under the mouse), a shine along its top and a thin edge.
fn paint_pill(r: &mut Renderer, theme: &Theme, rect: Rect, active: bool, hot: bool) {
    let radius = theme.corner(rect.h);
    let paint: Paint = if active {
        theme.tab_active
    } else if hot {
        theme.tab.lighter(if theme.dark { 0.08 } else { 0.45 })
    } else {
        theme.tab
    };
    r.fill_paint(rect, radius, &paint);
    r.fill_rect(Rect::new(rect.x + radius * 0.7, rect.y + 1.0, (rect.w - 1.4 * radius).max(0.0), 1.0), theme.highlight);
    if active && theme.tab_line_w > 0.0 {
        r.fill_rect(Rect::new(rect.x + radius, rect.y, (rect.w - 2.0 * radius).max(0.0), theme.tab_line_w), theme.tab_line);
    }
    if theme.bevels {
        crate::pathbar::bevel(r, theme, rect, true, paint.base());
    } else {
        r.stroke_round_rect(rect.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, theme.tab_border);
    }
}

/// An "×" centred on `c`, `half` DIPs from the middle to each end.
fn cross(r: &mut Renderer, c: Point, half: f32, width: f32, color: Color) {
    r.line(Point::new(c.x - half, c.y - half), Point::new(c.x + half, c.y + half), width, color);
    r.line(Point::new(c.x + half, c.y - half), Point::new(c.x - half, c.y + half), width, color);
}

impl Component for TitleBar {
    fn layout(&mut self, rect: Rect, _ctx: &Ctx, app: &AppState) {
        self.rect = rect;
        self.era = app.theme.era;
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        self.era = theme.era;
        if theme.era == Era::Win7 {
            // 7's glass runs on down through the toolbar (toolbar.rs draws its part of it).
            let glass = Rect::new(self.rect.x, self.rect.y, self.rect.w, self.rect.h + size::TOOLBAR_H);
            retro::glass(r, theme, glass, self.rect, ctx.active);
        } else if theme.era.old() {
            retro::title_band(r, theme, self.rect, ctx.active);
        } else if theme.title_across {
            r.fill_paint_across(self.rect, &theme.titlebar_paint);
        } else {
            r.fill_paint(self.rect, 0.0, &theme.titlebar_paint);
        }
        if theme.title_gloss.a > 0.0 && !theme.era.old() {
            let top = Rect::new(self.rect.x, self.rect.y, self.rect.w, (self.rect.h * 0.48).round());
            r.fill_paint(top, 0.0, &Paint::even(&[theme.title_gloss, theme.title_gloss.with_alpha(theme.title_gloss.a * 0.45)]));
        }
        // The shine along its top edge, and with pills, a line along its bottom (the tabs don't
        // run into the toolbar then).
        r.fill_rect(Rect::new(self.rect.x, self.rect.y, self.rect.w, 1.0), theme.highlight);
        // (98 / 2000's tabs stand on the toolbar's own etched line instead.)
        if theme.tabs == TabStyle::Pills && !theme.era.classic() {
            r.fill_rect(Rect::new(self.rect.x, self.rect.bottom() - 1.0, self.rect.w, 1.0), theme.toolbar_line);
        }

        // The logo: part of the caption (it's no part in `part_at`), so it drags the window too.
        crate::logo::draw(r, theme, self.rect);

        self.paint_tabs(r, theme, app);
        self.paint_caption_buttons(r, theme, ctx);
    }

    fn hit_test(&self, p: Point, _app: &AppState) -> Hit {
        match self.part_at(p) {
            Some(Part::Close) => Hit::Close,
            Some(Part::Maximize) => Hit::Maximize,
            Some(Part::Minimize) => Hit::Minimize,
            Some(_) => Hit::Client,
            None => Hit::Caption,
        }
    }

    fn mouse_move(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if let Some((id, x)) = self.held_tab {
            // Dragged sideways: the tab moves past the tabs whose middle the mouse has passed (not
            // as soon as it's over one: a narrow tab passing a wide one would land with the mouse
            // still over that one, and the two would swap back and forth).
            if (e.pos.x - x).abs() >= 6.0 {
                self.pressed = None;
                if let Some(from) = app.tabs.iter().position(|t| t.id == id) {
                    let n = app.tabs.len().min(self.tabs.len());
                    let mid = |i: usize| self.tabs[i].rect.x + self.tabs[i].rect.w / 2.0;
                    let right = (from + 1..n).take_while(|&i| e.pos.x >= mid(i)).last();
                    let left = (0..from.min(n)).rev().take_while(|&i| e.pos.x < mid(i)).last();
                    if let Some(to) = right.or(left) {
                        return Effect::Command(Command::MoveTab(from, to));
                    }
                }
            }
            return Effect::None;
        }
        let hover = self.part_at(e.pos);
        if hover != self.hover {
            self.hover = hover;
            return Effect::Redraw;
        }
        Effect::None
    }

    fn mouse_leave(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        if self.hover.take().is_some() {
            return Effect::Redraw;
        }
        Effect::None
    }

    fn mouse_down(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.pressed = None;
        self.held_tab = None;
        let Some(part) = self.part_at(e.pos) else { return Effect::None };
        if let (Part::Tab(i), MouseButton::Left) = (part, e.button) {
            self.held_tab = app.tabs.get(i).map(|t| (t.id, e.pos.x));
        }
        match (part, e.button) {
            // Switching happens on the way down, so it feels instant.
            // Settings' own tab is already the current one.
            (Part::Tab(i), MouseButton::Left) if i >= app.tabs.len() => Effect::None,
            (Part::Tab(i), MouseButton::Left) => Effect::Command(Command::SwitchTab(i)),
            // The tab menu comes on the way up, like everywhere in Windows.
            (Part::Tab(_) | Part::TabClose(_), MouseButton::Middle | MouseButton::Right) | (_, MouseButton::Left) => {
                self.pressed = Some((part, e.button));
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    fn mouse_up(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.held_tab = None;
        let Some((part, button)) = self.pressed.take() else { return Effect::None };
        if button != e.button || Some(part) != self.part_at(e.pos) {
            return Effect::Redraw;
        }
        match part {
            // Settings' tab: its × (or a middle-click) closes Settings; it has no menu.
            Part::Tab(i) | Part::TabClose(i) if i >= app.tabs.len() => {
                if matches!(part, Part::TabClose(_)) || button == MouseButton::Middle {
                    Effect::Command(Command::CloseSettings)
                } else {
                    Effect::Redraw
                }
            }
            Part::Tab(i) | Part::TabClose(i) if button == MouseButton::Right => self.tab_menu(i, e.pos, app),
            Part::Tab(i) | Part::TabClose(i) if button == MouseButton::Middle => Effect::Command(Command::CloseTab(i)),
            Part::TabClose(i) => Effect::Command(Command::CloseTab(i)),
            Part::NewTab => Effect::Command(Command::NewTab),
            Part::Minimize => Effect::Command(Command::Minimize),
            Part::Maximize => Effect::Command(Command::ToggleMaximize),
            Part::Close => Effect::Command(Command::Close),
            Part::Tab(_) => Effect::Redraw,
        }
    }

    fn menu_pick(&mut self, _ctx: &Ctx, tag: u32, item: u32, app: &mut AppState) -> Effect {
        if tag != MENU_TAB {
            return Effect::None;
        }
        let Some(i) = self.menu_tab.take().and_then(|id| app.tabs.iter().position(|t| t.id == id)) else { return Effect::None };
        match item {
            PICK_SAVE_SET => Effect::Command(Command::SaveTabSet),
            n if (PICK_OPEN_SET..PICK_REMOVE_SET).contains(&n) => Effect::Command(Command::OpenTabSet((n - PICK_OPEN_SET) as usize)),
            n if (PICK_REMOVE_SET..PICK_REMOVE_SET + 100).contains(&n) => Effect::Command(Command::DeleteTabSet((n - PICK_REMOVE_SET) as usize)),
            PICK_DUPLICATE => Effect::Command(Command::DuplicateTab(i)),
            // The menu's "Close tab" closes a pinned tab too (it's the way to close one).
            PICK_CLOSE => Effect::Command(Command::ForceCloseTab(i)),
            PICK_PIN => Effect::Command(Command::TogglePinTab(i)),
            PICK_CLOSE_OTHERS => Effect::Command(Command::CloseOtherTabs(i)),
            PICK_REOPEN => Effect::Command(Command::ReopenClosedTab),
            _ => Effect::None,
        }
    }

    /// A tab (its × too): things dropped on it go into its folder, and resting on it switches to
    /// it (window/drag.rs).
    fn drop_target(&self, p: Point, app: &AppState) -> Option<DropTarget> {
        match self.part_at(p)? {
            Part::Tab(i) | Part::TabClose(i) if i < app.tabs.len() => Some(DropTarget::Tab(i)),
            _ => None,
        }
    }

    fn drop_hover(&mut self, target: Option<&DropTarget>) -> Effect {
        let tab = match target {
            Some(DropTarget::Tab(i)) => Some(*i),
            _ => None,
        };
        if tab == self.drop_tab {
            return Effect::None;
        }
        self.drop_tab = tab;
        Effect::Redraw
    }
}
