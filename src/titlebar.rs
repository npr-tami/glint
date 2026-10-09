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
use crate::theme::{size, CaptionStyle, Color, Paint, TabStyle, Theme};
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
}

impl TitleBar {
    pub fn new() -> TitleBar {
        TitleBar::default()
    }

    /// The caption buttons: 0 = close (rightmost), 1 = maximise, 2 = minimise.
    fn button(&self, i: usize) -> Rect {
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
        if self.plus.contains(p) {
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
        let pills = app.theme.tabs == TabStyle::Pills;
        let (y, h, gap) = if pills {
            (self.rect.y + PILL_TOP, self.rect.h - PILL_TOP - PILL_BOTTOM, PILL_GAP)
        } else {
            (self.rect.y + TAB_TOP, self.rect.h - TAB_TOP, 2.0)
        };
        let left = self.rect.x + crate::logo::width(&app.theme) + 4.0;
        let room = (self.button(2).x - DRAG_ROOM - PLUS - 4.0 - left).max(0.0);
        let n = app.tabs.len().max(1) as f32;
        let mut widths: Vec<f32> = app
            .tabs
            .iter()
            .map(|t| {
                let title = app.display_name(&t.path);
                let text = self.measure(r, &title, pills);
                (TAB_PAD + size::ICON + TAB_ICON_GAP + text + 6.0 + TAB_CLOSE + TAB_PAD * 0.5).clamp(TAB_MIN, TAB_MAX)
            })
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + gap * (n - 1.0);
        if total > room {
            let each = ((room - gap * (n - 1.0)) / n).max(TAB_SQUEEZED);
            widths.iter_mut().for_each(|w| *w = w.min(each));
        }
        self.tabs.clear();
        let mut x = left;
        for w in widths {
            let rect = Rect::new(x, y, w, h);
            let close = Rect::new(rect.right() - TAB_PAD * 0.5 - TAB_CLOSE, y + (h - TAB_CLOSE) / 2.0, TAB_CLOSE, TAB_CLOSE);
            self.tabs.push(TabSlot { rect, close });
            x += w + gap;
        }
        self.plus = Rect::new(x + 2.0, y + (h - PLUS) / 2.0, PLUS, PLUS);
    }

    fn paint_tabs(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState) {
        self.layout_tabs(r, app);
        let pills = theme.tabs == TabStyle::Pills;
        // Tabs never run under the caption buttons.
        r.push_clip(Rect::new(self.rect.x, self.rect.y, self.button(2).x - self.rect.x, self.rect.h));
        for (i, tab) in app.tabs.iter().enumerate() {
            let Some(slot) = self.tabs.get_mut(i) else { break };
            let active = i == app.active;
            let dropping = self.drop_tab == Some(i);
            let hot = matches!(self.hover, Some(Part::Tab(t) | Part::TabClose(t)) if t == i) || dropping;
            let rect = slot.rect;
            if pills {
                paint_pill(r, theme, rect, active, hot);
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

            // The last tab has its × too: closing it closes the window, as in File Explorer.
            let show_close = (active || hot) && rect.w >= 52.0;
            if !show_close {
                slot.close = Rect::default();
            }
            let close = slot.close;
            let mid = rect.y + rect.h / 2.0 + if active && !pills { 1.0 } else { 0.0 };
            let mut x = rect.x + TAB_PAD;
            // The folder's icon, when there is room for it and a name.
            if rect.w >= 80.0 {
                let icon = if tab.path.is_empty() {
                    app.icons.for_path("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}")
                } else if tab.path.len() <= 3 {
                    // A drive: never ask the drive itself from here (see Icons::drive).
                    let kind = app.drives.iter().find(|d| d.root.eq_ignore_ascii_case(&tab.path)).map_or("Local Disk", |d| d.kind_name);
                    app.icons.drive(&tab.path, kind)
                } else {
                    app.icons.folder()
                };
                r.icon(&app.icons, icon, Rect::new(x, (mid - size::ICON / 2.0).round(), size::ICON, size::ICON));
                x += size::ICON + TAB_ICON_GAP;
            }
            let right = if show_close { close.x - 4.0 } else { rect.right() - TAB_PAD * 0.75 };
            let color = if active {
                theme.tab_active_text
            } else if hot {
                theme.text
            } else {
                theme.tab_text
            };
            let title = app.display_name(&tab.path);
            // The current pill's name is bold.
            let style = if pills && active { TextStyle::new(color).bold() } else { TextStyle::new(color) };
            r.text(&title, Rect::new(x, mid - 10.0, (right - x).max(0.0), 20.0), &style);

            if show_close {
                let close_hot = self.hover == Some(Part::TabClose(i));
                if close_hot {
                    let fill = if self.pressed.is_some_and(|(p, _)| p == Part::TabClose(i)) { theme.button_pressed } else { theme.button_hover };
                    r.fill_paint(close, theme.corner(close.h), &fill.into());
                }
                let c = Point::new(close.x + close.w / 2.0, close.y + close.h / 2.0);
                cross(r, c, 3.5, 1.2, if close_hot { theme.text } else { theme.tab_text });
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
            let color = if hot { theme.text } else { theme.tab_text };
            // A bolder "+" beside pills, as their names are.
            let w = if pills { 1.8 } else { 1.2 };
            r.line(Point::new(c.x - 5.5, c.y), Point::new(c.x + 5.5, c.y), w, color);
            r.line(Point::new(c.x, c.y - 5.5), Point::new(c.x, c.y + 5.5), w, color);
        }
    }

    /// The right-click menu of tab `i`, at the mouse.
    fn tab_menu(&mut self, i: usize, at: Point, app: &AppState) -> Effect {
        self.menu_tab = app.tabs.get(i).map(|t| t.id);
        // "Close tab" on the last tab closes the window (window.rs); there are no others to close.
        let one = app.tabs.len() <= 1;
        Effect::Menu(Menu {
            tag: MENU_TAB,
            at,
            items: vec![
                MenuItem::new(PICK_DUPLICATE, "Duplicate tab"),
                MenuItem::separator(),
                MenuItem::new(PICK_CLOSE, "Close tab\tCtrl+W"),
                MenuItem::new(PICK_CLOSE_OTHERS, "Close other tabs").disabled(one),
                MenuItem::separator(),
                MenuItem::new(PICK_REOPEN, "Reopen closed tab\tCtrl+Shift+T").disabled(!app.can_reopen_tab()),
            ],
            right_aligned: false,
        })
    }

    fn paint_caption_buttons(&self, r: &mut Renderer, theme: &Theme, ctx: &Ctx) {
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
    r.stroke_round_rect(rect.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, theme.tab_border);
}

/// An "×" centred on `c`, `half` DIPs from the middle to each end.
fn cross(r: &mut Renderer, c: Point, half: f32, width: f32, color: Color) {
    r.line(Point::new(c.x - half, c.y - half), Point::new(c.x + half, c.y + half), width, color);
    r.line(Point::new(c.x + half, c.y - half), Point::new(c.x - half, c.y + half), width, color);
}

impl Component for TitleBar {
    fn layout(&mut self, rect: Rect, _ctx: &Ctx, _app: &AppState) {
        self.rect = rect;
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        r.fill_paint(self.rect, 0.0, &theme.titlebar_paint);
        // The shine along its top edge, and with pills, a line along its bottom (the tabs don't
        // run into the toolbar then).
        r.fill_rect(Rect::new(self.rect.x, self.rect.y, self.rect.w, 1.0), theme.highlight);
        if theme.tabs == TabStyle::Pills {
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

    fn mouse_move(&mut self, _ctx: &Ctx, e: &MouseEvent, _app: &mut AppState) -> Effect {
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

    fn mouse_down(&mut self, _ctx: &Ctx, e: &MouseEvent, _app: &mut AppState) -> Effect {
        self.pressed = None;
        let Some(part) = self.part_at(e.pos) else { return Effect::None };
        match (part, e.button) {
            // Switching happens on the way down, so it feels instant.
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
        let Some((part, button)) = self.pressed.take() else { return Effect::None };
        if button != e.button || Some(part) != self.part_at(e.pos) {
            return Effect::Redraw;
        }
        match part {
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
            PICK_DUPLICATE => Effect::Command(Command::DuplicateTab(i)),
            PICK_CLOSE => Effect::Command(Command::CloseTab(i)),
            PICK_CLOSE_OTHERS => Effect::Command(Command::CloseOtherTabs(i)),
            PICK_REOPEN => Effect::Command(Command::ReopenClosedTab),
            _ => Effect::None,
        }
    }

    /// A tab (its × too): things dropped on it go into its folder, and resting on it switches to
    /// it (window/drag.rs).
    fn drop_target(&self, p: Point, _app: &AppState) -> Option<DropTarget> {
        match self.part_at(p)? {
            Part::Tab(i) | Part::TabClose(i) => Some(DropTarget::Tab(i)),
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
