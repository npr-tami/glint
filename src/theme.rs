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
}

impl Theme {
    /// AMD Radeon: layered charcoal, one red accent (the default skin).
    #[cfg(test)]
    pub fn dark() -> Theme {
        crate::skin::built_in_theme(crate::skin::DEFAULT)
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
