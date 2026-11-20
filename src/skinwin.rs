//! The skin customiser (Settings > Appearance > CUSTOMISE…): a window of its own, drawn by Glint in
//! the Settings window's style (kit.rs), for changing a skin in plain words and seeing it at once.
//!
//! - **Left:** every skin - the built-in ones, then the person's own - and the buttons NEW,
//!   DUPLICATE, RENAME, DELETE (own skins only, after a question; the file goes to the Recycle Bin,
//!   or is deleted for good on a drive without one), IMPORT… (with the skin's pictures and its
//!   light or dark other half; what didn't come is said), EXPORT… and OPEN SKINS FOLDER (in a
//!   new tab; this window stays open). Clicking a
//!   skin uses it, like Settings' drop-down.
//! - **Right:** the skin in use, in plain words: the button shape, the slant, the row height, the
//!   text size, how thick the tab's and the toolbar's lines are, the logo (empty: the chrome
//!   wordmark; a word: that word in a box), whether it's a dark skin; its style (glass buttons,
//!   marbles for the window buttons, tabs as pills, the sidebar in panels, grip lines on the
//!   scrollbars); and every colour it has, the window's from the top down. Each colour has a
//!   swatch that opens Windows' colour picker and a box for its #hex - or for a gradient,
//!   "#fdfdfd, #c3cddb 45%, #dfe6f0", whose first colour the picker changes. A colour that follows
//!   another ("tab-line: accent") shows that colour, says which it follows, and keeps following it
//!   until it's changed here. (No text view: every setting has its control, and the skin's file
//!   can still be opened in a text editor from the skins folder.)
//! - **Live, and saved by itself.** Every change goes into the skin's text (`skin::with_value`, so
//!   the rest of the text stays as it was), the main window shows it at once (`AppState::apply_skin`),
//!   and the file is written a moment later (SAVE_TIMER) by the skin file worker - never on this
//!   thread. A built-in skin is never changed: the first change makes a copy, "My skin", and goes
//!   there.
//!
//! How it fits in, as the Settings window (settingswin.rs): its state lives in the main window's
//! state and its messages go through the main window's guard (`window::skin_message`), and what
//! runs a message loop of its own (drop-down lists, the colour picker, the file dialogs, showing,
//! closing) comes back to the main window as a [`Job`] that runs after the message.
//!
//! Keyboard: Tab / Shift+Tab go round the skin list, the buttons and the controls; Up / Down (and
//! Home / End) in the list pick a skin; F2 renames and Del deletes one of your own; Space or Enter
//! presses, flips or opens; Ctrl+Shift+L switches light / dark as in the main window; Esc (or
//! Ctrl+W) closes.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, RedrawWindow, ScreenToClient,
    SetBkColor, SetTextColor, ValidateRect, COLOR_WINDOW, HDC, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, RDW_ALLCHILDREN,
    RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::{ChooseColorW, CC_ANYCOLOR, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VK_BACK, VK_CONTROL, VK_DELETE,
    VK_DOWN, VK_END, VK_ESCAPE, VK_F2, VK_F4, VK_HOME, VK_LEFT, VK_MENU, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB,
    VK_UP,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName, FOS_FILEMUSTEXIST,
    FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindow, IsIconic, KillTimer, LoadCursorW, LoadIconW, PostMessageW, RegisterClassExW, 
    SetTimer, SetWindowPos, ShowWindow, CS_DBLCLKS, GW_OWNER,
    HTCLIENT, IDC_ARROW, MINMAXINFO, SIZE_MINIMIZED, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE, SW_SHOW, WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_CHAR,
    WM_CLOSE, WM_COMMAND, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_KEYDOWN,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT,
    WM_SIZE, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN, WS_SYSMENU,
    WS_THICKFRAME,
};

use crate::app::AppState;
use crate::ask::{Answer, AskBox};
use crate::frame;
use crate::kit::{self, size as ksize, Look, Scroller};
use crate::log;
use crate::render::{Align, FrameResult, Renderer, TextStyle};
use crate::shell;
use crate::skin;
use crate::textbox::{self, TextBox, TextEvent, TextEventKind, WM_APP_TEXT};
use crate::thumbs;
use crate::settings::{LogoStyle, SkinsWindow as Layout};
use crate::theme::{size, ButtonStyle, CaptionStyle, Color, Era, IconSet, Paint, Shape, SidebarStyle, Stop, TabStyle, Theme, WallpaperArea, WallpaperFit};
use crate::ui::{Command, Menu, MenuItem, Point, Rect};
use crate::winutil::{hi_word, lo_word, place_visible, visible_rect};

/// The window's title (Alt+Tab, the taskbar's thumbnails).
pub const TITLE: &str = "Glint skins";
/// The skin list's width, its rows and its two small headings.
const LIST_W: f32 = 236.0;
const ITEM_H: f32 = 34.0;
const GROUP_H: f32 = 28.0;
/// The buttons under the list.
const BUTTONS_PAD: f32 = 14.0;
const BUTTON_GAP: f32 = 8.0;
/// The right side: the skin's name and notes at the top, then the controls (or its text).
const HEADER_H: f32 = 96.0;
const PAD_X: f32 = 28.0;
const GUTTER: f32 = 10.0;
const MAX_CARD_W: f32 = 720.0;
const HEADING_H: f32 = 22.0;
const ROW_H: f32 = 46.0;
/// A row with a note under its title.
const ROW_NOTE_H: f32 = 56.0;
const ROW_PAD_X: f32 = 16.0;
const SWATCH: f32 = 26.0;
/// A colour's box: wide enough for a short gradient.
const HEX_W: f32 = 196.0;
/// A colour's NONE button.
const NONE_W: f32 = 54.0;
const DROPDOWN_W: f32 = 200.0;
const LOGO_FIELD_W: f32 = 140.0;
/// The wallpaper's BROWSE… button, a slider, and the number after it.
const BROWSE_W: f32 = 96.0;
const SLIDER_W: f32 = 200.0;
/// A press this near a slider's knob (across) grabs the knob where it is.
const KNOB_GRAB: f32 = 8.0;
const VALUE_W: f32 = 64.0;
const VALUE_GAP: f32 = 10.0;
/// The pictures next to the skin, as small pictures to click: their size, the gap between, how
/// many at most, and their row's height.
const THUMB_W: f32 = 72.0;
const THUMB_H: f32 = 46.0;
const THUMB_GAP: f32 = 8.0;
const MAX_THUMBS: usize = 8;
const THUMBS_H: f32 = 72.0;
/// Where the wallpaper sits: the pad with its dot, the 3x3 squares, and their row's height.
const PAD_H: f32 = 100.0;
/// The pad is the list's shape, between these wide.
const PAD_MIN_W: f32 = 120.0;
const PAD_MAX_W: f32 = 220.0;
const GRID_CELL: f32 = 24.0;
const GRID_GAP: f32 = 4.0;
const PLACE_H: f32 = 124.0;
/// What the wallpaper's box says while the skin has none.
const PICTURE_HINT: &str = "None";
/// What the Logo box says while it's empty: no word means the chrome wordmark.
const LOGO_HINT: &str = "GLINT";
/// The box above the colours that finds them by name: its width, and what it says while empty.
const SEARCH_W: f32 = 220.0;
/// The search at the top of the smaller layouts, which finds any setting.
const SEARCH_ANY: &str = "Find any setting";
/// A small icon button (undo, redo, the menu; the slim layout's nine).
const ICON: f32 = 26.0;

const NOTE_LINE: f32 = 17.0;
/// The window's size, and the smallest it gets (DIPs).
const DEFAULT_W: f32 = 780.0;
const MIN_W: f32 = 780.0;
const MIN_H: f32 = 540.0;
/// The window's layout's message to itself: the layout changed, so dock it again at its width.
const WM_APP_DOCK: u32 = WM_APP + 83;

/// Sizes that differ between the window's layouts (the SkinsWindow setting).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// How wide the window opens, and how narrow it can get.
    width: f32,
    min_width: f32,
    /// The list of skins on the left (0 wide: none, the skin is picked at the top): its width, the
    /// first group's top, a group's title with the gap under it, the title's own height, the gap
    /// above the second group, a skin's height, how far in its name starts, the titles' size.
    list_w: f32,
    list_top: f32,
    group_h: f32,
    group_text_h: f32,
    group_gap: f32,
    item_h: f32,
    item_x: f32,
    group_font: f32,
    header_h: f32,
    /// The page: the margin each side of the cards, the room kept for the scroll bar, a card's
    /// least width, the first heading's top, a heading's height, the gap under it, the gap under
    /// a card.
    pad_x: f32,
    gutter: f32,
    min_card_w: f32,
    first_y: f32,
    heading_h: f32,
    heading_gap: f32,
    card_gap: f32,
    /// A row's height, with a note under its title, and the room at its ends.
    row_h: f32,
    row_note_h: f32,
    row_pad: f32,
    /// A colour: its swatch, its box and what clears it (NONE, or a small x).
    swatch: f32,
    hex_w: f32,
    hex_h: f32,
    clear_w: f32,
    clear_h: f32,
    /// Drop-downs and the other boxes.
    dropdown_w: f32,
    field_h: f32,
    logo_w: f32,
    browse_w: f32,
    picture_w: f32,
    /// A slider (its knob's room each end included), the gap after it, and its number's box.
    slider_w: f32,
    value_gap: f32,
    value_w: f32,
    value_h: f32,
    toggle_w: f32,
    toggle_h: f32,
    /// The pictures next to the skin, the room their row leaves for its title, and the row.
    thumb_w: f32,
    thumb_h: f32,
    thumbs_title: f32,
    thumbs_h: f32,
    /// The position pad: its height, how wide it can get, and its row.
    pad_h: f32,
    pad_max_w: f32,
    place_h: f32,
}

impl Metrics {
    fn of(layout: Layout) -> Metrics {
        let wide = Metrics {
            width: DEFAULT_W,
            min_width: MIN_W,
            list_w: LIST_W,
            list_top: 6.0,
            group_h: GROUP_H,
            group_text_h: GROUP_H,
            group_gap: 10.0,
            item_h: ITEM_H,
            item_x: 18.0,
            group_font: ksize::NOTE_FONT,
            header_h: HEADER_H,
            pad_x: PAD_X,
            gutter: GUTTER,
            min_card_w: 300.0,
            first_y: 10.0,
            heading_h: HEADING_H,
            heading_gap: 6.0,
            card_gap: 18.0,
            row_h: ROW_H,
            row_note_h: ROW_NOTE_H,
            row_pad: ROW_PAD_X,
            swatch: SWATCH,
            hex_w: HEX_W,
            hex_h: ksize::FIELD_H,
            clear_w: NONE_W,
            clear_h: ksize::FIELD_H,
            dropdown_w: DROPDOWN_W,
            field_h: ksize::FIELD_H,
            logo_w: LOGO_FIELD_W,
            browse_w: BROWSE_W,
            picture_w: HEX_W,
            slider_w: SLIDER_W,
            value_gap: VALUE_GAP,
            value_w: VALUE_W,
            value_h: ksize::FIELD_H,
            toggle_w: ksize::TOGGLE_W,
            toggle_h: ksize::TOGGLE_H,
            thumb_w: THUMB_W,
            thumb_h: THUMB_H,
            thumbs_title: 140.0,
            thumbs_h: THUMBS_H,
            pad_h: PAD_H,
            pad_max_w: PAD_MAX_W,
            place_h: PLACE_H,
        };
        // (The sliders' tracks are 120 and 150 wide: the knob takes 7 more each end.)
        let small = Metrics {
            list_w: 0.0,
            pad_x: 12.0,
            gutter: 0.0,
            min_card_w: 200.0,
            first_y: 10.0,
            heading_h: 17.0,
            heading_gap: 4.0,
            card_gap: 10.0,
            row_h: 32.0,
            row_note_h: 44.0,
            row_pad: 10.0,
            swatch: 20.0,
            hex_w: 84.0,
            hex_h: 24.0,
            clear_w: 22.0,
            clear_h: 22.0,
            dropdown_w: 170.0,
            field_h: 24.0,
            logo_w: 110.0,
            browse_w: 76.0,
            picture_w: 110.0,
            slider_w: 134.0,
            value_gap: 1.0,
            value_w: 44.0,
            value_h: 26.0,
            toggle_w: 34.0,
            toggle_h: 18.0,
            thumb_w: 56.0,
            thumb_h: 36.0,
            thumbs_title: 80.0,
            thumbs_h: 50.0,
            pad_h: 84.0,
            pad_max_w: 150.0,
            place_h: 100.0,
            ..wide
        };
        match layout {
            Layout::Wide => wide,
            Layout::Compact => Metrics { width: 420.0, min_width: 360.0, ..small },
            Layout::Slim => Metrics {
                width: 600.0,
                min_width: 520.0,
                list_w: 150.0,
                list_top: 10.0,
                group_h: 18.0,
                group_text_h: 14.0,
                group_gap: 8.0,
                item_h: 26.0,
                item_x: 12.0,
                group_font: 10.5,
                header_h: 52.0,
                pad_x: 16.0,
                first_y: 4.0,
                card_gap: 12.0,
                row_h: 34.0,
                row_note_h: 46.0,
                row_pad: 12.0,
                swatch: 22.0,
                hex_w: 104.0,
                hex_h: 26.0,
                clear_w: 24.0,
                clear_h: 24.0,
                field_h: 26.0,
                logo_w: 120.0,
                browse_w: 76.0,
                picture_w: 120.0,
                slider_w: 164.0,
                thumb_w: 60.0,
                thumb_h: 38.0,
                thumbs_title: 90.0,
                thumbs_h: 54.0,
                pad_h: 90.0,
                pad_max_w: 160.0,
                place_h: 106.0,
                ..small
            },
        }
    }
}

/// How wide a row's controls are together (the smaller layouts: its title has the rest).
fn controls_w(m: &Metrics, ctl: Ctl) -> f32 {
    match ctl {
        Ctl::Colour(_) => m.swatch + 6.0 + m.hex_w + 2.0 + m.clear_w,
        Ctl::Picture => m.browse_w + 6.0 + m.picture_w + 2.0 + m.clear_w,
        Ctl::Slide(_) => m.slider_w + m.value_gap + m.value_w,
        Ctl::LogoText => m.logo_w,
        Ctl::Search => SEARCH_W,
        Ctl::Dark | Ctl::Grip => m.toggle_w,
        Ctl::Part(p) if crate::parts::is_flag(p) => m.toggle_w,
        Ctl::Place => 3.0 * GRID_CELL + 2.0 * GRID_GAP + 22.0 + m.pad_max_w,
        Ctl::Thumbs => 0.0,
        _ => m.dropdown_w,
    }
}

/// The sections folded at first: all but the wallpaper and the main colours.
fn first_folded() -> Vec<&'static str> {
    CARDS.iter().copied().filter(|t| !["Overall look", "Main colours", "Wallpaper"].contains(t)).collect()
}
/// The cards and their rows: one card for each part of the window, in the order they sit on
/// screen (top to bottom), each with everything about that part: its sizes, shapes, text and
/// colours (CARDS has their titles). `hints`: a colour's "follows ..." note, by COLOURS' order.
fn cards(hints: &[String]) -> Vec<(&'static str, Vec<(Ctl, &'static str, &'static str)>)> {
    use crate::parts::P;
    let part = |p: P, title: &'static str| (Ctl::Part(p), title, "");
    // A colour by the skin's name for it, with its title in this card.
    let col = |key: &str, title: &'static str| -> (Ctl, &'static str, &'static str) {
        let i = COLOURS.iter().position(|(k, _)| *k == key).unwrap_or(0);
        (Ctl::Colour(i), title, if hints.get(i).is_some_and(|h| !h.is_empty()) { FOLLOWS } else { "" })
    };
    vec![
        (
            "Overall look",
            vec![
                (Ctl::Dark, "Dark skin", ""),
                (Ctl::Era, "Old Windows look", "3D edge colours: 3D edges"),
                (Ctl::Shape, "Shape", "Corners follow this unless set"),
                (Ctl::Slant, "Slant amount", "When Shape is Slanted"),
                (Ctl::Icons, "Icon set", ""),
                (Ctl::Font, "Font", ""),
                (Ctl::FontSize, "Text size, whole window", ""),
            ],
        ),
        (
            "Main colours",
            vec![
                col("accent", "Accent"),
                col("accent-text", "Text on accent"),
                col("background", "Background, behind the panels"),
                col("text", "Text, everywhere"),
                col("text-bright", "Strong text: titles and headings"),
                col("muted", "Faint text"),
                col("disabled", "Greyed-out text"),
                col("line", "Thin lines"),
                col("highlight", "Thin shine along the top of bars and tabs"),
            ],
        ),
        (
            "3D edges",
            vec![
                col("bevel-highlight", "Outer light edge"),
                col("bevel-light", "Inner light edge"),
                col("bevel-shadow", "Inner shadow edge"),
                col("bevel-dark", "Outer shadow edge"),
            ],
        ),
        (
            "Wallpaper",
            vec![
                (Ctl::Picture, "Picture", ""),
                (Ctl::Thumbs, "Same folder", ""),
                (Ctl::Area, "Picture covers", ""),
                (Ctl::Fit, "Fit", ""),
                (Ctl::Place, "Position", "Drag the picture"),
                (Ctl::Slide(Slide::Across), "Left / right", ""),
                (Ctl::Slide(Slide::Down), "Up / down", ""),
                (Ctl::Slide(Slide::Zoom), "Zoom", ""),
                (Ctl::Slide(Slide::Opacity), "Picture strength", ""),
                (Ctl::Slide(Slide::Blur), "Blur", ""),
                (Ctl::Slide(Slide::Fade), "Soft edges", ""),
                (Ctl::Slide(Slide::Panels), "See-through panels", "When covering the whole window"),
                (Ctl::Part(P::SeeTitle), "See-through title bar and tabs", "Auto: as panels"),
                (Ctl::Part(P::SeeToolbar), "See-through toolbar", "Auto: as panels"),
                (Ctl::Part(P::SeeSidebar), "See-through sidebar", "Auto: as panels"),
                (Ctl::Part(P::SeeList), "See-through file list", "Auto: as panels"),
                (Ctl::Part(P::SeeDetails), "See-through details panel", "Auto: as panels"),
                (Ctl::Part(P::SeeStatus), "See-through status bar", "Auto: as panels"),
            ],
        ),
        (
            "Title bar",
            vec![
                part(P::TitleH, "Title bar height"),
                (Ctl::Logo, "Logo style", ""),
                (Ctl::LogoText, "Logo text", "Only with Logo style In a box"),
                col("title-bar", "Title bar"),
                col("title-bar-inactive", "Title bar, inactive"),
                col("title-gloss", "Title bar shine, top half"),
                col("title-text", "Other windows' title text"),
                col("title-text-inactive", "Other windows' title text, inactive"),
                col("title-text-shadow", "Other windows' title text shadow"),
                col("logo", "Logo box"),
                col("logo-letters", "Logo box text"),
            ],
        ),
        (
            "Tabs",
            vec![
                (Ctl::Tabs, "Tab style", "Corners: Overall look > Shape"),
                part(P::TabMin, "Narrowest tab width"),
                part(P::TabMax, "Widest tab width"),
                part(P::TabGap, "Space between tabs"),
                part(P::TabText, "Tab text size"),
                part(P::TabBold, "Current tab in bold"),
                part(P::TabIcon, "Folder icon on tabs"),
                (Ctl::Part(P::TabClose), "Close button on tabs", "Hover colour: Window buttons"),
                (Ctl::TabLine, "Current tab line width", ""),
                col("tab", "Other tabs"),
                col("tab-text", "Other tab names"),
                col("tab-active", "Current tab"),
                col("tab-active-text", "Current tab text"),
                col("tab-border", "Tab edges"),
                col("tab-line", "Current tab line"),
            ],
        ),
        (
            "Window buttons",
            vec![
                (Ctl::Caption, "Window button style", ""),
                col("caption-hover", "Minimise, maximise, + and tab close, under the mouse"),
                col("caption-pressed", "Minimise, maximise, + and tab close, pressed"),
                col("close-hover", "Close (window X), under the mouse"),
                col("marble", "Minimise and maximise marbles"),
                col("marble-close", "Close marble"),
            ],
        ),
        (
            "Toolbar",
            vec![
                part(P::ToolbarH, "Toolbar height"),
                part(P::PathSep, "Sign between folders in the path"),
                (Ctl::Part(P::PathCurrent), "Current folder in the path", "Box colour: Buttons"),
                part(P::FilterW, "Search bar width"),
                part(P::BoxRadius, "Path box and search bar corners"),
                (Ctl::ToolbarLine, "Bottom line width", ""),
                col("toolbar", "Toolbar, also the current tab"),
                col("toolbar-line", "Bottom line"),
                col("tool-hover", "Back, forward, up and refresh, under the mouse"),
                col("tool-pressed", "Back, forward, up and refresh, pressed"),
                col("tool-border", "Back, forward, up and refresh, edge under the mouse"),
                col("back-button", "Back and forward glass buttons"),
                col("input", "Path box and search bar"),
                col("input-border", "Path box and search bar edges"),
                col("path-hover", "Path, under the mouse"),
            ],
        ),
        (
            "Buttons",
            vec![
                (Ctl::Buttons, "Button style", "The Sort, View and Settings buttons"),
                part(P::ToolButtons, "Buttons show"),
                part(P::ButtonCaps, "Words in capitals"),
                part(P::ButtonRadius, "Button corners"),
                col("button", "Buttons and current folder box"),
                col("button-hover", "Buttons, under the mouse"),
                col("button-text", "Button text"),
            ],
        ),
        ("Panels", vec![part(P::Gap, "Space between panels"), (Ctl::Part(P::PanelRadius), "Panel corners", "See-through: Wallpaper")]),
        (
            "Sidebar",
            vec![
                (Ctl::Sidebar, "Sidebar style", ""),
                part(P::SidebarW, "Sidebar width"),
                (Ctl::SidebarRow, "Sidebar row height", ""),
                part(P::SidebarText, "Sidebar text size"),
                part(P::SidebarIcon, "Sidebar icon size"),
                part(P::SidebarIndent, "Indent"),
                part(P::SidebarHeadings, "Headings"),
                part(P::DriveBars, "Drive bars"),
                part(P::DriveBarH, "Drive bar thickness"),
                (Ctl::SidebarEdge, "Sidebar selection line width", "Sidebar only. Side: Selection"),
                col("sidebar", "Sidebar background"),
                col("sidebar-text", "Sidebar text"),
                col("sidebar-selection", "Sidebar selected place"),
                col("sidebar-selection-text", "Sidebar selected place text"),
                col("sidebar-selection-edge", "Sidebar selection line"),
                col("sidebar-panel", "Sidebar group boxes"),
                col("panel-header", "Group box heading bar"),
                col("panel-header-text", "Sidebar heading text"),
                col("usage-bar", "Drive bar"),
                col("usage-fill", "Drive bar fill"),
            ],
        ),
        (
            "File list",
            vec![
                col("panel", "Background, also details panel"),
                (Ctl::RowHeight, "File list row height", ""),
                part(P::ListText, "File list text size"),
                part(P::ListIcon, "File list icon size"),
                (Ctl::Part(P::ListRows), "Lines or stripes between rows", "Line colour: Main colours > Thin lines"),
                (Ctl::Part(P::ColumnLines), "Show lines down the columns", "Colour: Main colours > Thin lines"),
                part(P::FoldersBold, "Folder names in bold"),
                (Ctl::RowEdge, "File list selection line width", "Also Home. Side: Selection"),
                part(P::HeaderH, "Column titles height"),
                part(P::HeaderWeight, "Column titles boldness"),
                col("selection", "File list selected rows"),
                col("selection-text", "File list selected row text"),
                col("selection-edge", "File list selection line"),
                col("list-header", "Column titles bar"),
                col("list-header-text", "Column title text"),
                col("column-line", "Lines between column titles"),
            ],
        ),
        (
            "Selection and hover",
            vec![
                (Ctl::Part(P::SelectLook), "Selected row look", "Colours: File list and Sidebar"),
                (Ctl::Part(P::SelectLine), "Selection line side", "File list and sidebar together"),
                part(P::SelectBold, "Selected name in bold"),
                part(P::RowRadius, "Row corners"),
                (Ctl::Part(P::HoverLook), "Rows under the mouse, look", "Outline uses Faint text"),
                col("hover", "Rows under the mouse"),
                col("selection-border", "Border round Band rows"),
            ],
        ),
        (
            "Scroll bars",
            vec![
                (Ctl::ScrollWidth, "Scroll bar width", ""),
                part(P::ScrollShow, "When scroll bars show"),
                part(P::ScrollEnds, "Scroll bar ends"),
                part(P::ScrollGap, "Space from the edge"),
                (Ctl::Grip, "Show grip lines", ""),
                col("scrollbar", "Scroll bars"),
                col("scrollbar-hover", "Scroll bars, under the mouse"),
                col("scrollbar-track", "Scroll bar track"),
                col("scrollbar-border", "Scroll bar edges"),
                col("scrollbar-grip-lines", "Grip lines"),
            ],
        ),
        ("Status bar", vec![part(P::StatusH, "Status bar height"), col("status-bar", "Status bar"), col("status-text", "Status bar text"), col("status-line", "Status bar top line")]),
        (
            "Menus",
            vec![
                part(P::MenuRowH, "Menu row height"),
                part(P::MenuPick, "Menu item under the mouse, look"),
                part(P::MenuGroups, "Line or space between menu groups"),
                part(P::MenuRadius, "Menu corners"),
                part(P::MenuShadow, "Menu shadow"),
                col("menu", "Menu background"),
                col("menu-text", "Menu text"),
                col("menu-selection", "Menu item under the mouse"),
                col("menu-selection-text", "Menu item under the mouse, text"),
                col("menu-border", "Menu edges"),
            ],
        ),
    ]
}
/// The cards, in order: one for each part of the window, top to bottom.
const CARDS: [&str; 16] = [
    "Overall look",
    "Main colours",
    "3D edges",
    "Wallpaper",
    "Title bar",
    "Tabs",
    "Window buttons",
    "Toolbar",
    "Buttons",
    "Panels",
    "Sidebar",
    "File list",
    "Selection and hover",
    "Scroll bars",
    "Status bar",
    "Menus",
];
const WHEEL_STEP: f32 = 96.0;
/// Text box ids (textbox.rs: 600-699 belong to the customiser): the one-line box (a colour's
/// #hex, the logo's word, renaming) and the skin's whole text.
const EDIT_BOX: u16 = 600;
/// Results of background work for this window; Tab or Esc in the skin's text.
const WM_APP_BG: u32 = WM_APP + 82;
/// Saving a moment after the last change; trying to draw again after a refused frame.
const SAVE_TIMER: usize = 1;
const RETRY_TIMER: usize = 2;
const SAVE_DELAY_MS: u32 = 600;
/// Undo steps kept (all skins together).
const UNDO_STEPS: usize = 200;
/// Typing in the skin's text is one undo step until a pause this long (seconds).
const TYPING_PAUSE: f64 = 1.5;
/// The undo group for typing in the skin's text (a box being edited has its own number).
const TEXT_GROUP: u64 = u64::MAX;

/// Every colour a skin has (skin::COLOURS), by the skin's name for it, with its title here, in
/// the order the skin's text lists them in (`cards` places each in its card).
const COLOURS: [(&str, &str); 76] = [
    // Main colours
    ("accent", "Accent"),
    ("accent-text", "Text on accent"),
    ("background", "Background, behind the panels"),
    ("text", "Text, everywhere"),
    ("text-bright", "Strong text: titles and headings"),
    ("muted", "Faint text"),
    ("disabled", "Greyed-out text"),
    ("line", "Thin lines"),
    ("hover", "Rows under the mouse"),
    // Title bar
    ("title-bar", "Title bar"),
    ("title-bar-inactive", "Title bar, inactive"),
    ("title-gloss", "Title bar shine, top half"),
    ("highlight", "Thin shine along the top of bars and tabs"),
    ("title-text", "Other windows' title text"),
    ("title-text-inactive", "Other windows' title text, inactive"),
    ("title-text-shadow", "Other windows' title text shadow"),
    ("logo", "Logo box"),
    ("logo-letters", "Logo box text"),
    // Window buttons
    ("caption-hover", "Minimise, maximise, + and tab close, under the mouse"),
    ("caption-pressed", "Minimise, maximise, + and tab close, pressed"),
    ("close-hover", "Close (window X), under the mouse"),
    ("marble", "Minimise and maximise marbles"),
    ("marble-close", "Close marble"),
    // Tabs
    ("tab", "Other tabs"),
    ("tab-text", "Other tab names"),
    ("tab-active", "Current tab"),
    ("tab-active-text", "Current tab text"),
    ("tab-border", "Tab edges"),
    ("tab-line", "Current tab line"),
    // Toolbar
    ("toolbar", "Toolbar, also the current tab"),
    ("toolbar-line", "Toolbar bottom line"),
    ("tool-hover", "Back, forward, up and refresh, under the mouse"),
    ("tool-pressed", "Back, forward, up and refresh, pressed"),
    ("tool-border", "Back, forward, up and refresh, edge under the mouse"),
    ("back-button", "Back and forward glass buttons"),
    ("input", "Path box and search bar"),
    ("input-border", "Path box and search bar edges"),
    ("path-hover", "Path, under the mouse"),
    // Buttons
    ("button", "Buttons and current folder box"),
    ("button-hover", "Sort, View and Settings, under the mouse"),
    ("button-text", "Button text"),
    // Sidebar
    ("sidebar", "Sidebar background"),
    ("sidebar-text", "Sidebar text"),
    ("sidebar-panel", "Sidebar group boxes"),
    ("panel-header", "Sidebar group box heading bar"),
    ("panel-header-text", "Sidebar heading text"),
    ("sidebar-selection", "Sidebar selected place"),
    ("sidebar-selection-text", "Sidebar selected place text"),
    ("sidebar-selection-edge", "Sidebar selection line"),
    // File list
    ("panel", "File list background"),
    ("list-header", "Column titles bar"),
    ("list-header-text", "Column title text"),
    ("column-line", "Lines between column titles"),
    ("selection", "File list selected rows"),
    ("selection-edge", "File list selection line"),
    ("selection-text", "File list selected row text"),
    ("selection-border", "Border round Band rows"),
    // Scroll bars
    ("scrollbar", "Scroll bars"),
    ("scrollbar-hover", "Scroll bars, under the mouse"),
    ("scrollbar-track", "Scroll bar track"),
    ("scrollbar-border", "Scroll bar edges"),
    ("scrollbar-grip-lines", "Grip lines"),
    // Status bar
    ("status-bar", "Status bar"),
    ("status-text", "Status bar text"),
    ("status-line", "Status bar top line"),
    // Drive usage bars
    ("usage-bar", "Drive bar"),
    ("usage-fill", "Drive bar fill"),
    // Menus
    ("menu", "Menu background"),
    ("menu-text", "Menu text"),
    ("menu-selection", "Menu item under the mouse"),
    ("menu-selection-text", "Menu item under the mouse, text"),
    ("menu-border", "Menu edges"),
    // 3D edges
    ("bevel-highlight", "Outer light edge"),
    ("bevel-light", "Inner light edge"),
    ("bevel-shadow", "Inner shadow edge"),
    ("bevel-dark", "Outer shadow edge"),
];

/// The choices of the drop-downs.
const SHAPES: [(Shape, &str, &str); 3] = [(Shape::Slanted, "slanted", "Slanted"), (Shape::Straight, "straight", "Straight"), (Shape::Rounded, "rounded", "Rounded")];
const SLANTS: [f32; 9] = [0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0];
const ROW_HEIGHTS: [f32; 7] = [24.0, 26.0, 28.0, 30.0, 32.0, 34.0, 36.0];
const FONT_SIZES: [f32; 8] = [8.0, 8.5, 9.0, 9.5, 10.0, 10.5, 11.0, 12.0];
const LINE_WIDTHS: [f32; 7] = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
const SIDEBAR_ROWS: [f32; 9] = [20.0, 22.0, 24.0, 26.0, 28.0, 30.0, 32.0, 36.0, 40.0];
const SCROLL_WIDTHS: [f32; 8] = [4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 18.0];

/// A line-width drop-down's skin setting and what the skin has it at now.
fn line_width(ctl: Ctl, t: &crate::theme::Theme) -> Option<(&'static str, f32)> {
    match ctl {
        Ctl::TabLine => Some(("tab-line-width", t.tab_line_w)),
        Ctl::ToolbarLine => Some(("toolbar-line-width", t.toolbar_line_w)),
        Ctl::RowEdge => Some(("selection-edge-width", t.select_edge_w)),
        Ctl::SidebarEdge => Some(("sidebar-selection-edge-width", t.sidebar_select_edge_w)),
        _ => None,
    }
}
const BUTTON_STYLES: [(ButtonStyle, &str, &str); 2] = [(ButtonStyle::Flat, "flat", "Flat"), (ButtonStyle::Liquid, "liquid", "Glossy")];
const CAPTION_STYLES: [(CaptionStyle, &str, &str); 3] = [(CaptionStyle::Symbols, "symbols", "Symbols"), (CaptionStyle::Marbles, "marbles", "Marbles"), (CaptionStyle::Tiles, "tiles", "Tiles")];
const TAB_STYLES: [(TabStyle, &str, &str); 2] = [(TabStyle::Attached, "attached", "Joined"), (TabStyle::Pills, "pills", "Pills")];
const SIDEBAR_STYLES: [(SidebarStyle, &str, &str); 2] = [(SidebarStyle::Plain, "plain", "Plain"), (SidebarStyle::Panels, "panels", "Panels")];
const ERAS: [(Era, &str, &str); 5] = [(Era::None, "none", "Off"), (Era::Win98, "98", "Windows 98"), (Era::Win2000, "2000", "Windows 2000"), (Era::Xp, "xp", "Windows XP"), (Era::Win7, "7", "Windows 7")];
const ICON_SETS: [(IconSet, &str, &str); 4] = [(IconSet::Windows, "windows", "Windows"), (IconSet::Classic, "classic", "Classic"), (IconSet::Xp, "xp", "Windows XP"), (IconSet::Aero, "aero", "Windows 7 (Aero)")];
/// The Logo setting (as Settings > Appearance offers it).
const LOGOS: [(LogoStyle, &str, &str); 4] = [(LogoStyle::Box, "box", "In a box"), (LogoStyle::Chrome, "chrome", "Chrome letters"), (LogoStyle::Caps, "caps", "GLiNT"), (LogoStyle::None, "none", "None")];
const FITS: [(WallpaperFit, &str, &str); 5] = [
    (WallpaperFit::Fill, "fill", "Fill"),
    (WallpaperFit::Whole, "whole", "Whole picture"),
    (WallpaperFit::Stretch, "stretch", "Stretch"),
    (WallpaperFit::Tile, "tile", "Tile"),
    (WallpaperFit::Actual, "actual", "Actual size"),
];
const AREAS: [(WallpaperArea, &str, &str); 4] = [
    (WallpaperArea::Middle, "middle", "Middle, stays put"),
    (WallpaperArea::Shrinks, "middle-shrinks", "Middle, shrinks"),
    (WallpaperArea::WindowSized, "window-sized", "Window size, in middle"),
    (WallpaperArea::Window, "window", "Whole window"),
];
/// DIPs in a point (9 pt = 12 DIPs).
const PT: f32 = 96.0 / 72.0;
/// A colour row's note when the colour follows another: the row shows which ("follows Accent").
const FOLLOWS: &str = "(follows)";

thread_local! {
    /// The text boxes' colours (background COLORREF, text COLORREF, brush), for WM_CTLCOLOREDIT.
    static EDIT_COLORS: Cell<(u32, u32, isize)> = const { Cell::new((0, 0, 0)) };
    /// How narrow the window can get (its layout's), for WM_GETMINMAXINFO.
    static MIN_WIDTH: Cell<f32> = const { Cell::new(MIN_W) };
    /// The folded sections, kept while Glint runs (None: as at first).
    static FOLDED: RefCell<Option<Vec<&'static str>>> = const { RefCell::new(None) };
}

/// Windows' colour picker remembers 16 colours of the person's ("custom colours") while Glint runs.
static CUSTOM_COLOURS: Mutex<[u32; 16]> = Mutex::new([0x00ff_ffff; 16]);

/// A control on the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctl {
    /// One of COLOURS.
    Colour(usize),
    Shape,
    Slant,
    RowHeight,
    FontSize,
    /// How thick the line on the current tab and the one under the toolbar are.
    TabLine,
    ToolbarLine,
    /// The selected rows' edge width (selection-edge-width).
    RowEdge,
    /// The sidebar's selected place's edge width (sidebar-selection-edge-width).
    SidebarEdge,
    LogoText,
    Dark,
    /// The style: buttons, window buttons, tabs, sidebar, scrollbar grip.
    Buttons,
    Caption,
    Tabs,
    Sidebar,
    Grip,
    /// The older Windows the skin looks like, and its icons.
    Era,
    Icons,
    /// The Logo setting (not part of the skin).
    Logo,
    /// The wallpaper: its picture (the box, BROWSE… and NONE), how it fits, and its sliders.
    Picture,
    /// The pictures next to the skin, to click.
    Thumbs,
    Fit,
    /// Where it lies (wallpaper-area).
    Area,
    /// Where it sits: the pad (across and down at once), the 3x3 squares and the two numbers.
    Place,
    Slide(Slide),
    /// The box above the colours: only colours whose name has what's typed in it show.
    Search,
    /// One of the main window's parts' settings (parts.rs): a drop-down, or a switch for one that
    /// is true or false.
    Part(crate::parts::P),
    /// The skin's font (the fonts installed), the sidebar's row height, the scroll bars' thickness.
    Font,
    SidebarRow,
    ScrollWidth,
}

/// A wallpaper slider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slide {
    Opacity,
    Zoom,
    /// The two halves of `wallpaper-position`: across, then down (0 to 100%).
    Across,
    Down,
    Blur,
    Fade,
    /// How much shows through the bars and panels behind the whole window (wallpaper-panels).
    Panels,
}

/// Each slider: its setting, smallest and biggest, a key's step and PageUp / PageDown's.
const SLIDES: [(Slide, &str, f32, f32, f32, f32); 7] = [
    (Slide::Opacity, "wallpaper-opacity", 0.0, 100.0, 1.0, 10.0),
    (Slide::Zoom, "wallpaper-zoom", -500.0, 500.0, 5.0, 50.0),
    (Slide::Across, "wallpaper-position", 0.0, 100.0, 1.0, 10.0),
    (Slide::Down, "wallpaper-position", 0.0, 100.0, 1.0, 10.0),
    (Slide::Blur, "wallpaper-blur", 0.0, 50.0, 1.0, 5.0),
    (Slide::Fade, "wallpaper-fade", 0.0, 50.0, 1.0, 5.0),
    (Slide::Panels, "wallpaper-panels", 0.0, 90.0, 1.0, 10.0),
];

/// Which part of a control: the main one (a colour's swatch), a colour's #hex box, or its NONE
/// button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Main,
    Hex,
    Clear,
    /// One of the wallpaper's 3x3 squares (0 the top left, 8 the bottom right).
    Grid(u8),
    /// One of the pictures next to the skin.
    Thumb(u8),
}

/// The buttons under the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Btn {
    Undo,
    Redo,
    New,
    Duplicate,
    Rename,
    Delete,
    Import,
    Export,
    Folder,
    /// One column: the skin drop-down at the top, and the menu with New and the rest.
    Skin,
    More,
}

/// Which sides of a button under the skin list have another button beside it (two to a row, OPEN
/// SKINS FOLDER on its own): those sides lean, the others are straight.
fn beside_of(b: Btn) -> crate::pathbar::Beside {
    use crate::pathbar::Beside;
    match b {
        Btn::Undo | Btn::New | Btn::Rename | Btn::Import => Beside::RIGHT,
        Btn::Redo | Btn::Duplicate | Btn::Delete | Btn::Export => Beside::LEFT,
        Btn::Folder | Btn::Skin | Btn::More => Beside::NONE,
    }
}

const BUTTONS: [(Btn, &str); 9] = [
    (Btn::Undo, "UNDO"),
    (Btn::Redo, "REDO"),
    (Btn::New, "NEW"),
    (Btn::Duplicate, "DUPLICATE"),
    (Btn::Rename, "RENAME"),
    (Btn::Delete, "DELETE"),
    (Btn::Import, "IMPORT\u{2026}"),
    (Btn::Export, "EXPORT\u{2026}"),
    (Btn::Folder, "OPEN SKINS FOLDER"),
];

/// The slim layout's icon buttons under the list (their names show over them under the mouse).
const ICONS: [(Btn, &str); 9] = [
    (Btn::Undo, "Undo"),
    (Btn::Redo, "Redo"),
    (Btn::New, "New"),
    (Btn::Duplicate, "Duplicate"),
    (Btn::Rename, "Rename"),
    (Btn::Delete, "Delete"),
    (Btn::Import, "Import\u{2026}"),
    (Btn::Export, "Export\u{2026}"),
    (Btn::Folder, "Open skins folder"),
];

/// The one-column layout's menu (a line after Delete).
const MORE: [(Btn, &str); 7] = [
    (Btn::New, "New"),
    (Btn::Duplicate, "Duplicate"),
    (Btn::Rename, "Rename"),
    (Btn::Delete, "Delete"),
    (Btn::Import, "Import\u{2026}"),
    (Btn::Export, "Export\u{2026}"),
    (Btn::Folder, "Open skins folder"),
];

/// The menu open that isn't a drop-down's on the page.
#[derive(Clone, Debug, PartialEq)]
enum Pop {
    /// The skins, by id in the menu's order.
    Skins(Vec<String>),
    /// MORE.
    More,
}

/// The small drawn icons (the sort order's editor's too).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Glyph {
    Undo,
    Redo,
    More,
    New,
    Duplicate,
    Rename,
    Delete,
    Import,
    Export,
    Folder,
    Search,
    Clear,
    /// ⇱ and ⇲: DETACH and DOCK.
    Detach,
    Dock,
    /// ? (GUIDE) and ☰ (START FROM…).
    Help,
    Menu,
}

fn glyph_of(b: Btn) -> Glyph {
    match b {
        Btn::Undo => Glyph::Undo,
        Btn::Redo => Glyph::Redo,
        Btn::New => Glyph::New,
        Btn::Duplicate => Glyph::Duplicate,
        Btn::Rename => Glyph::Rename,
        Btn::Delete => Glyph::Delete,
        Btn::Import => Glyph::Import,
        Btn::Export => Glyph::Export,
        Btn::Folder => Glyph::Folder,
        Btn::Skin | Btn::More => Glyph::More,
    }
}

/// What is under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hot {
    Close,
    /// A skin in the list (index into the laid-out items).
    Item(usize),
    Button(Btn),
    Ctl(Ctl, Part),
    ListThumb,
    PageThumb,
    /// A section's heading, which folds it (one column).
    Fold(&'static str),
}

/// What has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spot {
    List,
    Button(Btn),
    Ctl(Ctl, Part),
    /// A section's heading (one column): Space or Enter folds it.
    Fold(&'static str),
}

/// What the one-line text box is for while it's shown.
#[derive(Clone, Debug, PartialEq)]
enum Editing {
    /// A colour's #hex or gradient (COLOURS index), with how it was before (Esc puts it back).
    Hex(usize, Before),
    /// The word in the logo, with the word before.
    LogoText(String),
    /// Renaming this skin (its id).
    Rename(String),
    /// The wallpaper's file name, with what the skin had before.
    Picture(String),
    /// A slider's number typed in its box, with the value before (Esc puts it back).
    Number(Slide, f32),
    /// The search above the colours, with what it was before (Esc puts it back).
    Search(String),
}

/// A slider being dragged: which, and the skin's text before the drag (taken at its first
/// change, with the skin's id: Esc puts it back).
#[derive(Clone, Debug, PartialEq)]
struct Sliding {
    slide: Slide,
    /// The position pad (across and down at once), not a slider: where it was grabbed, the place
    /// then (0 to 100 each way), and how much room the picture has to move in the pad each way.
    pad: bool,
    grab: Option<(Point, (f32, f32), (f32, f32))>,
    before: Option<(String, String)>,
    /// A slider grabbed by its knob: how far right of the knob's middle the mouse was (the knob
    /// moves with the mouse from there, without jumping to it).
    dx: f32,
}

impl Sliding {
    fn new(slide: Slide) -> Sliding {
        Sliding { slide, pad: false, grab: None, before: None, dx: 0.0 }
    }

    fn pad(at: Point, place: (f32, f32), room: (f32, f32)) -> Sliding {
        Sliding { slide: Slide::Across, pad: true, grab: Some((at, place, room)), before: None, dx: 0.0 }
    }

    /// The drag is about to change skin `id`, whose text is `text` now: kept, the first time.
    fn changing(&mut self, id: &str, text: &str) {
        if self.before.is_none() {
            self.before = Some((id.to_string(), text.to_string()));
        }
    }

    /// What Esc puts back: the text before the drag - only into the skin it was taken from, and
    /// only if that's still the one in use.
    fn put_back(&self, in_use: &str) -> Option<(&str, &str)> {
        self.before.as_ref().filter(|(id, _)| id.eq_ignore_ascii_case(in_use)).map(|(id, t)| (id.as_str(), t.as_str()))
    }
}

/// A colour as it was when its box was clicked.
#[derive(Clone, Debug, PartialEq)]
struct Before {
    /// What the box showed.
    shown: String,
    /// The skin's own line for it (None: it had none).
    own: Option<String>,
    /// What goes back into the skin if the change is undone: its own line, or else the colour it
    /// followed, or else what the box showed.
    put_back: String,
}

/// One of COLOURS as the skin in use has it, ready to show.
#[derive(Clone, Debug, Default)]
struct Shown {
    paint: Paint,
    /// The paint as a skin writes it (`paint_text`).
    text: String,
    /// Another colour's name, when it follows that one ("accent", "accent 16%").
    follows: Option<String>,
    /// That, in words for the row ("follows Accent").
    hint: String,
}

/// Something the main window does for this window after the message (see the module comment).
#[derive(Clone, Debug)]
pub enum Job {
    /// Show this window (just made), cloaked until its first frame.
    Show,
    /// Bring it to the front (CUSTOMISE… while it's open).
    Activate,
    /// A drop-down's list: a native menu at `menu.at` (this window's client DIPs).
    Menu(Menu),
    /// A drop-down's list drawn by Glint under the box at this rect (client DIPs): at most a
    /// dozen rows, the rest scrolled to (a long list - the fonts - never fills the screen).
    Drop(crate::droplist::DropList, crate::ui::Rect),
    /// Windows' colour picker for COLOURS[at], starting at this COLORREF.
    Colour { at: usize, initial: u32 },
    /// Windows' "Open" dialog for a skin file.
    Import,
    /// Windows' "Save as" dialog, suggesting this file name.
    Export(String),
    /// Windows' "Open" dialog for a wallpaper, starting in this folder.
    Picture(Option<PathBuf>),
    /// Put the window here (after a DPI change).
    Place(RECT),
    /// Docked beside the main window again, this wide (pixels): its layout changed.
    Dock(i32),
    Close,
}

/// What a job brought back.
#[derive(Clone, Debug, PartialEq)]
pub enum JobDone {
    Nothing,
    /// The menu item picked (0: none).
    Picked(u32),
    /// COLOURS[at] picked as this COLORREF.
    Colour(usize, u32),
    Import(PathBuf),
    Export(PathBuf),
    Picture(PathBuf),
}

/// What the main window should do after this window handled something.
#[derive(Clone, Debug)]
pub enum Ask {
    Command(Command),
    Job(Job),
    /// Paint the main window again (the skin it shows changed).
    RedrawMain,
}

/// Results of background work.
#[derive(Debug)]
enum Bg {
    /// A skin file read for IMPORT…, with the pictures and the other half it needs brought in (or
    /// why it couldn't be read).
    Imported(Result<skin::Import, String>),
    /// The skins folder exists now: open it in a tab.
    Open(PathBuf),
    /// The pictures next to the skin (folder `dir`), newest first, small, and the wallpaper in use
    /// (`current`), small, for the position pad.
    Pictures { dir: PathBuf, current: String, list: Vec<(PathBuf, Option<Arc<thumbs::Picture>>)>, current_pic: Option<(Arc<thumbs::Picture>, (f32, f32))>, too_big: bool },
}

/// Background results waiting for their window: (the window's number, the result).
static RESULTS: Mutex<Vec<(u64, Bg)>> = Mutex::new(Vec::new());
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// Hands a background result to the window (by its handle and number) and wakes it.
fn post(hwnd: isize, serial: u64, bg: Bg) {
    RESULTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((serial, bg));
    // SAFETY: posting to a window by handle; if it's gone the message is simply dropped.
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_BG, WPARAM(0), LPARAM(0));
    }
}

/// A skin in the list, as laid out (list coordinates: y from the top of the scrolling list).
#[derive(Clone, Debug)]
struct Item {
    id: String,
    name: String,
    rect: Rect,
}

/// A row of controls on the right, as laid out (page coordinates).
#[derive(Clone, Debug)]
struct Row {
    ctl: Ctl,
    rect: Rect,
    title: &'static str,
    note: &'static str,
}

/// The drawn parts of the page other than rows.
#[derive(Clone, Debug)]
enum Block {
    Heading(Rect, &'static str),
    /// A heading that folds its section (one column), and whether it's open.
    Fold(Rect, &'static str, bool),
    Card(Rect),
    Line(Rect),
}

impl Block {
    fn rect(&self) -> Rect {
        match self {
            Block::Heading(r, _) | Block::Fold(r, _, _) | Block::Card(r) | Block::Line(r) => *r,
        }
    }
}

pub struct SkinWindow {
    hwnd: HWND,
    serial: u64,
    renderer: Renderer,
    dpi: u32,
    /// Client size in DIPs (inside 98 / 2000's window frame, when the skin draws one).
    size: (f32, f32),
    /// The skin is 98 / 2000's: the window has their raised frame (frame.rs).
    classic: bool,
    started: Instant,
    active: bool,
    tracking: bool,
    hover: Option<Hot>,
    pressed: Option<Hot>,
    focus: Spot,
    /// Show the keyboard's outline (after a key was used).
    cues: bool,
    /// Dragging a scrollbar's thumb: which, and where in the thumb it was grabbed.
    drag: Option<(Hot, f32)>,
    items: Vec<Item>,
    list: Scroller,
    rows: Vec<Row>,
    blocks: Vec<Block>,
    page: Scroller,
    layout_dirty: bool,
    /// The library and look last laid out for (a new skin, a renamed one, another skin in use).
    seen: (u64, u64, String),
    /// The one-line text box, and what it's editing.
    edit_box: TextBox,
    editing: Option<Editing>,
    /// The skin the box was opened for, and its text then: what it types goes only into that skin,
    /// and Esc puts that text back.
    editing_for: Option<(String, String)>,
    edit_at: Option<Rect>,
    /// The one-line box was just given its text and isn't in its place yet.
    edit_fresh: bool,
    /// What the search above the colours has in it (empty: every colour shows).
    colour_search: String,
    /// Skins changed and waiting for SAVE_TIMER to save them.
    save_due: Vec<String>,
    /// Undo and redo: a skin's text before a change, with its id; newest last.
    undo: Vec<(String, String)>,
    redo: Vec<(String, String)>,
    /// The group of the last undo step and when it last grew: a box typed in (or the skin's
    /// text typed in without a pause) is one step.
    undo_group: Option<(u64, f64)>,
    /// The group changes join while a box is being edited.
    edit_group: Option<u64>,
    next_group: u64,
    /// The group a key held down joins (Windows repeating it): one undo step for the whole hold.
    key_group: Option<u64>,
    /// The group of the last press on a slider or the pad: a double-click's reset joins it, so one
    /// undo puts back what was there before the first click moved the knob.
    press_group: Option<u64>,
    /// A line under the skin's name: what just happened (a copy made, a problem).
    status: Option<(String, bool)>,
    /// The status is this window's own (not the file worker's "Saved…"), which it doesn't hide.
    status_mine: bool,
    /// The drop-down whose list is open.
    menu_for: Option<Ctl>,
    /// The fonts installed (read when the font drop-down first opens).
    fonts: Vec<String>,
    /// The skin the colour picker or the Save dialog was opened for (the one in use can change
    /// meanwhile, from the main window).
    dialog_for: Option<String>,
    /// "Delete this skin?": the question (drawn by ask.rs) and the skin it's about.
    ask: AskBox,
    asking: Option<(u64, String)>,
    next_question: u64,
    /// Space or Enter on a drop-down: its list opens when that key's character arrives.
    act_on_char: Option<Spot>,
    /// Under Wine a hidden text box stays on screen until a later frame: paint until then.
    repaint_until: f64,
    /// The switches the person just flipped ("Show as text", "Dark skin"), gliding to their new
    /// place (kit.rs).
    flips: kit::Flips,
    /// How thick the scrollbars are in the skin.
    bar: kit::Bar,
    /// How far paired buttons lean where they meet, in the skin (for their exact shapes).
    lean: f32,
    /// An older Windows' scroll bar arrow or track held down, repeating (bar 0: the list, 1: the
    /// page).
    holding: kit::Holding,
    /// The skin's colours as shown (COLOURS's order), and the library, look and skin they were
    /// worked out for.
    colours: Vec<Shown>,
    colours_for: (u64, u64, String),
    /// A wallpaper slider being dragged.
    sliding: Option<Sliding>,
    /// The pictures next to the skin, small (None while one is being read), the folder and
    /// wallpaper they were read for, and the wallpaper in use, small.
    thumbs: Vec<(PathBuf, Option<Arc<thumbs::Picture>>)>,
    thumbs_for: Option<(PathBuf, String)>,
    current_pic: Option<(Arc<thumbs::Picture>, (f32, f32))>,
    /// The main window's file list, roughly (DIPs): the pad is a small copy of it.
    list_size: (f32, f32),
    /// How the window is laid out (the SkinsWindow setting).
    layout: Layout,
    /// The folded sections.
    folded: Vec<&'static str>,
    /// The skins' drop-down or the menu of the rest, while it's open.
    pop: Option<Pop>,
}

/// Registers the window class (once; registering again just fails harmlessly).
fn register_class() {
    // SAFETY: plain class registration with a static name and our own window procedure.
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            // (Double-clicks come as such: a double-click in the text picks a word.)
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hIcon: LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("GlintSkinWindow"),
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

/// Makes the customiser (hidden: show it with [`Job::Show`] once its state is kept), owned by
/// `owner`, beside it, laid out as `layout`. None if Windows can't make it.
pub fn create(owner: HWND, theme: &Theme, layout: Layout) -> Option<SkinWindow> {
    register_class();
    let title: Vec<u16> = TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: window creation and plain queries on our own windows; all structs are valid.
    unsafe {
        let scale = GetDpiForWindow(owner).max(96) as f32 / 96.0;
        // As tall as the main window, beside it (`beside`).
        let metrics = Metrics::of(layout);
        MIN_WIDTH.with(|m| m.set(metrics.min_width));
        let (x, y, w, h) = beside(owner, (metrics.width * scale) as i32);
        let instance = GetModuleHandleW(None).ok()?;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GlintSkinWindow"),
            PCWSTR(title.as_ptr()),
            WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        let dpi = GetDpiForWindow(hwnd).max(96);
        let renderer = match Renderer::new(hwnd, dpi) {
            Ok(r) => r,
            Err(e) => {
                log::error(&format!("Failed to draw the skin customiser: {e}"));
                let _ = DestroyWindow(hwnd);
                return None;
            }
        };
        frame::init(hwnd);
        let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        let edit_box = TextBox::new(hwnd, EDIT_BOX);
        let mut window = SkinWindow {
            hwnd,
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
            renderer,
            dpi,
            size: (0.0, 0.0),
            classic: false,
            started: Instant::now(),
            active: true,
            tracking: false,
            hover: None,
            pressed: None,
            focus: Spot::List,
            cues: false,
            drag: None,
            items: Vec::new(),
            list: Scroller::default(),
            rows: Vec::new(),
            blocks: Vec::new(),
            page: Scroller::default(),
            layout_dirty: true,
            seen: (0, 0, String::new()),
            edit_box,
            editing: None,
            editing_for: None,
            edit_at: None,
            colour_search: String::new(),
            edit_fresh: false,
            save_due: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            undo_group: None,
            edit_group: None,
            next_group: 1,
            key_group: None,
            press_group: None,
            status: None,
            status_mine: false,
            menu_for: None,
            fonts: Vec::new(),
            dialog_for: None,
            ask: AskBox::default(),
            asking: None,
            next_question: 1,
            act_on_char: None,
            repaint_until: 0.0,
            flips: kit::Flips::default(),
            bar: kit::Bar::default(),
            lean: 0.0,
            holding: kit::Holding::default(),
            colours: Vec::new(),
            colours_for: (0, 0, String::new()),
            sliding: None,
            thumbs: Vec::new(),
            thumbs_for: None,
            current_pic: None,
            list_size: (900.0, 560.0),
            layout,
            folded: FOLDED.with(|f| f.borrow().clone()).unwrap_or_else(first_folded),
            pop: None,
        };
        window.apply_theme(theme);
        let mut rc = RECT::default();
        if GetClientRect(hwnd, &mut rc).is_ok() {
            window.size = ((rc.right - rc.left) as f32 / window.scale(), (rc.bottom - rc.top) as f32 / window.scale());
        }
        Some(window)
    }
}

/// Runs a job for the window `hwnd` (no state is touched: the main window calls this outside its
/// guard, since menus, the colour picker and the file dialogs run their own message loop).
pub fn run_job(hwnd: HWND, scale: f32, job: Job, show_menu: fn(HWND, &Menu, f32) -> u32) -> JobDone {
    // SAFETY: window operations on our own window, with no borrow of any state held.
    unsafe {
        match job {
            Job::Show => {
                frame::cloak(hwnd, true);
                let _ = ShowWindow(hwnd, SW_SHOW);
                // Flush against the main window now that Windows knows its borders.
                if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                    let r = visible_rect(hwnd);
                    place_visible(hwnd, beside(owner, r.right - r.left));
                }
                let _ = RedrawWindow(Some(hwnd), None, None, RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN);
                frame::cloak(hwnd, false);
                let _ = crate::testhide::front(hwnd);
            }
            Job::Activate => {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = crate::testhide::front(hwnd);
            }
            Job::Menu(menu) => return JobDone::Picked(show_menu(hwnd, &menu, scale)),
            Job::Drop(mut list, at) => {
                let mut corners = [POINT { x: (at.x * scale).round() as i32, y: (at.y * scale).round() as i32 }, POINT { x: (at.right() * scale).round() as i32, y: (at.bottom() * scale).round() as i32 }];
                for p in &mut corners {
                    let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, p);
                }
                list.under = RECT { left: corners[0].x, top: corners[0].y, right: corners[1].x, bottom: corners[1].y };
                return JobDone::Picked(crate::droplist::show(hwnd, &list));
            }
            Job::Colour { at, initial } => return pick_colour(hwnd, initial).map_or(JobDone::Nothing, |c| JobDone::Colour(at, c)),
            Job::Import => return open_file(hwnd).map_or(JobDone::Nothing, JobDone::Import),
            Job::Export(name) => return save_file(hwnd, &name).map_or(JobDone::Nothing, JobDone::Export),
            Job::Picture(start) => return open_picture(hwnd, start.as_deref()).map_or(JobDone::Nothing, JobDone::Picture),
            Job::Place(r) => {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            Job::Dock(w) => {
                if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                    place_visible(hwnd, beside(owner, w));
                }
            }
            Job::Close => {
                // The main window gets the focus back (not whatever Windows would pick).
                if let Ok(owner) = GetWindow(hwnd, GW_OWNER) {
                    let _ = crate::testhide::front(owner);
                }
                let _ = DestroyWindow(hwnd);
            }
        }
    }
    JobDone::Nothing
}

/// Windows' colour picker, owned by `owner`, starting at `initial` (a COLORREF). None if cancelled.
fn pick_colour(owner: HWND, initial: u32) -> Option<u32> {
    let mut custom = *CUSTOM_COLOURS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut refs: [COLORREF; 16] = custom.map(COLORREF);
    let mut cc = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(initial),
        lpCustColors: refs.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN | CC_ANYCOLOR,
        ..Default::default()
    };
    // SAFETY: the struct and the 16 custom colours it points to live across the (modal) call.
    let ok = unsafe { ChooseColorW(&mut cc) }.as_bool();
    for (c, r) in custom.iter_mut().zip(refs) {
        *c = r.0;
    }
    *CUSTOM_COLOURS.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = custom;
    ok.then_some(cc.rgbResult.0)
}

/// The file types the file dialogs offer.
fn filters() -> [COMDLG_FILTERSPEC; 3] {
    [
        COMDLG_FILTERSPEC { pszName: w!("Skins (*.glint, *.zip)"), pszSpec: w!("*.glint;*.zip") },
        COMDLG_FILTERSPEC { pszName: w!("Text files (*.txt)"), pszSpec: w!("*.txt") },
        COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
    ]
}

/// The file types EXPORT… offers: a skin with its wallpaper (a zip) first when it has one.
fn save_filters(zip: bool) -> Vec<COMDLG_FILTERSPEC> {
    let packed = COMDLG_FILTERSPEC { pszName: w!("Skin with its wallpaper (*.zip)"), pszSpec: w!("*.zip") };
    let plain = COMDLG_FILTERSPEC { pszName: w!("Skin only (*.glint)"), pszSpec: w!("*.glint") };
    let all = COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") };
    if zip { vec![packed, plain, all] } else { vec![plain, all] }
}

/// Windows' "Open" dialog for a skin file. None if cancelled.
fn open_file(owner: HWND) -> Option<PathBuf> {
    // SAFETY: COM is set up on this (the UI) thread (main.rs); the interfaces are released when
    // dropped and the returned string is freed with CoTaskMemFree.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        let _ = dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        let _ = dialog.SetTitle(w!("Import a skin"));
        let _ = dialog.SetFileTypes(&filters());
        dialog.Show(Some(owner)).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text.filter(|t| !t.is_empty()).map(PathBuf::from)
    }
}

/// Windows' "Open" dialog for a wallpaper, starting in `start` (the skin's folder). None if
/// cancelled.
fn open_picture(owner: HWND, start: Option<&std::path::Path>) -> Option<PathBuf> {
    // SAFETY: as in `open_file`; the folder's name is NUL-terminated and outlives the call.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        let _ = dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST);
        let _ = dialog.SetTitle(w!("Pick a wallpaper"));
        let types = [
            COMDLG_FILTERSPEC { pszName: w!("Pictures"), pszSpec: w!("*.jpg;*.jpeg;*.png;*.bmp;*.gif;*.webp;*.tif;*.tiff;*.jxr;*.heic") },
            COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
        ];
        let _ = dialog.SetFileTypes(&types);
        if let Some(dir) = start.filter(|d| d.is_dir()) {
            let wide: Vec<u16> = dir.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(Some(owner)).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text.filter(|t| !t.is_empty()).map(PathBuf::from)
    }
}

/// Windows' "Save as" dialog, suggesting `file_name`. None if cancelled.
fn save_file(owner: HWND, file_name: &str) -> Option<PathBuf> {
    // SAFETY: as in `open_file`.
    unsafe {
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        let _ = dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT);
        let _ = dialog.SetTitle(w!("Export the skin"));
        let zip = file_name.to_ascii_lowercase().ends_with(".zip");
        let _ = dialog.SetFileTypes(&save_filters(zip));
        let _ = dialog.SetDefaultExtension(if zip { w!("zip") } else { w!("glint") });
        let _ = dialog.SetFileName(&HSTRING::from(file_name));
        dialog.Show(Some(owner)).ok()?;
        let name = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text.filter(|t| !t.is_empty()).map(PathBuf::from)
    }
}

/// The window procedure. Messages that need no state are answered here; the rest go to the state,
/// which the main window keeps, through its guard (window::skin_message).
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // The skin's own menus (oldmenu.rs).
    if let Some(r) = crate::oldmenu::handle(hwnd, msg, wparam, lparam) {
        return r;
    }
    match msg {
        // SAFETY: lparam is the NCCALCSIZE_PARAMS Windows passed with this message.
        WM_NCCALCSIZE if wparam.0 != 0 => return unsafe { frame::nc_calc_size(hwnd, wparam, lparam) },
        WM_ERASEBKGND => return LRESULT(1),
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => return edit_colors(HDC(wparam.0 as *mut _)),
        WM_GETMINMAXINFO => {
            // SAFETY: lparam points to the MINMAXINFO Windows passed.
            unsafe {
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                info.ptMinTrackSize = POINT { x: (MIN_WIDTH.with(Cell::get) * scale) as i32, y: (MIN_H * scale) as i32 };
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => return hit_test(hwnd, wparam, lparam),
        _ => {}
    }
    // SAFETY: plain query; the owner is the main window (set when this window was made).
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default();
    if let Some(r) = crate::window::skin_message(owner, hwnd, msg, wparam, lparam) {
        return r;
    }
    if msg == WM_PAINT {
        // The state is busy (a message inside another): paint on the next turn of the loop instead.
        // SAFETY: plain invalidate of our own window.
        unsafe {
            let _ = ValidateRect(Some(hwnd), None);
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        return LRESULT(0);
    }
    if msg == WM_CLOSE {
        return LRESULT(0); // closing goes through the state (see settingswin.rs)
    }
    // SAFETY: default handling.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Where the customiser goes, `w` pixels wide: docked beside `owner` (on its right if there's room,
/// else on its left, else at the screen's right edge), as tall as it and level with it, so the
/// skin shows in the main window as it changes.
fn beside(owner: HWND, w: i32) -> (i32, i32, i32, i32) {
    let main = visible_rect(owner);
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: plain queries with valid out-structs.
    let work = unsafe {
        if GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() { info.rcWork } else { main }
    };
    let w = w.min((work.right - work.left).max(200));
    let h = (main.bottom - main.top).min(work.bottom - work.top).max(300);
    let x = if main.right + w <= work.right {
        main.right
    } else if main.left - w >= work.left {
        main.left - w
    } else {
        (work.right - w).max(work.left)
    };
    let y = main.top.clamp(work.top, (work.bottom - h).max(work.top));
    (x, y, w, h)
}

/// WM_NCHITTEST: the edges resize, the rest is ours. The title bar doesn't move the window: it
/// stays docked to the main window (`follow_owner`).
fn hit_test(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(edge) = frame::resize_edge(hwnd, wparam, lparam) {
        return LRESULT(edge as isize);
    }
    let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
    let mut rc = RECT::default();
    // SAFETY: plain queries on our own window with valid out-structs.
    let scale = unsafe {
        let _ = ScreenToClient(hwnd, &mut pt);
        let _ = GetClientRect(hwnd, &mut rc);
        GetDpiForWindow(hwnd).max(96) as f32 / 96.0
    };
    let (x, y, w) = (pt.x as f32 / scale, pt.y as f32 / scale, rc.right as f32 / scale);
    let _ = (x, y, w);
    LRESULT(HTCLIENT as isize)
}

/// WM_CTLCOLOREDIT / WM_CTLCOLORSTATIC (a read-only text box): the text boxes in the skin's colours.
fn edit_colors(hdc: HDC) -> LRESULT {
    let (bg, fg, brush) = EDIT_COLORS.with(|c| c.get());
    // SAFETY: `hdc` is the control's DC for this paint; the brush lives until the theme changes.
    unsafe {
        SetTextColor(hdc, COLORREF(fg));
        SetBkColor(hdc, COLORREF(bg));
    }
    if brush == 0 {
        return LRESULT(COLOR_WINDOW.0 as isize + 1);
    }
    LRESULT(brush)
}

/// The skin's colours as the customiser shows them, in COLOURS's order (skin text `text`).
fn shown_colours(text: &str) -> Vec<Shown> {
    let all = skin::colours_of(text);
    COLOURS
        .iter()
        .map(|(key, _)| {
            let Some((paint, follows)) = skin::COLOURS.iter().position(|(k, _)| k == key).and_then(|at| all.get(at).cloned()) else { return Shown::default() };
            let hint = follows.as_deref().map(follows_hint).unwrap_or_default();
            Shown { paint, text: paint_text(&paint), follows, hint }
        })
        .collect()
}

/// "follows Accent" for a colour written as "accent", "follows Accent at 16%" for "accent 16%".
fn follows_hint(written: &str) -> String {
    let (name, strength) = match written.rsplit_once(char::is_whitespace) {
        Some((n, pct)) if pct.ends_with('%') => (n.trim(), Some(pct)),
        _ => (written.trim(), None),
    };
    let key = skin::normalize_key(name);
    let title = COLOURS.iter().find(|(k, _)| *k == key).map_or(name, |(_, t)| t);
    match strength {
        Some(pct) => format!("follows {title} at {pct}"),
        None => format!("follows {title}"),
    }
}

/// A paint as a skin writes it: "#e3262d", "none", or a gradient's colours with where each sits
/// ("#fdfdfd, #c3cddb 45%, #dfe6f0"; the first is at the start and the last at the end, so they
/// go without).
fn paint_text(p: &Paint) -> String {
    let Some(g) = p.gradient() else {
        return if p.is_clear() { "none".into() } else { skin::hex_of(p.base()) };
    };
    let stops = g.stops();
    let mut text = String::new();
    for (i, s) in stops.iter().enumerate() {
        if i > 0 {
            text.push_str(", ");
        }
        text.push_str(&skin::hex_of(s.color));
        let ends = (i == 0 && s.at <= 0.0) || (i == stops.len() - 1 && s.at >= 1.0);
        if !ends {
            text.push_str(&format!(" {}%", number(s.at * 100.0)));
        }
    }
    text
}

/// What was typed in colour `key`'s box, tidied ("#E3262D " is "#e3262d"), if a skin may write it
/// there and Glint understands all of it: a colour, "none", another colour's name ("accent 16%")
/// or a gradient. None otherwise (half typed, say).
fn typed_value(key: &str, typed: &str) -> Option<String> {
    let typed = typed.trim().trim_end_matches(';').trim().to_lowercase();
    if typed.is_empty() || typed.contains(['\n', '\r']) || typed.contains("//") {
        return None;
    }
    let line = format!("{key}: {typed}");
    let parsed = skin::parse(&line, "x");
    (parsed.notes.is_empty() && skin::value_in(&line, key).as_deref() == Some(typed.as_str())).then_some(typed)
}

/// The paint a typed value makes in colour `key`'s place (names followed in the standard skins).
#[cfg(test)]
fn typed_paint(key: &str, typed: &str) -> Option<Paint> {
    let value = typed_value(key, typed)?;
    let at = skin::COLOURS.iter().position(|(k, _)| *k == key)?;
    skin::colours_of(&format!("{key}: {value}")).get(at).map(|(p, _)| *p)
}

/// A gradient with its first colour changed to `c` (Windows' colour picker on a gradient's swatch).
fn with_first(p: &Paint, c: Color) -> Paint {
    let Some(g) = p.gradient() else { return c.into() };
    let mut stops = [Stop { at: 0.0, color: c }; crate::theme::MAX_STOPS];
    for (i, s) in g.stops().iter().enumerate() {
        stops[i] = if i == 0 { Stop { at: s.at, color: c } } else { *s };
    }
    Paint::from_stops(&stops[..g.stops().len()])
}

/// A number as a skin writes it: "8", "8.5".
fn number(x: f32) -> String {
    if (x - x.round()).abs() < 0.01 {
        format!("{}", x.round() as i32)
    } else {
        format!("{:.1}", x)
    }
}

/// A slider's setting, smallest, biggest and steps (SLIDES).
fn slide_spec(s: Slide) -> (&'static str, f32, f32, f32, f32) {
    let &(_, key, min, max, step, page) = SLIDES.iter().find(|x| x.0 == s).unwrap_or(&SLIDES[0]);
    (key, min, max, step, page)
}

/// Where a wallpaper sits up and down when its skin doesn't say: the middle, whatever the fit
/// (list.rs).
fn default_down(_fit: WallpaperFit) -> f32 {
    0.5
}

/// What slider `s` shows for skin `t`, as the skin writes it (opacity 0 to 100, zoom -500 to 500,
/// across and down 0 to 100%...).
fn slide_value(s: Slide, t: &Theme) -> f32 {
    let v = match s {
        Slide::Opacity => t.wallpaper_opacity * 100.0,
        // 0 its usual size, 100 twice as big, -100 half as big (skin.rs reads it so).
        Slide::Zoom if t.wallpaper_zoom >= 1.0 => (t.wallpaper_zoom - 1.0) * 100.0,
        Slide::Zoom => 100.0 * (1.0 - 1.0 / t.wallpaper_zoom.max(0.001)),
        Slide::Across => t.wallpaper_at * 100.0,
        Slide::Down => t.wallpaper_at_y.unwrap_or_else(|| default_down(t.wallpaper_fit)) * 100.0,
        Slide::Blur => t.wallpaper_blur as f32,
        Slide::Fade => t.wallpaper_fade * 100.0,
        Slide::Panels => t.wallpaper_panels * 100.0,
    };
    let (_, min, max, ..) = slide_spec(s);
    v.round().clamp(min, max)
}

/// `wallpaper-position` for `across` and `down` (0 to 1; down None: not said). Words where they
/// say it exactly ("left", "right bottom", "middle top"), else two percentages ("30% 70%").
fn position_text(across: f32, down: Option<f32>, fit: WallpaperFit) -> String {
    let near = |a: f32, b: f32| (a - b).abs() < 0.001;
    let word = |v: f32, ends: [&'static str; 2]| {
        if near(v, 0.0) {
            Some(ends[0])
        } else if near(v, 0.5) {
            Some("middle")
        } else if near(v, 1.0) {
            Some(ends[1])
        } else {
            None
        }
    };
    let x = word(across, ["left", "right"]);
    match (x, down) {
        (Some(x), None) => x.to_string(),
        (Some(x), Some(y)) if word(y, ["top", "bottom"]).is_some() => format!("{x} {}", word(y, ["top", "bottom"]).unwrap_or("middle")),
        _ => {
            let y = down.unwrap_or_else(|| default_down(fit));
            format!("{}% {}%", number(across * 100.0), number(y * 100.0))
        }
    }
}

/// What slider `s` at `v` writes in skin `t`'s text (nothing for a value that isn't a number).
/// The value is kept to the slider's ends and to whole steps.
fn slide_changes(s: Slide, v: f32, t: &Theme) -> Vec<(&'static str, String)> {
    if !v.is_finite() {
        return Vec::new();
    }
    let (key, min, max, ..) = slide_spec(s);
    let v = v.round().clamp(min, max);
    let value = match s {
        Slide::Across => position_text(v / 100.0, t.wallpaper_at_y, t.wallpaper_fit),
        Slide::Down => position_text(t.wallpaper_at, Some(v / 100.0), t.wallpaper_fit),
        _ => number(v),
    };
    vec![(key, value)]
}

/// Slider `s` at the point `x` along its track `area`: the value there - or its default
/// `standard` when that's very near, like a notch (within 1% of the slider, or 4 DIPs of the
/// track, whichever is less). Only the mouse snaps: a number typed stays as typed.
fn slide_at(s: Slide, area: Rect, x: f32, standard: f32) -> f32 {
    let (_, min, max, ..) = slide_spec(s);
    let track = (area.w - 14.0).max(1.0);
    let share = ((x - area.x - 7.0) / track).clamp(0.0, 1.0);
    let v = min + share * (max - min);
    let notch = ((max - min) * 0.01).min(4.0 / track * (max - min));
    if (v - standard).abs() <= notch {
        standard
    } else {
        v.round()
    }
}

/// What slider `s` is when the skin doesn't say (`t`: across and down follow its fit).
fn slide_default(s: Slide, t: &Theme) -> f32 {
    match s {
        Slide::Opacity => 50.0,
        Slide::Across => 50.0,
        Slide::Down => default_down(t.wallpaper_fit) * 100.0,
        Slide::Zoom | Slide::Blur | Slide::Fade => 0.0,
        Slide::Panels => 50.0,
    }
}

/// The skin's text with slider `s` back to its default: its line goes (across and down share one:
/// the other half stays).
fn slide_reset(text: &str, s: Slide, t: &Theme) -> String {
    let (key, ..) = slide_spec(s);
    match (s, t.wallpaper_at_y) {
        (Slide::Across, Some(y)) => skin::with_value(text, key, &position_text(0.5, Some(y), t.wallpaper_fit)),
        (Slide::Down, _) if (t.wallpaper_at - 0.5).abs() > 0.001 => skin::with_value(text, key, &position_text(t.wallpaper_at, None, t.wallpaper_fit)),
        _ => skin::without_value(text, key),
    }
}

/// A number typed in slider `s`'s box: "40", "-50", "+20", "30%", " 7.6 " (rounded). None while
/// it isn't one yet ("", "-", "abc"). Past either end it's that end.
fn typed_number(s: Slide, typed: &str) -> Option<f32> {
    let t = typed.trim().trim_end_matches('%').trim();
    let t = t.strip_prefix('+').unwrap_or(t).replace(',', ".");
    let v: f32 = t.parse().ok().filter(|v: &f32| v.is_finite())?;
    let (_, min, max, ..) = slide_spec(s);
    Some(v.round().clamp(min, max))
}

/// Where square `i` of the 3x3 puts the wallpaper: across and down, 0, 50 or 100.
fn grid_place(i: usize) -> (f32, f32) {
    ((i % 3) as f32 * 50.0, (i / 3 % 3) as f32 * 50.0)
}

/// The part of a picture `pic` big (pixels) that fills `dest` big without being pulled out of
/// shape: its middle, cut to `dest`'s shape.
fn cover_src(pic: (f32, f32), dest: (f32, f32)) -> Rect {
    let (pw, ph) = (pic.0.max(1.0), pic.1.max(1.0));
    let k = (dest.0 / pw).max(dest.1 / ph).max(0.0001);
    let (sw, sh) = ((dest.0 / k).min(pw), (dest.1 / k).min(ph));
    Rect::new((pw - sw) / 2.0, (ph - sh) / 2.0, sw, sh)
}

/// Whether `a` and `b` name the same file (as Windows reads names: any capitals, either slash).
fn same_path(a: &std::path::Path, b: &str) -> bool {
    let tidy = |t: &str| t.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    !b.is_empty() && tidy(&a.to_string_lossy()) == tidy(b)
}

/// The pictures in `dir` (not in its folders), newest first, at most `most`.
fn nearby_pictures(dir: &std::path::Path, most: usize) -> Vec<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| thumbs::is_picture(&x.to_string_lossy().to_lowercase())))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().take(most).map(|(_, p)| p).collect()
}

/// Reads the pictures next to the skin small (background thread), and the wallpaper in use
/// (`current`, its full path) small for the position pad. A picture on another computer isn't
/// read: a server that's away would hold this up. Also whether the wallpaper is too big to show
/// (then it isn't on the pad either).
#[allow(clippy::type_complexity)]
fn read_nearby(dir: &std::path::Path, current: &str) -> (Vec<(PathBuf, Option<Arc<thumbs::Picture>>)>, Option<(Arc<thumbs::Picture>, (f32, f32))>, bool) {
    let _com = shell::ComGuard::init();
    let wic = thumbs::wic_factory();
    // Small, with the size it is on disk.
    let small = |path: &std::path::Path, side: u32| {
        let wic = wic.as_ref()?;
        let d = thumbs::decode(wic, &path.to_string_lossy(), side, u64::from(side) * u64::from(side), thumbs::FILE_ORIENTATION)?;
        let size = (d.original.0 as f32, d.original.1 as f32);
        thumbs::Picture::new(d.width, d.height, d.pixels, false).map(|p| (p, size))
    };
    let list: Vec<_> = nearby_pictures(dir, MAX_THUMBS).into_iter().map(|p| {
        let pic = small(&p, 200).map(|(pic, _)| pic);
        (p, pic)
    }).collect();
    let local = !current.is_empty() && !skin::on_network(current);
    let too_big = local && wic.as_ref().is_some_and(|wic| thumbs::too_big_for_wallpaper(wic, current));
    let current_pic = if local && !too_big { small(std::path::Path::new(current), 480) } else { None };
    (list, current_pic, too_big)
}

/// The place (0 to 100 across and down) after the picture on the pad was grabbed at `from` and
/// dragged by `delta`: it moves with the mouse, as far as its `room` in the pad goes (negative
/// room: the picture is bigger than the pad, and its other side shows). No room: it stays. Very
/// near the default (`standard`, within 1%) it lands on it, like a notch.
fn pan_place(from: (f32, f32), delta: (f32, f32), room: (f32, f32), standard: (f32, f32)) -> (f32, f32) {
    let one = |at: f32, d: f32, room: f32, standard: f32| {
        if room.abs() < 1.0 || !d.is_finite() {
            return at;
        }
        let v = (at + d / room * 100.0).clamp(0.0, 100.0);
        if (v - standard).abs() <= 1.0 {
            standard
        } else {
            v.round()
        }
    };
    (one(from.0, delta.0, room.0, standard.0), one(from.1, delta.1, room.1, standard.1))
}

/// A slider's number, as shown after it.
fn slide_label(s: Slide, v: f32) -> String {
    match s {
        Slide::Zoom if v > 0.0 => format!("+{}", number(v)),
        Slide::Across | Slide::Down => format!("{}%", number(v)),
        _ => number(v),
    }
}

/// The wallpaper as the skin's text names it (its own line, without quotes; "" for none).
fn picture_of(text: &str) -> String {
    skin::value_in(text, "wallpaper").unwrap_or_default()
}

/// A wallpaper's name typed in its box, tidied (quotes and spaces round it gone). None for what a
/// skin's line can't hold (a line break, a note).
fn typed_picture(typed: &str) -> Option<String> {
    let v = skin::unquoted(typed);
    (!v.contains(['\n', '\r']) && !v.contains(" //")).then(|| v.to_string())
}

/// How the skin names a picture picked with BROWSE…: by its file name when it's next to the skin
/// (in `skin_dir`, else the skins folder `skins`, where a skin without a folder of its own is);
/// anywhere else by its full path - a bare name would find a picture of that name next to the
/// skin first, not the one picked.
fn written_picture(path: &std::path::Path, skin_dir: Option<&std::path::Path>, skins: &std::path::Path) -> String {
    let same = |a: &std::path::Path, b: &std::path::Path| {
        let tidy = |p: &std::path::Path| p.to_string_lossy().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase();
        tidy(a) == tidy(b)
    };
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) if same(skin_dir.unwrap_or(skins), dir) => name.to_string_lossy().into_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

/// A slider: a thin track and a round knob on it (bigger under the mouse), with the keyboard's
/// ring round the knob.
fn slider(r: &mut Renderer, theme: &Theme, area: Rect, share: f32, standard: f32, look: Look) {
    if area.w < 16.0 {
        return;
    }
    let y = area.y + area.h / 2.0;
    let track = Rect::new(area.x + 7.0, y - 2.0, area.w - 14.0, 4.0);
    r.fill_round_rect(track, 2.0, theme.bar_track);
    let ink = if look.enabled { theme.accent } else { theme.muted.with_alpha(theme.muted.a * 0.5) };
    let x = track.x + track.w * share.clamp(0.0, 1.0);
    // A small mark under the track where the skin's default is (double-click goes back to it),
    // left out only when the slider is at the default; drawn first, so the knob covers it.
    let d = track.x + track.w * standard.clamp(0.0, 1.0);
    if (share - standard).abs() > 0.0005 {
        r.fill_rect(Rect::new((d - 1.0).round(), track.bottom() + 4.0, 2.0, 4.0), theme.muted.with_alpha(theme.muted.a * if look.enabled { 0.9 } else { 0.4 }));
    }
    let k = if look.enabled && (look.hover || look.pressed) { 8.0 } else { 6.5 };
    let knob = Rect::new(x - k, y - k, k * 2.0, k * 2.0);
    r.fill_round_rect(knob, k, ink);
    if look.focused {
        kit::focus_ring_round(r, theme, knob, k, 0.0);
    }
}

impl SkinWindow {
    /// Docked beside the main window again (it moved or changed size, or the customiser did).
    pub fn follow_owner(&mut self) {
        // SAFETY: plain queries and a move of our own window.
        unsafe {
            let Ok(owner) = GetWindow(self.hwnd, GW_OWNER) else { return };
            if IsIconic(owner).as_bool() {
                return;
            }
            let r = visible_rect(self.hwnd);
            place_visible(self.hwnd, beside(owner, r.right - r.left));
        }
        self.fit_client();
    }

    /// Matches the drawing to the window's size. The move above sends WM_SIZE while the app's state
    /// is busy, so that message never reaches us: the old picture showed stretched.
    fn fit_client(&mut self) {
        let mut rc = RECT::default();
        // SAFETY: our own window and a valid out-struct.
        if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_err() || rc.right <= rc.left || rc.bottom <= rc.top {
            return;
        }
        let (w, h) = ((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
        self.renderer.resize(w, h);
        self.size = self.inner_size(w, h);
        self.layout_dirty = true;
        self.edit_at = None;
        self.invalidate();
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// DIPs to pixels.
    pub fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// How wide 98 / 2000's window frame is round everything (0 for other skins or maximised).
    fn border(&self) -> f32 {
        if self.classic && !frame::zoomed(self.hwnd) {
            4.0
        } else {
            0.0
        }
    }

    /// The client area's size in DIPs, less the frame (from `w` x `h` pixels).
    fn inner_size(&self, w: u32, h: u32) -> (f32, f32) {
        let (s, b) = (self.scale(), self.border());
        ((w as f32 / s - 2.0 * b).max(0.0), (h as f32 / s - 2.0 * b).max(0.0))
    }

    fn invalidate(&self) {
        // SAFETY: plain invalidate of our own window.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// The skin changed (or the window was just made): its colours and font here too.
    pub fn apply_theme(&mut self, theme: &Theme) {
        self.renderer.set_font(&theme.font, 1.0);
        self.renderer.set_title_font(&theme.title_font);
        self.layout_dirty = true;
        self.bar = kit::Bar::of(theme);
        self.lean = theme.lean(ksize::BUTTON_LEAN);
        self.edit_at = None;
        if self.classic != theme.era.classic() {
            self.classic = theme.era.classic();
            let mut rc = RECT::default();
            // SAFETY: our own window and a valid out-struct.
            if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
                self.size = self.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
            }
        }
        let (bg, fg) = (theme.input.to_colorref(), theme.text.to_colorref());
        // SAFETY: a new brush replaces the old one, which is deleted (no control paints in between:
        // this is the UI thread). The text box's scroll bars follow light or dark.
        unsafe {
            let brush = CreateSolidBrush(COLORREF(bg));
            let old = EDIT_COLORS.with(|c| c.replace((bg, fg, brush.0 as isize))).2;
            if old != 0 {
                let _ = DeleteObject(HGDIOBJ(old as *mut _));
            }
            frame::apply_theme(self.hwnd, theme);
            let _ = RedrawWindow(Some(self.hwnd), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN);
        }
    }

    /// Opened again (CUSTOMISE… while it's open).
    pub fn reopen(&mut self) -> Vec<Ask> {
        self.invalidate();
        vec![Ask::Job(Job::Activate)]
    }

    /// The main window heard news about skins (a file changed, one was saved): show it - a problem
    /// always, "Saved…" unless it would hide what this window just said (a copy was made).
    pub fn skins_news(&mut self, app: &mut AppState) {
        match app.skins.take_said() {
            Some(Err(e)) => self.say(e, true),
            Some(Ok(s)) if !self.status_mine || self.status.is_none() => self.status = Some((s, false)),
            _ => {}
        }
        self.invalidate();
    }

    /// The skin's colours (`colours`), worked out again when the skin in use or its text changed.
    fn refresh_colours(&mut self, app: &AppState) {
        let now = (app.skins.generation, app.theme_generation, app.settings.skin.clone());
        if now == self.colours_for && self.colours.len() == COLOURS.len() {
            return;
        }
        // A skin whose file is gone shows as AMD Radeon, light or dark.
        let text = app.skins.find(&app.settings.skin).map_or_else(|| format!("dark: {}", app.dark), |e| e.text.clone());
        self.colours = shown_colours(&text);
        self.colours_for = now;
    }

    /// COLOURS[i] as the skin in use has it.
    fn colour(&mut self, app: &AppState, i: usize) -> Shown {
        self.refresh_colours(app);
        self.colours.get(i).cloned().unwrap_or_default()
    }

    /// The skin's own line for COLOURS[i] (None: it has none).
    fn own_value(app: &AppState, i: usize) -> Option<String> {
        app.skins.find(&app.settings.skin).and_then(|e| skin::value_in(&e.text, COLOURS[i].0)).map(|v| v.trim().to_lowercase())
    }

    /// Colour `i`'s box says `typed` now (None: nothing usable, so as it was before): the skin gets
    /// it - or, typed back to what the box showed at first, gets back how it was (a colour that
    /// followed another follows it again).
    fn set_colour(&mut self, app: &mut AppState, i: usize, typed: Option<String>, before: &Before) -> Vec<Ask> {
        let key = COLOURS[i].0;
        let own = Self::own_value(app, i);
        match typed.filter(|t| *t != before.shown) {
            Some(value) if own.as_deref() != Some(value.as_str()) => self.change(app, &[(key, value)]),
            Some(_) => Vec::new(),
            None if own != before.own => self.change(app, &[(key, before.put_back.clone())]),
            None => Vec::new(),
        }
    }

    /// A line under the skin's name about what this window just did (`problem`: in the accent).
    fn say(&mut self, text: impl Into<String>, problem: bool) {
        self.status = Some((text.into(), problem));
        self.status_mine = true;
    }

    /// Everything not saved yet goes to the file worker now (the window is closing).
    pub fn save_now(&mut self, app: &mut AppState) {
        for id in std::mem::take(&mut self.save_due) {
            app.skins.save(&id);
        }
        // SAFETY: stops our own timer (fine if it isn't running).
        unsafe {
            let _ = KillTimer(Some(self.hwnd), SAVE_TIMER);
        }
    }

    // ------------------------------------------------------------ layout

    /// The sizes of the window's layout.
    fn m(&self) -> Metrics {
        Metrics::of(self.layout)
    }

    /// Another layout (the SkinsWindow setting changed): laid out again, then docked again at its
    /// width (once this message is done).
    fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        MIN_WIDTH.with(|w| w.set(Metrics::of(layout).min_width));
        self.layout_dirty = true;
        self.edit_at = None;
        self.list.jump_to(0.0);
        self.page.jump_to(0.0);
        if matches!(self.focus, Spot::Button(_) | Spot::Fold(_) | Spot::Ctl(Ctl::Search, _)) {
            self.focus = Spot::List;
        }
        // SAFETY: posting to our own window.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_APP_DOCK, WPARAM(0), LPARAM(0));
        }
    }

    /// The list of skins (its visible part, on screen; none in one column).
    fn list_rect(&self) -> Rect {
        let buttons = match self.layout {
            Layout::Wide => 5.0 * ksize::BUTTON_H + 4.0 * BUTTON_GAP + 2.0 * BUTTONS_PAD,
            Layout::Slim => 8.0 + 2.0 * ICON + 2.0 + 8.0,
            Layout::Compact => return Rect::new(0.0, size::TITLE_H, 0.0, 0.0),
        };
        Rect::new(0.0, size::TITLE_H, self.m().list_w, (self.size.1 - size::TITLE_H - buttons).max(0.0))
    }

    /// One column: the skin drop-down at the top (renaming puts the name's box over it).
    fn skin_box(&self) -> Rect {
        Rect::new(12.0, size::TITLE_H + 4.0, (self.size.0 - 24.0 - 3.0 * (ICON + 6.0)).max(60.0), 28.0)
    }

    /// A small icon button: undo, redo and the menu right of the skin drop-down (one column), or
    /// the nine under the list, five to a line (slim).
    fn icon_rect(&self, b: Btn) -> Rect {
        if self.layout == Layout::Slim {
            let n = ICONS.iter().position(|(x, _)| *x == b).unwrap_or(0);
            let (col, row) = ((n % 5) as f32, (n / 5) as f32);
            let top = self.size.1 - 8.0 - 2.0 * ICON - 2.0;
            return Rect::new(8.0 + col * (ICON + 1.0), top + row * (ICON + 2.0), ICON, ICON);
        }
        let n = match b {
            Btn::Undo => 0.0,
            Btn::Redo => 1.0,
            _ => 2.0,
        };
        let s = self.skin_box();
        Rect::new(s.right() + 6.0 + n * (ICON + 6.0), s.y + 1.0, ICON, ICON)
    }

    /// The search for any setting at the top (the smaller layouts; the wide one's is a row).
    fn search_rect(&self) -> Rect {
        let h = self.header_rect();
        match self.layout {
            Layout::Slim => Rect::new(h.right() - 16.0 - 190.0, h.y + 13.0, 190.0, 26.0),
            _ => Rect::new(12.0, size::TITLE_H + 40.0, (h.right() - 12.0 - 12.0).max(60.0), 26.0),
        }
    }

    /// Whether a button can be used now.
    fn button_on(&self, b: Btn, app: &AppState) -> bool {
        let entry = app.skins.find(&app.settings.skin);
        match b {
            Btn::Rename | Btn::Delete => entry.is_some_and(|e| !e.built_in()),
            Btn::Duplicate | Btn::Export => entry.is_some(),
            Btn::Undo => self.can_undo(app, false),
            Btn::Redo => self.can_undo(app, true),
            _ => true,
        }
    }

    fn button_look(&self, b: Btn, app: &AppState) -> Look {
        let me = Hot::Button(b);
        let focused = self.cues && if b == Btn::Skin { self.focus == Spot::List } else { self.focus == Spot::Button(b) };
        Look { hover: self.hover == Some(me), pressed: self.pressed == Some(me) && self.hover == Some(me), focused, enabled: self.button_on(b, app) }
    }

    fn button_rect(&self, b: Btn) -> Rect {
        let list = self.list_rect();
        let full = LIST_W - 2.0 * BUTTONS_PAD;
        let half = (full - BUTTON_GAP) / 2.0;
        let (row, col, wide) = match b {
            Btn::Undo => (0, 0, false),
            Btn::Redo => (0, 1, false),
            Btn::New => (1, 0, false),
            Btn::Duplicate => (1, 1, false),
            Btn::Rename => (2, 0, false),
            Btn::Delete => (2, 1, false),
            Btn::Import => (3, 0, false),
            Btn::Export => (3, 1, false),
            Btn::Folder | Btn::Skin | Btn::More => (4, 0, true),
        };
        let y = list.bottom() + BUTTONS_PAD + row as f32 * (ksize::BUTTON_H + BUTTON_GAP);
        let x = BUTTONS_PAD + col as f32 * (half + BUTTON_GAP);
        Rect::new(x, y, if wide { full } else { half }, ksize::BUTTON_H)
    }

    fn close_rect(&self) -> Rect {
        Rect::new(self.size.0 - ksize::CLOSE_W, 0.0, ksize::CLOSE_W, size::TITLE_H)
    }

    /// The right side's top (one column: the whole top): the skin's name and notes.
    fn header_rect(&self) -> Rect {
        let m = self.m();
        Rect::new(m.list_w, size::TITLE_H, (self.size.0 - m.list_w).max(0.0), m.header_h)
    }

    /// The scrolling controls under the header.
    fn page_rect(&self) -> Rect {
        let m = self.m();
        let top = size::TITLE_H + m.header_h;
        Rect::new(m.list_w, top, (self.size.0 - m.list_w).max(0.0), (self.size.1 - top).max(0.0))
    }

    fn list_track(&self) -> Rect {
        self.bar.track(self.list_rect())
    }

    fn page_track(&self) -> Rect {
        self.bar.track(self.page_rect())
    }

    /// A list item on screen.
    fn item_on_screen(&self, r: Rect) -> Rect {
        Rect::new(r.x, r.y + self.list_rect().y - self.list.offset, r.w, r.h)
    }

    /// A control on screen.
    fn on_page(&self, r: Rect) -> Rect {
        Rect::new(r.x, r.y + self.page_rect().y - self.page.offset, r.w, r.h)
    }

    /// Where a control's parts are, in page coordinates.
    fn part_rect(&self, row: &Row, part: Part) -> Option<Rect> {
        let m = self.m();
        let wide = self.layout == Layout::Wide;
        let right = row.rect.right() - m.row_pad;
        let mid = |h: f32| row.rect.y + ((row.rect.h - h) / 2.0).round();
        match (row.ctl, part) {
            // A colour: NONE, the swatch and its box - or, smaller, the swatch, its box and a small x.
            (Ctl::Colour(_), Part::Hex) if wide => Some(Rect::new(right - m.hex_w, mid(m.hex_h), m.hex_w, m.hex_h)),
            (Ctl::Colour(_), Part::Main) if wide => Some(Rect::new(right - m.hex_w - 10.0 - m.swatch, mid(m.swatch), m.swatch, m.swatch)),
            (Ctl::Colour(_), Part::Clear) if wide => Some(Rect::new(right - m.hex_w - 10.0 - m.swatch - 8.0 - m.clear_w, mid(m.clear_h), m.clear_w, m.clear_h)),
            (Ctl::Colour(_), Part::Clear) => Some(Rect::new(right - m.clear_w, mid(m.clear_h), m.clear_w, m.clear_h)),
            (Ctl::Colour(_), Part::Hex) => Some(Rect::new(right - m.clear_w - 2.0 - m.hex_w, mid(m.hex_h), m.hex_w, m.hex_h)),
            (Ctl::Colour(_), Part::Main) => Some(Rect::new(right - m.clear_w - 2.0 - m.hex_w - 6.0 - m.swatch, mid(m.swatch), m.swatch, m.swatch)),
            (Ctl::Part(p), Part::Main) if crate::parts::is_flag(p) => Some(Rect::new(right - m.toggle_w, mid(m.toggle_h), m.toggle_w, m.toggle_h)),
            (Ctl::Shape | Ctl::Slant | Ctl::RowHeight | Ctl::FontSize | Ctl::TabLine | Ctl::ToolbarLine | Ctl::RowEdge | Ctl::SidebarEdge | Ctl::Buttons | Ctl::Caption | Ctl::Tabs | Ctl::Sidebar | Ctl::Era | Ctl::Icons | Ctl::Logo | Ctl::Fit | Ctl::Area | Ctl::Part(_) | Ctl::Font | Ctl::SidebarRow | Ctl::ScrollWidth, Part::Main) => {
                Some(Rect::new(right - m.dropdown_w, mid(m.field_h), m.dropdown_w, m.field_h))
            }
            (Ctl::LogoText, Part::Main) => Some(Rect::new(right - m.logo_w, mid(m.field_h), m.logo_w, m.field_h)),
            (Ctl::Search, Part::Main) => Some(Rect::new(right - SEARCH_W, mid(m.field_h), SEARCH_W, m.field_h)),
            // The wallpaper: NONE, BROWSE… and its name - or, smaller, BROWSE…, its name and a small x.
            (Ctl::Picture, Part::Hex) if wide => Some(Rect::new(right - m.picture_w, mid(m.field_h), m.picture_w, m.field_h)),
            (Ctl::Picture, Part::Main) if wide => Some(Rect::new(right - m.picture_w - 8.0 - m.browse_w, mid(m.field_h), m.browse_w, m.field_h)),
            (Ctl::Picture, Part::Clear) if wide => Some(Rect::new(right - m.picture_w - 8.0 - m.browse_w - 8.0 - m.clear_w, mid(m.field_h), m.clear_w, m.field_h)),
            (Ctl::Picture, Part::Clear) => Some(Rect::new(right - m.clear_w, mid(m.clear_h), m.clear_w, m.clear_h)),
            (Ctl::Picture, Part::Hex) => Some(Rect::new(right - m.clear_w - 2.0 - m.picture_w, mid(m.field_h), m.picture_w, m.field_h)),
            (Ctl::Picture, Part::Main) => Some(Rect::new(right - m.clear_w - 2.0 - m.picture_w - 6.0 - m.browse_w, mid(m.field_h), m.browse_w, m.field_h)),
            // A slider, then its number.
            (Ctl::Slide(_), Part::Main) => Some(Rect::new(right - m.value_w - m.value_gap - m.slider_w, mid(m.field_h), m.slider_w, m.field_h)),
            (Ctl::Slide(_), Part::Hex) => Some(Rect::new(right - m.value_w, mid(m.value_h), m.value_w, m.value_h)),
            // Position: the pad, then the 3x3 squares (the sliders under it say the numbers).
            (Ctl::Place, Part::Main) => {
                let grid = 3.0 * GRID_CELL + 2.0 * GRID_GAP;
                let w = self.pad_w();
                Some(Rect::new(right - grid - 22.0 - w, mid(m.pad_h), w, m.pad_h))
            }
            (Ctl::Place, Part::Grid(i)) => {
                let grid = 3.0 * GRID_CELL + 2.0 * GRID_GAP;
                let (x0, y0) = (right - grid, mid(grid));
                let (c, r) = ((i % 3) as f32, (i / 3) as f32);
                (i < 9).then(|| Rect::new(x0 + c * (GRID_CELL + GRID_GAP), y0 + r * (GRID_CELL + GRID_GAP), GRID_CELL, GRID_CELL))
            }
            // The pictures, right to left from the newest.
            (Ctl::Thumbs, Part::Thumb(i)) => {
                let n = self.thumb_count(row);
                let i = i as usize;
                (i < n).then(|| {
                    let x0 = right - n as f32 * m.thumb_w - (n as f32 - 1.0) * THUMB_GAP;
                    Rect::new(x0 + i as f32 * (m.thumb_w + THUMB_GAP), mid(m.thumb_h), m.thumb_w, m.thumb_h)
                })
            }
            (Ctl::Dark | Ctl::Grip, Part::Main) => Some(Rect::new(right - m.toggle_w, mid(m.toggle_h), m.toggle_w, m.toggle_h)),
            _ => None,
        }
    }

    /// The pad's width: the file list's shape at PAD_H tall.
    fn pad_w(&self) -> f32 {
        let (w, h) = self.list_size;
        let m = self.m();
        (m.pad_h * w / h.max(1.0)).clamp(PAD_MIN_W.min(m.pad_max_w), m.pad_max_w)
    }

    /// The main window's file list, roughly: its window less the sidebar, the bars at the top and
    /// the status bar (`whole`: the wallpaper's laid out over the whole window: that).
    fn read_list_size(&mut self, whole: bool) {
        let mut rc = RECT::default();
        // SAFETY: plain queries on our owner window with a valid out-struct.
        let ok = unsafe { GetWindow(self.hwnd, GW_OWNER).is_ok_and(|o| GetClientRect(o, &mut rc).is_ok()) };
        if ok && rc.right > rc.left && rc.bottom > rc.top {
            let s = self.scale();
            let (w, h) = ((rc.right - rc.left) as f32 / s, (rc.bottom - rc.top) as f32 / s);
            self.list_size = if whole { (w.max(200.0), h.max(150.0)) } else { ((w - 230.0).max(200.0), (h - 120.0).max(150.0)) };
        }
    }

    /// The wallpaper as the pad shows it: where it's drawn in `pad` (as in the list, made small),
    /// and how big it is there. None without a picture.
    fn pad_wallpaper(&self, app: &AppState, pad: Rect) -> Option<(Arc<thumbs::Picture>, Option<crate::render::WallDraw>, (f32, f32))> {
        let (pic, (ow, oh)) = self.current_pic.as_ref().filter(|_| !app.theme.wallpaper.is_empty())?;
        let (tw, th) = (pic.width() as f32, pic.height() as f32);
        // The pad is the list made small: everything in it (the picture at its own size, the
        // offset) shrinks the same.
        let k = pad.w / self.list_size.0.max(1.0);
        let mut t = app.theme.clone();
        t.wallpaper_offset = (t.wallpaper_offset.0 * k, t.wallpaper_offset.1 * k);
        let dip_per_px = ow / tw.max(1.0) * k / self.scale();
        let _ = oh;
        let how = crate::list::wallpaper_layout(pad, (tw, th), &t, dip_per_px);
        let size = crate::list::wallpaper_drawn_size(pad, (tw, th), &t, dip_per_px);
        Some((pic.clone(), how, size))
    }

    /// How many of the pictures next to the skin fit in their row.
    fn thumb_count(&self, row: &Row) -> usize {
        let m = self.m();
        let room = ((row.rect.w - 2.0 * m.row_pad - m.thumbs_title + THUMB_GAP) / (m.thumb_w + THUMB_GAP)).floor().max(0.0) as usize;
        self.thumbs.len().min(room)
    }

    /// A row's parts besides the main one, its box and its NONE: the 3x3 squares, the pictures.
    fn extra_parts(&self, row: &Row) -> Vec<Part> {
        match row.ctl {
            Ctl::Place => (0..9).map(Part::Grid).collect(),
            Ctl::Thumbs => (0..self.thumb_count(row) as u8).map(Part::Thumb).collect(),
            _ => Vec::new(),
        }
    }

    /// Lays out the list and the controls.
    fn layout(&mut self, app: &AppState) {
        self.layout_dirty = false;
        self.refresh_colours(app);
        let m = self.m();
        // The list: the built-in skins, then the person's own (one column has no list, but the
        // keyboard and the skins' drop-down go by these).
        let mut items = Vec::new();
        let mut y = m.list_top + m.group_h;
        let entries = app.skins.entries();
        for e in entries.iter().filter(|e| e.built_in()) {
            items.push(Item { id: e.id.clone(), name: app.skins.shown_name(e), rect: Rect::new(0.0, y, m.list_w, m.item_h) });
            y += m.item_h;
        }
        y += m.group_gap + m.group_h;
        let own_top = y;
        for e in entries.iter().filter(|e| !e.built_in()) {
            items.push(Item { id: e.id.clone(), name: app.skins.shown_name(e), rect: Rect::new(0.0, y, m.list_w, m.item_h) });
            y += m.item_h;
        }
        if y == own_top {
            y += m.item_h; // room for "None yet"
        }
        self.items = items;
        self.list.set_page(y, 10.0, self.list_rect().h);

        // The controls: shapes, style, then the colours in groups.
        let page = self.page_rect();
        let card_w = (page.w - 2.0 * m.pad_x - m.gutter).clamp(m.min_card_w, MAX_CARD_W);
        let x0 = page.x + m.pad_x;
        let layout = self.layout;
        // The wide layout's search finds colours (it's a row there); the others' finds any setting
        // (by its title or its section's), and shows every section with something found, open.
        let typed = self.colour_search.trim().to_lowercase();
        let folded = if typed.is_empty() { self.folded.clone() } else { Vec::new() };
        let hints: Vec<String> = self.colours.iter().map(|c| c.hint.clone()).collect();
        let (title_style, note_style) = (TextStyle::new(Color::hex(0)), TextStyle::new(Color::hex(0)).size(ksize::NOTE_FONT));
        let renderer = &mut self.renderer;
        let mut blocks = Vec::new();
        let mut rows = Vec::new();
        let mut y = m.first_y;
        let mut card = |title: &'static str, ctls: &[(Ctl, &'static str, &'static str)], y: &mut f32, blocks: &mut Vec<Block>, rows: &mut Vec<Row>| {
            let ctls: Vec<_> = ctls
                .iter()
                .copied()
                .filter(|(c, t, _)| {
                    typed.is_empty()
                        || *c == Ctl::Search
                        || found_by(title, &typed)
                        || found_by(t, &typed)
                        // (a colour also by its whole name and its name in the skin's text, a
                        // part by its name there: "hover" finds every hover colour)
                        || matches!(c, Ctl::Colour(i) if COLOURS.get(*i).is_some_and(|(key, full)| found_by(full, &typed) || found_by(key, &typed)))
                        || matches!(c, Ctl::Part(p) if found_by(crate::parts::SPECS[*p as usize].key, &typed))
                })
                .collect();
            if ctls.is_empty() {
                return;
            }
            if title != "Search" {
                // Its heading folds it (the wide layout's search card stays open).
                let open = !folded.contains(&title);
                blocks.push(Block::Fold(Rect::new(x0, *y, card_w, m.heading_h), title, open));
                if !open {
                    *y += m.heading_h + m.card_gap;
                    return;
                }
            } else {
                blocks.push(Block::Heading(Rect::new(x0 + 1.0, *y, card_w, m.heading_h), title));
            }
            *y += m.heading_h + m.heading_gap;
            let top = *y;
            // The card goes under its rows' lines (drawn first).
            let at = blocks.len();
            for (i, (ctl, t, n)) in ctls.iter().enumerate() {
                if i > 0 {
                    blocks.push(Block::Line(Rect::new(x0 + 1.0, *y, card_w - 2.0, 1.0)));
                }
                // The smaller layouts put a note after its title when both fit on the line.
                let inline = layout != Layout::Wide && !n.is_empty() && !matches!(ctl, Ctl::Place | Ctl::Thumbs) && {
                    let note = match ctl {
                        Ctl::Colour(c) if *n == FOLLOWS => hints.get(*c).map_or("", String::as_str),
                        _ => n,
                    };
                    let room = card_w - 2.0 * m.row_pad - controls_w(&m, *ctl) - 8.0;
                    renderer.measure(t, &title_style) + 8.0 + renderer.measure(note, &note_style) <= room
                };
                let h = match ctl {
                    Ctl::Place => m.place_h,
                    Ctl::Thumbs => m.thumbs_h,
                    _ if n.is_empty() || inline => m.row_h,
                    _ => m.row_note_h,
                };
                rows.push(Row { ctl: *ctl, rect: Rect::new(x0, *y, card_w, h), title: t, note: n });
                *y += h;
            }
            blocks.insert(at, Block::Card(Rect::new(x0, top, card_w, *y - top)));
            *y += m.card_gap;
        };
        // One card for each part of the window, top to bottom (`cards`).
        if layout == Layout::Wide {
            card("Search", &[(Ctl::Search, "Find", "")], &mut y, &mut blocks, &mut rows);
        }
        let cards = cards(&hints);
        for (title, ctls) in &cards {
            card(title, ctls, &mut y, &mut blocks, &mut rows);
        }
        // Scrolls only when the cards don't fit (`y` has the gap for a next card in it).
        let end = blocks.iter().map(Block::rect).chain(rows.iter().map(|r| r.rect)).map(|r| r.bottom()).fold(0.0, f32::max);
        self.rows = rows;
        self.blocks = blocks;
        self.page.set_page(end, 36.0, page.h);
        // The keyboard can't stay on a control that's gone (the smaller layouts' search is at the top).
        if let Spot::Ctl(c, _) = self.focus {
            let top = c == Ctl::Search && layout != Layout::Wide;
            if !top && !self.rows.iter().any(|r| r.ctl == c) {
                self.focus = Spot::List;
            }
        }
        if let Spot::Fold(t) = self.focus {
            if !self.blocks.iter().any(|b| matches!(b, Block::Fold(_, f, _) if *f == t)) {
                self.focus = Spot::List;
            }
        }
    }

    // ------------------------------------------------------------ painting

    fn paint(&mut self, app: &mut AppState) {
        if app.settings.skins_window != self.layout {
            // (Typing in a box ends: its place is gone.)
            let _ = self.end_editing(app, true);
            self.set_layout(app.settings.skins_window);
        }
        let seen = (app.skins.generation, app.theme_generation, app.settings.skin.clone());
        if seen != self.seen {
            self.seen = seen;
            self.layout_dirty = true;
        }
        if self.layout_dirty {
            self.layout(app);
        }
        self.read_pictures(app);
        let was = self.pad_w();
        self.read_list_size(matches!(app.theme.wallpaper_area, WallpaperArea::WindowSized | WallpaperArea::Window));
        if (self.pad_w() - was).abs() > 0.5 {
            self.edit_at = None;
        }
        let now = self.now();
        let mut more = self.list.tick(now) | self.page.tick(now) | self.flips.tick(now) || now < self.repaint_until;
        if !self.renderer.begin() {
            // SAFETY: a timer on our own window.
            unsafe {
                SetTimer(Some(self.hwnd), RETRY_TIMER, 500, None);
            }
            return;
        }
        let theme = app.theme.clone();
        self.renderer.clear(theme.window);
        // 98 / 2000: everything inside the window's frame.
        let b = self.border();
        self.renderer.set_origin(b, b);
        // The list of skins on the sidebar's colour (a gradient running down the whole window).
        let below = Rect::new(0.0, size::TITLE_H, self.size.0, (self.size.1 - size::TITLE_H).max(0.0));
        self.renderer.fill_paint(below, 0.0, &theme.sidebar_paint);
        let list = self.list_rect();
        if list.w > 0.0 {
            kit::page_list_well(&mut self.renderer, &theme, list);
        }
        self.paint_caption(&theme);
        self.paint_list(&theme, app);
        self.paint_header(&theme, app);
        self.paint_page(&theme, app);
        self.paint_tip(&theme);
        if let Some((id, skin_id)) = &self.asking {
            let name = app.skins.find(skin_id).map_or_else(|| skin_id.clone(), |e| e.name.clone());
            let area = Rect::new(0.0, size::TITLE_H, self.size.0, (self.size.1 - size::TITLE_H).max(0.0));
            self.ask.paint_question(
                &mut self.renderer,
                &theme,
                *id,
                &format!("Delete the skin \u{201c}{name}\u{201d}?"),
                if app.skins.recycles(skin_id) { "Its file goes to the Recycle Bin." } else { "Its file is deleted for good: this drive has no Recycle Bin." },
                "DELETE",
                area,
            );
        }
        if b > 0.0 {
            self.renderer.set_origin(0.0, 0.0);
            frame::paint_classic_border(&mut self.renderer, &theme, Rect::new(0.0, 0.0, self.size.0 + 2.0 * b, self.size.1 + 2.0 * b));
        }
        if self.renderer.end() == FrameResult::DeviceLost {
            more = true;
        }
        self.place_boxes();
        if more {
            self.invalidate();
        }
    }

    fn paint_caption(&mut self, theme: &Theme) {
        let close = self.close_rect();
        let r = &mut self.renderer;
        let bar = Rect::new(0.0, 0.0, self.size.0, size::TITLE_H);
        kit::title_bar(r, theme, bar, self.active);
        let x = kit::logo(r, theme, bar);
        crate::retro::title(r, theme, Rect::new(x + 10.0, 0.0, (self.size.0 - x - 10.0 - ksize::CLOSE_W).max(0.0), size::TITLE_H), "Skins", self.active);
        let hot = self.hover == Some(Hot::Close);
        kit::close_button(r, theme, close, hot, hot && self.pressed == Some(Hot::Close), self.active);
    }

    fn paint_list(&mut self, theme: &Theme, app: &AppState) {
        if self.layout == Layout::Compact {
            return;
        }
        let m = self.m();
        let list = self.list_rect();
        let dy = list.y - self.list.offset;
        self.renderer.push_clip(list);
        let heading = TextStyle::new(if theme.era.old() { theme.sidebar_text } else { theme.muted }).size(m.group_font).semibold();
        let (hx, hw) = (m.item_x, m.list_w - m.item_x - 8.0);
        self.renderer.text("DEFAULT", Rect::new(hx, m.list_top + dy, hw, m.group_text_h), &heading);
        let own_heading_y = self.items.iter().filter(|i| !i.id.starts_with(skin::FILE_PREFIX)).map(|i| i.rect.bottom()).fold(m.list_top + m.group_h, f32::max) + m.group_gap;
        self.renderer.text("YOUR SKINS", Rect::new(hx, own_heading_y + dy, hw, m.group_text_h), &heading);
        if !self.items.iter().any(|i| i.id.starts_with(skin::FILE_PREFIX)) {
            let style = TextStyle::new(theme.muted.with_alpha(0.8)).size(ksize::NOTE_FONT);
            let y = own_heading_y + m.group_h + dy;
            self.renderer.text("None yet", Rect::new(hx, y, hw, NOTE_LINE), &style);
        }
        for (i, item) in self.items.iter().enumerate() {
            let rect = Rect::new(item.rect.x, item.rect.y + dy, item.rect.w, item.rect.h);
            if rect.bottom() < list.y || rect.y > list.bottom() {
                continue;
            }
            let selected = item.id.eq_ignore_ascii_case(&app.settings.skin);
            let look = Look { hover: self.hover == Some(Hot::Item(i)), focused: self.cues && self.focus == Spot::List && selected, ..Look::enabled() };
            let unsaved = app.skins.unsaved(&item.id) || self.save_due.contains(&item.id);
            let name = if unsaved { format!("{} \u{2022}", item.name) } else { item.name.clone() };
            if self.editing == Some(Editing::Rename(item.id.clone())) {
                // The name box sits there (in a glass skin, in a rounded text box's frame).
                if kit::glass(theme) {
                    kit::field(&mut self.renderer, theme, rect.inset(6.0, 3.0), None, true, Look::enabled());
                }
                continue;
            }
            kit::nav_item_inset(&mut self.renderer, theme, rect, &name, selected, look, m.item_x);
        }
        self.renderer.pop_clip();
        let hover = matches!(self.hover, Some(Hot::ListThumb)) || matches!(self.drag, Some((Hot::ListThumb, _)));
        let track = self.list_track();
        kit::scrollbar(&mut self.renderer, theme, track, &self.list, hover, self.holding.hold_of(0));
        // The buttons (slim: small icons).
        if self.layout == Layout::Slim {
            for (b, _) in ICONS {
                let (rect, look) = (self.icon_rect(b), self.button_look(b, app));
                icon_button(&mut self.renderer, theme, rect, glyph_of(b), look);
            }
            return;
        }
        for (b, label) in BUTTONS {
            let (rect, look) = (self.button_rect(b), self.button_look(b, app));
            kit::button_beside(&mut self.renderer, theme, rect, label, look, beside_of(b));
        }
    }

    /// Slim: the name of the icon button under the mouse, in a small box over it (on top of all).
    fn paint_tip(&mut self, theme: &Theme) {
        let Some(Hot::Button(b)) = self.hover else { return };
        let Some((_, name)) = ICONS.iter().find(|(x, _)| *x == b).filter(|_| self.layout == Layout::Slim && self.asking.is_none()) else { return };
        let (at, top) = (self.icon_rect(b), self.icon_rect(ICONS[0].0).y - 6.0 - 22.0);
        let r = &mut self.renderer;
        let style = TextStyle::new(theme.text).size(ksize::NOTE_FONT);
        let w = (r.measure(name, &style) + 14.0).ceil();
        let x = at.x.min(self.size.0 - w - 4.0).max(4.0);
        let tip = Rect::new(x, top, w, 22.0);
        r.fill_rect(tip, theme.panel);
        r.stroke_rect(tip, 1.0, theme.line);
        r.text(name, tip, &style.align(Align::Center));
    }

    fn paint_header(&mut self, theme: &Theme, app: &AppState) {
        if self.layout != Layout::Wide {
            return self.paint_top(theme, app);
        }
        let right = Rect::new(LIST_W, size::TITLE_H, (self.size.0 - LIST_W).max(0.0), (self.size.1 - size::TITLE_H).max(0.0));
        self.renderer.fill_paint(right, 0.0, &theme.toolbar_paint);
        let h = self.header_rect();
        let x = h.x + PAD_X;
        let w = (h.right() - PAD_X - 110.0 - x).max(60.0);
        let entry = app.skins.find(&app.settings.skin);
        let name = entry.map_or_else(|| app.settings.skin.clone(), |e| e.name.clone());
        let r = &mut self.renderer;
        kit::page_title(r, theme, Rect::new(x, h.y + 14.0, w, 30.0), &name);
        let note = match entry {
            Some(e) if e.built_in() => "A default skin. Changes go into your own copy.",
            Some(_) => "",
            None => "Its file is missing, so AMD Radeon is shown.",
        };
        let mut line_y = h.y + 46.0;
        if !note.is_empty() {
            r.text(note, Rect::new(x, line_y, w + 110.0, NOTE_LINE), &TextStyle::new(theme.muted).size(ksize::NOTE_FONT));
            line_y += NOTE_LINE + 2.0;
        }
        if let Some((said, problem)) = &self.status {
            let color = if *problem { theme.accent } else { theme.text };
            let style = TextStyle::new(color).size(ksize::NOTE_FONT).semibold();
            // A long message goes on as many lines as the header has room for.
            let room = (((h.bottom() - 2.0 - line_y) / NOTE_LINE).floor() as usize).max(1);
            let lines = kit::wrap(r, said, &style, w + 110.0);
            let shown = lines.len().min(room);
            for (n, line) in lines.iter().take(shown).enumerate() {
                // (The last line that fits takes the rest, cut short with "…".)
                let text = if n + 1 == shown { lines[n..].join(" ") } else { line.clone() };
                r.text(&text, Rect::new(x, line_y + n as f32 * NOTE_LINE, w + 110.0, NOTE_LINE), &style);
            }
        }
        r.fill_rect(Rect::new(h.x + PAD_X, h.bottom() - 1.0, (h.w - 2.0 * PAD_X).max(0.0), 1.0), theme.line);
    }

    /// The smaller layouts' top. One column: the skin's drop-down with undo, redo and the menu of
    /// the rest beside it; under it the search for any setting; then one line of what just
    /// happened. Slim: the skin's name over that line, and the search right of it.
    fn paint_top(&mut self, theme: &Theme, app: &AppState) {
        let m = self.m();
        let compact = self.layout == Layout::Compact;
        let page = self.page_rect();
        // The page (slim: the whole right side) on the toolbar's colour, the rest on the sidebar's.
        let fill = if compact { Rect::new(0.0, page.y, self.size.0, page.h) } else { Rect::new(m.list_w, size::TITLE_H, (self.size.0 - m.list_w).max(0.0), (self.size.1 - size::TITLE_H).max(0.0)) };
        self.renderer.fill_paint(fill, 0.0, &theme.toolbar_paint);
        let entry = app.skins.find(&app.settings.skin);
        let name = entry.map_or_else(|| app.settings.skin.clone(), |e| e.name.clone());
        let unsaved = app.skins.unsaved(&app.settings.skin) || self.save_due.iter().any(|id| id.eq_ignore_ascii_case(&app.settings.skin));
        let name = if unsaved { format!("{name} \u{2022}") } else { name };
        let note = match entry {
            Some(e) if e.built_in() => "A default skin. Changes go into your own copy.",
            Some(_) => "",
            None => "Its file is missing, so AMD Radeon is shown.",
        };
        let (search, skin_box, header) = (self.search_rect(), self.skin_box(), self.header_rect());
        let search_look = self.look_of(Ctl::Search, Part::Main, true);
        let skin_look = Look { enabled: true, ..self.button_look(Btn::Skin, app) };
        let icons: Vec<(Btn, Rect, Look)> = if compact { [Btn::Undo, Btn::Redo, Btn::More].iter().map(|b| (*b, self.icon_rect(*b), self.button_look(*b, app))).collect() } else { Vec::new() };
        let editing_search = matches!(self.editing, Some(Editing::Search(_)));
        let renaming = matches!(self.editing, Some(Editing::Rename(_)));
        let line = if compact {
            Rect::new(12.0, size::TITLE_H + 70.0, (self.size.0 - 24.0).max(0.0), 15.0)
        } else {
            let x = header.x + m.pad_x;
            Rect::new(x, size::TITLE_H + 29.0, (search.x - 10.0 - x).max(40.0), 15.0)
        };
        let r = &mut self.renderer;
        if compact {
            if renaming {
                // The name's box sits there.
                kit::field(r, theme, skin_box, None, true, Look::enabled());
            } else {
                kit::dropdown(r, theme, skin_box, "", skin_look);
                let at = Rect::new(skin_box.x + ksize::FIELD_PAD, skin_box.y, (skin_box.w - ksize::FIELD_PAD - ksize::ARROW_W).max(0.0), skin_box.h);
                r.text(&name, at, &TextStyle::new(theme.text).semibold());
            }
            for (b, at, look) in &icons {
                icon_button(r, theme, *at, glyph_of(*b), *look);
            }
        } else {
            let x = header.x + m.pad_x;
            r.text(&name, Rect::new(x, size::TITLE_H + 8.0, (search.x - 10.0 - x).max(40.0), 21.0), &TextStyle::new(theme.text_strong).size(16.0).semibold());
        }
        // The search, with its magnifier (the text box goes right of it while typing).
        kit::field(r, theme, search, None, editing_search, search_look);
        let ink = if search_look.enabled { theme.muted } else { theme.muted.with_alpha(theme.muted.a * 0.45) };
        glyph(r, Glyph::Search, Rect::new(search.x + 3.0, search.y, 18.0, search.h), ink);
        if !editing_search {
            let typed = self.colour_search.as_str();
            let (words, color) = if typed.is_empty() { (SEARCH_ANY, ink) } else { (typed, theme.text) };
            r.text(words, Rect::new(search.x + 24.0, search.y, (search.w - 30.0).max(0.0), search.h), &TextStyle::new(color));
        }
        // What just happened, or else what this skin is.
        match &self.status {
            Some((said, true)) => r.text(said, line, &TextStyle::new(theme.accent).size(ksize::NOTE_FONT).semibold()),
            Some((said, false)) => r.text(said, line, &TextStyle::new(theme.muted).size(ksize::NOTE_FONT)),
            None => r.text(note, line, &TextStyle::new(theme.muted).size(ksize::NOTE_FONT)),
        }
    }

    fn look_of(&self, ctl: Ctl, part: Part, enabled: bool) -> Look {
        let me = Hot::Ctl(ctl, part);
        Look { hover: self.hover == Some(me), pressed: self.pressed == Some(me) && self.hover == Some(me), focused: self.cues && self.focus == Spot::Ctl(ctl, part), enabled }
    }

    fn paint_page(&mut self, theme: &Theme, app: &AppState) {
        let m = self.m();
        let wide = self.layout == Layout::Wide;
        let page = self.page_rect();
        let dy = page.y - self.page.offset;
        let shift = |r: Rect| Rect::new(r.x, r.y + dy, r.w, r.h);
        let enabled = app.skins.find(&app.settings.skin).is_some();
        let now = self.now();
        let dark = self.flips.at("dark", app.theme.dark, now);
        let grip = self.flips.at("grip", app.theme.scrollbar_grip, now);
        // The parts' switches, each where it's drawn now (0 off to 1 on).
        let flag_at: Vec<f32> = crate::parts::ALL
            .iter()
            .map(|&p| if crate::parts::is_flag(p) { self.flips.at(crate::parts::SPECS[p as usize].key, app.theme.parts.on(p), now) } else { 0.0 })
            .collect();
        self.refresh_colours(app);
        self.renderer.push_clip(page);
        // 98 / 2000: a card's heading is drawn with it, on its top line.
        let classic = theme.era.classic();
        for (k, block) in self.blocks.iter().enumerate() {
            let r = &mut self.renderer;
            match block {
                Block::Heading(..) if classic && matches!(self.blocks.get(k + 1), Some(Block::Card(_))) => {}
                Block::Heading(rect, text) => kit::heading(r, theme, shift(*rect), text),
                Block::Fold(rect, text, open) => {
                    let me = Hot::Fold(*text);
                    let look = Look { hover: self.hover == Some(me), pressed: false, focused: self.cues && self.focus == Spot::Fold(*text), enabled: true };
                    fold_heading(r, theme, shift(*rect), text, *open, look);
                }
                Block::Card(rect) => match k.checked_sub(1).and_then(|j| self.blocks.get(j)) {
                    Some(Block::Heading(h, text)) if classic => kit::titled_group(r, theme, shift(*rect), shift(*h), text),
                    _ => kit::card(r, theme, shift(*rect)),
                },
                Block::Line(rect) => kit::separator(r, theme, rect.x, rect.y + dy, rect.w),
            }
        }
        let has_picture = !app.theme.wallpaper.is_empty();
        for i in 0..self.rows.len() {
            let row = self.rows[i].clone();
            let rect = shift(row.rect);
            if rect.bottom() < page.y || rect.y > page.bottom() {
                continue;
            }
            // (A colour's name ends a little before its NONE button; the smaller layouts' titles
            // have the room their controls leave.)
            let text_w = if wide {
                match row.ctl {
                    Ctl::Colour(_) => rect.w - 2.0 * ROW_PAD_X - HEX_W - 10.0 - SWATCH - 8.0 - NONE_W - 8.0,
                    Ctl::Picture => rect.w - 2.0 * ROW_PAD_X - HEX_W - 8.0 - BROWSE_W - 8.0 - NONE_W - 8.0,
                    _ => rect.w - 2.0 * ROW_PAD_X - HEX_W - SWATCH - 40.0,
                }
                .max(80.0)
            } else {
                match row.ctl {
                    Ctl::Thumbs => m.thumbs_title - 8.0,
                    Ctl::Place => rect.w - 2.0 * m.row_pad - (3.0 * GRID_CELL + 2.0 * GRID_GAP + 22.0 + self.pad_w()) - 8.0,
                    _ => rect.w - 2.0 * m.row_pad - controls_w(&m, row.ctl) - 8.0,
                }
                .max(40.0)
            };
            let title_style = TextStyle::new(if enabled { theme.text } else { theme.muted });
            let note_style = TextStyle::new(theme.muted).size(ksize::NOTE_FONT);
            let x = rect.x + m.row_pad;
            let note = match row.ctl {
                Ctl::Colour(c) if row.note == FOLLOWS => self.colours.get(c).map_or("", |s| s.hint.as_str()),
                _ => row.note,
            };
            if row.note.is_empty() {
                self.renderer.text(row.title, Rect::new(x, rect.y + ((rect.h - 19.0) / 2.0).round(), text_w, 19.0), &title_style);
            } else if wide {
                self.renderer.text(row.title, Rect::new(x, rect.y + 9.0, text_w, 19.0), &title_style);
                self.renderer.text(note, Rect::new(x, rect.y + 28.0, text_w, 16.0), &note_style);
            } else if rect.h <= m.row_h + 0.5 {
                // On one line: the note after the title.
                let y = rect.y + ((rect.h - 19.0) / 2.0).round();
                self.renderer.text(row.title, Rect::new(x, y, text_w, 19.0), &title_style);
                let after = self.renderer.measure(row.title, &title_style).ceil() + 8.0;
                self.renderer.text(note, Rect::new(x + after, y, (text_w - after).max(0.0), 19.0), &note_style);
            } else {
                let y = rect.y + ((rect.h - 35.0) / 2.0).round();
                self.renderer.text(row.title, Rect::new(x, y, text_w, 19.0), &title_style);
                self.renderer.text(note, Rect::new(x, y + 19.0, text_w, 16.0), &note_style);
            }
            let main = self.part_rect(&row, Part::Main).map(shift);
            let hex = self.part_rect(&row, Part::Hex).map(shift);
            // How the wallpaper looks means nothing until it has a picture.
            let enabled = enabled && (has_picture || !matches!(row.ctl, Ctl::Fit | Ctl::Area | Ctl::Slide(_) | Ctl::Place));
            let main_look = self.look_of(row.ctl, Part::Main, enabled || matches!(row.ctl, Ctl::Search));
            let hex_look = self.look_of(row.ctl, Part::Hex, enabled);
            let clear = self.part_rect(&row, Part::Clear).map(shift);
            let picture = if row.ctl == Ctl::Picture { app.skins.find(&app.settings.skin).map(|e| picture_of(&e.text)).unwrap_or_default() } else { String::new() };
            let clear_on = enabled
                && match row.ctl {
                    Ctl::Colour(c) => self.colours.get(c).is_some_and(|s| s.text != "none"),
                    Ctl::Picture => !picture.is_empty(),
                    _ => false,
                };
            let clear_look = self.look_of(row.ctl, Part::Clear, clear_on);
            let label = self.choice_label(row.ctl, &app.theme);
            let extra: Vec<(Part, Rect, Look)> = self.extra_parts(&row).into_iter().filter_map(|part| Some((part, shift(self.part_rect(&row, part)?), self.look_of(row.ctl, part, enabled)))).collect();
            let place_drag = self.sliding.as_ref().is_some_and(|d| d.pad);
            let pad_wall = if row.ctl == Ctl::Place { main.and_then(|m| self.pad_wallpaper(app, m)) } else { None };
            let r = &mut self.renderer;
            match row.ctl {
                Ctl::Colour(i) => {
                    let Some(c) = self.colours.get(i) else { continue };
                    if let Some(n) = clear {
                        if wide {
                            kit::button(r, theme, n, "NONE", clear_look);
                        } else {
                            icon_button(r, theme, n, Glyph::Clear, clear_look);
                        }
                    }
                    if let Some(s) = main {
                        swatch(r, theme, s, &c.paint, main_look);
                    }
                    if let Some(hx) = hex {
                        let editing = matches!(self.editing, Some(Editing::Hex(j, _)) if j == i);
                        kit::field(r, theme, hx, (!editing).then_some(c.text.as_str()), editing, hex_look);
                    }
                }
                Ctl::Part(p) if crate::parts::is_flag(p) => {
                    if let Some(m) = main {
                        kit::toggle(r, theme, m, flag_at[p as usize], main_look);
                    }
                }
                Ctl::Shape | Ctl::Slant | Ctl::RowHeight | Ctl::FontSize | Ctl::TabLine | Ctl::ToolbarLine | Ctl::RowEdge | Ctl::SidebarEdge | Ctl::Buttons | Ctl::Caption | Ctl::Tabs | Ctl::Sidebar | Ctl::Era | Ctl::Icons | Ctl::Logo | Ctl::Fit | Ctl::Area | Ctl::Part(_) | Ctl::Font | Ctl::SidebarRow | Ctl::ScrollWidth => {
                    if let Some(m) = main {
                        kit::dropdown(r, theme, m, &label, main_look);
                    }
                }
                Ctl::LogoText => {
                    if let Some(m) = main {
                        let editing = matches!(self.editing, Some(Editing::LogoText(_)));
                        let word = app.theme.logo_text.as_str();
                        kit::field(r, theme, m, (!editing && !word.is_empty()).then_some(word), editing, main_look);
                        if !editing && word.is_empty() {
                            // No word: the chrome wordmark, said in the hint's grey (as the box says
                            // it while typing).
                            let grey = if main_look.enabled { theme.muted } else { theme.muted.with_alpha(theme.muted.a * 0.38) };
                            r.text(LOGO_HINT, Rect::new(m.x + 8.0, m.y, (m.w - 16.0).max(0.0), m.h), &TextStyle::new(grey));
                        }
                    }
                }
                Ctl::Search => {
                    if let Some(m) = main {
                        let editing = matches!(self.editing, Some(Editing::Search(_)));
                        let typed = self.colour_search.as_str();
                        kit::field(r, theme, m, (!editing && !typed.is_empty()).then_some(typed), editing, main_look);
                        if !editing && typed.is_empty() {
                            r.text(SEARCH_ANY, Rect::new(m.x + 8.0, m.y, (m.w - 16.0).max(0.0), m.h), &TextStyle::new(theme.muted));
                        }
                    }
                }
                Ctl::Dark => {
                    if let Some(m) = main {
                        kit::toggle(r, theme, m, dark, main_look);
                    }
                }
                Ctl::Picture => {
                    // Wide: NONE and BROWSE… side by side lean where they meet. Otherwise BROWSE…
                    // is on its own (the × is at the row's end), so straight.
                    let together = wide && clear.is_some();
                    if let Some(n) = clear {
                        if wide {
                            kit::button_beside(r, theme, n, "NONE", clear_look, crate::pathbar::Beside::RIGHT);
                        } else {
                            icon_button(r, theme, n, Glyph::Clear, clear_look);
                        }
                    }
                    if let Some(b) = main {
                        let beside = if together { crate::pathbar::Beside::LEFT } else { crate::pathbar::Beside::NONE };
                        kit::button_beside(r, theme, b, "BROWSE\u{2026}", main_look, beside);
                    }
                    if let Some(hx) = hex {
                        let editing = matches!(self.editing, Some(Editing::Picture(_)));
                        kit::field(r, theme, hx, (!editing && !picture.is_empty()).then_some(picture.as_str()), editing, hex_look);
                        if !editing && picture.is_empty() {
                            let grey = if hex_look.enabled { theme.muted } else { theme.muted.with_alpha(theme.muted.a * 0.38) };
                            r.text(PICTURE_HINT, Rect::new(hx.x + 8.0, hx.y, (hx.w - 16.0).max(0.0), hx.h), &TextStyle::new(grey));
                        }
                    }
                }
                Ctl::Slide(s) => {
                    if let Some(m) = main {
                        let (_, min, max, ..) = slide_spec(s);
                        let v = slide_value(s, &app.theme);
                        let share = (v - min) / (max - min);
                        let look = Look { pressed: main_look.pressed || self.sliding.as_ref().is_some_and(|d| d.slide == s), ..main_look };
                        let standard = (slide_default(s, &app.theme) - min) / (max - min);
                        slider(r, theme, m, share, standard, look);
                    }
                    if let Some(hx) = hex {
                        let editing = matches!(self.editing, Some(Editing::Number(j, _)) if j == s);
                        let v = slide_value(s, &app.theme);
                        kit::field(r, theme, hx, (!editing).then_some(slide_label(s, v).as_str()), editing, hex_look);
                    }
                }
                Ctl::Thumbs => {
                    if self.thumbs.is_empty() && self.thumbs_for.is_some() {
                        let skip = m.thumbs_title + 10.0;
                        let at = Rect::new(rect.x + skip, rect.y, (rect.w - skip - m.row_pad).max(0.0), rect.h);
                        r.text("No pictures next to this skin yet", at, &TextStyle::new(theme.muted).size(ksize::NOTE_FONT).align(Align::Right));
                    }
                    for (part, at, look) in &extra {
                        let Part::Thumb(i) = part else { continue };
                        let Some((path, pic)) = self.thumbs.get(*i as usize) else { continue };
                        r.fill_rect(*at, theme.input);
                        if let Some(pic) = pic {
                            r.picture(pic, *at, Some(cover_src((pic.width() as f32, pic.height() as f32), (at.w, at.h))), 1.0, true);
                        }
                        if same_path(path, &app.theme.wallpaper) {
                            r.stroke_rect(at.inset(-1.0, -1.0), 2.0, theme.accent);
                        } else {
                            r.stroke_rect(*at, 1.0, if look.hover { theme.text } else { theme.line });
                        }
                        if look.focused {
                            kit::focus_ring(r, theme, at.inset(-2.0, -2.0), 0.0);
                        }
                    }
                }
                Ctl::Place => {
                    let (x, y) = (slide_value(Slide::Across, &app.theme), slide_value(Slide::Down, &app.theme));
                    let _ = has_picture;
                    if let Some(pad) = main {
                        // The file list in small: its background, the wallpaper exactly where it
                        // lies (fit, size, zoom, place, offset, strength, soft edges), and faint
                        // rows over it.
                        r.fill_rect(pad, theme.window);
                        r.push_clip(pad);
                        if let Some((pic, Some(how), _)) = &pad_wall {
                            let strength = app.theme.wallpaper_opacity * if enabled { 1.0 } else { 0.4 };
                            r.wallpaper(pic, *how, pad, strength, app.theme.wallpaper_fade);
                        }
                        let ink = theme.text.with_alpha(if enabled { 0.28 } else { 0.12 });
                        for n in 0..6 {
                            let y = pad.y + 10.0 + n as f32 * 14.0;
                            let w = pad.w * [0.55, 0.42, 0.62, 0.36, 0.5, 0.45][n];
                            r.fill_rect(Rect::new(pad.x + 10.0, y, 7.0, 7.0), ink);
                            r.fill_rect(Rect::new(pad.x + 22.0, y + 1.5, w, 4.0), ink);
                        }
                        r.pop_clip();
                        let edge = if enabled && (main_look.hover || place_drag) { theme.accent } else { theme.line };
                        r.stroke_rect(pad, if place_drag { 2.0 } else { 1.0 }, edge);
                        if main_look.focused {
                            kit::focus_ring(r, theme, pad.inset(-2.0, -2.0), 0.0);
                        }
                    }
                    for (part, at, look) in &extra {
                        let Part::Grid(i) = part else { continue };
                        let (gx, gy) = grid_place(*i as usize);
                        let on = enabled && (gx - x).abs() < 0.5 && (gy - y).abs() < 0.5;
                        r.fill_rect(*at, if on { theme.accent } else { theme.input });
                        r.stroke_rect(*at, 1.0, if on { theme.accent } else if look.hover && look.enabled { theme.text } else { theme.line });
                        if look.focused {
                            kit::focus_ring(r, theme, *at, 0.0);
                        }
                    }
                }
                Ctl::Grip => {
                    if let Some(m) = main {
                        kit::toggle(r, theme, m, grip, main_look);
                    }
                }
            }
        }
        self.renderer.pop_clip();
        let hover = matches!(self.hover, Some(Hot::PageThumb)) || matches!(self.drag, Some((Hot::PageThumb, _)));
        let track = self.page_track();
        kit::scrollbar(&mut self.renderer, theme, track, &self.page, hover, self.holding.hold_of(1));
    }

    /// The label of a drop-down's current choice.
    fn choice_label(&self, ctl: Ctl, t: &Theme) -> String {
        match ctl {
            Ctl::Shape => SHAPES.iter().find(|(s, _, _)| *s == t.shape).map_or("", |(_, _, l)| l).to_string(),
            Ctl::Slant => slant_label(t.slant),
            Ctl::RowHeight => row_label(t.row_h),
            Ctl::FontSize => size_label(t.font_size / PT),
            Ctl::TabLine => width_label(t.tab_line_w),
            Ctl::ToolbarLine => width_label(t.toolbar_line_w),
            Ctl::RowEdge => width_label(t.select_edge_w),
            Ctl::SidebarEdge => width_label(t.sidebar_select_edge_w),
            Ctl::Buttons => label_of(&BUTTON_STYLES, t.buttons),
            Ctl::Caption => label_of(&CAPTION_STYLES, t.caption_buttons),
            Ctl::Tabs => label_of(&TAB_STYLES, t.tabs),
            Ctl::Sidebar => label_of(&SIDEBAR_STYLES, t.sidebar_style),
            Ctl::Era => label_of(&ERAS, t.era),
            Ctl::Icons => label_of(&ICON_SETS, t.icons),
            Ctl::Logo => label_of(&LOGOS, t.logo_style),
            Ctl::Fit => label_of(&FITS, t.wallpaper_fit),
            Ctl::Area => label_of(&AREAS, t.wallpaper_area),
            Ctl::Part(p) => t.parts.label(p),
            Ctl::Font => t.font.clone(),
            Ctl::SidebarRow => number(t.sidebar_row_h),
            Ctl::ScrollWidth => number(t.scrollbar_w),
            _ => String::new(),
        }
    }

    /// Puts the one-line box over what it edits (when that's in view; otherwise editing ends), and
    /// the skin's text box over its place (or hides it).
    fn place_boxes(&mut self) {
        let want = match &self.editing {
            Some(Editing::Hex(i, _)) => self.rows.iter().find(|r| r.ctl == Ctl::Colour(*i)).and_then(|r| self.part_rect(r, Part::Hex)).map(|r| self.on_page(r)),
            Some(Editing::LogoText(_)) => self.rows.iter().find(|r| r.ctl == Ctl::LogoText).and_then(|r| self.part_rect(r, Part::Main)).map(|r| self.on_page(r)),
            // (The smaller layouts' search is at the top: the box goes right of its magnifier.)
            Some(Editing::Search(_)) if self.layout != Layout::Wide => {
                let s = self.search_rect();
                Some(Rect::new(s.x + 16.0, s.y, (s.w - 16.0).max(20.0), s.h))
            }
            Some(Editing::Search(_)) => self.rows.iter().find(|r| r.ctl == Ctl::Search).and_then(|r| self.part_rect(r, Part::Main)).map(|r| self.on_page(r)),
            Some(Editing::Picture(_)) => self.rows.iter().find(|r| r.ctl == Ctl::Picture).and_then(|r| self.part_rect(r, Part::Hex)).map(|r| self.on_page(r)),
            Some(Editing::Number(s, _)) => self.rows.iter().find(|r| r.ctl == Ctl::Slide(*s)).and_then(|r| self.part_rect(r, Part::Hex)).map(|r| self.on_page(r)),
            // (One column: over the skin's drop-down.)
            Some(Editing::Rename(_)) if self.layout == Layout::Compact => Some(self.skin_box()),
            Some(Editing::Rename(id)) => self.items.iter().find(|i| i.id == *id).map(|i| self.item_on_screen(i.rect).inset(6.0, 3.0)),
            None => None,
        };
        let top = match self.editing {
            Some(Editing::Rename(_)) => self.layout == Layout::Compact,
            Some(Editing::Search(_)) => self.layout != Layout::Wide,
            _ => false,
        };
        let area = if top {
            self.header_rect()
        } else if matches!(self.editing, Some(Editing::Rename(_))) {
            self.list_rect()
        } else {
            self.page_rect()
        };
        let want = want.filter(|r| r.y >= area.y && r.bottom() <= area.bottom() && self.asking.is_none());
        if want != self.edit_at {
            match want {
                Some(rect) => {
                    let b = self.border();
                    self.edit_box.place(Rect::new(rect.x + b, rect.y + b, rect.w, rect.h).inset(1.0, 1.0), 7.0, self.scale());
                    if self.edit_at.is_none() {
                        self.edit_box.show();
                    }
                    if std::mem::take(&mut self.edit_fresh) {
                        // Its text went in before it had its size, which can leave only the last
                        // letters in view: from the start again, all of it selected.
                        let n = self.edit_box.text().encode_utf16().count();
                        self.edit_box.select_range(n, 0);
                        self.edit_box.select_all();
                    }
                }
                None => {
                    self.edit_box.hide();
                    if frame::is_wine() {
                        self.repaint_until = self.now() + 0.3;
                        self.invalidate();
                    }
                }
            }
            self.edit_at = want;
        }
    }

    // ------------------------------------------------------------ the mouse

    fn target_at(&self, p: Point) -> Option<Hot> {
        if p.y < size::TITLE_H {
            return self.close_rect().contains(p).then_some(Hot::Close);
        }
        if p.x < self.m().list_w {
            if let Some(thumb) = self.list.thumb(self.list_track()) {
                if thumb.inset(-4.0, 0.0).contains(p) {
                    return Some(Hot::ListThumb);
                }
            }
            if self.list_rect().contains(p) {
                return self.items.iter().position(|i| self.item_on_screen(i.rect).contains(p)).map(Hot::Item);
            }
            if self.layout == Layout::Slim {
                return ICONS.iter().find(|(b, _)| self.icon_rect(*b).contains(p)).map(|(b, _)| Hot::Button(*b));
            }
            // Their exact shapes: the corner left empty by a slant isn't the button.
            return BUTTONS.iter().find(|(b, _)| crate::pathbar::box_contains_beside(self.button_rect(*b), self.lean, beside_of(*b), p)).map(|(b, _)| Hot::Button(*b));
        }
        if self.layout == Layout::Compact {
            if self.skin_box().contains(p) {
                return Some(Hot::Button(Btn::Skin));
            }
            if let Some(b) = [Btn::Undo, Btn::Redo, Btn::More].into_iter().find(|b| self.icon_rect(*b).contains(p)) {
                return Some(Hot::Button(b));
            }
        }
        if self.layout != Layout::Wide && self.search_rect().contains(p) {
            return Some(Hot::Ctl(Ctl::Search, Part::Main));
        }
        if !self.page_rect().contains(p) {
            return None;
        }
        if let Some(thumb) = self.page.thumb(self.page_track()) {
            if thumb.inset(-4.0, 0.0).contains(p) {
                return Some(Hot::PageThumb);
            }
        }
        for block in &self.blocks {
            if let Block::Fold(r, t, _) = block {
                if self.on_page(*r).contains(p) {
                    return Some(Hot::Fold(*t));
                }
            }
        }
        for row in &self.rows {
            for part in [Part::Clear, Part::Main, Part::Hex].into_iter().chain(self.extra_parts(row)) {
                let Some(r) = self.part_rect(row, part).map(|r| self.on_page(r)) else { continue };
                let hit = match (row.ctl, part) {
                    (Ctl::Dark | Ctl::Grip, _) => kit::toggle_contains(r, p),
                    (Ctl::Part(q), _) if crate::parts::is_flag(q) => kit::toggle_contains(r, p),
                    // (The pad's dot can sit on its edge: a little room round it.)
                    (Ctl::Place, Part::Main) => r.inset(-8.0, -8.0).contains(p),
                    (Ctl::Place | Ctl::Thumbs, _) => r.contains(p),
                    // Wide: the wallpaper's NONE and BROWSE… lean where they meet (their exact shapes).
                    (Ctl::Picture, Part::Clear) if self.layout == Layout::Wide => crate::pathbar::box_contains_beside(r, self.lean, crate::pathbar::Beside::RIGHT, p),
                    (Ctl::Picture, Part::Main) if self.layout == Layout::Wide => crate::pathbar::box_contains_beside(r, self.lean, crate::pathbar::Beside::LEFT, p),
                    (_, Part::Clear) | (Ctl::Picture, Part::Main) => kit::button_contains(r, p),
                    _ => r.contains(p),
                };
                if hit {
                    return Some(Hot::Ctl(row.ctl, part));
                }
            }
        }
        None
    }

    fn client_point(&self, lparam: LPARAM) -> Point {
        let (s, b) = (self.scale(), self.border());
        Point::new((lparam.0 & 0xffff) as i16 as f32 / s - b, ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s - b)
    }

    fn mouse_move(&mut self, p: Point, app: &mut AppState) -> Vec<Ask> {
        if !self.tracking {
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
            // SAFETY: a valid struct for our own window.
            self.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
        }
        if self.asking.is_some() {
            if self.ask.mouse_move(p) == Answer::Redraw {
                self.invalidate();
            }
            return Vec::new();
        }
        if self.holding.active() {
            self.holding.at = p;
            return Vec::new();
        }
        if let Some((s, pad)) = self.sliding.as_ref().map(|d| (d.slide, d.pad)) {
            return if pad { self.place_to_point(p, app) } else { self.slide_to_point(s, p, app) };
        }
        if let Some((which, grab)) = self.drag {
            let (track, scroller) = if which == Hot::ListThumb { (self.list_track(), &mut self.list) } else { (self.page_track(), &mut self.page) };
            if let Some(thumb) = scroller.thumb(track) {
                let to = scroller.offset_for_thumb(track, thumb.h, p.y - grab);
                scroller.jump_to(to);
                self.invalidate();
            }
            return Vec::new();
        }
        let hover = self.target_at(p);
        if hover != self.hover {
            self.hover = hover;
            self.invalidate();
        }
        Vec::new()
    }

    fn mouse_down(&mut self, p: Point, double: bool, app: &mut AppState) -> Vec<Ask> {
        self.cues = false;
        self.act_on_char = None;
        if let Some(id) = self.asking.as_ref().map(|q| q.0) {
            // (Not before the question is drawn: a click meant for something else mustn't answer it.)
            let a = if self.ask.shown(id) { self.ask.mouse_down(p) } else { Answer::Nothing };
            return self.answer(a, app);
        }
        // An older Windows' scroll bars: an arrow button moves a list or the page a line, the
        // track a page, again and again while the button is held.
        let (now, list, page) = (self.now(), self.bar.place(self.list_rect()), self.bar.place(self.page_rect()));
        if self.holding.press(self.hwnd, list, &mut self.list, 0, p, now)
            || (self.holding.press(self.hwnd, page, &mut self.page, 1, p, now))
        {
            self.invalidate();
            return Vec::new();
        }
        let target = self.target_at(p);
        // A click anywhere ends typing in the one-line box (what was typed is kept).
        if self.editing.is_some() && !matches!(target, Some(Hot::Ctl(_, Part::Hex))) {
            self.end_editing(app, true);
        }
        // SAFETY: focus and capture on our own window.
        unsafe {
            if GetFocus() != self.hwnd {
                let _ = SetFocus(Some(self.hwnd));
            }
            SetCapture(self.hwnd);
        }
        self.pressed = target;
        let mut asks = Vec::new();
        match target {
            Some(Hot::Item(i)) => {
                // Picking happens on the way down, like the tabs: it feels instant.
                if let Some(id) = self.items.get(i).map(|it| it.id.clone()) {
                    asks.extend(self.pick(app, &id));
                }
                self.focus = Spot::List;
            }
            Some(Hot::ListThumb) => {
                if let Some(thumb) = self.list.thumb(self.list_track()) {
                    self.drag = Some((Hot::ListThumb, p.y - thumb.y));
                }
            }
            Some(Hot::PageThumb) => {
                if let Some(thumb) = self.page.thumb(self.page_track()) {
                    self.drag = Some((Hot::PageThumb, p.y - thumb.y));
                }
            }
            Some(Hot::Button(Btn::Skin)) => self.focus = Spot::List,
            Some(Hot::Button(b)) => self.focus = Spot::Button(b),
            Some(Hot::Fold(t)) => self.focus = Spot::Fold(t),
            // A slider: its knob jumps to the click, and follows the mouse while it's held.
            // The pad: the dot jumps to the click and follows the mouse; a double-click puts it back.
            Some(Hot::Ctl(Ctl::Place, Part::Main)) if double => {
                self.focus = Spot::Ctl(Ctl::Place, Part::Main);
                asks.extend(self.reset_place(app));
            }
            Some(Hot::Ctl(Ctl::Place, Part::Main)) => {
                self.focus = Spot::Ctl(Ctl::Place, Part::Main);
                if app.skins.find(&app.settings.skin).is_some() && !app.theme.wallpaper.is_empty() {
                    // Grabbed where it is: it moves with the mouse from there (nothing jumps).
                    let pad = self.rows.iter().find(|r| r.ctl == Ctl::Place).and_then(|r| self.part_rect(r, Part::Main)).map(|r| self.on_page(r));
                    let size = pad.and_then(|pad| self.pad_wallpaper(app, pad).map(|w| (pad, w.2)));
                    let room = size.map_or((0.0, 0.0), |(pad, (w, h))| (pad.w - w, pad.h - h));
                    let place = (slide_value(Slide::Across, &app.theme), slide_value(Slide::Down, &app.theme));
                    self.sliding = Some(Sliding::pad(p, place, room));
                    self.edit_group = Some(self.next_group);
                    self.next_group += 1;
                }
            }
            Some(Hot::Ctl(Ctl::Slide(s), Part::Main)) if double => {
                self.focus = Spot::Ctl(Ctl::Slide(s), Part::Main);
                // The first click may have moved the knob: one undo puts back what was there before it.
                self.edit_group = self.press_group.take();
                asks.extend(self.reset_slide(s, app));
                self.edit_group = None;
            }
            Some(Hot::Ctl(Ctl::Slide(s), part @ Part::Main)) => {
                self.focus = Spot::Ctl(Ctl::Slide(s), part);
                if app.skins.find(&app.settings.skin).is_some() && !app.theme.wallpaper.is_empty() {
                    // On the knob: grabbed where it is (nothing jumps); elsewhere on the track the
                    // knob jumps to the click.
                    let mut sliding = Sliding::new(s);
                    let knob = self.knob_x(s, app).filter(|x| (p.x - x).abs() <= KNOB_GRAB);
                    if let Some(x) = knob {
                        sliding.dx = p.x - x;
                    }
                    self.sliding = Some(sliding);
                    self.edit_group = Some(self.next_group);
                    self.press_group = self.edit_group;
                    self.next_group += 1;
                    if knob.is_none() {
                        asks.extend(self.slide_to_point(s, p, app));
                    }
                }
            }
            Some(Hot::Ctl(c, part)) => self.focus = Spot::Ctl(c, part),
            _ => {}
        }
        self.invalidate();
        asks
    }

    fn mouse_up(&mut self, p: Point, app: &mut AppState) -> Vec<Ask> {
        if let Some(id) = self.asking.as_ref().map(|q| q.0) {
            let a = if self.ask.shown(id) { self.ask.mouse_up(p) } else { Answer::Nothing };
            return self.answer(a, app);
        }
        if self.holding.release(self.hwnd) {
            // SAFETY: releasing our own capture.
            unsafe {
                let _ = ReleaseCapture();
            }
            self.invalidate();
            return Vec::new();
        }
        let was = self.pressed.take();
        self.drag = None;
        self.end_sliding();
        // SAFETY: releasing our own capture.
        unsafe {
            let _ = ReleaseCapture();
        }
        self.invalidate();
        if was.is_none() || was != self.target_at(p) {
            return Vec::new();
        }
        match was {
            Some(Hot::Close) => vec![Ask::Job(Job::Close)],
            Some(Hot::Button(b)) => self.press(b, app),
            Some(Hot::Ctl(Ctl::Slide(_) | Ctl::Place, Part::Main)) => Vec::new(),
            Some(Hot::Ctl(c, part)) => self.activate(c, part, app),
            Some(Hot::Fold(t)) => {
                self.toggle_fold(t);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Slider `s` set from the mouse at `p`.
    fn slide_to_point(&mut self, s: Slide, p: Point, app: &mut AppState) -> Vec<Ask> {
        let Some(area) = self.rows.iter().find(|r| r.ctl == Ctl::Slide(s)).and_then(|r| self.part_rect(r, Part::Main)).map(|r| self.on_page(r)) else { return Vec::new() };
        let standard = slide_default(s, &app.theme);
        let dx = self.sliding.as_ref().map_or(0.0, |d| d.dx);
        self.slide_to(s, slide_at(s, area, p.x - dx, standard), app)
    }

    /// Where slider `s`'s knob is across, on screen (None when it isn't laid out).
    fn knob_x(&self, s: Slide, app: &AppState) -> Option<f32> {
        let area = self.rows.iter().find(|r| r.ctl == Ctl::Slide(s)).and_then(|r| self.part_rect(r, Part::Main)).map(|r| self.on_page(r))?;
        let (_, min, max, ..) = slide_spec(s);
        let share = ((slide_value(s, &app.theme) - min) / (max - min)).clamp(0.0, 1.0);
        Some(area.x + 7.0 + (area.w - 14.0).max(1.0) * share)
    }

    /// The group for a change made by a key: a new one, or while Windows repeats a key held down,
    /// the one its first press made.
    fn held_key_group(&mut self, repeat: bool) -> u64 {
        let g = match self.key_group.filter(|_| repeat) {
            Some(g) => g,
            None => {
                self.next_group += 1;
                self.next_group - 1
            }
        };
        self.key_group = Some(g);
        g
    }

    /// Slider `s` set to `v`: the skin's text gets it (unless it already says so).
    fn slide_to(&mut self, s: Slide, v: f32, app: &mut AppState) -> Vec<Ask> {
        if app.skins.find(&app.settings.skin).is_none() || app.theme.wallpaper.is_empty() || !v.is_finite() {
            return Vec::new();
        }
        let (_, min, max, ..) = slide_spec(s);
        if (v.round().clamp(min, max) - slide_value(s, &app.theme)).abs() < 0.5 {
            return Vec::new();
        }
        let changes = slide_changes(s, v, &app.theme);
        self.drag_change(app, &changes)
    }

    /// The picture on the pad dragged to the point `p`.
    fn place_to_point(&mut self, p: Point, app: &mut AppState) -> Vec<Ask> {
        let Some((from, place, room)) = self.sliding.as_ref().and_then(|d| d.grab) else { return Vec::new() };
        let standard = (slide_default(Slide::Across, &app.theme), slide_default(Slide::Down, &app.theme));
        let (x, y) = pan_place(place, (p.x - from.x, p.y - from.y), room, standard);
        self.place_to(x, y, app)
    }

    /// The wallpaper at `x` across and `y` down (0 to 100): one line for both.
    fn place_to(&mut self, x: f32, y: f32, app: &mut AppState) -> Vec<Ask> {
        if app.skins.find(&app.settings.skin).is_none() || app.theme.wallpaper.is_empty() || !x.is_finite() || !y.is_finite() {
            return Vec::new();
        }
        let (x, y) = (x.round().clamp(0.0, 100.0), y.round().clamp(0.0, 100.0));
        if (x - slide_value(Slide::Across, &app.theme)).abs() < 0.5 && (y - slide_value(Slide::Down, &app.theme)).abs() < 0.5 {
            return Vec::new();
        }
        let value = position_text(x / 100.0, Some(y / 100.0), app.theme.wallpaper_fit);
        self.drag_change(app, &[("wallpaper-position", value)])
    }

    /// The wallpaper's place back to the skin's default (double-click on the pad, Delete).
    fn reset_place(&mut self, app: &mut AppState) -> Vec<Ask> {
        if app.skins.find(&app.settings.skin).is_none() || app.theme.wallpaper.is_empty() {
            return Vec::new();
        }
        self.change_text(app, |text| skin::without_value(text, "wallpaper-position"))
    }

    /// A change from a slider or the pad (kept for Esc while one is held).
    fn drag_change(&mut self, app: &mut AppState, changes: &[(&str, String)]) -> Vec<Ask> {
        if self.sliding.is_some() {
            // (A built-in skin gets its copy first: what Esc puts back is the copy's text.)
            let Some(id) = self.editable(app) else { return Vec::new() };
            let text = app.skins.find(&id).map(|e| e.text.clone()).unwrap_or_default();
            if let Some(d) = self.sliding.as_mut() {
                d.changing(&id, &text);
            }
        }
        self.change(app, changes)
    }

    /// Reads the pictures next to the skin small, in the background, when the folder or the
    /// wallpaper changed (or the window came to the front: one may have been added).
    fn read_pictures(&mut self, app: &AppState) {
        let want = (self.picture_dir(app), app.theme.wallpaper.clone());
        if self.thumbs_for.as_ref() == Some(&want) {
            return;
        }
        self.thumbs_for = Some(want.clone());
        let (hwnd, serial) = (self.hwnd.0 as isize, self.serial);
        shell::background("skins: pictures", move || {
            let (dir, current) = want;
            let (list, current_pic, too_big) = read_nearby(&dir, &current);
            post(hwnd, serial, Bg::Pictures { dir, current, list, current_pic, too_big });
        });
    }

    /// Slider `s` back to the skin's default (double-click, or Delete on it).
    fn reset_slide(&mut self, s: Slide, app: &mut AppState) -> Vec<Ask> {
        if app.skins.find(&app.settings.skin).is_none() || app.theme.wallpaper.is_empty() {
            return Vec::new();
        }
        let t = app.theme.clone();
        self.change_text(app, |text| slide_reset(text, s, &t))
    }

    /// A slider let go of: the changes while it was held are one undo step.
    fn end_sliding(&mut self) {
        if self.sliding.take().is_some() {
            self.edit_group = None;
            self.invalidate();
        }
    }

    /// Esc while a slider is held: the skin as it was before the drag.
    fn cancel_sliding(&mut self, app: &mut AppState) -> Vec<Ask> {
        let Some(d) = self.sliding.take() else { return Vec::new() };
        self.edit_group = None;
        // SAFETY: releasing our own capture.
        unsafe {
            let _ = ReleaseCapture();
        }
        self.pressed = None;
        self.invalidate();
        let Some((id, text)) = d.put_back(&app.settings.skin).map(|(i, t)| (i.to_string(), t.to_string())) else { return Vec::new() };
        if app.skins.find(&id).is_none_or(|e| e.built_in() || e.text == text) {
            return Vec::new();
        }
        app.skins.set_text(&id, &text);
        app.apply_skin();
        self.save_soon(&id);
        vec![Ask::RedrawMain]
    }

    fn wheel(&mut self, p: Point, notches: f32) {
        let now = self.now();
        let moved = if p.x < self.m().list_w { self.list.scroll_by(-notches * WHEEL_STEP, now) } else { self.page.scroll_by(-notches * WHEEL_STEP, now) };
        if moved {
            self.invalidate();
        }
    }

    // ------------------------------------------------------------ the keyboard

    /// The places that can have the keyboard, in Tab order.
    fn spots(&self, app: &AppState) -> Vec<Spot> {
        let own = app.skins.find(&app.settings.skin).is_some_and(|e| !e.built_in());
        // (One column: the skin's drop-down is the list's place.)
        let mut spots = vec![Spot::List];
        let buttons: Vec<Btn> = match self.layout {
            Layout::Wide => BUTTONS.iter().map(|(b, _)| *b).collect(),
            Layout::Slim => ICONS.iter().map(|(b, _)| *b).collect(),
            Layout::Compact => vec![Btn::Undo, Btn::Redo, Btn::More],
        };
        for b in buttons {
            if own || !matches!(b, Btn::Rename | Btn::Delete) {
                spots.push(Spot::Button(b));
            }
        }
        if self.layout != Layout::Wide {
            spots.push(Spot::Ctl(Ctl::Search, Part::Main));
        }
        // In the page's order: the sections' headings (one column), and the rows when there's
        // a skin.
        let mut placed: Vec<(f32, Spot)> = self.blocks.iter().filter_map(|b| if let Block::Fold(r, t, _) = b { Some((r.y, Spot::Fold(*t))) } else { None }).collect();
        if app.skins.find(&app.settings.skin).is_some() {
            for row in &self.rows {
                let y = row.rect.y;
                if row.ctl == Ctl::Thumbs {
                    placed.extend(self.extra_parts(row).into_iter().map(|p| (y, Spot::Ctl(row.ctl, p))));
                    continue;
                }
                placed.push((y, Spot::Ctl(row.ctl, Part::Main)));
                if matches!(row.ctl, Ctl::Slide(_)) {
                    placed.push((y, Spot::Ctl(row.ctl, Part::Hex)));
                }
                if matches!(row.ctl, Ctl::Colour(_) | Ctl::Picture) {
                    placed.push((y, Spot::Ctl(row.ctl, Part::Hex)));
                    placed.push((y, Spot::Ctl(row.ctl, Part::Clear)));
                }
            }
        }
        placed.sort_by(|a, b| a.0.total_cmp(&b.0));
        spots.extend(placed.into_iter().map(|(_, s)| s));
        spots
    }

    fn move_focus(&mut self, step: i32, app: &AppState) {
        let spots = self.spots(app);
        let at = spots.iter().position(|s| *s == self.focus).unwrap_or(0) as i32;
        let next = spots[(at + step).rem_euclid(spots.len() as i32) as usize];
        self.set_focus(next);
    }

    fn set_focus(&mut self, spot: Spot) {
        self.focus = spot;
        self.cues = true;
        match spot {
            Spot::Ctl(c, part) => {
                if let Some(r) = self.rows.iter().find(|r| r.ctl == c).and_then(|r| self.part_rect(r, part)) {
                    let row = Rect::new(r.x, r.y - 14.0, r.w, r.h + 28.0);
                    self.page.reveal(row.y, row.bottom(), 8.0, self.now());
                }
            }
            Spot::Fold(t) => {
                if let Some(r) = self.blocks.iter().find_map(|b| if let Block::Fold(r, f, _) = b { (*f == t).then_some(*r) } else { None }) {
                    self.page.reveal(r.y - 8.0, r.bottom() + 8.0, 8.0, self.now());
                }
            }
            _ => {}
        }
        self.invalidate();
    }

    /// A key, when the window (not a text box) has the keyboard. Returns whether it was used.
    /// `repeat`: the key is held down and this is Windows repeating it.
    fn key(&mut self, vk: u16, repeat: bool, app: &mut AppState, asks: &mut Vec<Ask>) -> bool {
        // SAFETY: plain key-state queries.
        let (ctrl, shift, alt) = unsafe { (GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0) };
        let is = |k: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY| vk == k.0;
        self.act_on_char = None;
        if let Some(id) = self.asking.as_ref().map(|q| q.0) {
            // Enter held a moment too long on DELETE, or a key pressed before the question was even
            // drawn, must not answer it.
            if !repeat && self.ask.shown(id) {
                let a = self.ask.key(vk);
                asks.extend(self.answer(a, app));
            }
            return true;
        }
        if self.sliding.is_some() {
            if is(VK_ESCAPE) {
                asks.extend(self.cancel_sliding(app));
            }
            return true;
        }
        if alt && !is(VK_DOWN) {
            return false;
        }
        let now = self.now();
        if self.focus == Spot::Ctl(Ctl::Place, Part::Main) {
            let (x, y) = (slide_value(Slide::Across, &app.theme), slide_value(Slide::Down, &app.theme));
            let to = match () {
                _ if is(VK_LEFT) => Some((x - 1.0, y)),
                _ if is(VK_RIGHT) => Some((x + 1.0, y)),
                _ if is(VK_UP) => Some((x, y - 1.0)),
                _ if is(VK_DOWN) => Some((x, y + 1.0)),
                _ if is(VK_PRIOR) => Some((x, y - 10.0)),
                _ if is(VK_NEXT) => Some((x, y + 10.0)),
                _ if is(VK_HOME) => Some((0.0, y)),
                _ if is(VK_END) => Some((100.0, y)),
                _ => None,
            };
            if is(VK_DELETE) || is(VK_BACK) {
                self.cues = true;
                asks.extend(self.reset_place(app));
                return true;
            }
            if let Some((x, y)) = to {
                self.cues = true;
                self.edit_group = Some(self.held_key_group(repeat));
                asks.extend(self.place_to(x, y, app));
                self.edit_group = None;
                self.invalidate();
                return true;
            }
        }
        if let Spot::Ctl(Ctl::Slide(s), _) = self.focus {
            let (_, min, max, step, page) = slide_spec(s);
            let v = slide_value(s, &app.theme);
            let to = match () {
                _ if is(VK_LEFT) => Some(v - step),
                _ if is(VK_RIGHT) => Some(v + step),
                _ if is(VK_PRIOR) => Some(v + page),
                _ if is(VK_NEXT) => Some(v - page),
                _ if is(VK_HOME) => Some(min),
                _ if is(VK_END) => Some(max),
                _ => None,
            };
            if is(VK_DELETE) || is(VK_BACK) {
                self.cues = true;
                asks.extend(self.reset_slide(s, app));
                return true;
            }
            if let Some(to) = to {
                self.cues = true;
                // Held down, a key is one undo step (as a box typed in); the next change is another.
                self.edit_group = Some(self.held_key_group(repeat));
                asks.extend(self.slide_to(s, to.clamp(min, max), app));
                self.edit_group = None;
                self.invalidate();
                return true;
            }
        }
        match () {
            _ if is(VK_ESCAPE) || (ctrl && vk == 'W' as u16) => asks.push(Ask::Job(Job::Close)),
            // Light or dark, as in the main window: switches to the skin's twin (if it has one).
            _ if ctrl && shift && vk == 'L' as u16 => asks.push(Ask::Command(Command::ToggleTheme)),
            _ if ctrl && vk == 'Z' as u16 => asks.extend(self.undo_step(app, shift)),
            _ if ctrl && vk == 'Y' as u16 => asks.extend(self.undo_step(app, true)),
            _ if is(VK_TAB) => self.move_focus(if shift { -1 } else { 1 }, app),
            _ if self.focus == Spot::List && (is(VK_UP) || is(VK_DOWN) || is(VK_HOME) || is(VK_END)) => {
                let at = self.items.iter().position(|i| i.id.eq_ignore_ascii_case(&app.settings.skin));
                let last = self.items.len().saturating_sub(1);
                let next = match at {
                    _ if is(VK_HOME) => 0,
                    _ if is(VK_END) => last,
                    Some(a) if is(VK_UP) => a.saturating_sub(1),
                    Some(a) => (a + 1).min(last),
                    None => 0,
                };
                if let Some(item) = self.items.get(next) {
                    let (id, rect) = (item.id.clone(), item.rect);
                    asks.extend(self.pick(app, &id));
                    self.list.reveal(rect.y, rect.bottom(), 4.0, now);
                }
                self.cues = true;
                self.invalidate();
            }
            _ if self.focus == Spot::List && is(VK_F2) => asks.extend(self.press(Btn::Rename, app)),
            _ if self.focus == Spot::List && is(VK_DELETE) => asks.extend(self.press(Btn::Delete, app)),
            // (Alt+Down opens a drop-down, below.)
            _ if !alt && (is(VK_UP) || is(VK_DOWN)) => self.move_focus(if is(VK_UP) { -1 } else { 1 }, app),
            _ if is(VK_SPACE) || is(VK_RETURN) || is(VK_F4) || (alt && is(VK_DOWN)) => {
                self.cues = true;
                let dropdown = matches!(
                    self.focus,
                    Spot::Ctl(Ctl::Shape | Ctl::Slant | Ctl::RowHeight | Ctl::FontSize | Ctl::TabLine | Ctl::ToolbarLine | Ctl::RowEdge | Ctl::SidebarEdge | Ctl::Buttons | Ctl::Caption | Ctl::Tabs | Ctl::Sidebar | Ctl::Era | Ctl::Icons | Ctl::Logo | Ctl::Fit | Ctl::Area | Ctl::Part(_) | Ctl::Font | Ctl::SidebarRow | Ctl::ScrollWidth, _)
                );
                // (One column: the skin's drop-down and the menu of the rest open as drop-downs do.)
                let dropdown = dropdown || (self.layout == Layout::Compact && matches!(self.focus, Spot::List | Spot::Button(Btn::More)));
                if dropdown && (is(VK_SPACE) || is(VK_RETURN)) {
                    // Opened when the key's character has come (it would close the menu again).
                    self.act_on_char = Some(self.focus);
                } else if dropdown || !(is(VK_F4) || alt) {
                    asks.extend(self.activate_spot(self.focus, app));
                }
                self.invalidate();
            }
            _ if is(VK_NEXT) || is(VK_PRIOR) => {
                let step = (self.page_rect().h - 40.0).max(40.0);
                if self.page.scroll_by(if is(VK_NEXT) { step } else { -step }, now) {
                    self.invalidate();
                }
            }
            _ => return false,
        }
        true
    }

    fn activate_spot(&mut self, spot: Spot, app: &mut AppState) -> Vec<Ask> {
        match spot {
            Spot::Button(b) => self.press(b, app),
            Spot::Ctl(c, part) => self.activate(c, part, app),
            Spot::Fold(t) => {
                self.toggle_fold(t);
                Vec::new()
            }
            Spot::List if self.layout == Layout::Compact => self.press(Btn::Skin, app),
            Spot::List => Vec::new(),
        }
    }

    // ------------------------------------------------------------ changing things

    /// Uses skin `id` (as Settings' drop-down does).
    fn pick(&mut self, app: &mut AppState, id: &str) -> Vec<Ask> {
        if app.settings.skin.eq_ignore_ascii_case(id) {
            return Vec::new();
        }
        self.end_editing(app, true);
        app.pick_skin(id);
        self.status = None;
        self.status_mine = false;
        self.layout_dirty = true;
        self.page.jump_to(0.0);
        self.invalidate();
        vec![Ask::RedrawMain]
    }

    /// The skin to change: the one in use - or, when that's built in, a copy of it made now ("My
    /// skin"), which is used from now on. None when there is nothing to change (its file is gone).
    fn editable(&mut self, app: &mut AppState) -> Option<String> {
        let id = app.settings.skin.clone();
        let e = app.skins.find(&id)?;
        if !e.built_in() {
            return Some(e.id.clone());
        }
        let (from_name, from_text) = (e.name.clone(), e.text.clone());
        let name = app.skins.free_name("My skin");
        let text = skin::copy_text(&from_text, &name, &from_name);
        let new = app.skins.add(&name, &text);
        self.forget_steps(&new);
        app.pick_skin(&new);
        // A box open for the built-in skin types on into the copy (its next letters too).
        if self.editing_for.as_ref().is_some_and(|(for_id, _)| *for_id == id) {
            self.editing_for = Some((new.clone(), text));
        }
        self.say(format!("Made {name}, your copy of {from_name}."), false);
        self.layout_dirty = true;
        Some(new)
    }

    /// Changes the skin in use: these settings get these values in its text, the look follows at
    /// once and the file is saved a moment later.
    fn change(&mut self, app: &mut AppState, changes: &[(&str, String)]) -> Vec<Ask> {
        self.change_text(app, |text| changes.iter().fold(text.to_string(), |t, (key, value)| skin::with_value(&t, key, value)))
    }

    /// [`SkinWindow::change`] with the skin's new text made by `edit` from its text now.
    fn change_text(&mut self, app: &mut AppState, edit: impl Fn(&str) -> String) -> Vec<Ask> {
        // The next "Saved…" may show again (unless this change makes a copy, which says so).
        self.status_mine = false;
        let Some(id) = self.editable(app) else { return Vec::new() };
        let Some(text) = app.skins.find(&id).map(|e| e.text.clone()) else { return Vec::new() };
        let new = edit(&text);
        if new != text {
            self.remember(&id, text, self.edit_group);
        }
        app.skins.set_text(&id, &new);
        app.apply_skin();
        self.save_soon(&id);
        self.invalidate();
        vec![Ask::RedrawMain]
    }

    /// Keeps `before` (skin `id`'s text before a change) as an undo step - unless the change is
    /// part of the same `group` as the last step (one box typed in), which already has it.
    fn remember(&mut self, id: &str, before: String, group: Option<u64>) {
        let now = self.now();
        let joins = match (group, self.undo_group) {
            (Some(g), Some((last, at))) => g == last && (g != TEXT_GROUP || now - at < TYPING_PAUSE),
            _ => false,
        };
        self.undo_group = group.map(|g| (g, now));
        self.redo.retain(|s| s.0 != id);
        if joins && self.undo.last().is_some_and(|s| s.0 == id) {
            return;
        }
        self.undo.push((id.to_string(), before));
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
    }

    /// Forgets skin `id`'s undo and redo steps: the id is a new skin's now. (A skin deleted outside
    /// the customiser - its file deleted in a folder - leaves its steps behind, and a new skin can
    /// get its name; undoing in the new one mustn't bring the old one's text.)
    fn forget_steps(&mut self, id: &str) {
        self.undo.retain(|s| s.0 != id);
        self.redo.retain(|s| s.0 != id);
    }

    /// Whether the skin in use has a step to undo (or to redo).
    fn can_undo(&self, app: &AppState, redo: bool) -> bool {
        // (A skin whose file is gone has nothing to undo into.)
        if app.skins.find(&app.settings.skin).is_none_or(|e| e.built_in()) {
            return false;
        }
        let steps = if redo { &self.redo } else { &self.undo };
        steps.iter().any(|s| s.0 == app.settings.skin)
    }

    /// Undoes the last change to the skin in use (or redoes the last one undone).
    fn undo_step(&mut self, app: &mut AppState, redo: bool) -> Vec<Ask> {
        self.end_editing(app, true);
        let id = app.settings.skin.clone();
        let Some(now) = app.skins.find(&id).filter(|e| !e.built_in()).map(|e| e.text.clone()) else { return Vec::new() };
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        // (A step that wouldn't change anything, like a box put back with Esc, is passed over.)
        let text = loop {
            let Some(at) = from.iter().rposition(|s| s.0 == id) else { return Vec::new() };
            let (_, text) = from.remove(at);
            if text != now {
                break text;
            }
        };
        to.push((id.clone(), now));
        self.undo_group = None;
        let was_dark = app.dark;
        app.skins.set_text(&id, &text);
        app.apply_skin();
        if app.dark != was_dark {
            // "Light or dark" follows, as when the Dark skin switch is flipped.
            app.pick_skin(&id);
        }
        self.save_soon(&id);
        self.layout_dirty = true;
        self.invalidate();
        vec![Ask::RedrawMain]
    }

    /// Saves `id` a moment after the last change (SAVE_TIMER), so typing a colour is one write.
    fn save_soon(&mut self, id: &str) {
        if !self.save_due.iter().any(|d| d == id) {
            self.save_due.push(id.to_string());
        }
        // SAFETY: a (re)started timer on our own window.
        unsafe {
            SetTimer(Some(self.hwnd), SAVE_TIMER, SAVE_DELAY_MS, None);
        }
    }

    /// A control was clicked, or Space / Enter was pressed on it.
    fn activate(&mut self, ctl: Ctl, part: Part, app: &mut AppState) -> Vec<Ask> {
        self.pop = None;
        // (The search works whatever the skin.)
        if app.skins.find(&app.settings.skin).is_none() && !matches!(ctl, Ctl::Search) {
            return Vec::new();
        }
        let anchor = self.rows.iter().find(|r| r.ctl == ctl).and_then(|r| self.part_rect(r, part)).map(|r| self.on_page(r));
        let border = self.border();
        let look = crate::droplist::DropLook::of(&app.theme);
        let menu = |items: Vec<MenuItem>| {
            if let Some(a) = anchor {
                let under = crate::ui::Rect::new(a.x + border, a.y + border, a.w, a.h);
                let list = crate::droplist::DropList { items, under: RECT::default(), look: look.clone(), menu: false, right: false };
                return vec![Ask::Job(Job::Drop(list, under))];
            }
            let at = Point::new(LIST_W + PAD_X + border, size::TITLE_H + HEADER_H + border);
            vec![Ask::Job(Job::Menu(Menu { tag: 1, at, items, right_aligned: false }))]
        };
        let t = app.theme.clone();
        let styles = |labels: &[(&str, bool)]| labels.iter().enumerate().map(|(n, (l, on))| MenuItem::new(n as u32 + 1, l).checked(*on)).collect::<Vec<_>>();
        match (ctl, part) {
            // A gradient's swatch: the picker changes its first colour.
            (Ctl::Colour(i), Part::Main) => {
                self.dialog_for = Some(app.settings.skin.clone());
                vec![Ask::Job(Job::Colour { at: i, initial: self.colour(app, i).paint.first().to_colorref() })]
            }
            (Ctl::Colour(i), Part::Clear) => {
                if self.colour(app, i).text == "none" {
                    return Vec::new();
                }
                self.change(app, &[(COLOURS[i].0, "none".to_string())])
            }
            (Ctl::Colour(i), Part::Hex) => {
                let now = self.colour(app, i);
                let own = Self::own_value(app, i);
                let put_back = own.clone().or_else(|| now.follows.clone()).unwrap_or_else(|| now.text.clone());
                self.start_editing(Editing::Hex(i, Before { shown: now.text.clone(), own, put_back }), &now.text, app);
                Vec::new()
            }
            (Ctl::LogoText, _) => {
                self.start_editing(Editing::LogoText(t.logo_text.clone()), &t.logo_text, app);
                Vec::new()
            }
            (Ctl::Search, _) => {
                let typed = self.colour_search.clone();
                self.start_editing(Editing::Search(typed.clone()), &typed, app);
                Vec::new()
            }
            (Ctl::Dark, _) => {
                let now = self.now();
                self.flips.start("dark", !t.dark, now, kit::SWITCH_TIME);
                let asks = self.change(app, &[("dark", (!t.dark).to_string())]);
                // "Light or dark" follows (as when a skin is picked).
                let id = app.settings.skin.clone();
                app.pick_skin(&id);
                asks
            }
            (Ctl::Part(q), _) if crate::parts::is_flag(q) => {
                let key = crate::parts::SPECS[q as usize].key;
                let on = t.parts.on(q);
                let now = self.now();
                self.flips.start(key, !on, now, kit::SWITCH_TIME);
                self.change(app, &[(key, (!on).to_string())])
            }
            (Ctl::Part(q), _) => {
                self.menu_for = Some(ctl);
                let now = t.parts.label(q);
                menu(crate::parts::choices(q).iter().enumerate().map(|(n, (l, _))| MenuItem::new(n as u32 + 1, l).checked(*l == now)).collect())
            }
            (Ctl::Font, _) => {
                self.menu_for = Some(ctl);
                if self.fonts.is_empty() {
                    self.fonts = crate::render::system_fonts();
                }
                menu(self.fonts.iter().enumerate().map(|(n, f)| MenuItem::new(n as u32 + 1, f).checked(f.eq_ignore_ascii_case(&t.font))).collect())
            }
            (Ctl::SidebarRow, _) => {
                self.menu_for = Some(ctl);
                menu(SIDEBAR_ROWS.iter().enumerate().map(|(n, v)| MenuItem::new(n as u32 + 1, &number(*v)).checked((*v - t.sidebar_row_h).abs() < 0.01)).collect())
            }
            (Ctl::ScrollWidth, _) => {
                self.menu_for = Some(ctl);
                menu(SCROLL_WIDTHS.iter().enumerate().map(|(n, v)| MenuItem::new(n as u32 + 1, &number(*v)).checked((*v - t.scrollbar_w).abs() < 0.01)).collect())
            }
            (Ctl::Shape, _) => {
                self.menu_for = Some(ctl);
                menu(SHAPES.iter().enumerate().map(|(n, (s, _, l))| MenuItem::new(n as u32 + 1, l).checked(*s == t.shape)).collect())
            }
            (Ctl::Slant, _) => {
                self.menu_for = Some(ctl);
                menu(SLANTS.iter().enumerate().map(|(n, v)| MenuItem::new(n as u32 + 1, &slant_label(*v)).checked((*v - t.slant).abs() < 0.01)).collect())
            }
            (Ctl::RowHeight, _) => {
                self.menu_for = Some(ctl);
                menu(ROW_HEIGHTS.iter().enumerate().map(|(n, v)| MenuItem::new(n as u32 + 1, &row_label(*v)).checked((*v - t.row_h).abs() < 0.01)).collect())
            }
            (Ctl::TabLine | Ctl::ToolbarLine | Ctl::RowEdge | Ctl::SidebarEdge, _) => {
                self.menu_for = Some(ctl);
                let now = line_width(ctl, &t).map_or(0.0, |(_, w)| w);
                menu(LINE_WIDTHS.iter().enumerate().map(|(n, v)| MenuItem::new(n as u32 + 1, &width_label(*v)).checked((*v - now).abs() < 0.01)).collect())
            }
            (Ctl::FontSize, _) => {
                self.menu_for = Some(ctl);
                menu(FONT_SIZES.iter().enumerate().map(|(n, v)| MenuItem::new(n as u32 + 1, &size_label(*v)).checked((*v - t.font_size / PT).abs() < 0.01)).collect())
            }
            (Ctl::Buttons, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&BUTTON_STYLES.map(|(s, _, l)| (l, s == t.buttons))))
            }
            (Ctl::Caption, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&CAPTION_STYLES.map(|(s, _, l)| (l, s == t.caption_buttons))))
            }
            (Ctl::Tabs, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&TAB_STYLES.map(|(s, _, l)| (l, s == t.tabs))))
            }
            (Ctl::Sidebar, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&SIDEBAR_STYLES.map(|(s, _, l)| (l, s == t.sidebar_style))))
            }
            (Ctl::Era, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&ERAS.map(|(s, _, l)| (l, s == t.era))))
            }
            (Ctl::Icons, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&ICON_SETS.map(|(s, _, l)| (l, s == t.icons))))
            }
            (Ctl::Logo, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&LOGOS.map(|(s, _, l)| (l, s == t.logo_style))))
            }
            (Ctl::Grip, _) => {
                let now = self.now();
                self.flips.start("grip", !t.scrollbar_grip, now, kit::SWITCH_TIME);
                self.change(app, &[("scrollbar-grip", (!t.scrollbar_grip).to_string())])
            }
            (Ctl::Thumbs, Part::Thumb(i)) => {
                let Some(path) = self.thumbs.get(i as usize).map(|t| t.0.clone()) else { return Vec::new() };
                let name = written_picture(&path, Some(&self.picture_dir(app)), &skin::folder());
                if same_path(&path, &app.theme.wallpaper) {
                    return Vec::new();
                }
                self.set_picture(app, &name)
            }
            (Ctl::Thumbs, _) => Vec::new(),
            (Ctl::Fit | Ctl::Area | Ctl::Slide(_) | Ctl::Place, _) if app.theme.wallpaper.is_empty() => Vec::new(),
            (Ctl::Place, Part::Grid(i)) => {
                let (x, y) = grid_place(i as usize);
                self.place_to(x, y, app)
            }
            (Ctl::Place, _) => Vec::new(),
            (Ctl::Slide(s), Part::Hex) => {
                let v = slide_value(s, &app.theme);
                self.start_editing(Editing::Number(s, v), &slide_label(s, v), app);
                Vec::new()
            }
            (Ctl::Fit, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&FITS.map(|(s, _, l)| (l, s == t.wallpaper_fit))))
            }
            (Ctl::Area, _) => {
                self.menu_for = Some(ctl);
                menu(styles(&AREAS.map(|(s, _, l)| (l, s == t.wallpaper_area))))
            }
            (Ctl::Picture, Part::Hex) => {
                let now = app.skins.find(&app.settings.skin).map(|e| picture_of(&e.text)).unwrap_or_default();
                self.start_editing(Editing::Picture(now.clone()), &now, app);
                Vec::new()
            }
            (Ctl::Picture, Part::Clear) => self.set_picture(app, ""),
            (Ctl::Picture, Part::Main) => {
                self.dialog_for = Some(app.settings.skin.clone());
                vec![Ask::Job(Job::Picture(Some(self.picture_dir(app))))]
            }
            (Ctl::Slide(_), _) => Vec::new(),
            (Ctl::Colour(_) | Ctl::Picture, Part::Grid(_) | Part::Thumb(_)) => Vec::new(),
        }
    }

    /// The folder a wallpaper is looked for in first: the skin's own (a built-in skin's copy goes in
    /// the skins folder).
    fn picture_dir(&self, app: &AppState) -> PathBuf {
        app.skins.find(&app.settings.skin).and_then(|e| e.file.as_ref()).and_then(|f| f.parent()).map_or_else(skin::folder, std::path::Path::to_path_buf)
    }

    /// The skin's wallpaper is `name` now ("" for none: its line goes). A picture Glint can't find
    /// is said under the skin's name.
    fn set_picture(&mut self, app: &mut AppState, name: &str) -> Vec<Ask> {
        let asks = if name.is_empty() {
            self.change_text(app, |t| skin::without_value(t, "wallpaper"))
        } else {
            self.change(app, &[("wallpaper", name.to_string())])
        };
        // (A network path isn't looked for: a server or mapped drive that's away would hold up the
        // window.)
        let wall = &app.theme.wallpaper;
        let path = std::path::Path::new(wall);
        if name.is_empty() || wall.is_empty() || skin::on_network(wall) || path.is_file() {
            return asks;
        }
        if path.is_dir() {
            self.say(format!("\u{201c}{name}\u{201d} is a folder, not a picture."), true);
        } else {
            // (Only the file's own name: a whole typed path doesn't fit under the skin's name.)
            let file = name.rsplit(['\\', '/']).next().filter(|f| !f.is_empty()).unwrap_or(name);
            self.say(format!("Can't find \u{201c}{file}\u{201d}: put it next to the skin, or use BROWSE\u{2026} to pick it."), true);
        }
        asks
    }

    /// A drop-down's choice: what it means in the skin's text.
    fn picked(&mut self, ctl: Ctl, n: usize, app: &mut AppState) -> Vec<Ask> {
        let t = app.theme.clone();
        let changes: Vec<(&str, String)> = match ctl {
            Ctl::Shape => {
                let Some((shape, word, _)) = SHAPES.get(n) else { return Vec::new() };
                let mut c = vec![("shape", word.to_string())];
                // A slant or a corner that would show nothing gets AMD Radeon's.
                if *shape == Shape::Slanted && t.slant < 1.0 {
                    c.push(("slant", "8".into()));
                    c.push(("path-slant", "6".into()));
                }
                if *shape == Shape::Rounded && t.radius < 4.0 {
                    c.push(("radius", "6".into()));
                }
                c
            }
            Ctl::Slant => {
                let Some(v) = SLANTS.get(n) else { return Vec::new() };
                let mut c = vec![("slant", number(*v)), ("path-slant", number((v * 0.75 * 2.0).round() / 2.0))];
                if *v > 0.0 && t.shape != Shape::Slanted {
                    c.push(("shape", "slanted".into()));
                }
                c
            }
            Ctl::RowHeight => match ROW_HEIGHTS.get(n) {
                Some(v) => vec![("row-height", number(*v))],
                None => return Vec::new(),
            },
            Ctl::FontSize => match FONT_SIZES.get(n) {
                Some(v) => vec![("font-size", format!("{}pt", number(*v)))],
                None => return Vec::new(),
            },
            Ctl::TabLine | Ctl::ToolbarLine | Ctl::RowEdge | Ctl::SidebarEdge => match LINE_WIDTHS.get(n) {
                Some(v) => match line_width(ctl, &t) {
                    Some((key, _)) => vec![(key, number(*v))],
                    None => return Vec::new(),
                },
                None => return Vec::new(),
            },
            Ctl::Buttons => word_at(&BUTTON_STYLES, n, "buttons"),
            Ctl::Caption => word_at(&CAPTION_STYLES, n, "caption-buttons"),
            Ctl::Tabs => word_at(&TAB_STYLES, n, "tabs"),
            Ctl::Sidebar => word_at(&SIDEBAR_STYLES, n, "sidebar-style"),
            Ctl::Era => word_at(&ERAS, n, "era"),
            Ctl::Icons => word_at(&ICON_SETS, n, "icons"),
            Ctl::Fit => word_at(&FITS, n, "wallpaper-fit"),
            Ctl::Area => word_at(&AREAS, n, "wallpaper-area"),
            Ctl::Part(q) => {
                let key = crate::parts::SPECS[q as usize].key;
                match crate::parts::choices(q).into_iter().nth(n) {
                    Some((_, Some(word))) => vec![(key, word)],
                    // Auto: the skin's line goes.
                    Some((_, None)) => return self.change_text(app, |text| skin::without_value(text, key)),
                    None => return Vec::new(),
                }
            }
            Ctl::Font => match self.fonts.get(n) {
                Some(f) => vec![("font", f.clone())],
                None => return Vec::new(),
            },
            Ctl::SidebarRow => match SIDEBAR_ROWS.get(n) {
                Some(v) => vec![("sidebar-row-height", number(*v))],
                None => return Vec::new(),
            },
            Ctl::ScrollWidth => match SCROLL_WIDTHS.get(n) {
                Some(v) => vec![("scrollbar-width", number(*v))],
                None => return Vec::new(),
            },
            Ctl::Logo => word_at(&LOGOS, n, "logo-style"),
            _ => return Vec::new(),
        };
        if changes.is_empty() {
            return Vec::new();
        }
        self.change(app, &changes)
    }

    /// Shows only the colours whose name has `typed` in it (all of them for nothing).
    fn search_colours(&mut self, typed: String) {
        if typed != self.colour_search {
            self.colour_search = typed;
            self.layout_dirty = true;
            self.invalidate();
        }
    }

    /// Shows the one-line box with `text` in it, for `what`.
    fn start_editing(&mut self, what: Editing, text: &str, app: &AppState) {
        // Only the logo's word has a hint: empty means the chrome wordmark.
        self.edit_box.set_hint(match what {
            Editing::LogoText(_) => LOGO_HINT,
            Editing::Picture(_) => PICTURE_HINT,
            Editing::Search(_) => SEARCH_ANY,
            _ => "",
        });
        self.editing = Some(what);
        self.editing_for = app.skins.find(&app.settings.skin).map(|e| (e.id.clone(), e.text.clone()));
        self.edit_group = Some(self.next_group);
        self.next_group += 1;
        self.edit_at = None; // placed on the next frame
        self.edit_fresh = true;
        self.edit_box.set_text(text);
        self.edit_box.select_all();
        self.edit_box.focus();
        self.invalidate();
    }

    /// The one-line box is done: `keep` what was typed (Enter, a click elsewhere) or put the value
    /// before back (Esc).
    fn end_editing(&mut self, app: &mut AppState, keep: bool) -> Vec<Ask> {
        let Some(what) = self.editing.take() else { return Vec::new() };
        let typed = self.edit_box.text();
        if self.edit_box.has_focus() {
            self.edit_box.give_focus_back();
        }
        self.edit_box.hide();
        self.edit_at = None;
        self.invalidate();
        let for_skin = self.editing_for.take();
        // Another skin in use now (Windows switched light or dark, its file changed): nothing that
        // was typed for the first one goes into it. (A rename names its skin itself.)
        let asks = if matches!(what, Editing::Rename(_) | Editing::Search(_)) || self.still_for(app, for_skin.as_ref()) {
            self.finish_editing(what, typed, keep, for_skin.map(|f| f.1), app)
        } else {
            Vec::new()
        };
        self.edit_group = None;
        asks
    }

    /// Whether the skin in use is still the one the box was opened for.
    fn still_for(&self, app: &AppState, for_skin: Option<&(String, String)>) -> bool {
        for_skin.is_some_and(|(id, _)| app.skins.find(&app.settings.skin).is_some_and(|e| e.id == *id))
    }

    /// Esc in a slider's number box: the skin's text as it was when the box opened (`before`),
    /// exactly as written, and no undo step left for what was typed.
    fn put_text_back(&mut self, app: &mut AppState, before: Option<String>) -> Vec<Ask> {
        let Some(before) = before else { return Vec::new() };
        let Some(id) = self.editable(app) else { return Vec::new() };
        if app.skins.find(&id).is_none_or(|e| e.text == before) {
            return Vec::new();
        }
        let asks = self.change_text(app, |_| before.clone());
        if self.undo.last().is_some_and(|s| s.0 == id && s.1 == before) {
            self.undo.pop();
        }
        asks
    }

    fn finish_editing(&mut self, what: Editing, typed: String, keep: bool, before_text: Option<String>, app: &mut AppState) -> Vec<Ask> {
        match what {
            Editing::Hex(i, before) => {
                let typed = if keep { typed_value(COLOURS[i].0, &typed) } else { None };
                self.set_colour(app, i, typed, &before)
            }
            Editing::LogoText(before) => {
                // Empty means the standard word (GLINT); any other word goes in the box.
                let value = if keep { skin::logo_word(&typed) } else { before };
                if value != app.theme.logo_text {
                    return self.change(app, &[("logo-text", value)]);
                }
                Vec::new()
            }
            Editing::Search(before) => {
                if !keep {
                    self.search_colours(before);
                }
                Vec::new()
            }
            Editing::Number(_, _) if !keep => self.put_text_back(app, before_text),
            Editing::Number(s, before) => self.slide_to(s, typed_number(s, &typed).unwrap_or(before), app),
            Editing::Picture(before) => match typed_picture(&typed).filter(|_| keep) {
                Some(name) if name != before => self.set_picture(app, &name),
                _ => Vec::new(),
            },
            Editing::Rename(id) => {
                let name = typed.trim().to_string();
                if !keep || name.is_empty() || app.skins.find(&id).is_none_or(|e| e.name == name) {
                    return Vec::new();
                }
                // (Its own name in other capitals - in any letters, "élan" to "Élan" - is fine.)
                if !app.skins.may_rename(&id, &name) {
                    self.say(format!("There's a skin called \u{201c}{name}\u{201d} already."), true);
                    return Vec::new();
                }
                if let Some(new_id) = app.skins.rename(&id, &name) {
                    self.save_due.retain(|d| *d != id);
                    if new_id != id {
                        self.forget_steps(&new_id);
                    }
                    // The steps go with the skin, under its new name: undoing a change made before
                    // the rename mustn't bring the old name back into the text (its file and id stay
                    // the new ones).
                    for step in self.undo.iter_mut().chain(self.redo.iter_mut()).filter(|s| s.0 == id) {
                        step.0 = new_id.clone();
                        step.1 = skin::with_value(&step.1, "name", &name);
                    }
                    // The skin in use, renamed "X (Light)", turned light: "Light or dark" follows (as
                    // when it's picked), or the next start would open X's dark side.
                    if app.settings.skin == id {
                        app.pick_skin(&new_id);
                    } else {
                        app.apply_skin();
                    }
                    self.say(format!("Renamed to {name}."), false);
                }
                self.layout_dirty = true;
                vec![Ask::RedrawMain]
            }
        }
    }

    /// One of the buttons under the list.
    fn press(&mut self, b: Btn, app: &mut AppState) -> Vec<Ask> {
        self.end_editing(app, true);
        let current = app.skins.find(&app.settings.skin).cloned();
        match b {
            Btn::Undo => self.undo_step(app, false),
            Btn::Redo => self.undo_step(app, true),
            Btn::New => {
                // A new skin starts as AMD Radeon, light or dark as Glint is now.
                let base = if app.dark { skin::DEFAULT } else { skin::DEFAULT_LIGHT };
                let Some(from) = app.skins.find(base).cloned() else { return Vec::new() };
                let name = app.skins.free_name("My skin");
                let id = app.skins.add(&name, &skin::copy_text(&from.text, &name, &from.name));
                self.say(format!("Made {name} from {}.", from.name), false);
                self.after_new(app, &id)
            }
            Btn::Duplicate => {
                let Some(from) = current else { return Vec::new() };
                let name = app.skins.free_name(&format!("{} copy", from.name));
                let id = app.skins.add(&name, &skin::copy_text(&from.text, &name, &from.name));
                self.say(format!("Made {name}, a copy of {}.", from.name), false);
                self.after_new(app, &id)
            }
            Btn::Rename => {
                let Some(e) = current.filter(|e| !e.built_in()) else { return Vec::new() };
                if let Some(item) = self.items.iter().find(|i| i.id == e.id) {
                    let rect = item.rect;
                    self.list.reveal(rect.y, rect.bottom(), 4.0, self.now());
                }
                self.start_editing(Editing::Rename(e.id.clone()), &e.name, app);
                Vec::new()
            }
            Btn::Delete => {
                let Some(e) = current.filter(|e| !e.built_in()) else { return Vec::new() };
                self.end_editing(app, true);
                self.asking = Some((self.next_question, e.id));
                self.next_question += 1;
                self.invalidate();
                Vec::new()
            }
            Btn::Import => vec![Ask::Job(Job::Import)],
            Btn::Export => match current {
                Some(e) => {
                    self.dialog_for = Some(e.id.clone());
                    // A skin with a wallpaper goes out as a zip with its picture.
                    let ext = if skin::has_wallpaper(&e.text) { "zip" } else { skin::EXTENSION };
                    vec![Ask::Job(Job::Export(format!("{}.{ext}", skin::file_stem(&e.name))))]
                }
                None => Vec::new(),
            },
            Btn::Folder => {
                // Made first if it isn't there yet, away from this thread (it may be on a slow drive).
                let (hwnd, serial, dir) = (self.hwnd.0 as isize, self.serial, skin::folder());
                shell::background("skins: open folder", move || {
                    let _ = std::fs::create_dir_all(&dir);
                    post(hwnd, serial, Bg::Open(dir));
                });
                Vec::new()
            }
            Btn::Skin => self.skins_menu(app),
            Btn::More => self.more_menu(app),
        }
    }

    /// One column: the skin drop-down's list - the built-in skins, a line, then the person's own -
    /// under it.
    fn skins_menu(&mut self, app: &AppState) -> Vec<Ask> {
        let (mut items, mut ids) = (Vec::new(), Vec::new());
        for item in &self.items {
            if item.id.starts_with(skin::FILE_PREFIX) && !ids.iter().any(|id: &String| id.starts_with(skin::FILE_PREFIX)) && !ids.is_empty() {
                items.push(MenuItem::separator());
            }
            ids.push(item.id.clone());
            let on = item.id.eq_ignore_ascii_case(&app.settings.skin);
            let mut entry = MenuItem::new(ids.len() as u32, &item.name).checked(on);
            entry.bold = on;
            items.push(entry);
        }
        let (at, b) = (self.skin_box(), self.border());
        self.menu_for = None;
        self.pop = Some(Pop::Skins(ids));
        vec![Ask::Job(Job::Menu(Menu { tag: 1, at: Point::new(at.x + b, at.bottom() + 2.0 + b), items, right_aligned: false }))]
    }

    /// One column: the menu of the rest (New to Open skins folder), under its button.
    fn more_menu(&mut self, app: &AppState) -> Vec<Ask> {
        let mut items = Vec::new();
        for (n, (btn, label)) in MORE.iter().enumerate() {
            if *btn == Btn::Import {
                items.push(MenuItem::separator());
            }
            items.push(MenuItem::new(n as u32 + 1, label).disabled(!self.button_on(*btn, app)));
        }
        let (at, b) = (self.icon_rect(Btn::More), self.border());
        self.menu_for = None;
        self.pop = Some(Pop::More);
        vec![Ask::Job(Job::Menu(Menu { tag: 1, at: Point::new(at.right() + b, at.bottom() + 4.0 + b), items, right_aligned: true }))]
    }

    /// A pick in the skins' drop-down or the menu of the rest (`id` 0: none).
    fn popped(&mut self, pop: Pop, id: u32, app: &mut AppState) -> Vec<Ask> {
        let Some(n) = id.checked_sub(1).map(|n| n as usize) else { return Vec::new() };
        let asks = match pop {
            Pop::Skins(ids) => match ids.get(n) {
                Some(skin_id) => self.pick(app, skin_id),
                None => Vec::new(),
            },
            Pop::More => match MORE.get(n) {
                Some((b, _)) if self.button_on(*b, app) => self.press(*b, app),
                _ => Vec::new(),
            },
        };
        self.invalidate();
        asks
    }

    /// Folds a section (one column), or opens it again; kept while Glint runs.
    fn toggle_fold(&mut self, title: &'static str) {
        match self.folded.iter().position(|t| *t == title) {
            Some(i) => {
                self.folded.remove(i);
            }
            None => self.folded.push(title),
        }
        let kept = self.folded.clone();
        FOLDED.with(|f| *f.borrow_mut() = Some(kept));
        self.layout_dirty = true;
        self.invalidate();
    }

    /// A skin was just made: use it, and show it in the list.
    fn after_new(&mut self, app: &mut AppState, id: &str) -> Vec<Ask> {
        self.forget_steps(id);
        app.pick_skin(id);
        self.layout(app);
        if let Some(item) = self.items.iter().find(|i| i.id == id) {
            let rect = item.rect;
            self.list.reveal(rect.y, rect.bottom(), 4.0, self.now());
        }
        self.focus = Spot::List;
        self.invalidate();
        vec![Ask::RedrawMain]
    }

    /// The question about deleting was answered (or not yet).
    fn answer(&mut self, a: Answer, app: &mut AppState) -> Vec<Ask> {
        match a {
            Answer::Nothing => Vec::new(),
            Answer::Redraw => {
                self.invalidate();
                Vec::new()
            }
            Answer::No => {
                self.asking = None;
                self.invalidate();
                Vec::new()
            }
            Answer::Yes => {
                let Some((_, id)) = self.asking.take() else { return Vec::new() };
                let name = app.skins.find(&id).map(|e| e.name.clone()).unwrap_or_default();
                let dark = app.dark;
                self.save_due.retain(|d| *d != id);
                self.undo.retain(|s| s.0 != id);
                self.redo.retain(|s| s.0 != id);
                let recycled = app.skins.delete(&id);
                // Glint goes back to AMD Radeon (light or dark as it was).
                if app.settings.skin == id {
                    app.pick_skin(if dark { skin::DEFAULT } else { skin::DEFAULT_LIGHT });
                }
                self.say(if recycled { format!("Moved {name} to the Recycle Bin.") } else { format!("Deleted {name}.") }, false);
                // The file worker's word on it comes next and takes over (where the Recycle Bin is
                // switched off for the drive, Windows asks, then deletes it for good).
                self.status_mine = false;
                self.layout_dirty = true;
                self.focus = Spot::List;
                self.invalidate();
                vec![Ask::RedrawMain]
            }
        }
    }

    /// Takes in what background work found.
    fn take_results(&mut self, app: &mut AppState) -> Vec<Ask> {
        let mine = self.serial;
        let results: Vec<Bg> = std::mem::take(&mut *RESULTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
            .into_iter()
            .filter(|(serial, _)| *serial == mine)
            .map(|(_, b)| b)
            .collect();
        let mut asks = Vec::new();
        for bg in results {
            match bg {
                Bg::Imported(Ok(import)) => {
                    let (id, said, problem) = app.skins.take_import(import);
                    self.say(said, problem);
                    asks.extend(self.after_new(app, &id));
                }
                Bg::Imported(Err(why)) => self.say(why, true),
                Bg::Pictures { dir, current, list, current_pic, too_big } => {
                    // (Read for a folder or a wallpaper left behind meanwhile: the next is coming.)
                    if self.thumbs_for.as_ref().is_some_and(|(d, c)| *d == dir && *c == current) {
                        self.thumbs = list;
                        self.current_pic = current_pic;
                        if too_big {
                            let name = std::path::Path::new(&current).file_name().map_or_else(|| current.clone(), |n| n.to_string_lossy().into_owned());
                            self.say(format!("\u{201c}{name}\u{201d} is too big for a wallpaper (over 100 megapixels)."), true);
                        }
                    }
                }
                Bg::Open(dir) => {
                    // The folder exists now: watch it (a skin copied in shows at once).
                    app.skins.watch_folder();
                    // In a new tab; the customiser stays open beside it.
                    asks.push(Ask::Command(Command::NewTabAt(dir.display().to_string())));
                }
            }
        }
        self.invalidate();
        asks
    }

    /// The one-line box or the skin's text changed, or a key was pressed in them.
    fn text_event(&mut self, ev: TextEvent, app: &mut AppState) -> Vec<Ask> {
        if ev.id != EDIT_BOX {
            return Vec::new();
        }
        match ev.kind {
            TextEventKind::Changed => {
                // Colours and the logo show as they're typed (a colour once it's whole) - in the
                // skin the box was opened for only.
                let typed = self.edit_box.text();
                // The search finds colours as it's typed, whatever the skin.
                if matches!(self.editing, Some(Editing::Search(_))) {
                    self.search_colours(typed);
                    return Vec::new();
                }
                if !self.still_for(app, self.editing_for.as_ref()) {
                    return Vec::new();
                }
                match self.editing.clone() {
                    Some(Editing::Hex(i, before)) => match typed_value(COLOURS[i].0, &typed) {
                        Some(value) => self.set_colour(app, i, Some(value), &before),
                        None => Vec::new(),
                    },
                    // A slider's number moves it as it's typed (once it's a number).
                    Some(Editing::Number(s, _)) => match typed_number(s, &typed) {
                        Some(v) => self.slide_to(s, v, app),
                        None => Vec::new(),
                    },
                    Some(Editing::LogoText(_)) => {
                        let word = skin::logo_word(&typed);
                        if word != app.theme.logo_text {
                            return self.change(app, &[("logo-text", word)]);
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            TextEventKind::Enter => {
                self.cues = true;
                self.end_editing(app, true)
            }
            TextEventKind::Escape => {
                self.cues = true;
                self.end_editing(app, false)
            }
            TextEventKind::LostFocus => self.end_editing(app, true),
            TextEventKind::Down | TextEventKind::Tab => Vec::new(),
        }
    }

    /// What a job brought back (a drop-down's pick, a colour, a file).
    pub fn job_done(&mut self, done: JobDone, app: &mut AppState) -> Vec<Ask> {
        match done {
            JobDone::Nothing => Vec::new(),
            JobDone::Picked(id) => {
                if let Some(pop) = self.pop.take() {
                    return self.popped(pop, id, app);
                }
                let Some(ctl) = self.menu_for.take() else { return Vec::new() };
                match id.checked_sub(1) {
                    Some(n) => self.picked(ctl, n as usize, app),
                    None => Vec::new(),
                }
            }
            JobDone::Colour(i, colorref) => {
                // Another skin was picked while the picker was open: the colour isn't put in it.
                if self.dialog_for.take().is_some_and(|id| !id.eq_ignore_ascii_case(&app.settings.skin)) {
                    return Vec::new();
                }
                // Windows' picker has no see-through: the colour keeps the strength it had (a clear
                // one becomes solid). On a gradient it changes the first colour.
                let old = self.colour(app, i).paint;
                let alpha = if old.first().a > 0.0 { old.first().a } else { 1.0 };
                let new = with_first(&old, Color::from_colorref(colorref).with_alpha(alpha));
                if new == old {
                    return Vec::new();
                }
                self.change(app, &[(COLOURS[i].0, paint_text(&new))])
            }
            JobDone::Import(path) => {
                let (hwnd, serial) = (self.hwnd.0 as isize, self.serial);
                shell::background("skins: import", move || {
                    // (Its pictures and its other half are copied into the skins folder here too.)
                    post(hwnd, serial, Bg::Imported(skin::prepare_import(&path, &skin::folder())));
                });
                Vec::new()
            }
            JobDone::Picture(path) => {
                // Another skin was picked while the dialog was open: the picture isn't put in it.
                if self.dialog_for.take().is_some_and(|id| !id.eq_ignore_ascii_case(&app.settings.skin)) {
                    return Vec::new();
                }
                let name = written_picture(&path, Some(&self.picture_dir(app)), &skin::folder());
                self.set_picture(app, &name)
            }
            JobDone::Export(path) => {
                // The skin the dialog was opened for, even if another was picked meanwhile.
                let id = self.dialog_for.take().unwrap_or_else(|| app.settings.skin.clone());
                self.say(format!("Exporting to {}\u{2026}", path.display()), false);
                app.skins.export(&id, path);
                self.invalidate();
                Vec::new()
            }
        }
    }

    // ------------------------------------------------------------ messages

    /// Handles one message of this window, with the app's state. None: Windows' default.
    pub fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM, app: &mut AppState) -> (Option<LRESULT>, Vec<Ask>) {
        let mut asks = Vec::new();
        let result = match msg {
            WM_PAINT => {
                // SAFETY: marks our own window as painted.
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                self.paint(app);
                Some(LRESULT(0))
            }
            WM_SIZE if wparam.0 == SIZE_MINIMIZED as usize => Some(LRESULT(0)),
            // Resized from an edge: docked again (its new width stays).
            WM_EXITSIZEMOVE => {
                self.follow_owner();
                Some(LRESULT(0))
            }
            WM_SIZE => {
                let (w, h) = (lo_word(lparam.0 as usize), hi_word(lparam.0 as usize));
                self.renderer.resize(w, h);
                if w > 0 && h > 0 {
                    self.size = self.inner_size(w, h);
                    self.layout_dirty = true;
                    self.edit_at = None;
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                self.dpi = lo_word(wparam.0).max(96);
                self.renderer.set_dpi(self.dpi);
                let mut rc = RECT::default();
                // SAFETY: our own window and a valid out-struct.
                if unsafe { GetClientRect(self.hwnd, &mut rc) }.is_ok() && rc.right > rc.left {
                    self.size = self.inner_size((rc.right - rc.left) as u32, (rc.bottom - rc.top) as u32);
                }
                self.layout_dirty = true;
                self.edit_at = None;
                        self.invalidate();
                // SAFETY: lparam points to the suggested window rectangle.
                let suggested = unsafe { *(lparam.0 as *const RECT) };
                asks.push(Ask::Job(Job::Place(suggested)));
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_move(p, app));
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.drag.is_none() && self.hover.take().is_some() {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_down(p, msg == WM_LBUTTONDBLCLK, app));
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let p = self.client_point(lparam);
                asks.extend(self.mouse_up(p, app));
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                self.pressed = None;
                self.drag = None;
                self.end_sliding();
                self.holding.release(self.hwnd);
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                // The wheel's point is on the screen.
                let mut pt = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                // SAFETY: converts in place for our own window.
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut pt);
                }
                let s = self.scale();
                self.wheel(Point::new(pt.x as f32 / s - self.border(), pt.y as f32 / s - self.border()), hi_word(wparam.0) as u16 as i16 as f32 / 120.0);
                Some(LRESULT(0))
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => self.key(wparam.0 as u16, lparam.0 & 0x4000_0000 != 0, app, &mut asks).then_some(LRESULT(0)),
            WM_CHAR => {
                if let Some(spot) = self.act_on_char.take() {
                    asks.extend(self.activate_spot(spot, app));
                }
                Some(LRESULT(0))
            }
            WM_SYSCHAR if wparam.0 != ' ' as usize => Some(LRESULT(0)),
            WM_ACTIVATE => {
                self.active = lo_word(wparam.0) != WA_INACTIVE;
                if self.active {
                    self.thumbs_for = None;
                }
                self.invalidate();
                None
            }
            WM_CLOSE => {
                asks.push(Ask::Job(Job::Close));
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == SAVE_TIMER => {
                self.save_now(app);
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == RETRY_TIMER => {
                // SAFETY: stops our own one-off timer.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), RETRY_TIMER);
                }
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == kit::REPEAT_TIMER => {
                let now = self.now();
                let moved = if self.holding.bar == 0 {
                    let place = self.bar.place(self.list_rect());
                    self.holding.fire(self.hwnd, place, &mut self.list, now)
                } else {
                    let place = self.bar.place(self.page_rect());
                    self.holding.fire(self.hwnd, place, &mut self.page, now)
                };
                if moved {
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_APP_TEXT => {
                let ev = TextEvent { id: wparam.0 as u16, kind: TextEventKind::from_number(lparam.0) };
                asks.extend(self.text_event(ev, app));
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let (id, code) = (lo_word(wparam.0) as u16, hi_word(wparam.0));
                // EN_CHANGE (0x0300): typed (not set by us).
                if code == 0x0300 && id == EDIT_BOX && !textbox::setting_text() {
                    asks.extend(self.text_event(TextEvent { id, kind: TextEventKind::Changed }, app));
                }
                Some(LRESULT(0))
            }
            WM_APP_BG => {
                asks.extend(self.take_results(app));
                Some(LRESULT(0))
            }
            WM_APP_DOCK => {
                asks.push(Ask::Job(Job::Dock((self.m().width * self.scale()).round() as i32)));
                Some(LRESULT(0))
            }
            _ => None,
        };
        (result, asks)
    }
}

impl Drop for SkinWindow {
    fn drop(&mut self) {
        let brush = EDIT_COLORS.with(|c| c.replace((0, 0, 0))).2;
        // SAFETY: the brush and the font are ours, and the text boxes that used them are gone with
        // the window.
        unsafe {
            if brush != 0 {
                let _ = DeleteObject(HGDIOBJ(brush as *mut _));
            }
        }
    }
}

/// A small icon, drawn in `ink` in the middle of `at` (about 12 across).
pub(crate) fn glyph(r: &mut Renderer, g: Glyph, at: Rect, ink: Color) {
    let c = Point::new(r.snap(at.x + at.w / 2.0), r.snap(at.y + at.h / 2.0));
    let p = |x: f32, y: f32| Point::new(c.x + x, c.y + y);
    let w = 1.2;
    match g {
        Glyph::Undo | Glyph::Redo => {
            // Half a circle over the top, its arrow's head pointing down at its end (as the signs
            // for undo and redo).
            let s = if g == Glyph::Undo { 1.0 } else { -1.0 };
            let (rad, cy) = (4.5, 1.5);
            let arc: Vec<Point> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI * i as f32 / 12.0;
                    p(s * rad * a.cos(), cy - rad * a.sin())
                })
                .collect();
            r.polyline(&arc, w, ink);
            let e = p(-s * rad, cy);
            r.fill_polygon(&[Point::new(e.x - 3.0, e.y - 1.0), Point::new(e.x + 3.0, e.y - 1.0), Point::new(e.x, e.y + 3.0)], ink);
        }
        Glyph::More => {
            for x in [-5.0, 0.0, 5.0] {
                r.fill_ellipse(p(x, 0.0), 1.4, 1.4, ink);
            }
        }
        Glyph::New => {
            r.line(p(-5.0, 0.0), p(5.0, 0.0), w, ink);
            r.line(p(0.0, -5.0), p(0.0, 5.0), w, ink);
        }
        Glyph::Duplicate => {
            r.stroke_rect(Rect::new(c.x - 5.5, c.y - 5.5, 8.0, 8.0), 1.0, ink);
            r.stroke_rect(Rect::new(c.x - 2.5, c.y - 2.5, 8.0, 8.0), 1.0, ink);
        }
        Glyph::Rename => {
            // A pencil, its point down on the left.
            r.stroke_polygon(&[p(-5.5, 5.5), p(-4.5, 2.0), p(3.0, -5.5), p(5.5, -3.0), p(-2.0, 4.5)], w, ink);
            r.line(p(1.5, -4.0), p(4.0, -1.5), w, ink);
        }
        Glyph::Delete => {
            // A bin: its lid with a handle, and its body.
            r.line(p(-5.5, -3.5), p(5.5, -3.5), w, ink);
            r.polyline(&[p(-2.0, -3.5), p(-2.0, -5.5), p(2.0, -5.5), p(2.0, -3.5)], w, ink);
            r.polyline(&[p(-4.0, -3.5), p(-3.2, 5.5), p(3.2, 5.5), p(4.0, -3.5)], w, ink);
            r.line(p(-1.2, -1.0), p(-1.2, 3.5), 1.0, ink);
            r.line(p(1.2, -1.0), p(1.2, 3.5), 1.0, ink);
        }
        Glyph::Import | Glyph::Export => {
            // An arrow down onto a line (import), or up to one (export).
            let s = if g == Glyph::Import { 1.0 } else { -1.0 };
            r.line(p(0.0, -4.5 * s), p(0.0, 3.0 * s), w, ink);
            r.polyline(&[p(-3.5, 0.0), p(0.0, 3.5 * s), p(3.5, 0.0)], w, ink);
            r.line(p(-5.0, 5.5 * s), p(5.0, 5.5 * s), w, ink);
        }
        Glyph::Folder => {
            r.stroke_polygon(&[p(-6.0, -4.5), p(-2.0, -4.5), p(-0.5, -3.0), p(6.0, -3.0), p(6.0, 4.5), p(-6.0, 4.5)], w, ink);
            r.line(p(-6.0, -1.0), p(6.0, -1.0), 1.0, ink);
        }
        Glyph::Search => {
            r.stroke_ellipse(p(-1.0, -1.0), 3.8, 3.8, w, ink);
            r.line(p(1.8, 1.8), p(4.8, 4.8), 1.4, ink);
        }
        Glyph::Clear => {
            r.line(p(-3.5, -3.5), p(3.5, 3.5), 1.1, ink);
            r.line(p(-3.5, 3.5), p(3.5, -3.5), 1.1, ink);
        }
        Glyph::Detach | Glyph::Dock => {
            // An arrow to a corner under a line: up and left (DETACH), or down and right (DOCK).
            let s = if g == Glyph::Detach { 1.0 } else { -1.0 };
            r.line(p(-5.5 * s, -5.5 * s), p(5.5 * s, -5.5 * s), w, ink);
            r.line(p(4.5 * s, 4.5 * s), p(-3.0 * s, -3.0 * s), w, ink);
            r.polyline(&[p(-3.0 * s, 1.5 * s), p(-3.0 * s, -3.0 * s), p(1.5 * s, -3.0 * s)], w, ink);
        }
        Glyph::Help => r.text("?", at, &TextStyle::new(ink).size(13.0).bold().align(Align::Center)),
        Glyph::Menu => {
            for y in [-4.0, 0.0, 4.0] {
                r.line(p(-5.5, y), p(5.5, y), w, ink);
            }
        }
    }
}

/// A small icon button: its icon, on the hover colour while the mouse is over it (greyed when it
/// can't be used; a colour's x in the muted colour until the mouse is over it).
pub(crate) fn icon_button(r: &mut Renderer, theme: &Theme, at: Rect, g: Glyph, look: Look) {
    if look.enabled && (look.hover || look.pressed) {
        r.fill_round_rect(at, 4.0, if look.pressed { theme.button_pressed } else { theme.chip_hover });
    }
    let ink = match () {
        _ if !look.enabled => theme.muted.with_alpha(theme.muted.a * 0.45),
        _ if g == Glyph::Clear && !look.hover => theme.muted,
        _ => theme.text,
    };
    glyph(r, g, at, ink);
    if look.focused {
        kit::focus_ring(r, theme, at, 0.0);
    }
}

/// An icon on a button's face, in the skin's shape (the sort order's editor's ⋯, ? and ☰), leaning
/// only on the sides with another button `beside` it.
pub(crate) fn icon_face(r: &mut Renderer, theme: &Theme, at: Rect, g: Glyph, look: Look, beside: crate::pathbar::Beside) {
    kit::button_beside(r, theme, at, "", look, beside);
    let ink = if look.enabled { theme.text } else { theme.muted.with_alpha(theme.muted.a * 0.45) };
    glyph(r, g, crate::pathbar::face(at, theme.lean(ksize::BUTTON_LEAN), beside), ink);
}

/// A section's heading that folds it (one column): a small triangle (pointing right while it's
/// folded, down while it's open), then its title.
fn fold_heading(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, open: bool, look: Look) {
    let ink = if look.hover { theme.text } else { theme.muted };
    let c = Point::new(rect.x + 4.0, rect.y + (rect.h / 2.0).round());
    let tri = if open {
        [Point::new(c.x - 3.5, c.y - 2.0), Point::new(c.x + 3.5, c.y - 2.0), Point::new(c.x, c.y + 2.5)]
    } else {
        [Point::new(c.x - 2.0, c.y - 3.5), Point::new(c.x - 2.0, c.y + 3.5), Point::new(c.x + 2.5, c.y)]
    };
    r.fill_polygon(&tri, ink);
    kit::heading(r, theme, Rect::new(rect.x + 14.0, rect.y, (rect.w - 14.0).max(0.0), rect.h), text);
    if look.focused {
        kit::focus_ring(r, theme, rect.inset(-2.0, -1.0), 0.0);
    }
}

/// The label of a style's choice in a look.
fn label_of<T: PartialEq + Copy>(choices: &[(T, &str, &str)], now: T) -> String {
    choices.iter().find(|(s, _, _)| *s == now).map_or("", |(_, _, l)| l).to_string()
}

/// The change choice `n` of a style's drop-down makes: `key` gets its word.
fn word_at<T>(choices: &[(T, &'static str, &str)], n: usize, key: &'static str) -> Vec<(&'static str, String)> {
    choices.get(n).map(|(_, word, _)| vec![(key, word.to_string())]).unwrap_or_default()
}

/// A colour's swatch: the colour or gradient over a checked pattern (so a see-through one shows
/// as such), in a thin frame.
fn swatch(r: &mut Renderer, theme: &Theme, rect: Rect, p: &Paint, look: Look) {
    let see_through = p.gradient().map_or(p.base().a < 0.999, |g| g.stops().iter().any(|s| s.color.a < 0.999));
    if see_through {
        let cell = rect.w / 4.0;
        for row in 0..4 {
            for col in 0..4 {
                let light = (row + col) % 2 == 0;
                let tone = if light { Color::hex(0xffffff) } else { Color::hex(0xc8c8c8) };
                r.fill_rect(Rect::new(rect.x + col as f32 * cell, rect.y + row as f32 * cell, cell, cell), tone);
            }
        }
    }
    r.fill_paint(rect, 0.0, p);
    r.stroke_rect(rect, 1.0, if look.hover { theme.text } else { theme.line });
    if look.focused {
        kit::focus_ring_round(r, theme, rect, 0.0, 0.0);
    }
}

/// Whether a colour called `title` is found by the search `typed` (in any capitals; spaces round
/// it don't count, and nothing finds every colour).
fn found_by(title: &str, typed: &str) -> bool {
    title.to_lowercase().contains(&typed.trim().to_lowercase())
}

fn slant_label(v: f32) -> String {
    match v {
        x if x < 0.5 => "0 (upright)".into(),
        x if (x - 8.0).abs() < 0.01 => "8 (AMD Radeon)".into(),
        x => number(x),
    }
}

fn row_label(v: f32) -> String {
    if (v - size::ROW_H).abs() < 0.01 {
        format!("{} (standard)", number(v))
    } else {
        number(v)
    }
}

fn width_label(v: f32) -> String {
    if v < 0.5 {
        "0 (none)".into()
    } else {
        number(v)
    }
}

fn size_label(pt: f32) -> String {
    if (pt - 9.0).abs() < 0.01 {
        "9 pt (standard)".into()
    } else {
        format!("{} pt", number(pt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The customiser's class asks Windows for double-clicks (a double-click in the text picks a
    /// word): without CS_DBLCLKS the second click is just another click. Registering twice is fine.
    #[test]
    fn the_skin_window_hears_double_clicks() {
        register_class();
        register_class();
        let mut info = WNDCLASSEXW { cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32, ..Default::default() };
        // SAFETY: a class lookup by name into a struct we own.
        let found = unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            windows::Win32::UI::WindowsAndMessaging::GetClassInfoExW(Some(instance.into()), w!("GlintSkinWindow"), &mut info).is_ok()
        };
        assert!(found);
        assert!(info.style.contains(CS_DBLCLKS), "{:?}", info.style);
    }

    #[test]
    fn the_search_finds_colours_by_name() {
        let found: Vec<&str> = COLOURS.iter().map(|(_, t)| *t).filter(|t| found_by(t, "scroll")).collect();
        assert_eq!(found, ["Scroll bars", "Scroll bars, under the mouse", "Scroll bar track", "Scroll bar edges"]);
        assert!(found_by("Accent", " ACC "));
        assert!(COLOURS.iter().all(|(_, t)| found_by(t, "")));
    }

    #[test]
    fn labels_and_numbers() {
        assert_eq!(number(8.0), "8");
        assert_eq!(number(10.5), "10.5");
        assert_eq!(slant_label(0.0), "0 (upright)");
        assert_eq!(slant_label(8.0), "8 (AMD Radeon)");
        assert_eq!(row_label(28.0), "28 (standard)");
        assert_eq!(size_label(9.0), "9 pt (standard)");
        assert_eq!(size_label(10.5), "10.5 pt");
        assert_eq!((width_label(0.0), width_label(2.0)), ("0 (none)".to_string(), "2".to_string()));
    }

    #[test]
    fn every_skin_colour_is_offered_once() {
        for (key, _) in skin::COLOURS {
            assert_eq!(COLOURS.iter().filter(|(k, _)| k == key).count(), 1, "{key}");
        }
        assert_eq!(COLOURS.len(), skin::COLOURS.len());
        // Every colour and every part shows once, in a card of CARDS, in its order.
        let cards = cards(&[]);
        assert_eq!(cards.iter().map(|(t, _)| *t).collect::<Vec<_>>(), CARDS.to_vec());
        for i in 0..COLOURS.len() {
            assert_eq!(cards.iter().flat_map(|(_, r)| r).filter(|(c, ..)| *c == Ctl::Colour(i)).count(), 1, "{}", COLOURS[i].0);
        }
        for p in crate::parts::ALL {
            assert_eq!(cards.iter().flat_map(|(_, r)| r).filter(|(c, ..)| *c == Ctl::Part(p)).count(), 1, "{p:?}");
        }
        let rows: Vec<Ctl> = cards.iter().flat_map(|(_, r)| r.iter().map(|(c, ..)| *c)).collect();
        assert!(rows.iter().enumerate().all(|(n, c)| !rows[..n].contains(c)), "no row twice");
        assert_eq!(rows.len(), 161, "85 settings and 76 colours");
        // Each shows what the skin says, read back from its own line.
        let text: String = COLOURS.iter().enumerate().map(|(i, (k, _))| format!("{k}: #{:02x}0203\n", i + 1)).collect();
        for (i, c) in shown_colours(&text).iter().enumerate() {
            assert_eq!(c.text, format!("#{:02x}0203", i + 1), "{}", COLOURS[i].0);
            assert!(c.follows.is_none() && c.hint.is_empty());
        }
        let at = |key: &str| COLOURS.iter().position(|(k, _)| *k == key).unwrap();
        let shown = shown_colours("dark: true\naccent: #102030\nselection: accent 20%\ntab: none\n");
        assert_eq!(shown[at("selection")].text, "#10203033");
        assert_eq!(shown[at("selection")].follows.as_deref(), Some("accent 20%"));
        assert_eq!(shown[at("selection")].hint, "follows Accent at 20%");
        assert_eq!(shown[at("title-bar")].hint, "follows Background, behind the panels");
        assert_eq!(shown[at("tab")].text, "none");
    }

    #[test]
    fn gradients_are_shown_and_typed_as_text() {
        let lib = skin::Library::new();
        let chrome_text = &lib.find("chrome").unwrap().text;
        let chrome = skin::built_in_theme("chrome");
        let shown = shown_colours(chrome_text);
        let at = |key: &str| COLOURS.iter().position(|(k, _)| *k == key).unwrap();
        assert_eq!(shown[at("title-bar")].text, "#fdfdfd, #c3cddb 45%, #8d9db8 55%, #dfe6f0");
        assert_eq!(shown[at("input")].text, "#e9eef6, #ffffff");
        assert_eq!(shown[at("title-bar")].paint, chrome.titlebar_paint);
        // A colour that follows another shows that one's colour, and says so.
        assert_eq!(shown[at("tab-line")].paint, chrome.accent_paint);
        assert_eq!(shown[at("tab-line")].hint, "follows Accent");
        // What the box shows, typed back, is the same gradient; a flat colour is a colour.
        let typed = typed_paint("title-bar", "#fdfdfd, #c3cddb 45%, #8d9db8 55%, #dfe6f0").unwrap();
        assert_eq!(typed, chrome.titlebar_paint);
        assert_eq!(typed_paint("toolbar", " #123456 "), Some(Color::hex(0x123456).into()));
        assert_eq!(typed_value("toolbar", " #ABCDEF; "), Some("#abcdef".to_string()));
        assert_eq!(typed_value("tab-line", "Accent 50%"), Some("accent 50%".to_string()));
        assert_eq!(typed_value("tab", "none"), Some("none".to_string()));
        // Half typed, or not colours: nothing yet.
        for bad in ["#fdfdfd, #c3", "#fdfdfd,", "red, blue", "#fdfdfd, #c3cddb 4x%", "", "#12", "acc", "tab-line", "#fff // x"] {
            assert_eq!(typed_value("tab-line", bad), None, "{bad:?}");
        }
        // Windows' colour picker on a gradient changes its first colour only.
        let picked = with_first(&chrome.titlebar_paint, Color::hex(0xff0000));
        assert_eq!(paint_text(&picked), "#ff0000, #c3cddb 45%, #8d9db8 55%, #dfe6f0");
        assert_eq!(with_first(&Color::hex(0x010203).into(), Color::hex(0xff0000)), Color::hex(0xff0000).into());
    }

    #[test]
    fn the_style_choices_write_what_the_skin_reads() {
        for (style, word, _) in BUTTON_STYLES {
            assert_eq!(skin::parse(&format!("buttons: {word}"), "x").theme.buttons, style);
        }
        for (style, word, _) in CAPTION_STYLES {
            assert_eq!(skin::parse(&format!("caption-buttons: {word}"), "x").theme.caption_buttons, style);
        }
        for (style, word, _) in TAB_STYLES {
            assert_eq!(skin::parse(&format!("tabs: {word}"), "x").theme.tabs, style);
        }
        for (style, word, _) in SIDEBAR_STYLES {
            assert_eq!(skin::parse(&format!("sidebar-style: {word}"), "x").theme.sidebar_style, style);
        }
        assert_eq!(word_at(&TAB_STYLES, 1, "tabs"), [("tabs", "pills".to_string())]);
        assert!(word_at(&TAB_STYLES, 7, "tabs").is_empty());
        let chrome = skin::built_in_theme("chrome");
        assert_eq!(label_of(&BUTTON_STYLES, chrome.buttons), "Glossy");
        assert_eq!(label_of(&SIDEBAR_STYLES, skin::built_in_theme(skin::DEFAULT).sidebar_style), "Plain");
    }

    #[test]
    fn the_choices_write_what_the_skin_reads() {
        for (shape, word, _) in SHAPES {
            assert_eq!(skin::parse(&format!("shape: {word}"), "x").theme.shape, shape);
        }
        for v in FONT_SIZES {
            let t = skin::parse(&format!("font-size: {}pt", number(v)), "x");
            assert!(t.notes.is_empty(), "{v}: {:?}", t.notes);
            assert!((t.theme.font_size / PT - v).abs() < 0.01);
        }
        for v in ROW_HEIGHTS {
            assert_eq!(skin::parse(&format!("row-height: {}", number(v)), "x").theme.row_h, v);
        }
    }


    /// The skin's text after slider `s` is moved to `v` (as the customiser does it: read the skin,
    /// write what the slider says into its text).
    fn slid(text: &str, s: Slide, v: f32) -> String {
        let theme = skin::parse(text, "x").theme;
        slide_changes(s, v, &theme).iter().fold(text.to_string(), |t, (k, val)| skin::with_value(&t, k, val))
    }

    fn shown(text: &str, s: Slide) -> f32 {
        slide_value(s, &skin::parse(text, "x").theme)
    }

    #[test]
    fn every_slider_value_is_written_and_read_back_the_same() {
        for (s, _, min, max, ..) in SLIDES {
            let mut v = min;
            while v <= max {
                let text = slid("dark: true\n", s, v);
                let p = skin::parse(&text, "x");
                assert!(p.notes.is_empty(), "{s:?} {v}: {:?}\n{text}", p.notes);
                assert_eq!(shown(&text, s), v, "{s:?} at {v}:\n{text}");
                v += 1.0;
            }
        }
    }

    #[test]
    fn sliders_stop_at_their_ends_and_ignore_nonsense() {
        for (s, _, min, max, ..) in SLIDES {
            assert_eq!(shown(&slid("", s, min - 9999.0), s), min, "{s:?}");
            assert_eq!(shown(&slid("", s, max + 9999.0), s), max, "{s:?}");
            for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert!(slide_changes(s, bad, &skin::parse("", "x").theme).is_empty(), "{s:?} {bad}");
            }
            // Between whole steps: the nearest one.
            assert_eq!(shown(&slid("", s, min + 10.4), s), min + 10.0, "{s:?}");
        }
        // With nothing written, each shows the skin's standard: half strong, its usual size, in
        // the middle.
        assert_eq!((shown("", Slide::Opacity), shown("", Slide::Zoom), shown("", Slide::Blur)), (50.0, 0.0, 0.0));
        assert_eq!((shown("", Slide::Across), shown("", Slide::Down)), (50.0, 50.0));
    }

    #[test]
    fn the_track_maps_the_mouse_to_values_and_pulls_to_the_middle() {
        let area = Rect::new(100.0, 0.0, 214.0, 30.0);
        for (s, _, min, max, ..) in SLIDES {
            let mid = (min + max) / 2.0;
            assert_eq!(slide_at(s, area, -1e6, mid), min, "{s:?}");
            assert_eq!(slide_at(s, area, 1e6, mid), max, "{s:?}");
            assert_eq!(slide_at(s, area, 107.0, mid), min, "{s:?}");
            assert_eq!(slide_at(s, area, 307.0, mid), max, "{s:?}");
        }
        // Very near the default: exactly it (a notch); a little further: just the value.
        assert_eq!(slide_at(Slide::Zoom, area, 208.5, 0.0), 0.0);
        assert_eq!(slide_at(Slide::Zoom, area, 212.0, 0.0), 25.0, "5 DIPs away doesn't snap");
        assert_eq!(slide_at(Slide::Opacity, area, 208.5, 50.0), 50.0);
        assert_eq!(slide_at(Slide::Opacity, area, 210.0, 50.0), 52.0, "1.5% away doesn't snap");
        assert_eq!(slide_at(Slide::Opacity, area, 209.0, 40.0), 51.0, "only the default snaps");
        // A slider squashed to nothing doesn't divide by zero.
        assert!(slide_at(Slide::Zoom, Rect::new(0.0, 0.0, 0.0, 0.0), 5.0, 0.0).is_finite());
    }

    #[test]
    fn position_uses_words_when_it_can_and_keeps_the_other_half() {
        assert_eq!(slid("", Slide::Across, 0.0).trim(), "wallpaper-position: left");
        assert_eq!(slid("", Slide::Across, 50.0).trim(), "wallpaper-position: middle");
        // Down said: the second word.
        assert_eq!(slid("wallpaper-position: left\n", Slide::Down, 0.0).trim(), "wallpaper-position: left top");
        assert_eq!(slid("wallpaper-position: right\n", Slide::Down, 50.0).trim(), "wallpaper-position: right middle");
        assert_eq!(slid("wallpaper-position: middle\n", Slide::Down, 100.0).trim(), "wallpaper-position: middle bottom");
        // In between: percentages, the other half kept as it shows.
        assert_eq!(slid("wallpaper-position: left bottom\n", Slide::Across, 30.0).trim(), "wallpaper-position: 30% 100%");
        assert_eq!(slid("wallpaper-position: 30% 70%\n", Slide::Down, 20.0).trim(), "wallpaper-position: 30% 20%");
        // Down not said is the middle, whatever the fit.
        let t = slid("wallpaper-fit: whole\n", Slide::Across, 25.0);
        assert!(t.contains("wallpaper-position: 25% 50%"), "{t}");
        assert_eq!(shown("wallpaper-fit: whole\n", Slide::Down), 50.0);
        assert_eq!(shown("wallpaper-fit: tile\n", Slide::Down), 50.0);
        // Every place across and down reads back where it was put.
        for x in (0..=100).step_by(5) {
            for y in (0..=100).step_by(5) {
                let t = slid(&slid("", Slide::Across, x as f32), Slide::Down, y as f32);
                assert_eq!((shown(&t, Slide::Across), shown(&t, Slide::Down)), (x as f32, y as f32), "{t}");
            }
        }
    }

    #[test]
    fn text_typed_while_a_slider_moves_is_kept() {
        // Typed in the text: a note on the line, and other lines.
        let mut text = "name: Mine\r\nwallpaper-opacity: 10 // my note\r\naccent: #ff0000\r\n".to_string();
        for v in [20.0, 35.0, 90.0] {
            text = slid(&text, Slide::Opacity, v);
            assert!(text.contains(&format!("wallpaper-opacity: {} // my note", number(v))), "{text}");
            // Typing in between: a new line, and the opacity written by hand.
            text.push_str("tab: none\r\n");
            text = text.replace(&format!("wallpaper-opacity: {}", number(v)), "wallpaper-opacity: 55");
            assert_eq!(shown(&text, Slide::Opacity), 55.0, "the slider follows the text");
        }
        assert_eq!(text.matches("wallpaper-opacity").count(), 1, "{text}");
        assert_eq!(text.matches("tab: none").count(), 3);
        assert!(text.contains("accent: #ff0000"));
        // Something the text has wrong: the slider shows the standard, and moving it puts it right.
        let bad = "wallpaper-zoom: lots\r\n";
        assert_eq!(shown(bad, Slide::Zoom), 0.0);
        let fixed = slid(bad, Slide::Zoom, 50.0);
        assert!(skin::parse(&fixed, "x").notes.is_empty(), "{fixed}");
        assert_eq!(shown(&fixed, Slide::Zoom), 50.0);
        // The same setting written twice: the last line counts, and is the one moved.
        let twice = slid("wallpaper-blur: 3\r\nwallpaper-blur: 4\r\n", Slide::Blur, 9.0);
        assert_eq!(shown(&twice, Slide::Blur), 9.0, "{twice}");
        // Closed and opened again: the sliders read the saved text.
        let saved = slid(&slid("", Slide::Fade, 12.0), Slide::Zoom, -250.0);
        assert_eq!((shown(&saved, Slide::Fade), shown(&saved, Slide::Zoom)), (12.0, -250.0));
    }

    #[test]
    fn esc_puts_back_the_text_from_before_the_drag() {
        let mut d = Sliding::new(Slide::Zoom);
        assert_eq!(d.put_back("mine"), None, "nothing changed yet");
        d.changing("mine", "before");
        d.changing("mine", "during");
        assert_eq!(d.put_back("MINE"), Some(("mine", "before")));
        // Another skin in use now (picked from the main window): nothing goes into it.
        assert_eq!(d.put_back("other"), None);
    }

    #[test]
    fn the_wallpaper_is_named_as_glint_finds_it() {
        use std::path::Path;
        let skins = Path::new(r"C:\Users\x\AppData\Roaming\Glint\skins");
        let pack = Path::new(r"C:\Users\x\AppData\Roaming\Glint\skins\beach");
        assert_eq!(written_picture(Path::new(r"C:\Users\x\AppData\Roaming\Glint\skins\beach\sea.jpg"), Some(pack), skins), "sea.jpg");
        // In the skins folder, for a skin in a folder of its own: its full path (a bg.png next to
        // the skin would be found first by its name).
        assert_eq!(written_picture(Path::new(r"C:\Users\x\AppData\Roaming\Glint\skins\sun.png"), Some(pack), skins), r"C:\Users\x\AppData\Roaming\Glint\skins\sun.png");
        // A skin in the skins folder itself: a picture there is next to it.
        assert_eq!(written_picture(Path::new(r"c:\users\X\appdata\roaming\glint\SKINS\sun.png"), Some(skins), skins), "sun.png");
        assert_eq!(written_picture(Path::new(r"c:\users\X\appdata\roaming\glint\SKINS\sun.png"), None, skins), "sun.png");
        assert_eq!(written_picture(Path::new(r"D:\Pictures\cat.jpg"), Some(pack), skins), r"D:\Pictures\cat.jpg");
        assert_eq!(written_picture(Path::new(r"\\server\share\a b.jpg"), None, skins), r"\\server\share\a b.jpg");
        // Typed: quotes and spaces round it go; a line break or a note can't go in a line.
        assert_eq!(typed_picture("  \"beach.jpg\" ").as_deref(), Some("beach.jpg"));
        assert_eq!(typed_picture("").as_deref(), Some(""));
        assert_eq!(typed_picture("a.jpg\r\naccent: #fff"), None);
        assert_eq!(typed_picture("a.jpg // x"), None);
        // What the box shows is the skin's own line.
        assert_eq!(picture_of("wallpaper: 'sea.jpg'\r\n"), "sea.jpg");
        assert_eq!(picture_of("accent: #fff\r\n"), "");
        // A path with spaces and a drive's colon is one value.
        let t = skin::with_value("", "wallpaper", r"D:\My Pictures\a b.jpg");
        assert_eq!(picture_of(&t), r"D:\My Pictures\a b.jpg");
        // Names that start with a quote or a space, or a network path, come back as written.
        for name in ["'90s.png", " sea.png", "sea.png ", "\"x\".png", "//server/share/a.png", r"\\server\share\a.png", "a;"] {
            let t = skin::with_value("name: a\r\n", "wallpaper", name);
            assert_eq!(picture_of(&t), name, "{t}");
        }
        assert_eq!(typed_picture("'90s.png").as_deref(), Some("'90s.png"));
        assert_eq!(typed_picture("//server/share/a.png").as_deref(), Some("//server/share/a.png"));
        // NONE takes the line out; the rest stays.
        let t = skin::without_value("name: a\r\nwallpaper: sea.jpg // the sea\r\nwallpaper-opacity: 30\r\n", "wallpaper");
        assert_eq!(t, "name: a\r\nwallpaper-opacity: 30\r\n");
        assert_eq!(picture_of(&t), "");
    }

    #[test]
    fn the_fit_choices_write_what_the_skin_reads() {
        for (fit, word, _) in FITS {
            let p = skin::parse(&format!("wallpaper-fit: {word}"), "x");
            assert_eq!(p.theme.wallpaper_fit, fit);
            assert!(p.notes.is_empty());
        }
        assert_eq!(word_at(&FITS, 3, "wallpaper-fit"), [("wallpaper-fit", "tile".to_string())]);
        assert!(word_at(&FITS, 9, "wallpaper-fit").is_empty());
        assert_eq!(label_of(&FITS, WallpaperFit::default()), "Fill");
    }

    #[test]
    fn slider_numbers_and_labels_are_short() {
        assert_eq!(slide_label(Slide::Zoom, 50.0), "+50");
        assert_eq!(slide_label(Slide::Zoom, -50.0), "-50");
        assert_eq!(slide_label(Slide::Zoom, 0.0), "0");
        assert_eq!(slide_label(Slide::Across, 30.0), "30%");
        assert_eq!(slide_label(Slide::Opacity, 40.0), "40");
        for (s, _, min, max, ..) in SLIDES {
            assert!(slide_label(s, min).chars().count() <= 5 && slide_label(s, max).chars().count() <= 5, "{s:?}");
        }
    }

    #[test]
    fn numbers_typed_in_the_box_are_understood_or_waited_for() {
        assert_eq!(typed_number(Slide::Zoom, "-50"), Some(-50.0));
        assert_eq!(typed_number(Slide::Zoom, "+20"), Some(20.0));
        assert_eq!(typed_number(Slide::Zoom, " 120 "), Some(120.0));
        assert_eq!(typed_number(Slide::Across, "30%"), Some(30.0));
        assert_eq!(typed_number(Slide::Across, "30 %"), Some(30.0));
        assert_eq!(typed_number(Slide::Opacity, "7.6"), Some(8.0));
        assert_eq!(typed_number(Slide::Opacity, "7,4"), Some(7.0), "a decimal comma");
        // Past the ends: the end. A minus where there can't be one: the smallest.
        assert_eq!(typed_number(Slide::Zoom, "-99999"), Some(-500.0));
        assert_eq!(typed_number(Slide::Zoom, "1e9"), Some(500.0));
        assert_eq!(typed_number(Slide::Opacity, "-5"), Some(0.0));
        assert_eq!(typed_number(Slide::Blur, "51"), Some(50.0));
        // Not a number yet: nothing changes.
        for half in ["", " ", "-", "+", "%", "abc", "1-", "--5", "NaN", "inf", "-inf", "5 5"] {
            assert_eq!(typed_number(Slide::Zoom, half), None, "{half:?}");
        }
        // What the box shows reads back as the same number.
        for (s, _, min, max, ..) in SLIDES {
            let mut v = min;
            while v <= max {
                assert_eq!(typed_number(s, &slide_label(s, v)), Some(v), "{s:?} {v}");
                v += 1.0;
            }
        }
    }

    #[test]
    fn defaults_are_what_a_skin_without_the_line_shows() {
        for fit in ["fill", "whole", "stretch", "tile", "actual"] {
            let text = format!("wallpaper-fit: {fit}\n");
            let t = skin::parse(&text, "x").theme;
            for (s, ..) in SLIDES {
                assert_eq!(slide_default(s, &t), shown(&text, s), "{s:?} with {fit}");
            }
        }
        assert_eq!([Slide::Opacity, Slide::Zoom, Slide::Across, Slide::Down, Slide::Blur, Slide::Fade].map(|s| slide_default(s, &skin::parse("", "x").theme)), [50.0, 0.0, 50.0, 50.0, 0.0, 0.0]);
    }

    #[test]
    fn a_reset_slider_shows_its_default_and_keeps_the_rest() {
        let reset = |text: &str, s: Slide| slide_reset(text, s, &skin::parse(text, "x").theme);
        let text = "name: Mine\r\nwallpaper-opacity: 70 // strong\r\nwallpaper-zoom: -200\r\nwallpaper-blur: 9\r\n";
        let t = reset(text, Slide::Opacity);
        assert_eq!(t, "name: Mine\r\nwallpaper-zoom: -200\r\nwallpaper-blur: 9\r\n");
        assert_eq!(shown(&t, Slide::Opacity), 50.0);
        assert_eq!(shown(&reset(text, Slide::Zoom), Slide::Zoom), 0.0);
        assert_eq!(shown(&reset(text, Slide::Zoom), Slide::Blur), 9.0);
        // Across and down share a line: resetting one keeps the other.
        let both = "wallpaper-position: 30% 70%\r\n";
        let t = reset(both, Slide::Across);
        assert_eq!((shown(&t, Slide::Across), shown(&t, Slide::Down)), (50.0, 70.0), "{t}");
        let t = reset(both, Slide::Down);
        assert_eq!((shown(&t, Slide::Across), shown(&t, Slide::Down)), (30.0, 50.0), "{t}");
        let t = reset("wallpaper-position: left bottom\r\n", Slide::Down);
        assert_eq!(t.trim(), "wallpaper-position: left");
        // Both at their defaults: the line goes.
        let t = reset(&reset(both, Slide::Across), Slide::Down);
        assert!(!t.contains("wallpaper-position"), "{t}");
        // Nothing to reset: nothing changes.
        assert_eq!(reset("name: a\r\n", Slide::Blur), "name: a\r\n");
    }

    #[test]
    fn each_square_of_the_3x3_writes_its_words() {
        let words = ["left top", "middle top", "right top", "left middle", "middle middle", "right middle", "left bottom", "middle bottom", "right bottom"];
        for (i, w) in words.iter().enumerate() {
            let (x, y) = grid_place(i);
            assert_eq!(position_text(x / 100.0, Some(y / 100.0), WallpaperFit::Fill), *w);
            let text = format!("wallpaper-position: {w}\n");
            assert!(skin::parse(&text, "x").notes.is_empty(), "{w}");
            assert_eq!((shown(&text, Slide::Across), shown(&text, Slide::Down)), (x, y), "{w}");
        }
    }

    #[test]
    fn small_pictures_are_cut_to_shape_not_squashed() {
        // Wide into square: its middle.
        assert_eq!(cover_src((200.0, 100.0), (50.0, 50.0)), Rect::new(50.0, 0.0, 100.0, 100.0));
        // Tall into wide: its middle band.
        assert_eq!(cover_src((100.0, 400.0), (100.0, 50.0)), Rect::new(0.0, 175.0, 100.0, 50.0));
        // Same shape: all of it.
        assert_eq!(cover_src((300.0, 200.0), (150.0, 100.0)), Rect::new(0.0, 0.0, 300.0, 200.0));
        // Nothing to draw into, or nothing to draw: still a sensible part.
        for (pic, dest) in [((0.0, 0.0), (10.0, 10.0)), ((100.0, 100.0), (0.0, 0.0))] {
            let r = cover_src(pic, dest);
            assert!(r.w.is_finite() && r.h.is_finite() && r.w >= 0.0 && r.h >= 0.0, "{r:?}");
        }
    }

    #[test]
    fn nearby_pictures_are_the_skin_folders_pictures_newest_first() {
        let dir = std::env::temp_dir().join(format!("glint-nearby-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let now = std::time::SystemTime::now();
        for (name, age) in [("old.jpg", 300), ("new.PNG", 10), ("mid.webp", 100), ("notes.txt", 0), ("skin.glint", 0)] {
            let f = std::fs::File::create(dir.join(name)).unwrap();
            f.set_modified(now - std::time::Duration::from_secs(age)).unwrap();
        }
        std::fs::File::create(dir.join("inner").join("deep.jpg")).unwrap();
        let names = |v: Vec<PathBuf>| v.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>();
        assert_eq!(names(nearby_pictures(&dir, 8)), ["new.PNG", "mid.webp", "old.jpg"]);
        assert_eq!(names(nearby_pictures(&dir, 2)), ["new.PNG", "mid.webp"]);
        assert!(nearby_pictures(&dir.join("not there"), 8).is_empty());
        // Read small in the background: these aren't real pictures, so none, but the list stays.
        let (list, current, too_big) = read_nearby(&dir, &dir.join("old.jpg").to_string_lossy());
        assert_eq!(list.len(), 3);
        assert!(list.iter().all(|(_, pic)| pic.is_none()) && current.is_none() && !too_big);
        // A picture on another computer isn't read for the pad.
        assert!(read_nearby(&dir, r"\\nowhere\share\a.jpg").1.is_none());
        let _ = std::fs::remove_dir_all(&dir);
        // Which one is in use, whatever the capitals or slashes.
        assert!(same_path(std::path::Path::new(r"C:\Skins\Sea.JPG"), "c:/skins/sea.jpg"));
        assert!(!same_path(std::path::Path::new(r"C:\Skins\sea.jpg"), ""));
        assert!(!same_path(std::path::Path::new(r"C:\Skins\sea.jpg"), r"C:\Skins\sea.jpeg"));
    }

    #[test]
    fn the_picture_on_the_pad_moves_with_the_mouse() {
        // Smaller than the pad (room to move): dragged right, it goes right.
        let d = (50.0, 50.0);
        assert_eq!(pan_place((0.0, 0.0), (30.0, 0.0), (60.0, 40.0), d), (50.0, 0.0));
        assert_eq!(pan_place((50.0, 50.0), (-15.0, 10.0), (60.0, 40.0), d), (25.0, 75.0));
        // Bigger than the pad (it fills it): dragged right, its left side comes into view.
        assert_eq!(pan_place((100.0, 0.0), (30.0, 0.0), (-60.0, -40.0), d), (50.0, 0.0));
        // Past the ends: the end.
        assert_eq!(pan_place((10.0, 90.0), (-1000.0, 1000.0), (60.0, 40.0), d), (0.0, 100.0));
        // No room (as wide as the pad), or nonsense: it stays.
        assert_eq!(pan_place((30.0, 70.0), (25.0, 25.0), (0.4, -0.2), d), (30.0, 70.0));
        assert_eq!(pan_place((30.0, 70.0), (f32::NAN, f32::INFINITY), (60.0, 40.0), d), (30.0, 70.0));
        // Very near the default: exactly it; a bit further: where it was dragged.
        assert_eq!(pan_place((0.0, 0.0), (29.6, 0.0), (60.0, 40.0), d), (50.0, 0.0));
        assert_eq!(pan_place((0.0, 0.0), (28.8, 0.0), (60.0, 40.0), d), (48.0, 0.0));
    }

    /// The three layouts: the wide one is as it always was; one column is 420 wide with no list and
    /// 32-tall rows (the default); slim is 600 wide with a 150-wide list and 34-tall rows. The menu
    /// of the rest has what the wide one's buttons do but undo and redo, and slim's icons are those
    /// buttons. At first only the wallpaper and the main colours are open.
    #[test]
    fn the_three_layouts_have_their_sizes_and_every_button() {
        let (w, c, s) = (Metrics::of(Layout::Wide), Metrics::of(Layout::Compact), Metrics::of(Layout::Slim));
        assert_eq!((w.width, w.list_w, w.row_h, w.header_h, w.row_pad), (780.0, 236.0, 46.0, 96.0, 16.0));
        assert_eq!((c.width, c.list_w, c.row_h, c.header_h, c.row_pad), (420.0, 0.0, 32.0, 96.0, 10.0));
        assert_eq!((s.width, s.list_w, s.row_h, s.item_h, s.header_h), (600.0, 150.0, 34.0, 26.0, 52.0));
        for m in [w, c, s] {
            assert!(m.min_width <= m.width);
        }
        assert_eq!(Layout::default(), Layout::Compact);
        assert_eq!(serde_json::to_value(crate::settings::Settings::default()).unwrap()["SkinsWindow"], "compact");
        let buttons: Vec<Btn> = BUTTONS.iter().map(|(b, _)| *b).collect();
        assert_eq!(MORE.iter().map(|(b, _)| *b).collect::<Vec<_>>(), buttons[2..].to_vec());
        assert_eq!(ICONS.iter().map(|(b, _)| *b).collect::<Vec<_>>(), buttons);
        let folded = first_folded();
        assert!(!folded.contains(&"Wallpaper") && !folded.contains(&"Main colours"));
        assert!(!folded.contains(&"Overall look") && folded.contains(&"Tabs") && folded.contains(&"3D edges"));
        assert_eq!(folded.len(), CARDS.len() - 3);
    }

    #[test]
    fn the_pad_shows_the_wallpaper_as_the_list_does_only_smaller() {
        let list = Rect::new(0.0, 0.0, 900.0, 560.0);
        let k = 160.0 / 900.0;
        let pad = Rect::new(0.0, 0.0, 160.0, 560.0 * k);
        let (orig, thumb) = ((1200.0, 800.0), (480.0, 320.0));
        for fit in ["fill", "whole", "stretch", "tile", "actual"] {
            for zoom in ["0", "100", "-50"] {
                let t = skin::parse(&format!("wallpaper-fit: {fit}\nwallpaper-zoom: {zoom}\nwallpaper-position: 30% 70%\n"), "x").theme;
                let big = crate::list::wallpaper_drawn_size(list, orig, &t, 1.0);
                let small = crate::list::wallpaper_drawn_size(pad, thumb, &t, orig.0 / thumb.0 * k);
                assert!((small.0 - big.0 * k).abs() < 0.01 && (small.1 - big.1 * k).abs() < 0.01, "{fit} {zoom}: {big:?} {small:?}");
            }
        }
    }
}
