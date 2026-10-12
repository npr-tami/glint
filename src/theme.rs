//! The Glint look: the active skin's colours and shapes as plain values (`Theme`), and the sizes every
//! part of the window uses.
//!
//! Skins are small text files (skin.rs). The one in use is read into a `Theme` once, when it's
//! chosen or changes, and every part draws with that - so drawing costs the same as hard-coded
//! colours. The default is AMD Radeon (skins/amd-radeon.glint). Sizes are in DIPs
//! ("device independent pixels", 1/96 inch): the renderer scales them to the screen's DPI, so a
//! 28-DIP row is 28 pixels at 100 % and 42 pixels at 150 %. Never multiply by the DPI yourself.

/// A colour with straight (not premultiplied) alpha, each part 0.0 - 1.0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    /// An opaque colour from 0xRRGGBB, e.g. `Color::hex(0xe3262d)`.
    pub const fn hex(rgb: u32) -> Color {
        Color {
            r: ((rgb >> 16) & 0xff) as f32 / 255.0,
            g: ((rgb >> 8) & 0xff) as f32 / 255.0,
            b: (rgb & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    /// A see-through colour, like CSS `rgba(227, 38, 45, 0.16)`.
    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
        Color { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
    }

    /// The same colour with a different opacity.
    pub const fn with_alpha(self, a: f32) -> Color {
        Color { a, ..self }
    }

    /// The colour as a GDI COLORREF (0x00BBGGRR), for the few Win32 controls we use (text boxes).
    pub fn to_colorref(self) -> u32 {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        c(self.r) | (c(self.g) << 8) | (c(self.b) << 16)
    }

    /// The other way round: an opaque colour from a GDI COLORREF (Windows' colour picker).
    pub fn from_colorref(c: u32) -> Color {
        Color::hex(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff))
    }

    /// How light the colour looks, 0 (black) to 1 (white).
    pub fn lightness(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    /// `t` of the way from this colour to `other` (0: this one, 1: the other), opacity too.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let m = |a: f32, b: f32| a + (b - a) * t;
        Color { r: m(self.r, other.r), g: m(self.g, other.g), b: m(self.b, other.b), a: m(self.a, other.a) }
    }

    /// Lighter: `t` of the way to white, keeping its opacity.
    pub fn lighter(self, t: f32) -> Color {
        self.mix(Color { a: self.a, ..Color::hex(0xffffff) }, t)
    }

    /// Darker: `t` of the way to black, keeping its opacity.
    pub fn darker(self, t: f32) -> Color {
        self.mix(Color { a: self.a, ..Color::hex(0) }, t)
    }

    /// The grey of the same lightness (a disabled button's marble).
    #[allow(dead_code)]
    pub fn grey(self) -> Color {
        let l = self.lightness();
        Color { r: l, g: l, b: l, a: self.a }
    }
}

/// The most colours a gradient has.
pub const MAX_STOPS: usize = 8;

/// One colour of a gradient and where it sits along it: 0 at the start, 1 at the end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub at: f32,
    pub color: Color,
}

/// A gradient of 2 to `MAX_STOPS` colours, their positions in order from 0 to 1. Kept in a fixed
/// array, so a paint is a plain value: copying one or keeping it in a `Theme` never allocates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gradient {
    stops: [Stop; MAX_STOPS],
    len: u8,
}

impl Gradient {
    /// From `stops` (at most `MAX_STOPS` are kept): positions are kept between 0 and 1 and never
    /// go back (one before the stop ahead of it moves up to it, as in CSS). None for fewer than two.
    pub fn new(stops: &[Stop]) -> Option<Gradient> {
        if stops.len() < 2 {
            return None;
        }
        let mut g = Gradient { stops: [Stop { at: 0.0, color: Color::rgba(0, 0, 0, 0.0) }; MAX_STOPS], len: 0 };
        let mut last = 0.0f32;
        for s in stops.iter().take(MAX_STOPS) {
            let at = if s.at.is_finite() { s.at.clamp(0.0, 1.0) } else { last };
            last = at.max(last);
            g.stops[g.len as usize] = Stop { at: last, color: s.color };
            g.len += 1;
        }
        Some(g)
    }

    /// The stops, first to last.
    pub fn stops(&self) -> &[Stop] {
        &self.stops[..self.len as usize]
    }

    /// The colour at `t` (0 to 1) along the gradient; before the first stop the first colour,
    /// after the last the last one.
    pub fn color_at(&self, t: f32) -> Color {
        let stops = self.stops();
        let (first, last) = (stops[0], stops[stops.len() - 1]);
        if t <= first.at {
            return first.color;
        }
        for pair in stops.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if t <= b.at {
                let span = b.at - a.at;
                return if span <= 0.0 { b.color } else { a.color.mix(b.color, (t - a.at) / span) };
            }
        }
        last.color
    }

    /// The same gradient with every colour changed by `f`.
    fn map(mut self, f: impl Fn(Color) -> Color) -> Gradient {
        for s in &mut self.stops[..self.len as usize] {
            s.color = f(s.color);
        }
        self
    }
}

/// What a surface is filled with: one colour, or a gradient (skin.rs: any colour a skin sets can
/// be written as a gradient). Draw one with `Renderer::fill_paint`.
///
/// A gradient runs across the short side of the shape it fills: top to bottom on a wide shape (a
/// bar, a button, a row), left to right on a tall one (a vertical scrollbar's thumb). So one paint
/// looks right on a title bar and on a scrollbar alike.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Flat(Color),
    Gradient(Gradient),
}

impl Default for Paint {
    fn default() -> Paint {
        Paint::CLEAR
    }
}

impl From<Color> for Paint {
    fn from(c: Color) -> Paint {
        Paint::Flat(c)
    }
}

// (Not every part draws with all of these yet.)
#[allow(dead_code)]
impl Paint {
    /// Nothing: a fill with it draws nothing.
    pub const CLEAR: Paint = Paint::Flat(Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 });

    /// A gradient of `colors` spread evenly from start to end (one colour: that colour, flat).
    pub fn even(colors: &[Color]) -> Paint {
        let n = colors.len().min(MAX_STOPS);
        let mut stops = [Stop { at: 0.0, color: Color::rgba(0, 0, 0, 0.0) }; MAX_STOPS];
        for (i, c) in colors.iter().take(n).enumerate() {
            stops[i] = Stop { at: if n > 1 { i as f32 / (n - 1) as f32 } else { 0.0 }, color: *c };
        }
        Paint::from_stops(&stops[..n])
    }

    /// A gradient of `stops`; fewer than two stops make a flat paint of the one colour (or nothing).
    pub fn from_stops(stops: &[Stop]) -> Paint {
        match Gradient::new(stops) {
            Some(g) => Paint::Gradient(g),
            None => stops.first().map_or(Paint::CLEAR, |s| Paint::Flat(s.color)),
        }
    }

    /// One colour that stands for the paint, for code that needs a single colour (a Windows text
    /// box's background, telling whether text on it should be light or dark): the colour itself,
    /// or a gradient's average along its length.
    pub fn base(&self) -> Color {
        let g = match self {
            Paint::Flat(c) => return *c,
            Paint::Gradient(g) => g,
        };
        let stops = g.stops();
        let (first, last) = (stops[0], stops[stops.len() - 1]);
        // The ends hold their colours out to 0 and 1; each span between stops counts by its length.
        let mut sum = [0.0f32; 4];
        let mut add = |c: Color, w: f32| {
            sum[0] += c.r * w;
            sum[1] += c.g * w;
            sum[2] += c.b * w;
            sum[3] += c.a * w;
        };
        add(first.color, first.at);
        add(last.color, 1.0 - last.at);
        for pair in stops.windows(2) {
            let w = pair[1].at - pair[0].at;
            add(pair[0].color.mix(pair[1].color, 0.5), w);
        }
        Color { r: sum[0], g: sum[1], b: sum[2], a: sum[3] }
    }

    /// The gradient, if it is one.
    pub fn gradient(&self) -> Option<&Gradient> {
        match self {
            Paint::Flat(_) => None,
            Paint::Gradient(g) => Some(g),
        }
    }

    /// The colour at the start (top or left) and at the end (bottom or right).
    pub fn first(&self) -> Color {
        match self {
            Paint::Flat(c) => *c,
            Paint::Gradient(g) => g.stops()[0].color,
        }
    }
    pub fn last(&self) -> Color {
        match self {
            Paint::Flat(c) => *c,
            Paint::Gradient(g) => g.stops()[g.stops().len() - 1].color,
        }
    }

    /// The colour at `t` (0 to 1) along it.
    pub fn color_at(&self, t: f32) -> Color {
        match self {
            Paint::Flat(c) => *c,
            Paint::Gradient(g) => g.color_at(t),
        }
    }

    /// Whether filling with it shows nothing (every colour fully see-through).
    pub fn is_clear(&self) -> bool {
        match self {
            Paint::Flat(c) => c.a <= 0.0,
            Paint::Gradient(g) => g.stops().iter().all(|s| s.color.a <= 0.0),
        }
    }

    /// The same paint with every colour changed by `f`.
    pub fn map(self, f: impl Fn(Color) -> Color) -> Paint {
        match self {
            Paint::Flat(c) => Paint::Flat(f(c)),
            Paint::Gradient(g) => Paint::Gradient(g.map(f)),
        }
    }

    /// At a strength: every colour's opacity times `s` (a skin's "accent 16%").
    pub fn with_strength(self, s: f32) -> Paint {
        self.map(|c| c.with_alpha(c.a * s))
    }

    /// Lighter or darker by `t` (0 to 1) - a button under the mouse, or pressed.
    pub fn lighter(self, t: f32) -> Paint {
        self.map(|c| c.lighter(t))
    }
    pub fn darker(self, t: f32) -> Paint {
        self.map(|c| c.darker(t))
    }
}

/// How the toolbar's buttons look: the skin's `buttons`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Signs on the toolbar, boxes in the skin's shape for SORT, VIEW and SETTINGS.
    #[default]
    Flat,
    /// Glossy glass: round marbles with white signs for back, forward, up and refresh, shiny
    /// capsules with white words for SORT, VIEW and SETTINGS (the `button` paint).
    Liquid,
}

/// How minimise, maximise and close look: the skin's `caption-buttons`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptionStyle {
    /// Windows' signs (_, a square, ×) on a plain button.
    #[default]
    Symbols,
    /// Three small glass marbles without signs; close in its own colour (`marble-close`).
    Marbles,
    /// Windows XP and 7: small rounded glass tiles with white signs and a white edge, close in its
    /// own colour (`marble` and `marble-close`).
    Tiles,
}

/// How the tabs sit in the title bar: the skin's `tabs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabStyle {
    /// The current tab runs into the toolbar below it; the others are just their names.
    #[default]
    Attached,
    /// Every tab a rounded box of its own with an edge, inside the title bar.
    Pills,
}

/// How the sidebar is laid out: the skin's `sidebar-style`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarStyle {
    /// Headings and places straight on the sidebar.
    #[default]
    Plain,
    /// Each group (Quick access, Pinned, This PC) in a rounded panel with a header bar.
    Panels,
}

/// How the skin cuts buttons and boxes (SORT, SETTINGS, the folder you're in, a logo word's box, the
/// buttons and switches of Glint's own windows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shape {
    /// Leaning like "/" (AMD Radeon): both sides slanted by the skin's `slant`.
    #[default]
    Slanted,
    /// Plain square boxes (NVIDIA).
    Straight,
    /// Boxes with round corners (the skin's `radius`).
    Rounded,
}

/// How far SORT and SETTINGS lean in AMD Radeon; every other slant is in proportion to it.
pub const STANDARD_SLANT: f32 = 8.0;

/// Which older Windows a skin dresses Glint as: the skin's `era`. It picks how the parts that
/// Windows drew its own way then are drawn (caption buttons, the Address bar, the toolbar's
/// pictures, scroll bars, column titles, menus, the controls of Glint's own windows). None for a
/// skin of today, which is drawn as Glint always was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Era {
    #[default]
    None,
    /// Windows 98 (Eggplant): Windows 2000's 3D edges, its toolbar pictures grey until the mouse
    /// comes.
    Win98,
    /// Windows 2000: grey raised and sunken 3D edges.
    Win2000,
    /// Windows XP (Luna, Silver, Royale): rounded glossy controls.
    Xp,
    /// Windows 7: glass, soft blue highlights.
    Win7,
}

impl Era {
    /// Windows 98 and 2000: the grey 3D edges.
    pub fn classic(self) -> bool {
        matches!(self, Era::Win98 | Era::Win2000)
    }
    /// Any older Windows.
    pub fn old(self) -> bool {
        self != Era::None
    }
}

/// Whose icons the places, drives, folders and files get: the skin's `icons`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconSet {
    /// Windows' own icons (as Explorer shows them today).
    #[default]
    Windows,
    /// Windows 98 and 2000's: flat, hard-edged, a black shadow line.
    Classic,
    /// Windows XP's: soft and rounded, lit from the top left.
    Xp,
    /// Windows 7's: glossy and light.
    Aero,
}

/// The active skin as plain values: every colour the window uses, by role, and the shapes and text.
/// Pick the role, not the hex value, so every skin works. It is made from a skin's text once, when
/// the skin is chosen or changed (skin.rs).
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub dark: bool,
    /// The darkest layer: the gaps between the panels.
    pub window: Color,
    /// The title bar (logo, tabs, minimise / maximise / close).
    pub titlebar: Color,
    /// The toolbar band under the title bar, and the active tab.
    pub toolbar: Color,
    /// The file list panel.
    pub panel: Color,
    /// The sidebar, and the status bar.
    pub sidebar: Color,
    pub statusbar: Color,
    /// Text boxes (path, filter).
    pub input: Color,
    /// Word buttons (SORT, SETTINGS) and the current step of the path.
    pub chip: Color,
    pub chip_hover: Color,
    /// Normal text.
    pub text: Color,
    /// Text on a selected row or the current path step (white in dark, near-black in light).
    pub text_strong: Color,
    /// Secondary text: headings, column titles, inactive tabs, the status bar, hidden files.
    pub muted: Color,
    /// Disabled buttons (e.g. Back with no history).
    pub disabled: Color,
    /// The one strong colour (AMD Radeon's red, NVIDIA's green).
    pub accent: Color,
    /// Selected rows: normally the accent at low opacity, drawn over the row.
    pub accent_tint: Color,
    /// The edge on the left of a selected row.
    pub select_edge: Color,
    /// Text and signs on the accent (close button hover, a switch's knob, the yes button).
    pub on_accent: Color,
    /// A row under the mouse (list and sidebar).
    pub hover: Color,
    /// A toolbar button under the mouse.
    pub button_hover: Color,
    pub button_pressed: Color,
    /// A path step under the mouse.
    pub crumb_hover: Color,
    /// Thin separators (under the list header, around menus).
    pub line: Color,
    pub scroll_thumb: Color,
    pub scroll_thumb_hover: Color,
    /// The background behind a drive's usage bar (the used part is `accent`).
    pub bar_track: Color,
    /// The logo's box and its letters, when the skin's logo is a word (`logo_text`).
    pub logo: Color,
    pub logo_ink: Color,
    /// The line on top of the active tab, and the one along the bottom of the toolbar.
    pub tab_line: Color,
    pub toolbar_line: Color,
    /// "+", minimise and maximise under the mouse (and pressed); close under the mouse.
    pub caption_hover: Color,
    pub caption_pressed: Color,
    pub close_hover: Color,
    /// Windows' own popup menus in dark (or light).
    pub menus_dark: bool,
    /// How buttons and boxes are cut, how far they lean (SORT and SETTINGS; the folder you're in)
    /// and how round their corners are, in DIPs.
    pub shape: Shape,
    pub slant: f32,
    pub path_slant: f32,
    pub radius: f32,
    /// The file list's rows at the "Small" size (Medium and Large add to it).
    pub row_h: f32,
    /// The sidebar's rows (`sidebar-row-height`; Glint's own 28 unless the skin says).
    pub sidebar_row_h: f32,
    /// How thick the active tab's line and the toolbar's line are (0: none).
    pub tab_line_w: f32,
    pub toolbar_line_w: f32,
    /// The fonts to try, best first ("Segoe UI Variable Text, Segoe UI"), and the main window's
    /// text size in DIPs (12 = 9 pt).
    pub font: String,
    pub font_size: f32,
    /// The word in the logo's box; empty for the chrome wordmark (logo.rs), which is what a skin
    /// without a `logo-text` gets.
    /// What the title bars show at the left (the "Logo" setting; not part of a skin).
    pub logo_style: crate::settings::LogoStyle,
    pub logo_text: String,

    // ---- Paints. What the surfaces above are filled with: a flat colour in most skins, a gradient
    // in some. The colour fields above keep the paint's one colour (`Paint::base`) for code that
    // needs a single colour; drawing a surface should use its paint.
    /// `window` (the skin's `background`).
    pub window_paint: Paint,
    pub titlebar_paint: Paint,
    pub toolbar_paint: Paint,
    pub panel_paint: Paint,
    pub sidebar_paint: Paint,
    pub statusbar_paint: Paint,
    /// `input`: the path and filter boxes (Glint's own windows' text boxes too).
    pub input_paint: Paint,
    /// `chip` and `chip_hover`: the skin's `button` and `button-hover` (with `buttons: liquid`,
    /// the glass of the toolbar's marbles and capsules).
    pub chip_paint: Paint,
    pub chip_hover_paint: Paint,
    pub accent_paint: Paint,
    /// `accent_tint`: selected rows.
    pub accent_tint_paint: Paint,
    pub hover_paint: Paint,
    pub scroll_thumb_paint: Paint,
    pub scroll_thumb_hover_paint: Paint,
    /// `bar_track`: behind a drive's usage bar.
    pub bar_track_paint: Paint,
    pub logo_paint: Paint,

    // ---- More surfaces. Each one follows an older colour unless the skin sets it, so a skin that
    // doesn't know about them looks as it always did.
    /// Tabs other than the current one (clear: just their names), and the current one (the
    /// toolbar's paint). Their names, and a thin edge round each tab (`tabs: pills`).
    pub tab: Paint,
    pub tab_active: Paint,
    pub tab_text: Color,
    pub tab_active_text: Color,
    pub tab_border: Color,
    /// A thin light line along the top of the title bar, the tabs and the sidebar's panel headers
    /// (clear: none) - the shine on a polished edge.
    pub highlight: Color,
    /// The sidebar's panels and their header bars (`sidebar-style: panels`); the header's text is
    /// also the colour of the plain sidebar's headings.
    pub sidebar_panel: Paint,
    pub panel_header: Paint,
    pub panel_header_text: Color,
    /// The place you're in, in the sidebar, and its name.
    pub sidebar_selection: Paint,
    pub sidebar_selection_text: Color,
    /// The file list's band of column titles, their text, and the lines between the titles (clear:
    /// none).
    pub list_header: Paint,
    pub list_header_text: Color,
    pub column_line: Color,
    /// The status bar's text, and a line along its top (clear: none).
    pub status_text: Color,
    pub status_line: Color,
    /// Names on selected rows (`text_strong` unless the skin sets `selection-text`), and the
    /// other columns of a selected row (`text` unless the skin sets `selection-text`).
    pub selection_text: Color,
    pub selection_details: Color,
    /// The edge of the path and filter boxes (clear: none).
    pub input_border: Color,
    /// Words and signs on buttons (SORT, VIEW, SETTINGS; with `buttons: liquid`, the toolbar's
    /// signs too).
    pub button_text: Color,
    /// The caption marbles (`caption-buttons: marbles`): minimise and maximise, and close.
    pub marble: Paint,
    pub marble_close: Paint,
    /// The used part of a drive's usage bar.
    pub usage_fill: Paint,
    /// Behind a scrollbar's thumb (clear: nothing), the thumb's edge (clear: none), and the colour
    /// of its grip lines (`scrollbar_grip`).
    pub scrollbar_track: Paint,
    pub scrollbar_border: Color,
    pub scrollbar_grip_lines: Color,

    // ---- Styles.
    pub buttons: ButtonStyle,
    pub caption_buttons: CaptionStyle,
    pub tabs: TabStyle,
    pub sidebar_style: SidebarStyle,
    /// Three short lines across the middle of a scrollbar's thumb.
    pub scrollbar_grip: bool,
    /// How thick the scrollbars' thumbs are, in DIPs (6 = `size::SCROLLBAR_W`).
    pub scrollbar_w: f32,
    /// Windows 2000's 3D edges (`bevels: true`): raised caption buttons, tabs and word buttons,
    /// sunken path and filter boxes, toolbar buttons that rise under the mouse.
    pub bevels: bool,
    /// Back and forward as round glass buttons of their own (XP's green, Windows 7's blue); clear
    /// for the usual signs.
    pub back_button: Paint,
    /// An edge round toolbar buttons under the mouse, which then have round corners (XP); clear
    /// for none.
    pub tool_border: Color,
    /// An edge round selected rows (and, fainter, rows under the mouse), which then have round
    /// corners (Windows 7); clear for none.
    pub selection_border: Color,
    /// A shine over the top half of the title bar (Windows 7's glass); clear for none.
    pub title_gloss: Color,
    /// The title bar's gradient runs left to right (`title-bar-across: true`), as in Windows 2000.
    pub title_across: bool,
    /// How much of what is behind the window shows through 7's glass title bar, 0 (none) to 0.9
    /// (`see-through`; Windows 11 only, see frame.rs `set_glass`).
    pub see_through: f32,

    // ---- Older Windows. Each is off (or follows an older colour) unless the skin sets it.
    /// Which older Windows the skin dresses Glint as (`era`), and whose icons it shows (`icons`).
    pub era: Era,
    pub icons: IconSet,
    /// A window's title on its title bar (Settings, Sort order...), when the window is active and
    /// when it isn't, and a shadow under it (clear: none).
    pub title_text: Color,
    pub title_text_inactive: Color,
    pub title_shadow: Color,
    /// The title bar while its window isn't the active one.
    pub titlebar_inactive_paint: Paint,
    /// The font of the windows' titles (empty: the skin's font).
    pub title_font: String,
    /// The four colours of a 3D edge (`bevels`): the lit outer and inner lines, the shaded inner and
    /// outer ones. Clear: worked out from the face colour.
    pub bevel_highlight: Color,
    pub bevel_light: Color,
    pub bevel_shadow: Color,
    pub bevel_dark: Color,
    /// Menus drawn by Glint in the skin's colours: their background, words, the item under the
    /// mouse and its words, and their edge. A clear `menu` leaves menus to Windows.
    pub menu: Color,
    pub menu_text: Color,
    pub menu_selection: Color,
    pub menu_selection_text: Color,
    pub menu_border: Color,
    /// The names of the places in the sidebar.
    pub sidebar_text: Color,
}

impl Theme {
    /// AMD Radeon: layered charcoal, one red accent (the default skin).
    #[cfg(test)]
    pub fn dark() -> Theme {
        crate::skin::built_in_theme(crate::skin::DEFAULT)
    }

    /// AMD Radeon (Light): the same in soft greys.
    #[cfg(test)]
    pub fn light() -> Theme {
        crate::skin::built_in_theme(crate::skin::DEFAULT_LIGHT)
    }

    /// The space between a scrollbar's thumb and the sides of its lane: `size::SCROLLBAR_MARGIN`,
    /// or less where the skin paints a track, so the thumb nearly fills it.
    pub fn scrollbar_margin(&self) -> f32 {
        // An older Windows: the bar is as wide as that Windows had it (16, XP and 7 17) and its
        // thumb fills it across (retroctl.rs).
        let standard = match self.era {
            Era::Win98 | Era::Win2000 => 16.0,
            Era::Xp | Era::Win7 => 17.0,
            Era::None => 0.0,
        };
        if standard > 0.0 {
            return ((standard - self.scrollbar_w) / 2.0).max(0.0);
        }
        if self.scrollbar_track.is_clear() {
            size::SCROLLBAR_MARGIN
        } else {
            1.5
        }
    }

    /// How wide a scrollbar's lane is: the thumb and the space on both sides of it.
    #[allow(dead_code)]
    pub fn scrollbar_lane(&self) -> f32 {
        self.scrollbar_w + 2.0 * self.scrollbar_margin()
    }

    /// How far a box leans whose lean in AMD Radeon is `standard` (SORT and SETTINGS 8, a switch
    /// 6), in this skin: in proportion to its slant, and 0 unless its shape is slanted.
    pub fn lean(&self, standard: f32) -> f32 {
        if self.shape == Shape::Slanted {
            standard * self.slant / STANDARD_SLANT
        } else {
            0.0
        }
    }

    /// How far the box of the folder you're in (the path's last step) leans.
    pub fn path_lean(&self) -> f32 {
        if self.shape == Shape::Slanted {
            self.path_slant
        } else {
            0.0
        }
    }

    /// The corner radius of a box `h` DIPs high (0 unless the shape is rounded).
    /// The gap between the window's panels (and below the toolbar): Windows 7 parts its panes
    /// with a thin pale line instead (window.rs draws 7's line colour where the gaps are).
    pub fn gap(&self) -> f32 {
        if self.era == Era::Win7 {
            1.0
        } else {
            size::GAP
        }
    }

    pub fn corner(&self, h: f32) -> f32 {
        if self.shape == Shape::Rounded {
            self.radius.min(h / 2.0).max(0.0)
        } else {
            0.0
        }
    }

    /// Whether the title bars show the chrome wordmark (rather than the skin's word in a box).
    pub fn chrome_logo(&self) -> bool {
        self.logo_style == crate::settings::LogoStyle::Chrome
    }

    /// How far the right edge of a logo word's box is cut, for a title bar `h` DIPs high (35 % of
    /// it in a slanted skin).
    pub fn logo_lean(&self, h: f32) -> f32 {
        self.lean(h * 0.35)
    }
}

/// Sizes, in DIPs. One place, so all parts line up.
pub mod size {
    /// Normal text: 9 pt = 12 DIPs, like Explorer.
    pub const FONT: f32 = 12.0;
    /// A skin's logo word (10 pt, bold).
    pub const FONT_LOGO: f32 = 13.333;

    pub const TITLE_H: f32 = 40.0;
    /// The logo's room at the left of a title bar: the chrome wordmark (80 DIPs wide, 8 in from
    /// the left), or the box of a skin's logo word.
    pub const LOGO_W: f32 = 90.0;
    pub const CAPTION_BUTTON_W: f32 = 46.0;
    /// Caption glyphs (minimise, maximise, close) are 10 DIPs across.
    pub const CAPTION_GLYPH: f32 = 10.0;

    pub const TOOLBAR_H: f32 = 44.0;
    pub const NAV_BUTTON: f32 = 32.0;
    pub const INPUT_H: f32 = 30.0;

    /// The gap between floating panels.
    pub const GAP: f32 = 6.0;
    pub const SIDEBAR_W: f32 = 210.0;
    /// The sidebar's rows (the file list's come from the skin, `Theme::row_h`).
    pub const ROW_H: f32 = 28.0;
    pub const HEADER_H: f32 = 32.0;
    /// The red edge on the left of a selected row.
    pub const SELECT_EDGE: f32 = 3.0;
    pub const ICON: f32 = 16.0;
    /// The file list's rows with the "Size" setting at medium (more room between rows) and large
    /// (bigger text and icons) are this much taller than at small (the skin's row height).
    pub const ROW_EXTRA_MEDIUM: f32 = 6.0;
    pub const ROW_EXTRA_LARGE: f32 = 16.0;
    /// Icons and text in the file list at large (10.5 pt).
    pub const ICON_LARGE: f32 = 32.0;
    pub const FONT_LARGE: f32 = 14.0;
    pub const STATUS_H: f32 = 24.0;

    pub const SCROLLBAR_W: f32 = 6.0;
    pub const SCROLLBAR_MARGIN: f32 = 3.0;

    /// The smallest the window may get.
    pub const MIN_W: f32 = 640.0;
    pub const MIN_H: f32 = 400.0;
}
