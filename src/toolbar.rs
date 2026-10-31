//! The toolbar under the title bar: the sidebar switch; back, forward, up and refresh; the path bar
//! (pathbar.rs); the filter box; the SORT, VIEW and SETTINGS word buttons. A line in the skin's
//! colour runs along its bottom (2 DIPs of red in AMD Radeon).
//!
//! - Nav buttons draw their arrows as vector shapes (no icon font needed) and grey out when there
//!   is nowhere to go (no history, already at This PC).
//! - The path: click a step to go there, middle-click to open it in a new tab, click "…" for the
//!   hidden earlier steps, right-click to copy or edit it. Clicking the empty part, double-clicking,
//!   Ctrl+L or F4 switch to typing: a real Win32 text box (textbox.rs) with Windows' own folder
//!   suggestions; Enter goes there, Esc cancels.
//! - The filter box filters the list live as you type (several words, or a pattern like `*.pdf`;
//!   the first match is selected and Enter opens it, Down goes to the list); Esc or its × clears
//!   it. The switch at its right end (Ctrl+Shift+F) makes it search the subfolders too (search.rs).
//! - SORT, VIEW and SETTINGS are word buttons in the skin's shape - slanted in AMD Radeon (both
//!   sides lean the same way). With `buttons: liquid` they are glossy capsules with white words,
//!   and the sidebar switch, back, forward, up and refresh are glass marbles with white signs.
//!   SORT opens a popup menu; VIEW a menu of the six views (Ctrl+Shift+1 to 6; list.rs, views.rs)
//!   and the sidebar and status bar switches; SETTINGS opens the Settings window (settingswin.rs),
//!   and right-clicking it opens a small menu of the quickest choices.
//! - The sidebar switch (Ctrl+B) sits at the left end: a little
//!   window with its left pane filled while the sidebar shows.
//!
//! The window calls two extra methods: `begin_path_edit` (Ctrl+L / F4 / Alt+D) and `focus_filter`
//! (Ctrl+F).

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{SHAutoComplete, SHACF_AUTOAPPEND_FORCE_OFF, SHACF_AUTOSUGGEST_FORCE_ON, SHACF_FILESYS_DIRS};

use crate::app::{AppState, SortKey};
use crate::settings::ViewMode;
use crate::frame;
use crate::pathbar::{box_contains, fill_box, fill_box_paint, fill_field, PathBar, PathHit};
use crate::render::{Align, Renderer, TextStyle};
use crate::shell;
use crate::textbox::{TextBox, TextEvent, TextEventKind};
use crate::pathbar::Crumbs;
use crate::retrotool::{self, Look, Tool};
use crate::theme::{size, ButtonStyle, Color, Era, Theme};
use crate::ui::{Command, Component, Ctx, CursorKind, DropTarget, Effect, Menu, MenuItem, MouseButton, MouseEvent, Point, Rect};

/// Text box ids (100-199 belong to the toolbar).
const PATH_BOX: u16 = 100;
const FILTER_BOX: u16 = 101;

/// Our popup menus, told apart in `menu_pick`.
const MENU_SORT: u32 = 1;

/// Whether the menu `tag` drops from a button (SORT, VIEW, SETTINGS, the hidden steps, Back and
/// Forward's lists), so Glint draws it itself, like the Settings window's drop-downs. A right-click
/// on the path gets Windows' own menu, as right-clicks do everywhere.
pub fn drawn_menu(tag: u32) -> bool {
    tag != MENU_PATH
}
const MENU_SETTINGS: u32 = 2;
/// The steps hidden behind "…".
const MENU_HIDDEN: u32 = 3;
/// Right-click on the path.
const MENU_PATH: u32 = 4;
/// VIEW's menu: the views (from VIEW_FIRST, in `ViewMode::ALL`'s order), the view rules and two
/// switches.
const MENU_VIEW: u32 = 5;
/// XP's lists beside Back and Forward (`history_menu`), and how many places they show at most.
const MENU_BACK: u32 = 6;
const MENU_FORWARD: u32 = 7;
const HISTORY_MAX: usize = 12;
const VIEW_FIRST: u32 = 1;
const VIEW_SIDEBAR: u32 = 20;
const VIEW_STATUS_BAR: u32 = 21;
const VIEW_SPLIT: u32 = 23;
const VIEW_RULES: u32 = 22;
const VIEW_CUSTOM: u32 = 24;

/// The SETTINGS button's right-click menu: what the Settings window doesn't have, and a shortcut to
/// its About page.
const SET_WINDOW: u32 = 80;
const SET_ABOUT: u32 = 50;
const SET_QUIT: u32 = 95;
/// SORT and SETTINGS lean this far in AMD Radeon (their top edge sits this many DIPs right of the
/// bottom edge); other skins lean in proportion, or not at all (`Theme::lean`).
const WORD_LEAN: f32 = 8.0;
const SORT_W: f32 = 74.0;
const VIEW_W: f32 = 72.0;
const SETTINGS_W: f32 = 108.0;
/// The space between two word buttons, measured across (between their parallel sides).
const WORD_GAP: f32 = 6.0;
/// Room for the magnifier on the left of the filter box, and for its × on the right.
const FILTER_ICON_W: f32 = 28.0;
const FILTER_CLEAR_W: f32 = 26.0;
/// The Subfolders switch at the right end of the filter box.
const SUBFOLDERS_W: f32 = 26.0;
/// 2000 and XP: the word "Address" before the path, and the Go button after it.
const ADDRESS_W: f32 = 54.0;
const GO_W: f32 = 44.0;
/// 7: Refresh, joined to the right end of the path box.
const REFRESH7_W: f32 = 26.0;
/// The era's text boxes are this tall (Glint's own are `size::INPUT_H`).
const ERA_FIELD_H: f32 = 24.0;
/// XP: the room the words "Back" and "Folders" take beside their pictures.
const XP_BACK_WORD_W: f32 = 30.0;
const XP_FOLDERS_WORD_W: f32 = 42.0;
/// XP: the drop arrow at the end of Back and of Forward.
const XP_DROP_W: f32 = 13.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    /// The sidebar switch (Ctrl+B).
    Sidebar,
    Back,
    Forward,
    Up,
    Refresh,
    Path,
    Filter,
    FilterClear,
    /// The switch that makes the filter search subfolders too.
    Subfolders,
    Sort,
    View,
    Settings,
    /// The Address band's Go (2000 and XP skins).
    Go,
    /// XP's drop arrows beside Back and Forward: the places you've been.
    BackMenu,
    ForwardMenu,
}

const PARTS: [Part; 15] = [
    Part::Sidebar,
    Part::Back,
    Part::Forward,
    Part::Up,
    Part::Refresh,
    Part::Path,
    Part::FilterClear,
    Part::Subfolders,
    Part::Filter,
    Part::Sort,
    Part::View,
    Part::Settings,
    Part::Go,
    Part::BackMenu,
    Part::ForwardMenu,
];

pub struct Toolbar {
    rect: Rect,
    hover: Option<Part>,
    /// A button that went down and acts when the mouse comes up on it.
    pressed: Option<Part>,
    /// The path step a button went down on; it acts when the button comes up on the same step.
    pressed_step: Option<(usize, MouseButton)>,
    /// What was typed in the path box when Go went down: Go goes there when it comes up. (Taken at
    /// once: the click takes the keyboard from the box, which may close before the button is up.)
    go_text: Option<String>,
    /// Where the mouse is while it is over the path (the step under it is drawn highlighted).
    mouse: Option<Point>,
    path: PathBar,
    path_box: TextBox,
    filter_box: TextBox,
    /// Windows' folder suggestions are switched on for the path box the first time it is used.
    autocomplete: bool,
    /// The filter text the box shows, to notice when the tab's filter changes under us.
    shown_filter: String,
    shown_tab: u64,
    /// Whether the filter box's grey hint says it searches subfolders.
    hint_subfolders: bool,
    /// The paths behind the items of the last "…" popup (item id - 1).
    menu_paths: Vec<String>,
    /// The path the last menu was about (the path's right-click menu; SETTINGS > When Glint
    /// starts, open > This folder).
    menu_target: String,
    /// Keep painting until this time (seconds, as `Ctx::now`). Under Wine a text box that was just
    /// hidden stays on screen until Wine catches up with a later frame; see `hide_path_box`.
    repaint_until: f64,
    /// The skin's lean for SORT and SETTINGS, and how thick the line along the bottom is (taken in
    /// `layout`; the window lays everything out again when the skin changes).
    lean: f32,
    line_w: f32,
    /// The skin's older Windows, if any (taken in `layout`): its own toolbar (retrotool.rs).
    era: Era,
}

impl Toolbar {
    /// Creates the toolbar and its (hidden) text boxes as children of the main window.
    pub fn new(hwnd: HWND) -> Toolbar {
        let filter_box = TextBox::new(hwnd, FILTER_BOX);
        filter_box.set_hint("Filter");
        // Window shortcuts (Ctrl+Shift+F, Ctrl+T...) work while typing in these boxes too.
        filter_box.pass_shortcuts();
        let path_box = TextBox::new(hwnd, PATH_BOX);
        path_box.pass_shortcuts();
        // A path pasted with its line break ("X:\glint-qa\r\n") goes there at once, as in Explorer.
        path_box.go_on_pasted_line();
        Toolbar {
            rect: Rect::default(),
            hover: None,
            pressed: None,
            pressed_step: None,
            go_text: None,
            mouse: None,
            path: PathBar::default(),
            path_box,
            filter_box,
            autocomplete: false,
            shown_filter: String::new(),
            shown_tab: 0,
            hint_subfolders: false,
            menu_paths: Vec::new(),
            menu_target: String::new(),
            repaint_until: 0.0,
            lean: WORD_LEAN,
            line_w: 2.0,
            era: Era::None,
        }
    }

    /// Switch the path bar to typing, with the current path selected (Ctrl+L, F4, Alt+D, a click on
    /// the empty part of the path).
    pub fn begin_path_edit(&mut self, _ctx: &Ctx, app: &mut AppState) -> Effect {
        if !self.autocomplete {
            self.autocomplete = true;
            // Windows' own suggestions while typing a folder path (the list Explorer's address bar
            // shows). Optional: if it can't be had, typing still works.
            // SAFETY: the box is our live EDIT control; COM is initialised on this thread (main.rs).
            unsafe {
                let _ = SHAutoComplete(self.path_box.hwnd(), SHACF_FILESYS_DIRS | SHACF_AUTOSUGGEST_FORCE_ON | SHACF_AUTOAPPEND_FORCE_OFF);
            }
        }
        let path = &app.tab().path;
        self.path_box.set_text(if path.is_empty() {
            "This PC"
        } else if crate::recycle::is_bin(path) {
            crate::recycle::NAME // typing it goes there too
        } else if crate::recent::is_recent(path) {
            crate::recent::NAME
        } else {
            path
        });
        self.path_box.show();
        self.path_box.focus();
        self.path_box.select_all();
        self.pressed_step = None;
        Effect::Redraw
    }

    /// Put the cursor in the filter box (Ctrl+F).
    pub fn focus_filter(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        self.filter_box.focus();
        self.filter_box.select_all();
        Effect::None
    }

    /// Back from typing to the steps. `focus_back`: give the keyboard back to the window (after
    /// Enter / Esc; not when the focus already went somewhere else).
    fn hide_path_box(&mut self, ctx: &Ctx, focus_back: bool) {
        self.path_box.hide();
        if focus_back {
            self.path_box.give_focus_back();
        }
        // Wine (only) shows the hidden box's last picture over our next frame, until a later frame
        // replaces it - so paint for a moment. Windows doesn't need this.
        if frame::is_wine() {
            self.repaint_until = ctx.now + 0.3;
        }
    }

    // ------------------------------------------------------------ layout

    fn band(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, self.rect.h - self.line_w)
    }

    fn nav_rect(&self, i: f32) -> Rect {
        let band = self.band();
        let y = band.y + (band.h - size::NAV_BUTTON) / 2.0;
        if self.era == Era::Xp {
            return self.xp_nav_rect(i, y);
        }
        Rect::new(band.x + 8.0 + i * (size::NAV_BUTTON + 2.0), y, size::NAV_BUTTON, size::NAV_BUTTON)
    }

    /// XP's Standard Buttons in its order: Back (with its word), Forward, Up, a separator, Folders
    /// (the sidebar switch, with its word), Refresh. `i` as in `nav_rect` (0 Folders ... 4 Refresh,
    /// 5 the end).
    fn xp_nav_rect(&self, i: f32, y: f32) -> Rect {
        let n = size::NAV_BUTTON;
        let x0 = self.band().x + 6.0;
        let back = Rect::new(x0, y, n + XP_BACK_WORD_W + XP_DROP_W, n);
        let forward = Rect::new(back.right() + 2.0, y, n + XP_DROP_W, n);
        let up = Rect::new(forward.right() + 2.0, y, n, n);
        let folders = Rect::new(up.right() + 8.0, y, n + XP_FOLDERS_WORD_W, n);
        let refresh = Rect::new(folders.right() + 2.0, y, n, n);
        match i as i32 {
            0 => folders,
            1 => back,
            2 => forward,
            3 => up,
            4 => refresh,
            _ => Rect::new(refresh.right() + 2.0, y, n, n),
        }
    }

    /// The filter box gets narrower when the window does, so the path keeps some room.
    fn filter_w(&self) -> f32 {
        if self.rect.w >= 1100.0 {
            220.0
        } else if self.rect.w >= 860.0 {
            180.0
        } else {
            130.0
        }
    }

    fn rect_of(&self, part: Part) -> Rect {
        let band = self.band();
        let iy = band.y + (band.h - size::INPUT_H) / 2.0;
        let settings = Rect::new(band.right() - 8.0 - SETTINGS_W, iy, SETTINGS_W, size::INPUT_H);
        // Boxes leaning the same way: across, the gap between two is WORD_GAP when one's right
        // edge sits the lean minus WORD_GAP right of the next one's left edge.
        let view = Rect::new(settings.x + self.lean - WORD_GAP - VIEW_W, iy, VIEW_W, size::INPUT_H);
        let sort = Rect::new(view.x + self.lean - WORD_GAP - SORT_W, iy, SORT_W, size::INPUT_H);
        let filter = Rect::new(sort.x - 8.0 - self.filter_w(), iy, self.filter_w(), size::INPUT_H);
        // The address takes what is left between the buttons and the filter. 7 has Refresh at its
        // right end instead of among the buttons; 2000 and XP put "Address" before it and Go after.
        let seven = self.era == Era::Win7;
        let band_era = matches!(self.era, Era::Win98 | Era::Win2000 | Era::Xp);
        let left = self.nav_rect(if seven { 4.0 } else { 5.0 }).x + 6.0;
        let area = Rect::new(left, iy, (filter.x - 8.0 - left).max(40.0), size::INPUT_H);
        match part {
            Part::Sidebar => self.nav_rect(0.0),
            // XP: the drop arrow at the button's right end is a part of its own.
            Part::Back | Part::Forward if self.era == Era::Xp => {
                let b = self.nav_rect(if part == Part::Back { 1.0 } else { 2.0 });
                Rect::new(b.x, b.y, b.w - XP_DROP_W, b.h)
            }
            Part::BackMenu | Part::ForwardMenu if self.era == Era::Xp => {
                let b = self.nav_rect(if part == Part::BackMenu { 1.0 } else { 2.0 });
                Rect::new(b.right() - XP_DROP_W, b.y, XP_DROP_W, b.h)
            }
            Part::BackMenu | Part::ForwardMenu => Rect::default(),
            Part::Back => self.nav_rect(1.0),
            Part::Forward => self.nav_rect(2.0),
            Part::Up => self.nav_rect(3.0),
            Part::Refresh if seven => Rect::new(area.right() - REFRESH7_W, iy, REFRESH7_W, size::INPUT_H),
            Part::Refresh => self.nav_rect(4.0),
            Part::Path if seven => Rect::new(area.x, iy, (area.w - REFRESH7_W).max(40.0), size::INPUT_H),
            Part::Path if band_era => {
                let x = area.x + ADDRESS_W;
                Rect::new(x, iy, (area.right() - GO_W - 4.0 - x).max(40.0), size::INPUT_H)
            }
            Part::Path => area,
            Part::Go if band_era => Rect::new(area.right() - GO_W, iy + (size::INPUT_H - ERA_FIELD_H) / 2.0 - 1.0, GO_W, ERA_FIELD_H + 2.0),
            Part::Go => Rect::default(),
            Part::Filter => filter,
            Part::FilterClear => Rect::new(filter.right() - SUBFOLDERS_W - FILTER_CLEAR_W, iy, FILTER_CLEAR_W, size::INPUT_H),
            Part::Subfolders => Rect::new(filter.right() - SUBFOLDERS_W, iy, SUBFOLDERS_W, size::INPUT_H),
            Part::Sort => sort,
            Part::View => view,
            Part::Settings => settings,
        }
    }

    /// Where the filter's text box sits: between the magnifier and the × (and the switch).
    fn filter_edit_rect(&self) -> Rect {
        let f = self.rect_of(Part::Filter);
        Rect::new(f.x + FILTER_ICON_W, f.y, (f.w - FILTER_ICON_W - FILTER_CLEAR_W - SUBFOLDERS_W).max(10.0), f.h)
    }

    fn part_at(&self, p: Point, app: &AppState) -> Option<Part> {
        PARTS.into_iter().find(|&part| match part {
            Part::Sort | Part::View | Part::Settings => box_contains(self.rect_of(part), self.lean, p),
            Part::FilterClear => !app.tab().filter.is_empty() && self.rect_of(part).contains(p),
            _ => self.rect_of(part).contains(p),
        })
    }

    /// The Subfolders switch: a little folder tree (a stem with two branches ending in folders),
    /// red while it's on.
    fn paint_subfolders(r: &mut Renderer, c: Point, color: Color) {
        let x = c.x - 4.5;
        r.line(Point::new(x, c.y - 4.5), Point::new(x, c.y + 4.0), 1.2, color);
        r.line(Point::new(x, c.y - 0.5), Point::new(c.x + 0.5, c.y - 0.5), 1.2, color);
        r.line(Point::new(x, c.y + 4.0), Point::new(c.x + 0.5, c.y + 4.0), 1.2, color);
        r.fill_rect(Rect::new(x - 2.0, c.y - 7.0, 4.5, 3.5), color);
        r.fill_rect(Rect::new(c.x + 1.0, c.y - 2.5, 4.5, 3.5), color);
        r.fill_rect(Rect::new(c.x + 1.0, c.y + 2.0, 4.5, 3.5), color);
    }

    fn enabled(&self, part: Part, app: &AppState) -> bool {
        let t = app.tab();
        match part {
            Part::Back | Part::BackMenu => !t.back.is_empty(),
            Part::Forward | Part::ForwardMenu => !t.forward.is_empty(),
            Part::Up => !t.path.is_empty(),
            _ => true,
        }
    }

    fn path_hit(&self, p: Point) -> PathHit {
        if self.path_box.is_visible() {
            PathHit::Nothing
        } else {
            self.path.hit(p)
        }
    }

    // ------------------------------------------------------------ drawing

    fn paint_glyph(r: &mut Renderer, part: Part, b: Rect, color: Color) {
        Self::paint_glyph_w(r, part, b, color, 1.3);
    }

    /// A back, forward, up or refresh sign in `b`, its lines `w` DIPs thick.
    fn paint_glyph_w(r: &mut Renderer, part: Part, b: Rect, color: Color, w: f32) {
        let c = Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0);
        match part {
            Part::Back | Part::Forward => {
                let s = if part == Part::Back { 1.0 } else { -1.0 };
                r.line(Point::new(c.x - 6.0 * s, c.y), Point::new(c.x + 6.0 * s, c.y), w, color);
                r.polyline(&[Point::new(c.x - 1.0 * s, c.y - 5.0), Point::new(c.x - 6.0 * s, c.y), Point::new(c.x - 1.0 * s, c.y + 5.0)], w, color);
            }
            Part::Up => {
                r.line(Point::new(c.x, c.y + 6.0), Point::new(c.x, c.y - 6.0), w, color);
                r.polyline(&[Point::new(c.x - 5.0, c.y - 1.0), Point::new(c.x, c.y - 6.0), Point::new(c.x + 5.0, c.y - 1.0)], w, color);
            }
            Part::Refresh => {
                // Three quarters of a circle with an arrow head at its open end.
                let rad = 5.5;
                let pts: Vec<Point> = (0..=20)
                    .map(|i| {
                        let a = (-60.0 + i as f32 * 13.5).to_radians();
                        Point::new(c.x + rad * a.cos(), c.y + rad * a.sin())
                    })
                    .collect();
                r.polyline(&pts, w, color);
                let end = pts[0];
                r.polyline(&[Point::new(end.x - 4.0, end.y - 1.0), end, Point::new(end.x + 0.5, end.y + 4.0)], w, color);
            }
            _ => {}
        }
    }

    /// The sidebar switch: a little window, its left pane filled while the sidebar shows.
    fn paint_sidebar_switch(r: &mut Renderer, b: Rect, shown: bool, color: Color) {
        Self::paint_sidebar_switch_w(r, b, shown, color, 1.2);
    }

    /// The sidebar switch with its lines `lw` DIPs thick.
    fn paint_sidebar_switch_w(r: &mut Renderer, b: Rect, shown: bool, color: Color, lw: f32) {
        let w = Rect::new(b.x + (b.w - 16.0) / 2.0, b.y + (b.h - 12.0) / 2.0, 16.0, 12.0);
        if shown {
            r.fill_rect(Rect::new(w.x + 1.0, w.y + 1.0, 5.0, w.h - 2.0), color.with_alpha(color.a * 0.45));
        }
        r.stroke_round_rect(w, 1.5, lw, color);
        r.line(Point::new(w.x + 6.0, w.y + 0.6), Point::new(w.x + 6.0, w.bottom() - 0.6), lw, color);
    }

    /// One of the sidebar switch, back, forward, up and refresh as a glass marble (`buttons:
    /// liquid`) with its sign in white on it, a darker shadow under the sign. A button that can't
    /// be used is a faded grey marble; one under the mouse is lighter, one pressed darker.
    #[allow(clippy::too_many_arguments)]
    fn paint_liquid_button(r: &mut Renderer, theme: &Theme, part: Part, b: Rect, enabled: bool, hot: bool, pressed: bool, sidebar_shown: bool) {
        let c = Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0);
        let radius = (b.w.min(b.h) / 2.0 - 2.0).max(4.0);
        let glass = if !enabled {
            theme.chip_paint.map(|c| c.grey().lighter(0.15).with_alpha(c.a * 0.55))
        } else if pressed {
            theme.chip_paint.darker(0.15)
        } else if hot {
            theme.chip_hover_paint
        } else {
            theme.chip_paint
        };
        r.marble(c, radius, &glass);
        let ink = if enabled { theme.button_text } else { theme.button_text.with_alpha(theme.button_text.a * 0.8) };
        let shadow = text_shadow(theme);
        let shadow = shadow.with_alpha(shadow.a * if enabled { 1.0 } else { 0.25 });
        let below = Rect::new(b.x, b.y + 1.0, b.w, b.h);
        if part == Part::Sidebar {
            Self::paint_sidebar_switch_w(r, below, sidebar_shown, shadow, 1.9);
            Self::paint_sidebar_switch_w(r, b, sidebar_shown, ink, 1.7);
        } else {
            Self::paint_glyph_w(r, part, below, shadow, 2.0);
            Self::paint_glyph_w(r, part, b, ink, 1.8);
        }
    }

    /// Back or forward as a round glass button of its own (the skin's `back-button`: XP's green,
    /// Windows 7's blue) with a white arrow; grey while it can't be used.
    fn paint_back_button(r: &mut Renderer, theme: &Theme, part: Part, b: Rect, enabled: bool, hot: bool, pressed: bool) {
        let c = Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0);
        let radius = (b.w.min(b.h) / 2.0 - 1.0).max(4.0);
        let glass = if !enabled {
            theme.back_button.map(|c| c.grey().lighter(0.25))
        } else if pressed {
            theme.back_button.darker(0.18)
        } else if hot {
            theme.back_button.lighter(0.2)
        } else {
            theme.back_button
        };
        r.marble(c, radius, &glass);
        let below = Rect::new(b.x, b.y + 1.0, b.w, b.h);
        Self::paint_glyph_w(r, part, below, glass.last().darker(0.45).with_alpha(0.7), 2.2);
        Self::paint_glyph_w(r, part, b, Color::hex(0xffffff), 2.0);
    }

    /// SORT, VIEW or SETTINGS as a glossy capsule (`buttons: liquid`): the skin's `button` glass
    /// (`button-hover` under the mouse), its word in bold `button-text` with a dark shadow under it.
    fn paint_capsule(r: &mut Renderer, theme: &Theme, b: Rect, label: &str, hot: bool) {
        let capsule = b.inset(0.0, 2.0);
        r.liquid_capsule(capsule, if hot { &theme.chip_hover_paint } else { &theme.chip_paint });
        let style = TextStyle::new(theme.button_text).bold().size(size::FONT - 1.0).align(Align::Center);
        r.text(label, Rect::new(capsule.x, capsule.y + 1.0, capsule.w, capsule.h), &style.color(text_shadow(theme)));
        r.text(label, capsule, &style);
    }

    /// A magnifying glass: a ring and a handle going down to the right.
    fn paint_magnifier(r: &mut Renderer, c: Point, color: Color) {
        let (cx, cy, rad) = (c.x - 1.5, c.y - 1.5, 4.0);
        let ring: Vec<Point> = (0..=24)
            .map(|i| {
                let a = (i as f32 * 15.0).to_radians();
                Point::new(cx + rad * a.cos(), cy + rad * a.sin())
            })
            .collect();
        r.polyline(&ring, 1.2, color);
        let d = rad * std::f32::consts::FRAC_1_SQRT_2;
        r.line(Point::new(cx + d + 0.6, cy + d + 0.6), Point::new(c.x + 5.5, c.y + 5.5), 1.5, color);
    }

    fn paint_cross(r: &mut Renderer, c: Point, half: f32, width: f32, color: Color) {
        r.line(Point::new(c.x - half, c.y - half), Point::new(c.x + half, c.y + half), width, color);
        r.line(Point::new(c.x + half, c.y - half), Point::new(c.x - half, c.y + half), width, color);
    }

    // ------------------------------------------------------------ menus

    fn sort_menu(&self, app: &AppState) -> Effect {
        let t = app.tab();
        let b = self.rect_of(Part::Sort);
        // While the custom sort order decides here, no column is checked. (It can be on and still
        // leave a folder it has no rules for to its column.)
        let custom = app.settings.custom_sort;
        let here = t.custom_here().is_some();
        let key = |id, label, k| MenuItem::new(id, label).checked(t.sort == k && !here);
        Effect::Menu(Menu {
            tag: MENU_SORT,
            at: Point::new(b.x, b.bottom() + 2.0),
            items: vec![
                key(1, "Name", SortKey::Name),
                key(2, "Date modified", SortKey::Date),
                key(7, "Date created", SortKey::Created),
                key(3, "Type", SortKey::Type),
                key(4, "Size", SortKey::Size),
                key(10, "Picture size", SortKey::Dimensions),
                key(11, "Length", SortKey::Length),
                MenuItem::separator(),
                MenuItem::new(5, "Ascending").checked(!t.descending && !here),
                MenuItem::new(6, "Descending").checked(t.descending && !here),
                MenuItem::separator(),
                // (Not checked while a column sorts this folder for now: picking it brings the rules back.)
                MenuItem::new(8, "Custom order").checked(custom && !t.column_now),
                MenuItem::new(9, "Edit custom order\u{2026}"),
            ],
            right_aligned: false,
        })
    }

    /// VIEW: the six views (the tab's is checked) with Explorer's keys, the sidebar, status bar and
    /// split view switches, then custom views (folders' own views, written in the custom sort
    /// order's editor) last, as SORT has its custom order last.
    fn view_menu(&self, app: &AppState) -> Effect {
        let b = self.rect_of(Part::View);
        let mut items: Vec<MenuItem> = ViewMode::ALL
            .iter()
            .enumerate()
            .map(|(i, (v, name))| MenuItem::new(VIEW_FIRST + i as u32, &format!("{name}\tCtrl+Shift+{}", i + 1)).checked(app.tab().view_mode == *v))
            .collect();
        items.push(MenuItem::separator());
        items.push(MenuItem::new(VIEW_SIDEBAR, "Sidebar\tCtrl+B").checked(app.settings.show_sidebar));
        items.push(MenuItem::new(VIEW_STATUS_BAR, "Status bar").checked(app.settings.show_status_bar));
        items.push(MenuItem::new(VIEW_SPLIT, "Split view\tCtrl+\\").checked(app.split.is_some()));
        items.push(MenuItem::separator());
        items.push(MenuItem::new(VIEW_CUSTOM, "Custom views").checked(app.settings.custom_views));
        items.push(MenuItem::new(VIEW_RULES, "Edit custom views\u{2026}"));
        Effect::Menu(Menu { tag: MENU_VIEW, at: Point::new(b.x, b.bottom() + 2.0), items, right_aligned: false })
    }

    /// Right-click on SETTINGS (a click opens the Settings window).
    fn settings_menu(&mut self) -> Effect {
        let b = self.rect_of(Part::Settings);
        Effect::Menu(Menu {
            tag: MENU_SETTINGS,
            // Opens leftwards from the button's right end, so it stays inside the window.
            at: Point::new(b.right(), b.bottom() + 2.0),
            right_aligned: true,
            items: vec![
                MenuItem::new(SET_WINDOW, "Settings\u{2026}\tCtrl+,"),
                MenuItem::new(SET_ABOUT, "About Glint"),
                MenuItem::separator(),
                MenuItem::new(SET_QUIT, "Quit Glint\tCtrl+Q"),
            ],
        })
    }

    fn settings_pick(&mut self, item: u32) -> Effect {
        match item {
            SET_WINDOW => Effect::Command(Command::OpenSettings(None)),
            SET_ABOUT => Effect::Command(Command::OpenSetting(crate::settingswin::ABOUT, crate::settingswin::ABOUT_ROW.to_string())),
            SET_QUIT => Effect::Command(Command::Quit),
            _ => Effect::None,
        }
    }

    /// The popup of the steps hidden behind "…", nearest first, under it.
    /// XP's list beside Back or Forward: the places you've been, the nearest first.
    fn history_menu(&self, forward: bool, app: &AppState) -> Effect {
        let t = app.tab();
        let places = if forward { &t.forward } else { &t.back };
        let items = places.iter().rev().take(HISTORY_MAX).enumerate().map(|(i, p)| MenuItem::new(i as u32 + 1, &app.display_name(p).replace('&', "&&"))).collect();
        let b = self.rect_of(if forward { Part::Forward } else { Part::Back });
        Effect::Menu(Menu { tag: if forward { MENU_FORWARD } else { MENU_BACK }, at: Point::new(b.x, b.bottom() + 2.0), items, right_aligned: false })
    }

    fn hidden_menu(&mut self, at: Rect) -> Effect {
        let hidden = self.path.hidden_nearest_first();
        self.menu_paths = hidden.iter().map(|(_, p)| p.clone()).collect();
        let items = hidden.iter().enumerate().map(|(i, (name, _))| MenuItem::new(i as u32 + 1, &name.replace('&', "&&"))).collect();
        Effect::Menu(Menu { tag: MENU_HIDDEN, at: Point::new(at.x, self.rect_of(Part::Path).bottom() + 2.0), items, right_aligned: false })
    }

    /// Right-click on the path: about the step under the mouse, or the folder you're in.
    fn path_menu(&mut self, p: Point, app: &AppState) -> Effect {
        let step = match self.path_hit(p) {
            PathHit::Step(i) => self.path.crumb(i).filter(|c| !c.more).map(|c| c.path.clone()),
            _ => None,
        };
        let on_step = step.is_some();
        self.menu_target = step.unwrap_or_else(|| app.tab().path.clone());
        let mut items = Vec::new();
        if on_step {
            items.push(MenuItem::new(1, "Open in new tab"));
        }
        items.push(MenuItem::new(2, "Copy path"));
        items.push(MenuItem::separator());
        items.push(MenuItem::new(3, "Edit path\tCtrl+L"));
        Effect::Menu(Menu { tag: MENU_PATH, at: p, items, right_aligned: false })
    }

    fn copy_path(&self, ctx: &Ctx, path: &str, app: &mut AppState) {
        let text = if path.is_empty() { "This PC" } else { path };
        app.say(if shell::copy_text(ctx.hwnd, text) { "Path copied" } else { "Couldn't copy (the clipboard is busy)" });
    }
}

/// The shadow under white words and signs on liquid buttons: the glass's darkest colour, darker.
impl Toolbar {
    /// The toolbar of a skin with an older Windows (retrotool.rs): the era's band, its buttons
    /// and pictures, the Address band or 7's breadcrumb box, and its word buttons.
    fn paint_era(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, active: bool) {
        let era = theme.era;
        let seven = era == Era::Win7;
        let glass = Rect::new(self.rect.x, self.rect.y - size::TITLE_H, self.rect.w, self.rect.h + size::TITLE_H);
        retrotool::band(r, theme, self.rect, glass, active);

        if seven {
            let (back, forward) = (self.rect_of(Part::Back), self.rect_of(Part::Forward));
            retrotool::seven_socket(r, Rect::new(back.x, back.y, forward.right() - back.x, back.h));
        }
        let xp = era == Era::Xp;
        if xp {
            // Back and Forward with their drop arrows: under the mouse, one frame round both
            // parts with a line between them.
            for (main, drop) in [(Part::Back, Part::BackMenu), (Part::Forward, Part::ForwardMenu)] {
                let (m, d) = (self.rect_of(main), self.rect_of(drop));
                let enabled = self.enabled(main, app);
                if enabled && (self.hover == Some(main) || self.hover == Some(drop)) {
                    let pressed = self.hover == Some(main) && self.pressed == Some(main);
                    retrotool::xp_frame(r, theme, Rect::new(m.x, m.y, d.right() - m.x, m.h).inset(1.0, 1.0), Look { enabled, hot: true, pressed, checked: false });
                    r.fill_rect(Rect::new(d.x, d.y + 4.0, 1.0, d.h - 8.0), Color::rgba(0, 0, 0, 0.2));
                }
                let ink = if enabled { theme.button_text } else { theme.disabled };
                retrotool::triangle_down(r, Point::new(d.x + d.w / 2.0 - 1.0, d.y + d.h / 2.0), 5.0, ink);
            }
        }
        for part in [Part::Sidebar, Part::Back, Part::Forward, Part::Up, Part::Refresh] {
            if seven && part == Part::Refresh {
                continue;
            }
            let enabled = self.enabled(part, app);
            // (XP's Back and Forward have their frames drawn above, round their drop arrows too.)
            let framed = !(xp && matches!(part, Part::Back | Part::Forward));
            let hot = enabled && self.hover == Some(part) && framed;
            let look = Look { enabled, hot, pressed: hot && self.pressed == Some(part), checked: part == Part::Sidebar && app.settings.show_sidebar };
            let tool = match part {
                Part::Sidebar => Tool::Folders,
                Part::Back => Tool::Back,
                Part::Forward => Tool::Forward,
                Part::Up => Tool::Up,
                _ => Tool::Refresh,
            };
            retrotool::nav_button(r, theme, self.rect_of(part), tool, look);
        }
        if !seven {
            // A separator between the buttons and the address.
            let last = self.rect_of(Part::Refresh);
            let x = (last.right() + 3.0).round();
            if era == Era::Xp {
                // And one between Up and Folders, as XP had.
                let up = self.rect_of(Part::Up);
                for x in [x, (up.right() + 3.0).round()] {
                    r.fill_rect(Rect::new(x, last.y + 3.0, 1.0, last.h - 6.0), Color::hex(0xc5c2b8));
                    r.fill_rect(Rect::new(x + 1.0, last.y + 3.0, 1.0, last.h - 6.0), Color::hex(0xffffff));
                }
            } else {
                crate::retro::etched_upright(r, theme, x, last.y + 2.0, last.h - 4.0, theme.toolbar);
            }
        }

        // The address.
        let path = self.rect_of(Part::Path);
        let path_hot = self.hover == Some(Part::Path);
        if seven {
            let refresh = self.rect_of(Part::Refresh);
            let both = Rect::new(path.x, path.y, refresh.right() - path.x, path.h).center(refresh.right() - path.x, ERA_FIELD_H);
            retrotool::field(r, theme, both, path_hot || self.hover == Some(Part::Refresh) || self.path_box.is_visible());
            let inner = Rect::new(path.x, both.y, path.w, both.h);
            retrotool::drop_arrow(r, theme, inner, false);
            r.fill_rect(Rect::new(refresh.x, both.y + 1.0, 1.0, both.h - 2.0), Color::hex(0x8e9baa));
            let hot = self.hover == Some(Part::Refresh);
            let look = Look { enabled: true, hot, pressed: hot && self.pressed == Some(Part::Refresh), checked: false };
            retrotool::seven_refresh(r, Rect::new(refresh.x + 1.0, both.y + 1.0, refresh.w - 2.0, both.h - 2.0), look);
        } else {
            let label = Rect::new(path.x - ADDRESS_W, path.y, ADDRESS_W, path.h);
            retrotool::address_label(r, theme, label);
            let field = path.center(path.w, ERA_FIELD_H);
            retrotool::field(r, theme, field, path_hot);
            retrotool::drop_arrow(r, theme, field, false);
            let go = self.rect_of(Part::Go);
            let hot = self.hover == Some(Part::Go);
            retrotool::go_button(r, theme, go, Look { enabled: true, hot, pressed: hot && self.pressed == Some(Part::Go), checked: false });
        }
        let mouse = self.mouse.filter(|_| path_hot);
        self.path.paint(r, theme, app, self.path_box.is_visible(), mouse);

        // The filter: the era's box, its magnifier, and × while filtering.
        let f = self.rect_of(Part::Filter);
        retrotool::field(r, theme, f.center(f.w, ERA_FIELD_H), self.hover == Some(Part::Filter));
        let filtering = !app.tab().filter.is_empty();
        retrotool::magnifier(r, theme, Point::new(f.x + FILTER_ICON_W / 2.0 + 1.0, f.y + f.h / 2.0), filtering);
        if filtering {
            let x = self.rect_of(Part::FilterClear);
            let hot = self.hover == Some(Part::FilterClear);
            Self::paint_cross(r, Point::new(x.x + x.w / 2.0, x.y + x.h / 2.0), 3.5, 1.2, if hot { theme.text } else { theme.muted });
        }
        let sub = self.rect_of(Part::Subfolders);
        let color = if app.tab().path.is_empty() {
            theme.disabled
        } else if app.subfolders {
            theme.accent
        } else if self.hover == Some(Part::Subfolders) {
            theme.text
        } else {
            theme.muted
        };
        Self::paint_subfolders(r, Point::new(sub.x + sub.w / 2.0, sub.y + sub.h / 2.0), color);

        for (part, label, menu) in [(Part::Sort, "SORT", true), (Part::View, "VIEW", true), (Part::Settings, "SETTINGS", false)] {
            retrotool::word_button(r, theme, self.rect_of(part), label, self.hover == Some(part), menu);
        }
    }
}

fn text_shadow(theme: &Theme) -> Color {
    theme.chip_paint.last().darker(0.5).with_alpha(0.8)
}

impl Component for Toolbar {
    fn layout(&mut self, rect: Rect, ctx: &Ctx, app: &AppState) {
        self.rect = rect;
        self.era = app.theme.era;
        self.lean = app.theme.lean(WORD_LEAN);
        self.line_w = app.theme.toolbar_line_w;
        let path = self.rect_of(Part::Path);
        self.path.set_rect(path);
        self.path.set_style(match self.era {
            Era::None => Crumbs::Modern,
            Era::Win7 => Crumbs::Seven,
            _ => Crumbs::Plain,
        });
        if self.era.old() {
            // Typing goes after the place's icon, short of the drop arrow.
            let (lead, trail) = if self.era == Era::Win7 { (24.0, 20.0) } else { (22.0, 20.0) };
            self.path_box.place(Rect::new(path.x + lead, path.y, (path.w - lead - trail).max(10.0), path.h), 0.0, ctx.scale);
        } else {
            self.path_box.place(path, 9.0, ctx.scale);
        }
        self.filter_box.place(self.filter_edit_rect(), 0.0, ctx.scale);
        self.filter_box.show();
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, _ctx: &Ctx) {
        if theme.era.old() {
            return self.paint_era(r, theme, app, _ctx.active);
        }
        r.fill_paint(self.rect, 0.0, &theme.toolbar_paint);
        r.fill_rect(Rect::new(self.rect.x, self.rect.bottom() - self.line_w, self.rect.w, self.line_w), theme.toolbar_line);

        let liquid = theme.buttons == ButtonStyle::Liquid;
        for part in [Part::Sidebar, Part::Back, Part::Forward, Part::Up, Part::Refresh] {
            let b = self.rect_of(part);
            let enabled = self.enabled(part, app);
            let hot = enabled && self.hover == Some(part);
            let pressed = hot && self.pressed == Some(part);
            if liquid {
                Self::paint_liquid_button(r, theme, part, b, enabled, hot, pressed, app.settings.show_sidebar);
                continue;
            }
            if matches!(part, Part::Back | Part::Forward) && !theme.back_button.is_clear() {
                Self::paint_back_button(r, theme, part, b, enabled, hot, pressed);
                continue;
            }
            if theme.tool_border.a > 0.0 && hot {
                let face = b.inset(1.0, 1.0);
                r.fill_paint(face, 3.0, &crate::theme::Paint::Flat(if pressed { theme.button_pressed } else { theme.button_hover }));
                if !pressed {
                    let shine = Rect::new(face.x, face.y, face.w, face.h / 2.0);
                    r.fill_paint(shine, 3.0, &crate::theme::Paint::even(&[Color::rgba(255, 255, 255, 0.7), Color::rgba(255, 255, 255, 0.0)]));
                }
                r.stroke_round_rect(face.inset(0.5, 0.5), 2.5, 1.0, theme.tool_border);
            } else if pressed {
                r.fill_rect(b, theme.button_pressed);
            } else if hot {
                r.fill_rect(b, theme.button_hover);
            }
            if theme.bevels && hot {
                crate::pathbar::bevel(r, theme, b, !pressed, theme.toolbar);
            }
            let color = if !enabled {
                theme.disabled
            } else if hot {
                theme.text
            } else {
                theme.muted
            };
            if part == Part::Sidebar {
                Self::paint_sidebar_switch(r, b, app.settings.show_sidebar, color);
            } else {
                Self::paint_glyph(r, part, b, color);
            }
        }

        let mouse = self.mouse.filter(|_| self.hover == Some(Part::Path));
        self.path.paint(r, theme, app, self.path_box.is_visible(), mouse);

        // Filter: magnifier, the text box (a child window drawn by Windows), and × while filtering.
        let f = self.rect_of(Part::Filter);
        fill_field(r, theme, f);
        let filtering = !app.tab().filter.is_empty();
        let mag = Point::new(f.x + FILTER_ICON_W / 2.0 + 1.0, f.y + f.h / 2.0);
        Self::paint_magnifier(r, mag, if filtering { theme.accent } else { theme.muted });
        if filtering {
            let x = self.rect_of(Part::FilterClear);
            let hot = self.hover == Some(Part::FilterClear);
            if hot {
                fill_box(r, theme, x.center(20.0, 20.0), 0.0, theme.button_hover);
            }
            Self::paint_cross(r, Point::new(x.x + x.w / 2.0, x.y + x.h / 2.0), 3.5, 1.2, if hot { theme.text } else { theme.muted });
        }
        // The Subfolders switch: red while on, grey (and doing nothing but saying why) in This PC.
        let sub = self.rect_of(Part::Subfolders);
        let hot = self.hover == Some(Part::Subfolders);
        if hot {
            fill_box(r, theme, sub.center(20.0, 20.0), 0.0, theme.button_hover);
        }
        let color = if app.tab().path.is_empty() {
            theme.disabled
        } else if app.subfolders {
            theme.accent
        } else if hot {
            theme.text
        } else {
            theme.muted
        };
        Self::paint_subfolders(r, Point::new(sub.x + sub.w / 2.0, sub.y + sub.h / 2.0), color);

        // SORT, VIEW and SETTINGS: word buttons in the skin's shape, or glossy capsules.
        for (part, label) in [(Part::Sort, "SORT"), (Part::View, "VIEW"), (Part::Settings, "SETTINGS")] {
            let b = self.rect_of(part);
            let hot = self.hover == Some(part);
            // (Their menus open as the button goes down, so there is no "pressed" look to show.)
            if liquid {
                Self::paint_capsule(r, theme, b, label, hot);
            } else {
                if theme.bevels || crate::kit::old_windows(theme) {
                    let look = crate::kit::Look { hover: hot, enabled: true, ..crate::kit::Look::default() };
                    crate::kit::push_button(r, theme, b.inset(1.0, 1.0), label, look, false);
                    continue;
                }
                fill_box_paint(r, theme, b, self.lean, if hot { &theme.chip_hover_paint } else { &theme.chip_paint });
                r.text(label, b, &TextStyle::new(theme.button_text).semibold().align(Align::Center));
            }
        }
    }

    fn cursor(&self, p: Point, app: &AppState) -> CursorKind {
        match self.part_at(p, app) {
            Some(Part::Path) => match self.path_hit(p) {
                PathHit::Step(_) => CursorKind::Hand,
                PathHit::Empty => CursorKind::IBeam,
                PathHit::Nothing => CursorKind::Arrow,
            },
            Some(Part::Filter) => CursorKind::IBeam,
            _ => CursorKind::Arrow,
        }
    }

    fn mouse_move(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        let hover = self.part_at(e.pos, app);
        let before = self.mouse.map(|m| self.path_hit(m));
        self.mouse = (hover == Some(Part::Path)).then_some(e.pos);
        let after = self.mouse.map(|m| self.path_hit(m));
        if hover != self.hover || before != after {
            self.hover = hover;
            return Effect::Redraw;
        }
        Effect::None
    }

    fn mouse_leave(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        let had = self.hover.take().is_some();
        self.mouse = None;
        if had {
            Effect::Redraw
        } else {
            Effect::None
        }
    }

    fn mouse_down(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.pressed = None;
        self.pressed_step = None;
        self.go_text = None;
        let part = self.part_at(e.pos, app);
        if e.button == MouseButton::Right {
            // The menu comes on the way up, like everywhere in Windows.
            if part == Some(Part::Path) && !self.path_box.is_visible() {
                self.pressed = Some(Part::Path);
            }
            // SETTINGS: right-click for the quick choices (a click opens the Settings window).
            if part == Some(Part::Settings) {
                self.pressed = Some(Part::Settings);
            }
            return Effect::None;
        }
        if e.button == MouseButton::Middle {
            if let (Some(Part::Path), PathHit::Step(i)) = (part, self.path_hit(e.pos)) {
                self.pressed_step = Some((i, MouseButton::Middle));
            }
            return Effect::None;
        }
        match part {
            Some(Part::Path) if self.path_box.is_visible() => {
                // A click on the box around the text being typed: keep typing.
                self.path_box.focus();
                Effect::None
            }
            Some(Part::Path) => match self.path_hit(e.pos) {
                PathHit::Step(i) => {
                    self.pressed_step = Some((i, MouseButton::Left));
                    Effect::Redraw
                }
                PathHit::Empty => self.begin_path_edit(ctx, app),
                PathHit::Nothing => Effect::None,
            },
            Some(Part::Filter) => self.focus_filter(ctx, app),
            Some(Part::Sort) => self.sort_menu(app),
            Some(Part::View) => self.view_menu(app),
            Some(p @ (Part::BackMenu | Part::ForwardMenu)) if self.enabled(p, app) => self.history_menu(p == Part::ForwardMenu, app),
            Some(Part::Go) if self.path_box.is_visible() => {
                // Go for what is typed: keep it, and keep the box open (the window took the
                // keyboard from it for this click) until the button comes up.
                self.go_text = Some(self.path_box.text());
                self.path_box.focus();
                self.pressed = Some(Part::Go);
                Effect::Redraw
            }
            Some(p) if self.enabled(p, app) => {
                self.pressed = Some(p);
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    fn double_click(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        // Double-clicking the path switches to typing (the first click already went to the step).
        if e.button == MouseButton::Left && self.part_at(e.pos, app) == Some(Part::Path) && !self.path_box.is_visible() {
            return self.begin_path_edit(ctx, app);
        }
        Effect::None
    }

    fn mouse_up(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        let (pressed, step) = (self.pressed.take(), self.pressed_step.take());
        let go_text = self.go_text.take();
        if e.button == MouseButton::Right {
            if pressed == Some(Part::Path) && self.part_at(e.pos, app) == Some(Part::Path) {
                return self.path_menu(e.pos, app);
            }
            if pressed == Some(Part::Settings) && self.part_at(e.pos, app) == Some(Part::Settings) {
                return self.settings_menu();
            }
            return Effect::None;
        }
        if let Some((i, button)) = step {
            if button != e.button || self.path_hit(e.pos) != PathHit::Step(i) {
                return Effect::Redraw;
            }
            let Some(c) = self.path.crumb(i).cloned() else { return Effect::Redraw };
            return match button {
                MouseButton::Left if c.more => self.hidden_menu(c.rect),
                // The folder you're in too: going there again reloads it.
                MouseButton::Left => Effect::Command(Command::Navigate(c.path)),
                MouseButton::Middle if !c.more => Effect::Command(Command::NewTabAt(c.path)),
                _ => Effect::Redraw,
            };
        }
        if pressed.is_none() || pressed != self.part_at(e.pos, app) {
            return Effect::Redraw;
        }
        match pressed {
            // Go: to what is typed (when it went down), or the folder you're in again.
            Some(Part::Go) => {
                let typed = go_text.or_else(|| self.path_box.is_visible().then(|| self.path_box.text()));
                if typed.is_some() {
                    self.hide_path_box(_ctx, true);
                }
                Effect::Command(go_command(typed))
            }
            Some(Part::Back) => Effect::Command(Command::Back),
            Some(Part::Forward) => Effect::Command(Command::Forward),
            Some(Part::Up) => Effect::Command(Command::Up),
            Some(Part::Refresh) => Effect::Command(Command::Refresh),
            Some(Part::Settings) => Effect::Command(Command::OpenSettings(None)),
            Some(Part::Sidebar) => Effect::Command(Command::ToggleSidebar),
            Some(Part::FilterClear) => {
                self.filter_box.set_text("");
                self.shown_filter.clear();
                Effect::Command(Command::SetFilter(String::new()))
            }
            Some(Part::Subfolders) => Effect::Command(Command::ToggleSubfolders),
            _ => Effect::Redraw,
        }
    }

    fn text_event(&mut self, ctx: &Ctx, ev: &TextEvent, app: &mut AppState) -> Effect {
        if ev.id == self.path_box.id() {
            return match ev.kind {
                TextEventKind::Enter => {
                    let text = self.path_box.text();
                    self.hide_path_box(ctx, true);
                    Effect::Command(Command::Navigate(text))
                }
                TextEventKind::Escape => {
                    self.hide_path_box(ctx, true);
                    Effect::Redraw
                }
                // Focus left the box (a click elsewhere, Alt+Tab). The message was posted, so the box
                // may have the focus back by now (a click on the path box's own padding).
                TextEventKind::LostFocus if self.path_box.is_visible() && !self.path_box.has_focus() => {
                    self.hide_path_box(ctx, false);
                    Effect::Redraw
                }
                _ => Effect::None,
            };
        }
        if ev.id == self.filter_box.id() {
            return match ev.kind {
                TextEventKind::Changed => {
                    self.shown_filter = self.filter_box.text();
                    Effect::Command(Command::SetFilter(self.shown_filter.clone()))
                }
                TextEventKind::Escape => {
                    self.filter_box.set_text("");
                    self.shown_filter.clear();
                    self.filter_box.give_focus_back();
                    Effect::Command(Command::SetFilter(String::new()))
                }
                TextEventKind::Enter | TextEventKind::Down => {
                    // The keyboard goes to the list, with the first match selected if nothing is;
                    // Enter also opens what's selected.
                    let t = app.tab_mut();
                    if t.selected_entries().is_empty() && !t.view.is_empty() && !t.waiting_long() {
                        t.select_only_row(0);
                        t.reveal_focus = true;
                    }
                    let open = ev.kind == TextEventKind::Enter && !t.selected_entries().is_empty() && !t.waiting_long();
                    self.filter_box.give_focus_back();
                    if open {
                        Effect::Command(Command::OpenSelected)
                    } else {
                        Effect::Redraw
                    }
                }
                TextEventKind::LostFocus | TextEventKind::Tab => Effect::None,
            };
        }
        Effect::None
    }

    fn menu_pick(&mut self, ctx: &Ctx, tag: u32, item: u32, app: &mut AppState) -> Effect {
        match (tag, item) {
            (MENU_SORT, 1..=4) => {
                let key = [SortKey::Name, SortKey::Date, SortKey::Type, SortKey::Size][item as usize - 1];
                Effect::Command(Command::SetSort(key, app.tab().descending))
            }
            (MENU_SORT, 5 | 6) => Effect::Command(Command::SetSort(app.tab().sort, item == 6)),
            (MENU_SORT, 7) => Effect::Command(Command::SetSort(SortKey::Created, app.tab().descending)),
            (MENU_SORT, 10) => Effect::Command(Command::SetSort(SortKey::Dimensions, app.tab().descending)),
            (MENU_SORT, 11) => Effect::Command(Command::SetSort(SortKey::Length, app.tab().descending)),
            (MENU_SORT, 8) => Effect::Command(Command::SetCustomSort(!app.settings.custom_sort || app.tab().column_now)),
            (MENU_SORT, 9) => Effect::Command(Command::OpenSortEditor),
            (MENU_SETTINGS, _) => self.settings_pick(item),
            (MENU_VIEW, VIEW_RULES) => Effect::Command(Command::OpenSortEditor),
            (MENU_VIEW, VIEW_CUSTOM) => Effect::Command(Command::SetCustomViews(!app.settings.custom_views)),
            (MENU_VIEW, VIEW_SIDEBAR) => Effect::Command(Command::ToggleSidebar),
            (MENU_VIEW, VIEW_STATUS_BAR) => Effect::Command(Command::ShowStatusBar(!app.settings.show_status_bar)),
            (MENU_VIEW, VIEW_SPLIT) => Effect::Command(Command::ToggleSplit),
            (MENU_VIEW, n) => match n.checked_sub(VIEW_FIRST).and_then(|i| ViewMode::ALL.get(i as usize)) {
                Some((v, _)) => Effect::Command(Command::SetView(*v)),
                None => Effect::None,
            },
            (MENU_HIDDEN, n) => match n.checked_sub(1).and_then(|i| self.menu_paths.get(i as usize)) {
                Some(path) => Effect::Command(Command::Navigate(path.clone())),
                None => Effect::None,
            },
            (MENU_PATH, 1) => Effect::Command(Command::NewTabAt(self.menu_target.clone())),
            (MENU_PATH, 2) => {
                let path = self.menu_target.clone();
                self.copy_path(ctx, &path, app);
                Effect::Redraw
            }
            (MENU_PATH, 3) => self.begin_path_edit(ctx, app),
            (MENU_BACK, n) => Effect::Command(Command::BackBy(n as usize)),
            (MENU_FORWARD, n) => Effect::Command(Command::ForwardBy(n as usize)),
            _ => Effect::None,
        }
    }

    /// A step of the path: things dropped on it go into that folder (not "…", not This PC).
    fn drop_target(&self, p: Point, app: &AppState) -> Option<DropTarget> {
        if self.part_at(p, app) != Some(Part::Path) {
            return None;
        }
        let PathHit::Step(i) = self.path_hit(p) else { return None };
        let step = self.path.crumb(i).filter(|c| !c.more && !c.path.is_empty())?;
        Some(DropTarget::Folder(step.path.clone()))
    }

    fn drop_hover(&mut self, target: Option<&DropTarget>) -> Effect {
        let path = match target {
            Some(DropTarget::Folder(p)) => Some(p.clone()),
            _ => None,
        };
        if path == self.path.drop_path {
            return Effect::None;
        }
        self.path.drop_path = path;
        Effect::Redraw
    }

    fn tick(&mut self, now: f64, app: &mut AppState) -> bool {
        // Navigation clears the filter and each tab has its own: keep the box showing the truth.
        let tab = app.tab();
        if tab.filter != self.shown_filter || tab.id != self.shown_tab {
            self.shown_filter = tab.filter.clone();
            self.shown_tab = tab.id;
            self.filter_box.set_text(&self.shown_filter);
        }
        // The grey hint says what the filter does: filter this folder, or search below it too.
        let subfolders = app.subfolders && !tab.path.is_empty();
        if subfolders != self.hint_subfolders {
            self.hint_subfolders = subfolders;
            self.filter_box.set_hint(if subfolders { "Search subfolders" } else { "Filter" });
        }
        now < self.repaint_until
    }
}

/// What Go does: goes to what was typed in the path box, or (nothing typed) loads the folder again.
fn go_command(typed: Option<String>) -> Command {
    match typed {
        Some(text) => Command::Navigate(text),
        None => Command::Refresh,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_goes_to_what_was_typed() {
        assert!(matches!(go_command(Some("D:\\Work".into())), Command::Navigate(p) if p == "D:\\Work"));
        assert!(matches!(go_command(Some("C:\\Fotos 📷\\😀".into())), Command::Navigate(p) if p == "C:\\Fotos 📷\\😀"));
        // An emptied box still goes (the window says what's wrong), never just reloads.
        assert!(matches!(go_command(Some(String::new())), Command::Navigate(p) if p.is_empty()));
        assert!(matches!(go_command(None), Command::Refresh));
    }
}
