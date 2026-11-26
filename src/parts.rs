//! The skin's settings for the main window's parts, one by one: sizes, the selected row and the row
//! under the mouse, corners, tabs, text, the toolbar, the sidebar, the file list, scroll bars and
//! menus. Each starts at how Glint looked before it existed, so a skin that doesn't set it looks
//! the same.
//!
//! [`SPECS`] is the one list: skin.rs reads them with it, the skin's text editor offers and explains
//! them with it, and the Skins window shows a row for each.

use std::collections::HashMap;

use crate::skin::Note;

/// What a part's setting can be.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// A number from `min` to `max`, `standard` when not set; the Skins window offers `offer`.
    /// `auto`: not set means "as the skin's other settings say" (Auto), not `standard`.
    Num { min: f32, max: f32, standard: f32, offer: &'static [f32], auto: bool },
    /// One of these words (the first of each pair, what the skin writes; the second, its title in
    /// the Skins window); the `standard`th when not set.
    Words { words: &'static [(&'static str, &'static str)], standard: usize },
    /// True or false.
    Flag { standard: bool },
}

/// One setting.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub key: &'static str,
    /// Its section and title (the Skins window lays its rows out by hand, in skinwin.rs, as the
    /// mockups had them; these say where each belongs).
    #[allow(dead_code)]
    pub section: &'static str,
    #[allow(dead_code)]
    pub title: &'static str,
    /// What it does, for the skin's text editor.
    pub about: &'static str,
    pub kind: Kind,
}

/// The settings, by their place in [`SPECS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum P {
    TitleH,
    ToolbarH,
    StatusH,
    HeaderH,
    Gap,
    SidebarW,
    SelectLook,
    SelectLine,
    HoverLook,
    SelectBold,
    ButtonRadius,
    BoxRadius,
    RowRadius,
    PanelRadius,
    TabMin,
    TabMax,
    TabGap,
    TabIcon,
    TabClose,
    TabBold,
    TabText,
    SidebarText,
    ListText,
    HeaderWeight,
    ButtonCaps,
    ToolButtons,
    PathSep,
    PathCurrent,
    FilterW,
    SidebarIcon,
    SidebarIndent,
    SidebarHeadings,
    DriveBarH,
    DriveBars,
    ListRows,
    ColumnLines,
    ListIcon,
    FoldersBold,
    ScrollEnds,
    ScrollShow,
    ScrollGap,
    MenuRowH,
    MenuPick,
    MenuRadius,
    MenuShadow,
    MenuGroups,
    SeeTitle,
    SeeToolbar,
    SeeSidebar,
    SeeList,
    SeeDetails,
    SeeStatus,
}

/// How many there are.
pub const COUNT: usize = P::SeeStatus as usize + 1;

/// Every setting, in [`SPECS`]' order (the Skins window's rows look them up by place).
pub const ALL: [P; COUNT] = [
    P::TitleH,
    P::ToolbarH,
    P::StatusH,
    P::HeaderH,
    P::Gap,
    P::SidebarW,
    P::SelectLook,
    P::SelectLine,
    P::HoverLook,
    P::SelectBold,
    P::ButtonRadius,
    P::BoxRadius,
    P::RowRadius,
    P::PanelRadius,
    P::TabMin,
    P::TabMax,
    P::TabGap,
    P::TabIcon,
    P::TabClose,
    P::TabBold,
    P::TabText,
    P::SidebarText,
    P::ListText,
    P::HeaderWeight,
    P::ButtonCaps,
    P::ToolButtons,
    P::PathSep,
    P::PathCurrent,
    P::FilterW,
    P::SidebarIcon,
    P::SidebarIndent,
    P::SidebarHeadings,
    P::DriveBarH,
    P::DriveBars,
    P::ListRows,
    P::ColumnLines,
    P::ListIcon,
    P::FoldersBold,
    P::ScrollEnds,
    P::ScrollShow,
    P::ScrollGap,
    P::MenuRowH,
    P::MenuPick,
    P::MenuRadius,
    P::MenuShadow,
    P::MenuGroups,
    P::SeeTitle,
    P::SeeToolbar,
    P::SeeSidebar,
    P::SeeList,
    P::SeeDetails,
    P::SeeStatus,
];

const SIZES: &str = "Sizes";
const SELECTION: &str = "Selection";
const CORNERS: &str = "Corners";
const TEXT: &str = "Text";
const SEE: &str = "Wallpaper";

const fn num(key: &'static str, section: &'static str, title: &'static str, about: &'static str, min: f32, max: f32, standard: f32, offer: &'static [f32]) -> Spec {
    Spec { key, section, title, about, kind: Kind::Num { min, max, standard, offer, auto: false } }
}
const fn auto(key: &'static str, section: &'static str, title: &'static str, about: &'static str, min: f32, max: f32, offer: &'static [f32]) -> Spec {
    Spec { key, section, title, about, kind: Kind::Num { min, max, standard: 0.0, offer, auto: true } }
}
const fn words(key: &'static str, section: &'static str, title: &'static str, about: &'static str, words: &'static [(&'static str, &'static str)]) -> Spec {
    Spec { key, section, title, about, kind: Kind::Words { words, standard: 0 } }
}
const fn flag(key: &'static str, section: &'static str, title: &'static str, about: &'static str, standard: bool) -> Spec {
    Spec { key, section, title, about, kind: Kind::Flag { standard } }
}

const CORNER_OFFER: &[f32] = &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 16.0];
const TEXT_OFFER: &[f32] = &[-2.0, -1.0, 0.0, 1.0, 2.0, 3.0];
const ICON_OFFER: &[f32] = &[16.0, 20.0, 24.0];
const SEE_OFFER: &[f32] = &[0.0, 10.0, 25.0, 40.0, 50.0, 60.0, 75.0, 90.0, 100.0];

/// Every setting, in [`P`]'s order.
pub const SPECS: [Spec; COUNT] = [
    num("title-bar-height", SIZES, "Title bar height", "how tall the title bar (logo, tabs) is, 28 to 64", 28.0, 64.0, 40.0, &[32.0, 34.0, 36.0, 38.0, 40.0, 44.0, 48.0, 52.0]),
    num("toolbar-height", SIZES, "Toolbar height", "how tall the toolbar (back, the path, SORT) is, 34 to 64", 34.0, 64.0, 44.0, &[34.0, 36.0, 38.0, 40.0, 44.0, 48.0, 52.0]),
    num("status-bar-height", SIZES, "Status bar height", "how tall the status bar is, 18 to 36", 18.0, 36.0, 24.0, &[18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 32.0]),
    num("column-titles-height", SIZES, "Column titles height", "how tall the file list's column titles are, 20 to 48", 20.0, 48.0, 32.0, &[22.0, 24.0, 26.0, 28.0, 30.0, 32.0, 36.0, 40.0]),
    auto("panel-gap", SIZES, "Space between panels", "the space between the sidebar, the file list and the panels, 0 to 16 (6 if not set)", 0.0, 16.0, &[0.0, 1.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0]),
    num("sidebar-width", SIZES, "Sidebar width", "how wide the sidebar is, 140 to 400", 140.0, 400.0, 210.0, &[160.0, 180.0, 200.0, 210.0, 240.0, 260.0, 280.0, 320.0]),
    words("selection-look", SELECTION, "Selected row", "how a selected row looks: band (straight across), rounded, or outline", &[("band", "Straight band"), ("rounded", "Rounded"), ("outline", "Outline")]),
    words("selection-line", SELECTION, "Selection line", "where a selected row's line is: left, right or under", &[("left", "Left"), ("right", "Right"), ("under", "Underneath")]),
    words("hover-look", SELECTION, "Row under the mouse", "how the row under the mouse looks: fill, outline or none", &[("fill", "Filled"), ("outline", "Outline"), ("none", "Nothing")]),
    flag("selection-bold", SELECTION, "Selected name in bold", "a selected row's name in bold: true or false", false),
    auto("button-radius", CORNERS, "Buttons", "the corners of SORT, VIEW and SETTINGS, 0 to 16 (the shape's if not set)", 0.0, 16.0, CORNER_OFFER),
    auto("box-radius", CORNERS, "Text boxes", "the corners of the path box and the search bar, 0 to 16 (the shape's if not set)", 0.0, 16.0, CORNER_OFFER),
    auto("row-radius", CORNERS, "Rows", "the corners of selected rows and the row under the mouse, 0 to 16 (the shape's if not set)", 0.0, 16.0, CORNER_OFFER),
    auto("panel-radius", CORNERS, "Panels", "the corners of the sidebar, the file list and the details panel, 0 to 16 (the shape's if not set)", 0.0, 16.0, CORNER_OFFER),
    num("tab-min-width", "Tabs", "Narrowest tab", "how narrow a tab gets before they squeeze, 60 to 200", 60.0, 200.0, 96.0, &[60.0, 72.0, 84.0, 96.0, 120.0, 140.0]),
    num("tab-max-width", "Tabs", "Widest tab", "how wide a tab gets, 100 to 400", 100.0, 400.0, 220.0, &[120.0, 150.0, 180.0, 220.0, 260.0, 300.0, 360.0]),
    auto("tab-gap", "Tabs", "Space between tabs", "the space between tabs, 0 to 16 (the tab style's if not set)", 0.0, 16.0, &[0.0, 1.0, 2.0, 4.0, 6.0, 8.0, 10.0]),
    words("tab-icon", "Tabs", "Tab icon", "a tab's folder icon: fits (when the tab is wide enough) or never", &[("fits", "When there\u{2019}s room"), ("never", "Never")]),
    words("tab-close", "Tabs", "Close \u{d7}", "a tab's \u{d7}: fits (when the tab is wide enough), hover (under the mouse) or never", &[("fits", "When there\u{2019}s room"), ("hover", "Under the mouse"), ("never", "Never")]),
    flag("tab-bold", "Tabs", "Current tab in bold", "the current tab's name in bold: true or false", false),
    num("tab-text-size", TEXT, "Tabs", "the tabs' text, bigger or smaller than the rest, -2 to 4", -2.0, 4.0, 0.0, TEXT_OFFER),
    num("sidebar-text-size", TEXT, "Sidebar", "the sidebar's text, bigger or smaller than the rest, -2 to 4", -2.0, 4.0, 0.0, TEXT_OFFER),
    num("list-text-size", TEXT, "File list", "the file list's text, bigger or smaller than the rest, -2 to 4", -2.0, 4.0, 0.0, TEXT_OFFER),
    words("column-titles-weight", TEXT, "Column titles", "how heavy the column titles are: regular, semibold or bold", &[("regular", "Regular"), ("semibold", "Semi-bold"), ("bold", "Bold")]),
    flag("button-capitals", TEXT, "Button words in capitals", "SORT, VIEW and SETTINGS in capitals: true or false", true),
    words("toolbar-buttons", "Toolbar", "Buttons show", "SORT, VIEW and SETTINGS show: words, icons or both", &[("words", "Words"), ("icons", "Icons"), ("both", "Icons and words")]),
    words("path-separator", "Toolbar", "Between folders", "the sign between the path's folders: chevron, slash or backslash", &[("chevron", "\u{203a}"), ("slash", "/"), ("backslash", "\\")]),
    words("path-current", "Toolbar", "Folder you\u{2019}re in", "how the path's last folder stands out: box, bold or plain", &[("box", "In a box"), ("bold", "Bold"), ("plain", "Plain")]),
    words("filter-width", "Toolbar", "Search bar", "how wide the search bar is: auto (by the window), narrow, normal or wide", &[("auto", "Automatic"), ("narrow", "Narrow"), ("normal", "Normal"), ("wide", "Wide")]),
    num("sidebar-icon-size", "Sidebar", "Icon size", "the sidebar's icons, 12 to 32", 12.0, 32.0, 16.0, ICON_OFFER),
    num("sidebar-indent", "Sidebar", "Space on the left", "the space before the sidebar's icons, 4 to 40", 4.0, 40.0, 16.0, &[4.0, 8.0, 10.0, 12.0, 16.0, 20.0, 24.0, 28.0]),
    words("sidebar-headings", "Sidebar", "Headings", "the sidebar's headings (Quick access, This PC): shown, capitals or hidden", &[("shown", "Shown"), ("capitals", "CAPITALS"), ("hidden", "Hidden")]),
    num("drive-bar-height", "Sidebar", "Drive bar thickness", "how thick the drives' usage bars are, 1 to 10", 1.0, 10.0, 3.0, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0]),
    words("drive-bars", "Sidebar", "Drive bars", "the drives' usage bars: always, full (only when nearly full) or never", &[("always", "Always"), ("full", "When nearly full"), ("never", "Never")]),
    words("list-rows", "File list", "Between rows", "what's between the file list's rows: nothing, lines or stripes", &[("nothing", "Nothing"), ("lines", "Lines"), ("stripes", "Stripes")]),
    flag("column-lines", "File list", "Lines between columns", "lines between the file list's columns, down the rows: true or false", false),
    num("list-icon-size", "File list", "Icon size", "the file list's icons at the small size, 12 to 32", 12.0, 32.0, 16.0, ICON_OFFER),
    flag("folders-bold", "File list", "Folder names in bold", "folders' names in bold: true or false", false),
    words("scrollbar-ends", "Scroll bars", "Ends", "the scroll bars' ends: round or square", &[("round", "Round"), ("square", "Square")]),
    words("scrollbar-show", "Scroll bars", "Show", "when the scroll bars show: always, using (while the mouse is over them or you scroll) or never", &[("always", "Always"), ("using", "While you use them"), ("never", "Never")]),
    auto("scrollbar-gap", "Scroll bars", "Space from the edge", "the space beside the scroll bars, 0 to 8 (3 if not set)", 0.0, 8.0, &[0.0, 1.0, 2.0, 3.0, 4.0, 6.0]),
    num("menu-row-height", "Menus", "Row height", "how tall a row of Glint's menus is, 18 to 40", 18.0, 40.0, 22.0, &[20.0, 22.0, 24.0, 26.0, 28.0, 32.0]),
    words("menu-pick", "Menus", "Picked row", "how a menu's picked row looks: accent, tint, or tint-line (a tint and a line)", &[("accent", "Accent colour"), ("tint", "Light tint"), ("tint-line", "Tint and line")]),
    num("menu-radius", "Menus", "Corners", "the corners of Glint's menus, 0 to 12", 0.0, 12.0, 0.0, &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0]),
    flag("menu-shadow", "Menus", "Shadow", "a shadow under Glint's menus: true or false", false),
    words("menu-groups", "Menus", "Between groups", "what's between a menu's groups: line or space", &[("line", "A line"), ("space", "A space")]),
    auto("see-through-title-bar", SEE, "Title bar and tabs", "how see-through the title bar and tabs are over the wallpaper, 0 (solid) to 100 (wallpaper-panels if not set)", 0.0, 100.0, SEE_OFFER),
    auto("see-through-toolbar", SEE, "Toolbar", "how see-through the toolbar is over the wallpaper, 0 (solid) to 100 (wallpaper-panels if not set)", 0.0, 100.0, SEE_OFFER),
    auto("see-through-sidebar", SEE, "Sidebar", "how see-through the sidebar is over the wallpaper, 0 (solid) to 100 (wallpaper-panels if not set)", 0.0, 100.0, SEE_OFFER),
    auto("see-through-file-list", SEE, "File list", "how see-through the file list and its column titles are over the wallpaper, 0 (solid) to 100 (wallpaper-panels if not set)", 0.0, 100.0, SEE_OFFER),
    auto("see-through-details", SEE, "Details panel", "how see-through the details and preview panel is over the wallpaper, 0 (solid) to 100 (wallpaper-panels if not set)", 0.0, 100.0, SEE_OFFER),
    auto("see-through-status-bar", SEE, "Status bar", "how see-through the status bar is over the wallpaper, 0 (solid) to 100 (wallpaper-panels if not set)", 0.0, 100.0, SEE_OFFER),
];

/// The spec of setting `key`.
pub fn spec(key: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.key == key)
}

fn show(x: f32) -> String {
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

/// Whether `p` is true or false (a switch in the Skins window, not a drop-down).
pub fn is_flag(p: P) -> bool {
    matches!(SPECS[p as usize].kind, Kind::Flag { .. })
}

/// A drop-down's choices for `p` in the Skins window: each one's label and what the skin then
/// says (None: the line goes, for Auto).
pub fn choices(p: P) -> Vec<(String, Option<String>)> {
    match SPECS[p as usize].kind {
        Kind::Num { offer, auto, .. } => {
            let mut v: Vec<(String, Option<String>)> = if auto { vec![("Auto".into(), None)] } else { Vec::new() };
            v.extend(offer.iter().map(|&x| (label_of(p, x), Some(show(x)))));
            v
        }
        Kind::Words { words, .. } => words.iter().map(|(w, t)| (t.to_string(), Some(w.to_string()))).collect(),
        Kind::Flag { .. } => vec![("On".into(), Some("true".into())), ("Off".into(), Some("false".into()))],
    }
}

/// A number's label in the Skins window: "+1" for a text size, "Same" for 0 there.
pub fn label_of(p: P, x: f32) -> String {
    match p {
        P::TabText | P::SidebarText | P::ListText if x == 0.0 => "Same".into(),
        P::TabText | P::SidebarText | P::ListText if x > 0.0 => format!("+{}", show(x)),
        P::SeeTitle | P::SeeToolbar | P::SeeSidebar | P::SeeList | P::SeeDetails | P::SeeStatus if x <= 0.0 => "0% (solid)".into(),
        P::SeeTitle | P::SeeToolbar | P::SeeSidebar | P::SeeList | P::SeeDetails | P::SeeStatus => format!("{}%", show(x)),
        _ => show(x),
    }
}

/// The values a skin sets for the parts (None: not set).
#[derive(Clone, Debug, PartialEq)]
pub struct Parts {
    v: [Option<f32>; COUNT],
}

impl Default for Parts {
    fn default() -> Parts {
        Parts { v: [None; COUNT] }
    }
}

/// A selected row's look (`selection-look`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectLook {
    Band,
    Rounded,
    Outline,
}

/// Where a selected row's line is (`selection-line`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSide {
    Left,
    Right,
    Under,
}

/// The row under the mouse (`hover-look`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverLook {
    Fill,
    Outline,
    None,
}

/// When a tab's icon or × shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Fits,
    Hover,
    Never,
}

/// The toolbar's word buttons (`toolbar-buttons`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolButtons {
    Words,
    Icons,
    Both,
}

/// How the path's last folder stands out (`path-current`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCurrent {
    Box,
    Bold,
    Plain,
}

/// The sidebar's headings (`sidebar-headings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Headings {
    Shown,
    Capitals,
    Hidden,
}

/// The drives' usage bars (`drive-bars`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveBars {
    Always,
    Full,
    Never,
}

/// Between the file list's rows (`list-rows`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListRows {
    Nothing,
    Lines,
    Stripes,
}

/// A menu's picked row (`menu-pick`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuPick {
    Accent,
    Tint,
    TintLine,
}

impl Parts {
    /// What the skin sets `p` to (a word's or a flag's as its number), or None.
    pub fn get(&self, p: P) -> Option<f32> {
        self.v[p as usize]
    }

    /// Number `p`: as set, or its standard.
    pub fn num(&self, p: P) -> f32 {
        match (self.get(p), SPECS[p as usize].kind) {
            (Some(x), _) => x,
            (None, Kind::Num { standard, .. }) => standard,
            _ => 0.0,
        }
    }

    /// Words setting `p`, as its place in the list.
    pub fn word(&self, p: P) -> usize {
        match (self.get(p), SPECS[p as usize].kind) {
            (Some(x), _) => x as usize,
            (None, Kind::Words { standard, .. }) => standard,
            _ => 0,
        }
    }

    /// Flag `p`.
    pub fn on(&self, p: P) -> bool {
        match (self.get(p), SPECS[p as usize].kind) {
            (Some(x), _) => x != 0.0,
            (None, Kind::Flag { standard }) => standard,
            _ => false,
        }
    }

    /// What the Skins window's drop-down for `p` says now: its choice's label ("Auto" for an Auto
    /// number that isn't set).
    pub fn label(&self, p: P) -> String {
        match SPECS[p as usize].kind {
            Kind::Num { auto: true, .. } if self.get(p).is_none() => "Auto".into(),
            Kind::Num { .. } => label_of(p, self.num(p)),
            Kind::Words { words, .. } => words.get(self.word(p)).map_or("", |w| w.1).to_string(),
            Kind::Flag { .. } => if self.on(p) { "On" } else { "Off" }.into(),
        }
    }

    /// What `p` is now as the skin writes it ("" for an Auto number that isn't set).
    #[cfg(test)]
    pub fn written(&self, p: P) -> String {
        match SPECS[p as usize].kind {
            Kind::Num { auto: true, .. } if self.get(p).is_none() => String::new(),
            Kind::Num { .. } => show(self.num(p)),
            Kind::Words { words, .. } => words.get(self.word(p)).map_or("", |w| w.0).to_string(),
            Kind::Flag { .. } => self.on(p).to_string(),
        }
    }

    pub fn select_look(&self) -> SelectLook {
        [SelectLook::Band, SelectLook::Rounded, SelectLook::Outline][self.word(P::SelectLook).min(2)]
    }
    pub fn select_line(&self) -> LineSide {
        [LineSide::Left, LineSide::Right, LineSide::Under][self.word(P::SelectLine).min(2)]
    }
    pub fn hover_look(&self) -> HoverLook {
        [HoverLook::Fill, HoverLook::Outline, HoverLook::None][self.word(P::HoverLook).min(2)]
    }
    pub fn tab_icon(&self) -> Shown {
        [Shown::Fits, Shown::Never][self.word(P::TabIcon).min(1)]
    }
    pub fn tab_close(&self) -> Shown {
        [Shown::Fits, Shown::Hover, Shown::Never][self.word(P::TabClose).min(2)]
    }
    /// The column titles' weight (400, 600 or 700).
    pub fn header_weight(&self) -> u16 {
        [400, 600, 700][self.word(P::HeaderWeight).min(2)]
    }
    pub fn tool_buttons(&self) -> ToolButtons {
        [ToolButtons::Words, ToolButtons::Icons, ToolButtons::Both][self.word(P::ToolButtons).min(2)]
    }
    /// The sign between the path's folders: None for the drawn chevron.
    pub fn path_sep(&self) -> Option<&'static str> {
        [None, Some("/"), Some("\\")][self.word(P::PathSep).min(2)]
    }
    pub fn path_current(&self) -> PathCurrent {
        [PathCurrent::Box, PathCurrent::Bold, PathCurrent::Plain][self.word(P::PathCurrent).min(2)]
    }
    /// The filter box's width, or None to go by the window's width.
    pub fn filter_w(&self) -> Option<f32> {
        [None, Some(130.0), Some(180.0), Some(260.0)][self.word(P::FilterW).min(3)]
    }
    pub fn sidebar_headings(&self) -> Headings {
        [Headings::Shown, Headings::Capitals, Headings::Hidden][self.word(P::SidebarHeadings).min(2)]
    }
    pub fn drive_bars(&self) -> DriveBars {
        [DriveBars::Always, DriveBars::Full, DriveBars::Never][self.word(P::DriveBars).min(2)]
    }
    pub fn list_rows(&self) -> ListRows {
        [ListRows::Nothing, ListRows::Lines, ListRows::Stripes][self.word(P::ListRows).min(2)]
    }
    pub fn square_scrollbars(&self) -> bool {
        self.word(P::ScrollEnds) == 1
    }
    /// The scroll bars show only while they're used.
    pub fn scrollbars_while_used(&self) -> bool {
        self.word(P::ScrollShow) == 1
    }
    /// The scroll bars never show (the mouse wheel still scrolls).
    pub fn scrollbars_hidden(&self) -> bool {
        self.word(P::ScrollShow) == 2
    }
    pub fn menu_pick(&self) -> MenuPick {
        [MenuPick::Accent, MenuPick::Tint, MenuPick::TintLine][self.word(P::MenuPick).min(2)]
    }
    /// A menu's groups are parted by space, not a line.
    pub fn menu_space(&self) -> bool {
        self.word(P::MenuGroups) == 1
    }
}

/// Reads the parts' settings: as the skin says (`own`: value and line), else as its base skin says.
/// A value that isn't one of the setting's is noted, and the base's (or the standard) used.
pub fn read(own: &HashMap<String, (String, usize)>, base: &HashMap<String, String>, notes: &mut Vec<Note>) -> Parts {
    let mut parts = Parts::default();
    for (i, s) in SPECS.iter().enumerate() {
        let theirs = base.get(s.key).and_then(|v| value(s, v).ok());
        parts.v[i] = match own.get(s.key) {
            Some((v, line)) => match value(s, v) {
                Ok(x) => x,
                Err(why) => {
                    notes.push(Note { line: *line, text: why });
                    theirs.flatten()
                }
            },
            None => theirs.flatten(),
        };
    }
    parts
}

/// What `v` means for setting `s`: its number (a word's place, a flag's 0 or 1), None for "auto",
/// or why it can't be. A number past either end is kept to it, and that is said too.
fn value(s: &Spec, v: &str) -> Result<Option<f32>, String> {
    let w = crate::skin::normalize_word(v);
    match s.kind {
        Kind::Num { min, max, auto, .. } => {
            if auto && (w == "auto" || w.is_empty()) {
                return Ok(None);
            }
            let x = v.trim().trim_end_matches("px").trim().parse::<f32>().ok().filter(|x| x.is_finite());
            match x {
                Some(x) if (min..=max).contains(&x) => Ok(Some(x)),
                Some(_) => Err(format!("{} goes from {} to {}.", s.key, show(min), show(max))),
                None => Err(format!("{} is a number from {} to {} (not \u{201c}{}\u{201d}).", s.key, show(min), show(max), v.trim())),
            }
        }
        Kind::Words { words, .. } => match words.iter().position(|(word, _)| *word == w) {
            Some(i) => Ok(Some(i as f32)),
            None => {
                let all: Vec<&str> = words.iter().map(|(word, _)| *word).collect();
                let list = match all.split_last() {
                    Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
                    _ => all.join(""),
                };
                Err(format!("{} is {list} (not \u{201c}{}\u{201d}).", s.key, v.trim()))
            }
        },
        Kind::Flag { .. } => match w.as_str() {
            "true" | "yes" | "on" | "1" => Ok(Some(1.0)),
            "false" | "no" | "off" | "0" => Ok(Some(0.0)),
            _ => Err(format!("{} is true or false (not \u{201c}{}\u{201d}).", s.key, v.trim())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_in_order_and_every_setting_starts_as_before() {
        assert_eq!(SPECS.len(), COUNT);
        assert_eq!(SPECS[P::MenuGroups as usize].key, "menu-groups");
        assert_eq!(SPECS[P::TabBold as usize].key, "tab-bold");
        assert_eq!(SPECS[P::ScrollGap as usize].key, "scrollbar-gap");
        let p = Parts::default();
        assert_eq!(p.num(P::TitleH), 40.0);
        assert_eq!(p.num(P::ToolbarH), 44.0);
        assert_eq!(p.num(P::StatusH), 24.0);
        assert_eq!(p.num(P::HeaderH), 32.0);
        assert_eq!(p.num(P::SidebarW), 210.0);
        assert_eq!(p.get(P::Gap), None);
        assert_eq!(p.select_look(), SelectLook::Band);
        assert_eq!(p.select_line(), LineSide::Left);
        assert!(p.on(P::ButtonCaps));
        assert!(!p.on(P::SelectBold));
        assert_eq!(p.num(P::MenuRowH), 22.0);
        assert_eq!(p.filter_w(), None);
        assert_eq!(p.path_sep(), None);
        // Keys are unique, and each standard is in range and among what's offered.
        for (i, s) in SPECS.iter().enumerate() {
            assert!(SPECS[..i].iter().all(|o| o.key != s.key), "{}", s.key);
            if let Kind::Num { min, max, standard, offer, auto } = s.kind {
                assert!(auto || (min..=max).contains(&standard), "{}", s.key);
                assert!(auto || offer.contains(&standard), "{}", s.key);
                assert!(offer.iter().all(|x| (min..=max).contains(x)), "{}", s.key);
            }
        }
    }

    #[test]
    fn a_skin_sets_them_and_wrong_values_are_said() {
        let own: HashMap<String, (String, usize)> = [
            ("title-bar-height", "34"),
            ("selection-look", "Rounded"),
            ("tab-bold", "yes"),
            ("panel-gap", "auto"),
            ("menu-row-height", "99"),
            ("list-rows", "zebra"),
        ]
        .iter()
        .enumerate()
        .map(|(i, (k, v))| (k.to_string(), (v.to_string(), i + 1)))
        .collect();
        let base: HashMap<String, String> = [("menu-row-height".to_string(), "24".to_string())].into();
        let mut notes = Vec::new();
        let p = read(&own, &base, &mut notes);
        assert_eq!(p.num(P::TitleH), 34.0);
        assert_eq!(p.select_look(), SelectLook::Rounded);
        assert!(p.on(P::TabBold));
        assert_eq!(p.get(P::Gap), None);
        // Past the end: the base's.
        assert_eq!(p.num(P::MenuRowH), 24.0);
        assert_eq!(p.list_rows(), ListRows::Nothing);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.line == 6 && n.text.contains("nothing, lines or stripes")));
        assert_eq!(p.written(P::SelectLook), "rounded");
        assert_eq!(p.written(P::Gap), "");
        assert_eq!(label_of(P::ListText, 1.0), "+1");
        assert_eq!(label_of(P::ListText, 0.0), "Same");
    }
}
