//! Skins: how Glint looks, written as small text files.
//!
//! **The format.** One setting per line, `name: value` (or `name = value`). Lines starting with `//`,
//! `#` or `;` are notes, and so is anything after ` //` on a line. Names don't care about capitals,
//! spaces or underscores ("Title bar", "title_bar" and "title-bar" are the same). A `;` at the end
//! and quotes round a value are allowed (so a line copied from CSS works). For example:
//!
//! ```text
//! name: My skin
//! dark: true
//! accent: #76b900          // NVIDIA green
//! selection: accent 20%    // the accent, see-through
//! shape: straight
//! ```
//!
//! - Colours are `#rrggbb`, `#rgb` or `#rrggbbaa` (aa: how solid, 00 = clear, ff = solid), `none`,
//!   or another colour's name (`tab-line: accent`), optionally at a strength (`accent 16%`). The
//!   names are in [`COLOURS`].
//! - Any colour can also be a gradient: colours separated by commas, each with where it sits
//!   (`title-bar: #fdfdfd, #c3cddb 45%, #8d9db8 55%, #dfe6f0`). Colours without a % are spread
//!   evenly between their neighbours, as in CSS (a line copied from a CSS `linear-gradient(...)`
//!   works too; its direction is left out). A see-through colour in a gradient gives its strength
//!   first: `accent 50% 30%` is the accent at half strength, 30 % along. A gradient runs across the
//!   short side of what it fills: top to bottom on bars and buttons, left to right on a scrollbar's
//!   thumb (theme.rs `Paint`). Up to 8 colours.
//! - The rest is in [`OTHERS`]: the name, light or dark, the shapes (straight / slanted / rounded,
//!   how far things lean, how round corners are), the row height, the font and its size, a word for
//!   the logo (without one, the title bars show the chrome wordmark: logo.rs), and the styles of
//!   the buttons, caption buttons, tabs, sidebar and scrollbars.
//! - Whatever a skin leaves out comes from AMD Radeon (a dark skin) or AMD Radeon (Light) (a light
//!   one), so a skin can be three lines. Colours that came later than those two skins follow an
//!   older colour when left out ([`DERIVED`]), so a skin made before them looks as it always did.
//!   A line Glint doesn't understand is skipped with a note (the customiser shows them); nothing in
//!   a skin can stop Glint from starting.
//!
//! **Where skins are.** The built-in ones (AMD Radeon, NVIDIA and their "(Light)" versions, Chrome
//! and Gunmetal) are compiled into glint.exe from `skins/*.glint`, in the same format. A person's
//! own skins are `*.glint` files in the `skins` folder of the config folder (settings.rs), made by
//! the customiser (skinwin.rs), imported with its IMPORT… (which brings the skin's picture and its
//! light or dark other half too, see `prepare_import`) or put there by hand. Newer looks (the
//! Windows 2000, 98, XP and 7 skins) are such files, shared on their own rather than built in; the
//! drawing they switch on (`era`, `icons` and the rest) is Glint's. Ids in settings.json:
//! `amd-radeon`, `amd-radeon-light`, `nvidia`, `nvidia-light`, `chrome`, `gunmetal`, and
//! `file:<file name without .glint>`.
//!
//! **Light and dark.** A skin's light version is the skin with the same name and " (Light)" after
//! it (and `dark: false`); the light/dark choice picks between the two (`Library::twin`). Two skins
//! whose names don't match name each other with `twin` (Chrome and Gunmetal).
//!
//! **Fast.** A skin is read into a `Theme` (plain colours and sizes) once, when it's picked or its
//! text changes; drawing never looks at the text.
//!
//! **Never waiting for the disk.** The folder is listed, written and watched on threads of its own:
//! - `Library::refresh` lists it in the background (the result comes back as WM_APP_SKINS);
//! - `Watch` sleeps in WaitForMultipleObjects until a file in the folder changes (a skin edited in
//!   Notepad), then asks for a new listing - a skin in use is applied again at once;
//! - one file worker (`queue`) writes, renames and deletes skin files in order, each write atomic.
//!
//! Only at start, when the skin in use is one of the person's own, is the folder read on the UI
//! thread (like settings.json, it's needed for the first frame).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, GetDriveTypeW, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::UI::Shell::{
    SHFileOperationW, SHQueryRecycleBinW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING, FO_DELETE,
    SHFILEOPSTRUCTW, SHQUERYRBINFO,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::fileops;
use crate::log;
use crate::settings;
use crate::shell;
use crate::theme::{size, ButtonStyle, CaptionStyle, Color, Era, IconSet, Paint, Shape, SidebarStyle, Stop, TabStyle, Theme, WallpaperFit, MAX_STOPS};

/// The skin Glint starts with, and its light version.
pub const DEFAULT: &str = "amd-radeon";
pub const DEFAULT_LIGHT: &str = "amd-radeon-light";
/// The folder in the config folder that holds a person's own skins, and their files' extension.
pub const FOLDER: &str = "skins";
pub const EXTENSION: &str = "glint";
/// Ids of a person's own skins start with this ("file:My skin").
pub const FILE_PREFIX: &str = "file:";

/// Wakes the main window about skins: wparam [`EVENT_CHANGED`] (a file in the folder changed),
/// [`EVENT_LISTED`] (a listing is ready) or [`EVENT_WRITTEN`] (the file worker finished something).
pub const WM_APP_SKINS: u32 = WM_APP + 80;
pub const EVENT_CHANGED: usize = 0;
pub const EVENT_LISTED: usize = 1;
pub const EVENT_WRITTEN: usize = 2;

/// The built-in skins, in the order they're listed (each light or dark pair together): id and text.
const BUILT_IN: [(&str, &str); 6] = [
    (DEFAULT, include_str!("../skins/amd-radeon.glint")),
    (DEFAULT_LIGHT, include_str!("../skins/amd-radeon-light.glint")),
    ("nvidia", include_str!("../skins/nvidia.glint")),
    ("nvidia-light", include_str!("../skins/nvidia-light.glint")),
    ("chrome", include_str!("../skins/chrome.glint")),
    ("gunmetal", include_str!("../skins/gunmetal.glint")),
];

/// A person's own skins: at most this many files are read, each at most this big.
const MAX_OWN: usize = 500;
const MAX_FILE_BYTES: u64 = 256 * 1024;
/// The word in the logo is at most this many letters.
pub const MAX_LOGO_TEXT: usize = 12;
/// The standard word in the logo's box.
pub const DEFAULT_LOGO_TEXT: &str = "GLINT";

/// Every colour a skin can set, with what it colours.
pub const COLOURS: &[(&str, &str)] = &[
    ("accent", "Lines, selected rows' edge, switches"),
    ("accent-text", "Text on the accent colour"),
    ("background", "The darkest layer, between the panels"),
    ("title-bar", "The bar with the logo and tabs"),
    ("toolbar", "The bar with the path, and the current tab"),
    ("panel", "File list and details panel"),
    ("sidebar", "Sidebar"),
    ("status-bar", "Status bar"),
    ("input", "Path and filter boxes"),
    ("button", "Buttons, and the current folder in the path"),
    ("button-hover", "Buttons under the mouse"),
    ("text", "Text"),
    ("text-bright", "Text on selected rows and buttons"),
    ("muted", "Headings, dates, inactive tabs"),
    ("disabled", "Buttons that can't be used"),
    ("selection", "Selected rows"),
    ("selection-edge", "Left edge of selected rows"),
    ("hover", "Rows under the mouse"),
    ("tool-hover", "Back, Forward, Up and Refresh under the mouse"),
    ("tool-pressed", "Those while clicked"),
    ("path-hover", "A step of the path under the mouse"),
    ("line", "Thin lines"),
    ("scrollbar", "Scroll bars"),
    ("scrollbar-hover", "Scroll bars under the mouse"),
    ("usage-bar", "Behind a drive's usage bar"),
    ("logo", "Logo box"),
    ("logo-letters", "Logo letters"),
    ("tab-line", "Line on top of the current tab"),
    ("toolbar-line", "Line under the toolbar"),
    ("caption-hover", "+, minimise and maximise under the mouse"),
    ("caption-pressed", "Those while clicked"),
    ("close-hover", "Close button under the mouse"),
    ("tab", "Tabs other than the current one"),
    ("tab-active", "The current tab"),
    ("tab-text", "Names of the other tabs"),
    ("tab-active-text", "Name of the current tab"),
    ("tab-border", "Edge of each tab (tabs: pills)"),
    ("highlight", "Thin shine along the top of bars and tabs"),
    ("sidebar-panel", "Sidebar panels (sidebar-style: panels)"),
    ("panel-header", "Header bars of the sidebar panels"),
    ("panel-header-text", "Sidebar headings"),
    ("sidebar-selection", "The place you're in, in the sidebar"),
    ("sidebar-selection-text", "Its name"),
    ("list-header", "Column titles' bar"),
    ("list-header-text", "Column titles"),
    ("column-line", "Lines between the column titles"),
    ("status-text", "Status bar text"),
    ("status-line", "Line along the top of the status bar"),
    ("selection-text", "Text on selected rows"),
    ("input-border", "Edge of the path and filter boxes"),
    ("button-text", "Words and signs on buttons"),
    ("marble", "Minimise and maximise (caption-buttons: marbles)"),
    ("marble-close", "Close (caption-buttons: marbles)"),
    ("usage-fill", "Used part of a drive's usage bar"),
    ("scrollbar-track", "Behind the scroll bars"),
    ("scrollbar-border", "Edge of the scroll bars"),
    ("scrollbar-grip-lines", "Lines on the scroll bars (scrollbar-grip)"),
    ("back-button", "Back and forward as round glass buttons"),
    ("tool-border", "Edge of toolbar buttons under the mouse"),
    ("selection-border", "Edge of selected rows"),
    ("title-gloss", "Shine over the top half of the title bar"),
    ("title-text", "Window titles (Settings, Sort order)"),
    ("title-text-inactive", "Window titles while the window isn't active"),
    ("title-text-shadow", "Shadow under window titles"),
    ("title-bar-inactive", "The title bar while the window isn't active"),
    ("bevel-highlight", "3D edges: the lit outer line (bevels)"),
    ("bevel-light", "3D edges: the lit inner line"),
    ("bevel-shadow", "3D edges: the shaded inner line"),
    ("bevel-dark", "3D edges: the shaded outer line"),
    ("menu", "Menus drawn in the skin (none: Windows' own)"),
    ("menu-text", "Words in menus"),
    ("menu-selection", "The menu item under the mouse"),
    ("menu-selection-text", "Its words"),
    ("menu-border", "Edge of menus"),
    ("sidebar-text", "Names of the places in the sidebar"),
];

/// Colours that came later than AMD Radeon and NVIDIA: each follows an older colour when a skin
/// leaves it out, so every skin made before them looks exactly as it did.
pub const DERIVED: &[(&str, &str)] = &[
    ("tab", "none"),
    ("tab-active", "toolbar"),
    ("tab-text", "muted"),
    ("tab-active-text", "text"),
    ("tab-border", "none"),
    ("highlight", "none"),
    ("sidebar-panel", "panel"),
    ("panel-header", "toolbar"),
    ("panel-header-text", "muted"),
    ("sidebar-selection", "selection"),
    ("sidebar-selection-text", "selection-text"),
    ("list-header", "panel"),
    ("list-header-text", "muted"),
    ("column-line", "none"),
    ("status-text", "muted"),
    ("status-line", "none"),
    ("selection-text", "text-bright"),
    ("input-border", "none"),
    ("button-text", "text"),
    ("marble", "muted"),
    ("marble-close", "close-hover"),
    ("usage-fill", "accent"),
    ("scrollbar-track", "none"),
    ("scrollbar-border", "none"),
    ("scrollbar-grip-lines", "muted"),
    ("back-button", "none"),
    ("tool-border", "none"),
    ("selection-border", "none"),
    ("title-gloss", "none"),
    ("title-text", "text"),
    ("title-text-inactive", "muted"),
    ("title-text-shadow", "none"),
    ("title-bar-inactive", "title-bar"),
    ("bevel-highlight", "none"),
    ("bevel-light", "none"),
    ("bevel-shadow", "none"),
    ("bevel-dark", "none"),
    ("menu", "none"),
    ("menu-text", "text"),
    ("menu-selection", "accent"),
    ("menu-selection-text", "accent-text"),
    ("menu-border", "line"),
    ("sidebar-text", "text"),
];

/// Everything else a skin can set, with what it does.
pub const OTHERS: &[(&str, &str)] = &[
    ("name", "Name in Settings; its light version adds \" (Light)\""),
    ("description", "One line about it"),
    ("author", "Who made it"),
    ("dark", "true or false"),
    ("twin", "Its light or dark other half, by id or name (when the names don't pair up)"),
    ("menus", "Right-click menus: dark or light"),
    ("shape", "slanted, straight or rounded"),
    ("slant", "How far buttons lean, 0 to 20"),
    ("path-slant", "How far the current folder's box leans, 0 to 20"),
    ("radius", "Corner roundness, 0 to 16"),
    ("row-height", "File list row height, 20 to 48"),
    ("sidebar-row-height", "Sidebar row height, 16 to 48"),
    ("tab-line-width", "Current tab's line, 0 to 6"),
    ("toolbar-line-width", "Toolbar's line, 0 to 6"),
    ("font", "Fonts to try, best first"),
    ("font-size", "8pt to 14pt (or pixels: 12px)"),
    ("logo-text", "The word in the logo's box, at most 12 letters"),
    ("buttons", "flat, or liquid (glossy glass marbles and capsules)"),
    ("caption-buttons", "symbols, marbles or tiles (minimise, maximise, close)"),
    ("tabs", "attached (the current one joins the toolbar), or pills"),
    ("sidebar-style", "plain, or panels (each group in a rounded panel)"),
    ("scrollbar-grip", "Lines across the scroll bars: true or false"),
    ("scrollbar-width", "How thick the scroll bars are, 4 to 18"),
    ("bevels", "Raised and sunken 3D edges, as in Windows 2000: true or false"),
    ("title-bar-across", "The title bar's gradient runs left to right: true or false"),
    ("see-through", "How much shows through the glass title bar (era 7, Windows 11): 0 to 90"),
    ("wallpaper", "A picture behind the files: its file name (next to the skin, or in the skins folder), or a full path"),
    ("wallpaper-opacity", "How strongly the wallpaper shows: 0 to 100 (40 if not set)"),
    ("wallpaper-position", "Which side of the wallpaper stays in view: left, middle or right, then top, middle or bottom if you like; or two percentages, across then down (30% 70%)"),
    ("wallpaper-zoom", "Zoom the wallpaper: -500 to 500 (0 is its usual size, 100 twice as big)"),
    ("wallpaper-fit", "fill (fills the list), whole (all of it, at its side), stretch (to the list's shape), tile (repeated) or actual (its own size)"),
    ("wallpaper-size", "The wallpaper's width as a part of the list's, 1 to 400 (50% is half). Instead of the fit's size; the zoom goes on top"),
    ("wallpaper-offset", "Moves the wallpaper across, then down: -2000 to 2000 each (20 -10)"),
    ("wallpaper-blur", "Blurs the wallpaper: 0 to 50 (0 for none)"),
    ("wallpaper-fade", "Fades the wallpaper out at the list's edges: 0 to 50 (0 for none), as a part of the list's size"),
    ("era", "Older Windows to look like: none, 98, 2000, xp or 7"),
    ("icons", "Icons: windows (today's), classic, xp or aero (follows the era)"),
    ("title-font", "Font of window titles (empty: the skin's font)"),
];

/// The settings that are a number, with the smallest and biggest they can be (font-size in points:
/// the skin writes "9pt"). A number past either end is noted and the nearest end used.
const NUMBERS: &[(&str, f32, f32, bool)] = &[
    ("slant", 0.0, 20.0, false),
    ("path-slant", 0.0, 20.0, false),
    ("radius", 0.0, 16.0, false),
    ("row-height", 20.0, 48.0, false),
    ("sidebar-row-height", 16.0, 48.0, false),
    ("tab-line-width", 0.0, 6.0, false),
    ("toolbar-line-width", 0.0, 6.0, false),
    ("font-size", 8.0, 14.0, true),
    ("see-through", 0.0, 90.0, false),
    ("wallpaper-opacity", 0.0, 100.0, false),
    ("wallpaper-zoom", -500.0, 500.0, false),
    ("wallpaper-size", 1.0, 400.0, false),
    ("wallpaper-blur", 0.0, 50.0, false),
    ("wallpaper-fade", 0.0, 50.0, false),
    ("scrollbar-width", 4.0, 18.0, false),
];

/// What a number setting can be, as a skin writes it ("0 to 20", "8pt to 14pt"); None for a
/// setting that isn't a number.
pub fn range_of(key: &str) -> Option<String> {
    let &(_, min, max, in_pt) = NUMBERS.iter().find(|n| n.0 == key)?;
    let pt = in_pt.then_some(PT);
    let k = pt.unwrap_or(1.0);
    Some(format!("{} to {}", show(min * k, pt), show(max * k, pt)))
}

/// Other names people may write for a setting (CSS habits, the code's names).
const ALIASES: &[(&str, &str)] = &[
    ("window", "background"),
    ("titlebar", "title-bar"),
    ("statusbar", "status-bar"),
    ("accent-tint", "selection"),
    ("selected", "selection"),
    ("on-accent", "accent-text"),
    ("chip", "button"),
    ("chip-hover", "button-hover"),
    ("font-family", "font"),
    ("mode", "dark"),
    ("field", "input"),
    ("field-border", "input-border"),
    ("scrollbar-thumb", "scrollbar"),
    ("scrollbar-thumb-hover", "scrollbar-hover"),
    ("selected-text", "selection-text"),
    ("caption", "caption-buttons"),
    ("button-style", "buttons"),
    ("tab-style", "tabs"),
];

// ---------------------------------------------------------------- reading a skin

/// Something about a skin's text worth telling: line `line` (0: the skin as a whole) and what.
#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub line: usize,
    pub text: String,
}

/// A skin read from its text.
#[derive(Clone, Debug)]
pub struct Parsed {
    pub theme: Theme,
    pub name: String,
    pub description: String,
    /// The skin's light or dark other half as its `twin` line names it (an id or a name; "" for
    /// none: then names pair up, see `Library::twin`).
    pub twin: String,
    pub notes: Vec<Note>,
}

/// One "name: value" line.
#[derive(Clone, Debug)]
struct Line {
    key: String,
    value: String,
    /// 1-based; 0 for lines of the base skin.
    number: usize,
}

/// A setting's name as Glint knows it: lower case, words joined with "-", aliases resolved.
pub fn normalize_key(key: &str) -> String {
    let mut k = String::new();
    for word in key.trim().split(|c: char| c.is_whitespace() || c == '_' || c == '-').filter(|w| !w.is_empty()) {
        if !k.is_empty() {
            k.push('-');
        }
        k.push_str(&word.to_lowercase());
    }
    ALIASES.iter().find(|(a, _)| *a == k).map_or(k, |(_, real)| real.to_string())
}

/// Whether Glint knows setting `key` (as [`normalize_key`] makes it).
pub fn known_key(key: &str) -> bool {
    COLOURS.iter().any(|(k, _)| *k == key) || OTHERS.iter().any(|(k, _)| *k == key)
}

/// The text of a line without its note (" // ..."), or None for a line that is only a note.
fn without_note(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with("//") || t.starts_with('#') || t.starts_with(';') {
        return None;
    }
    let mut end = t.len();
    let bytes = t.as_bytes();
    for i in 1..bytes.len().saturating_sub(1) {
        if bytes[i] == b'/' && bytes[i + 1] == b'/' && bytes[i - 1].is_ascii_whitespace() {
            end = i;
            break;
        }
    }
    Some(t[..end].trim())
}

/// Splits a line into its name and value, or None when it has neither ":" nor "=".
fn split_line(t: &str) -> Option<(&str, &str)> {
    let at = t.find([':', '='])?;
    Some((&t[..at], &t[at + 1..]))
}

/// A value without the frills: the spaces, a ";" or "," at the end, quotes round it.
fn clean_value(v: &str) -> String {
    let v = v.trim().trim_end_matches([';', ',']).trim();
    let unquoted = if v.len() >= 2 && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\''))) { &v[1..v.len() - 1] } else { v };
    unquoted.to_string()
}

/// The settings in `text`, in order, and notes about lines that aren't settings.
fn read_lines(text: &str, notes: &mut Vec<Note>) -> Vec<Line> {
    let mut lines = Vec::new();
    for (i, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let number = i + 1;
        let Some(t) = without_note(raw) else { continue };
        // Bits of CSS that carry nothing here ("window {", "}").
        if t == "{" || t == "}" || (t.ends_with('{') && !t.contains(':')) {
            continue;
        }
        let Some((key, value)) = split_line(t) else {
            notes.push(Note { line: number, text: format!("\u{201c}{t}\u{201d} isn't a setting: write it as name: value. Skipped.") });
            continue;
        };
        let key = normalize_key(key.trim_start_matches('{'));
        if key.is_empty() {
            notes.push(Note { line: number, text: "No name before the colon. Skipped.".into() });
            continue;
        }
        lines.push(Line { key, value: clean_value(value), number });
    }
    lines
}

/// A yes/no value.
fn yes_no(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "dark" => Some(true),
        "false" | "no" | "off" | "0" | "light" => Some(false),
        _ => None,
    }
}

/// A colour written as hex: #rgb, #rrggbb or #rrggbbaa.
pub fn parse_hex(v: &str) -> Option<Color> {
    let h = v.trim().strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|n| n * 17);
            Some(Color::rgba(d(0)?, d(1)?, d(2)?, 1.0))
        }
        6 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, 1.0)),
        8 => Some(Color::rgba(byte(0)?, byte(2)?, byte(4)?, byte(6)? as f32 / 255.0)),
        _ => None,
    }
}

/// A colour as a skin writes it: #rrggbb, or #rrggbbaa when it's see-through.
pub fn hex_of(c: Color) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if c.a >= 0.999 {
        format!("#{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b))
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", b(c.r), b(c.g), b(c.b), b(c.a))
    }
}

/// "rgb(1, 2, 3)" or "rgba(1, 2, 3, 0.5)".
fn parse_rgb(v: &str) -> Option<Color> {
    let v = v.trim().to_lowercase();
    let inner = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb("))?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    let n = |s: &str| s.parse::<f32>().ok().filter(|x| x.is_finite()).map(|x| x.clamp(0.0, 255.0) as u8);
    match parts.as_slice() {
        [r, g, b] => Some(Color::rgba(n(r)?, n(g)?, n(b)?, 1.0)),
        [r, g, b, a] => Some(Color::rgba(n(r)?, n(g)?, n(b)?, a.parse::<f32>().ok().filter(|x| x.is_finite())?.clamp(0.0, 1.0))),
        _ => None,
    }
}

/// A number with an optional unit: "8", "8px", "9pt" (pt only where `pt` says how many DIPs a
/// point is).
fn parse_number(v: &str, pt: Option<f32>) -> Option<f32> {
    let v = v.trim().to_lowercase();
    let (num, factor) = if let Some(n) = v.strip_suffix("pt") {
        (n, pt?)
    } else if let Some(n) = v.strip_suffix("px").or_else(|| v.strip_suffix('%')) {
        (n, 1.0)
    } else {
        (v.as_str(), 1.0)
    };
    num.trim().parse::<f32>().ok().filter(|x| x.is_finite()).map(|x| x * factor)
}

/// Works out the colours, following names to other colours, with every problem noted once.
struct Resolver<'a> {
    /// The base skin's lines, then the skin's own (later ones win).
    base: HashMap<String, String>,
    own: HashMap<String, (String, usize)>,
    done: HashMap<String, Paint>,
    busy: HashSet<String>,
    notes: &'a mut Vec<Note>,
}

/// Why a colour couldn't be read, and - for a gradient with a bad colour or position - what is
/// used instead (and how to say so). Without that, the standard colour is used.
struct Bad {
    why: String,
    instead: Option<Box<(Paint, String)>>,
}

impl From<String> for Bad {
    fn from(why: String) -> Bad {
        Bad { why, instead: None }
    }
}

/// `v` cut at the commas that aren't inside brackets: "rgb(1, 2, 3), #fff" is two parts.
fn split_commas(v: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0i32, 0);
    for (i, ch) in v.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                parts.push(&v[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&v[start..]);
    parts
}

/// A gradient's colours without a CSS `linear-gradient( ... )` round them and its direction ("to
/// bottom", "180deg"): Glint runs a gradient across the short side of what it fills.
fn without_css_wrapper(v: &str) -> &str {
    let t = v.trim();
    let lower = t.to_ascii_lowercase();
    let Some(inner) = lower.strip_prefix("linear-gradient(").and_then(|_| t.get("linear-gradient(".len()..)).and_then(|s| s.strip_suffix(')')) else { return t };
    let first = split_commas(inner)[0].trim().to_ascii_lowercase();
    let direction = first.starts_with("to ") || ["deg", "turn", "rad", "grad"].iter().any(|u| first.ends_with(u) && first[..first.len() - u.len()].trim().parse::<f32>().is_ok());
    match (direction, inner.find(',')) {
        (true, Some(at)) if split_commas(inner).len() > 1 => inner[at + 1..].trim(),
        _ => inner.trim(),
    }
}

/// Fills in the positions a gradient's colours left out, as CSS does: the first at the start, the
/// last at the end, the ones between spread evenly between their neighbours. A position before an
/// earlier one moves up to it.
fn fill_positions(at: &mut [Option<f32>]) -> Vec<f32> {
    let n = at.len();
    if n == 0 {
        return Vec::new();
    }
    if at[0].is_none() {
        at[0] = Some(0.0);
    }
    if at[n - 1].is_none() {
        at[n - 1] = Some(1.0);
    }
    let mut out: Vec<f32> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        match at[i] {
            Some(p) => {
                out.push(p.max(out.last().copied().unwrap_or(0.0)));
                i += 1;
            }
            None => {
                // A run of missing ones, between the one before (known) and the next known one.
                let from = out.last().copied().unwrap_or(0.0);
                let end = (i..n).find(|&j| at[j].is_some()).unwrap_or(n - 1);
                let to = at[end].unwrap_or(1.0).max(from);
                let steps = (end - i + 1) as f32;
                for k in i..end {
                    out.push(from + (to - from) * (k - i + 1) as f32 / steps);
                }
                i = end;
            }
        }
    }
    out
}

impl Resolver<'_> {
    /// A colour expression: "#hex", "none", "rgb(..)", a colour's name, each maybe with "NN%"; or
    /// a gradient of those, separated by commas.
    fn expression(&mut self, v: &str) -> Result<Paint, Bad> {
        let inner = without_css_wrapper(v);
        let parts = split_commas(inner);
        if parts.len() == 1 {
            return self.single(parts[0]).map_err(Bad::from);
        }
        self.gradient(&parts)
    }

    /// One colour, maybe at a strength ("accent 16%"). A name can stand for a gradient.
    fn single(&mut self, v: &str) -> Result<Paint, String> {
        let v = v.trim();
        let (term, strength) = match v.rsplit_once(char::is_whitespace) {
            Some((t, pct)) if pct.ends_with('%') => {
                let p = pct.trim_end_matches('%').trim().parse::<f32>().ok().filter(|p| p.is_finite()).ok_or_else(|| format!("\u{201c}{pct}\u{201d} isn't a strength like 16%"))?;
                (t.trim(), Some((p / 100.0).clamp(0.0, 1.0)))
            }
            _ => (v, None),
        };
        let lower = term.to_lowercase();
        let paint = if lower == "none" || lower == "transparent" {
            Paint::CLEAR
        } else if let Some(c) = parse_hex(term).or_else(|| parse_rgb(term)) {
            Paint::Flat(c)
        } else {
            let name = normalize_key(term);
            if !COLOURS.iter().any(|(k, _)| *k == name) {
                return Err(if term.is_empty() {
                    "A colour is missing (two commas in a row?)".to_string()
                } else {
                    format!("\u{201c}{term}\u{201d} isn't a colour: write it like #e3262d, or another colour's name")
                });
            }
            self.colour(&name).ok_or_else(|| format!("\u{201c}{name}\u{201d} refers back to itself"))?
        };
        Ok(match strength {
            Some(s) => paint.with_strength(s),
            None => paint,
        })
    }

    /// A gradient: each part a colour with an optional position ("#c3cddb 45%"). A part that
    /// can't be read makes the whole a flat colour (its first good one), with a note.
    fn gradient(&mut self, parts: &[&str]) -> Result<Paint, Bad> {
        let mut colours: Vec<Color> = Vec::new();
        let mut at: Vec<Option<f32>> = Vec::new();
        let mut problem: Option<String> = None;
        for part in parts {
            let part = part.trim();
            // A last word with % is the position (a strength, if any, comes before it).
            let (colour_text, position) = match part.rsplit_once(char::is_whitespace) {
                Some((c, pct)) if pct.ends_with('%') => match pct.trim_end_matches('%').trim().parse::<f32>().ok().filter(|p| p.is_finite()) {
                    Some(p) => (c.trim(), Some((p / 100.0).clamp(0.0, 1.0))),
                    None => {
                        problem.get_or_insert(format!("\u{201c}{pct}\u{201d} isn't a position like 45%"));
                        continue;
                    }
                },
                _ => (part, None),
            };
            match self.single(colour_text) {
                // A name that is a gradient itself gives its one colour here.
                Ok(p) => {
                    colours.push(p.base());
                    at.push(position);
                }
                Err(why) => {
                    problem.get_or_insert(why);
                }
            }
        }
        let first_good = colours.first().copied();
        if let Some(why) = problem {
            let instead = first_good.map(|c| Box::new((Paint::Flat(c), format!("Using {}, its first colour, without the gradient.", hex_of(c)))));
            return Err(Bad { why, instead });
        }
        if colours.len() > MAX_STOPS {
            colours.truncate(MAX_STOPS);
            at.truncate(MAX_STOPS);
            let kept = Paint::from_stops(&stops_of(&colours, &mut at));
            return Err(Bad { why: format!("A gradient has at most {MAX_STOPS} colours"), instead: Some(Box::new((kept, "The rest are left out.".into()))) });
        }
        Ok(Paint::from_stops(&stops_of(&colours, &mut at)))
    }

    /// The paint called `key` (None while it's being worked out: a circle).
    fn colour(&mut self, key: &str) -> Option<Paint> {
        if let Some(c) = self.done.get(key) {
            return Some(*c);
        }
        if !self.busy.insert(key.to_string()) {
            return None;
        }
        let mut result = None;
        if let Some((v, line)) = self.own.get(key).cloned() {
            match self.expression(&v) {
                Ok(c) => result = Some(c),
                Err(Bad { why, instead: Some(instead) }) => {
                    let (paint, said) = *instead;
                    self.notes.push(Note { line, text: format!("{why}. {said}") });
                    result = Some(paint);
                }
                Err(Bad { why, instead: None }) => self.notes.push(Note { line, text: format!("{why}. Using the standard {key} instead.") }),
            }
        }
        if result.is_none() {
            if let Some(v) = self.base.get(key).cloned() {
                result = self.expression(&v).ok();
            }
        }
        if result.is_none() {
            if let Some((_, v)) = DERIVED.iter().find(|(k, _)| *k == key) {
                result = self.expression(v).ok();
            }
        }
        self.busy.remove(key);
        // (Never reached with the built-in base skins and DERIVED, which name every colour; the
        // tests check.)
        let c = result.unwrap_or(Paint::Flat(Color::hex(0x808080)));
        self.done.insert(key.to_string(), c);
        Some(c)
    }
}

/// Stops from colours and their positions as written (the missing ones filled in).
fn stops_of(colours: &[Color], at: &mut [Option<f32>]) -> Vec<Stop> {
    fill_positions(at).into_iter().zip(colours).map(|(at, color)| Stop { at, color: *color }).collect()
}

/// Reads a skin. Never fails: what can't be understood is noted and taken from the base skin.
/// `fallback_name` is the name when the text has none (the file's name).
pub fn parse(text: &str, fallback_name: &str) -> Parsed {
    parse_in(text, fallback_name, None)
}

/// `parse` for a skin whose file is in `dir`: a wallpaper named by its file name is looked for
/// there first (a skin in a folder of its own, with its pictures).
fn parse_in(text: &str, fallback_name: &str, dir: Option<&Path>) -> Parsed {
    let mut notes = Vec::new();
    let lines = read_lines(text, &mut notes);
    let mut own: HashMap<String, (String, usize)> = HashMap::new();
    for l in &lines {
        if !known_key(&l.key) {
            notes.push(Note { line: l.number, text: format!("\u{201c}{}\u{201d} isn't a skin setting. Skipped.", l.key) });
            continue;
        }
        // The same setting twice (or under another of its names): the later line wins, and says so.
        if let Some((_, earlier)) = own.insert(l.key.clone(), (l.value.clone(), l.number)) {
            notes.push(Note { line: l.number, text: format!("{} is already set on line {earlier} (this one wins).", l.key) });
        }
    }
    // Light or dark decides the base skin: as written, or guessed from the background.
    let dark = match own.get("dark") {
        Some((v, line)) => yes_no(v).unwrap_or_else(|| {
            notes.push(Note { line: *line, text: format!("dark is true or false (not \u{201c}{v}\u{201d}). Guessing from the background.") });
            guess_dark(&own)
        }),
        None => guess_dark(&own),
    };
    let base_text = BUILT_IN.iter().find(|(id, _)| *id == if dark { DEFAULT } else { DEFAULT_LIGHT }).map_or("", |(_, t)| t);
    let base: HashMap<String, String> = read_lines(base_text, &mut Vec::new()).into_iter().map(|l| (l.key, l.value)).collect();

    let mut r = Resolver { base: base.clone(), own: own.clone(), done: HashMap::new(), busy: HashSet::new(), notes: &mut notes };
    let mut p = |key: &str| r.colour(key).unwrap_or(Paint::Flat(Color::hex(0x808080)));
    // Each surface's paint, and the one colour that stands for it.
    let window_paint = p("background");
    let titlebar_paint = p("title-bar");
    let toolbar_paint = p("toolbar");
    let panel_paint = p("panel");
    let sidebar_paint = p("sidebar");
    let statusbar_paint = p("status-bar");
    let input_paint = p("input");
    let chip_paint = p("button");
    let chip_hover_paint = p("button-hover");
    let accent_paint = p("accent");
    let accent_tint_paint = p("selection");
    let hover_paint = p("hover");
    let scroll_thumb_paint = p("scrollbar");
    let scroll_thumb_hover_paint = p("scrollbar-hover");
    let bar_track_paint = p("usage-bar");
    let logo_paint = p("logo");
    let text = p("text").base();
    let selection_text = p("selection-text").base();
    let mut theme = Theme {
        dark,
        window: window_paint.base(),
        titlebar: titlebar_paint.base(),
        toolbar: toolbar_paint.base(),
        panel: panel_paint.base(),
        sidebar: sidebar_paint.base(),
        statusbar: statusbar_paint.base(),
        input: input_paint.base(),
        chip: chip_paint.base(),
        chip_hover: chip_hover_paint.base(),
        text,
        text_strong: p("text-bright").base(),
        muted: p("muted").base(),
        disabled: p("disabled").base(),
        accent: accent_paint.base(),
        accent_tint: accent_tint_paint.base(),
        select_edge: p("selection-edge").base(),
        on_accent: p("accent-text").base(),
        hover: hover_paint.base(),
        button_hover: p("tool-hover").base(),
        button_pressed: p("tool-pressed").base(),
        crumb_hover: p("path-hover").base(),
        line: p("line").base(),
        scroll_thumb: scroll_thumb_paint.base(),
        scroll_thumb_hover: scroll_thumb_hover_paint.base(),
        bar_track: bar_track_paint.base(),
        logo: logo_paint.base(),
        logo_ink: p("logo-letters").base(),
        tab_line: p("tab-line").base(),
        toolbar_line: p("toolbar-line").base(),
        caption_hover: p("caption-hover").base(),
        caption_pressed: p("caption-pressed").base(),
        close_hover: p("close-hover").base(),
        menus_dark: dark,
        shape: Shape::Slanted,
        slant: 8.0,
        path_slant: 6.0,
        radius: 6.0,
        row_h: size::ROW_H,
        sidebar_row_h: size::ROW_H,
        tab_line_w: 3.0,
        toolbar_line_w: 2.0,
        font: String::new(),
        font_size: size::FONT,
        logo_text: DEFAULT_LOGO_TEXT.into(),
        logo_style: crate::settings::LogoStyle::Box,
        window_paint,
        titlebar_paint,
        toolbar_paint,
        panel_paint,
        sidebar_paint,
        statusbar_paint,
        input_paint,
        chip_paint,
        chip_hover_paint,
        accent_paint,
        accent_tint_paint,
        hover_paint,
        scroll_thumb_paint,
        scroll_thumb_hover_paint,
        bar_track_paint,
        logo_paint,
        tab: p("tab"),
        tab_active: p("tab-active"),
        tab_text: p("tab-text").base(),
        tab_active_text: p("tab-active-text").base(),
        tab_border: p("tab-border").base(),
        highlight: p("highlight").base(),
        sidebar_panel: p("sidebar-panel"),
        panel_header: p("panel-header"),
        panel_header_text: p("panel-header-text").base(),
        sidebar_selection: p("sidebar-selection"),
        sidebar_selection_text: p("sidebar-selection-text").base(),
        list_header: p("list-header"),
        list_header_text: p("list-header-text").base(),
        column_line: p("column-line").base(),
        status_text: p("status-text").base(),
        status_line: p("status-line").base(),
        selection_text,
        // A selected row's other columns stay in the normal text colour unless the skin says
        // otherwise (as before `selection-text` existed).
        selection_details: if own.contains_key("selection-text") { selection_text } else { text },
        input_border: p("input-border").base(),
        button_text: p("button-text").base(),
        marble: p("marble"),
        marble_close: p("marble-close"),
        usage_fill: p("usage-fill"),
        scrollbar_track: p("scrollbar-track"),
        scrollbar_border: p("scrollbar-border").base(),
        scrollbar_grip_lines: p("scrollbar-grip-lines").base(),
        buttons: ButtonStyle::Flat,
        caption_buttons: CaptionStyle::Symbols,
        tabs: TabStyle::Attached,
        sidebar_style: SidebarStyle::Plain,
        scrollbar_grip: false,
        scrollbar_w: size::SCROLLBAR_W,
        bevels: false,
        back_button: p("back-button"),
        tool_border: p("tool-border").base(),
        selection_border: p("selection-border").base(),
        title_gloss: p("title-gloss").base(),
        title_across: false,
        see_through: 0.0,
        wallpaper: String::new(),
        wallpaper_opacity: 0.4,
        wallpaper_at: 1.0,
        wallpaper_fit: WallpaperFit::Fill,
        wallpaper_at_y: None,
        wallpaper_zoom: 1.0,
        wallpaper_size: None,
        wallpaper_offset: (0.0, 0.0),
        wallpaper_blur: 0,
        wallpaper_fade: 0.0,
        era: Era::None,
        icons: IconSet::Windows,
        title_text: p("title-text").base(),
        title_text_inactive: p("title-text-inactive").base(),
        title_shadow: p("title-text-shadow").base(),
        titlebar_inactive_paint: p("title-bar-inactive"),
        title_font: String::new(),
        bevel_highlight: p("bevel-highlight").base(),
        bevel_light: p("bevel-light").base(),
        bevel_shadow: p("bevel-shadow").base(),
        bevel_dark: p("bevel-dark").base(),
        menu: p("menu").base(),
        menu_text: p("menu-text").base(),
        menu_selection: p("menu-selection").base(),
        menu_selection_text: p("menu-selection-text").base(),
        menu_border: p("menu-border").base(),
        sidebar_text: p("sidebar-text").base(),
    };

    // The rest: as the skin says, else as the base says.
    let value = |key: &str| -> (Option<(String, usize)>, Option<String>) { (own.get(key).cloned(), base.get(key).cloned()) };
    let number = |key: &str, notes: &mut Vec<Note>| -> Option<f32> {
        let &(_, min, max, in_pt) = NUMBERS.iter().find(|n| n.0 == key)?;
        let pt = in_pt.then_some(PT);
        let (min, max) = (min * pt.unwrap_or(1.0), max * pt.unwrap_or(1.0));
        let (mine, theirs) = value(key);
        if let Some((v, line)) = mine {
            match parse_number(&v, pt) {
                Some(x) if (min..=max).contains(&x) => return Some(x),
                Some(x) => {
                    notes.push(Note { line, text: format!("{key} goes from {} to {}. Using {}.", show(min, pt), show(max, pt), show(x.clamp(min, max), pt)) });
                    return Some(x.clamp(min, max));
                }
                None => notes.push(Note { line, text: format!("{key} is a number (not \u{201c}{v}\u{201d}). Using the standard one.") }),
            }
        }
        theirs.and_then(|v| parse_number(&v, pt)).map(|x| x.clamp(min, max))
    };
    if let Some(x) = number("slant", &mut notes) {
        theme.slant = x;
    }
    // A skin that sets only "slant" gets a path box in proportion (AMD Radeon's 8 and 6).
    let path_default = if own.contains_key("slant") && !own.contains_key("path-slant") { Some(theme.slant * 0.75) } else { None };
    match path_default {
        Some(x) => theme.path_slant = x,
        None => {
            if let Some(x) = number("path-slant", &mut notes) {
                theme.path_slant = x;
            }
        }
    }
    if let Some(x) = number("radius", &mut notes) {
        theme.radius = x;
    }
    if let Some(x) = number("row-height", &mut notes) {
        theme.row_h = x.round();
    }
    if let Some(x) = number("sidebar-row-height", &mut notes) {
        theme.sidebar_row_h = x.round();
    }
    if let Some(x) = number("tab-line-width", &mut notes) {
        theme.tab_line_w = x;
    }
    if let Some(x) = number("toolbar-line-width", &mut notes) {
        theme.toolbar_line_w = x;
    }
    if let Some(x) = number("font-size", &mut notes) {
        theme.font_size = x;
    }
    let (mine, theirs) = value("shape");
    theme.shape = match mine.as_ref().map(|(v, line)| (parse_shape(v), v, *line)) {
        Some((Some(s), _, _)) => s,
        Some((None, v, line)) => {
            notes.push(Note { line, text: format!("shape is slanted, straight or rounded (not \u{201c}{v}\u{201d}).") });
            theirs.as_deref().and_then(parse_shape).unwrap_or_default()
        }
        None => theirs.as_deref().and_then(parse_shape).unwrap_or_default(),
    };
    // Menus follow the skin's darkness unless it says otherwise.
    theme.menus_dark = match own.get("menus") {
        Some((v, line)) => yes_no(v).unwrap_or_else(|| {
            notes.push(Note { line: *line, text: format!("menus is dark or light (not \u{201c}{v}\u{201d}).") });
            dark
        }),
        None => dark,
    };
    let (mine, theirs) = value("font");
    theme.font = font_list(&mine.map(|(v, _)| v).or(theirs).unwrap_or_default());
    let (mine, theirs) = value("logo-text");
    // Not set anywhere, or set to nothing: the standard word (GLINT in the slanted box).
    let logo: String = mine.map(|(v, _)| v).or(theirs).unwrap_or_else(|| DEFAULT_LOGO_TEXT.into());
    if logo.trim().chars().count() > MAX_LOGO_TEXT {
        notes.push(Note { line: own.get("logo-text").map_or(0, |(_, l)| *l), text: format!("The logo fits {MAX_LOGO_TEXT} letters; the rest is left out.") });
    }
    theme.logo_text = logo_word(&logo);

    // The styles: one of a few words each (as the skin says, else as the base says, else the
    // first word, which is how Glint looked before the style existed).
    let mut style = |key: &str, words: &[(&str, bool)], choices: &str| -> bool {
        let read = |v: &str| {
            let v = normalize_key(v);
            words.iter().find(|(w, _)| *w == v).map(|(_, second)| *second)
        };
        let (mine, theirs) = value(key);
        if let Some((v, line)) = mine {
            match read(&v) {
                Some(second) => return second,
                None => notes.push(Note { line, text: format!("{key} is {choices} (not \u{201c}{v}\u{201d}).") }),
            }
        }
        theirs.as_deref().and_then(read).unwrap_or(false)
    };
    if style("buttons", &[("flat", false), ("plain", false), ("liquid", true), ("glass", true)], "flat or liquid") {
        theme.buttons = ButtonStyle::Liquid;
    }
    let tiles = |v: &str| v.trim().eq_ignore_ascii_case("tiles");
    if own.get("caption-buttons").map_or_else(|| base.get("caption-buttons").is_some_and(|v| tiles(v)), |(v, _)| tiles(v)) {
        theme.caption_buttons = CaptionStyle::Tiles;
    } else if style("caption-buttons", &[("symbols", false), ("signs", false), ("marbles", true), ("marble", true)], "symbols, marbles or tiles") {
        theme.caption_buttons = CaptionStyle::Marbles;
    }
    if style("tabs", &[("attached", false), ("pills", true), ("pill", true)], "attached or pills") {
        theme.tabs = TabStyle::Pills;
    }
    if style("sidebar-style", &[("plain", false), ("panels", true), ("panel", true)], "plain or panels") {
        theme.sidebar_style = SidebarStyle::Panels;
    }
    theme.scrollbar_grip = match own.get("scrollbar-grip") {
        Some((v, line)) => yes_no(v).unwrap_or_else(|| {
            notes.push(Note { line: *line, text: format!("scrollbar-grip is true or false (not \u{201c}{v}\u{201d}).") });
            false
        }),
        None => base.get("scrollbar-grip").and_then(|v| yes_no(v)).unwrap_or(false),
    };
    let mut flag = |key: &str| match own.get(key) {
        Some((v, line)) => yes_no(v).unwrap_or_else(|| {
            notes.push(Note { line: *line, text: format!("{key} is true or false (not \u{201c}{v}\u{201d}).") });
            false
        }),
        None => base.get(key).and_then(|v| yes_no(v)).unwrap_or(false),
    };
    theme.bevels = flag("bevels");
    theme.title_across = flag("title-bar-across");
    if let Some(x) = number("see-through", &mut notes) {
        theme.see_through = x / 100.0;
    }
    let (mine, theirs) = value("wallpaper");
    let written = mine.as_ref().map(|(v, _)| v.clone()).or(theirs).unwrap_or_default();
    let written = written.trim().trim_matches(['"', '\'']);
    theme.wallpaper = wallpaper_path(written, dir);
    // A picture the skin names that isn't there: noted (the skin still loads, without it). A
    // network path isn't looked for (nor one on a drive that is a network share): a server that's
    // away would hold up the window.
    if let Some((_, line)) = mine {
        let path = theme.wallpaper.as_str();
        if !path.is_empty() && !on_network(path) && !Path::new(path).exists() {
            let text = if Path::new(written).is_absolute() || written.starts_with('\\') {
                format!("{WALLPAPER_MISSING} \u{201c}{written}\u{201d}. Showing none.")
            } else {
                format!("{WALLPAPER_MISSING} \u{201c}{written}\u{201d}: put it next to the skin, or in the skins folder. Showing none.")
            };
            notes.push(Note { line, text });
        }
    }
    if let Some(x) = number("wallpaper-opacity", &mut notes) {
        theme.wallpaper_opacity = x / 100.0;
    }
    let fit = |v: &str| match normalize_key(v).as_str() {
        "fill" => Some(WallpaperFit::Fill),
        "whole" => Some(WallpaperFit::Whole),
        "stretch" => Some(WallpaperFit::Stretch),
        "tile" => Some(WallpaperFit::Tile),
        "actual" => Some(WallpaperFit::Actual),
        _ => None,
    };
    let (mine, theirs) = value("wallpaper-fit");
    let standard_fit = theirs.as_deref().and_then(fit).unwrap_or_default();
    theme.wallpaper_fit = match mine {
        Some((v, line)) => fit(&v).unwrap_or_else(|| {
            notes.push(Note { line, text: format!("wallpaper-fit is fill, whole, stretch, tile or actual (not \u{201c}{v}\u{201d}).") });
            standard_fit
        }),
        None => standard_fit,
    };
    let (mine, theirs) = value("wallpaper-position");
    let standard_at = theirs.as_deref().and_then(wallpaper_position).unwrap_or((None, None));
    let (across, down) = match mine {
        Some((v, line)) => wallpaper_position(&v).unwrap_or_else(|| {
            notes.push(Note { line, text: format!("wallpaper-position is left, middle or right, then top, middle or bottom, or two percentages like 30% 70% (not \u{201c}{v}\u{201d}).") });
            standard_at
        }),
        None => standard_at,
    };
    if let Some(x) = across {
        theme.wallpaper_at = x;
    }
    theme.wallpaper_at_y = down;
    // 0 its usual size; 100 twice as big and -100 half as big, up to 500 (6 times) either way.
    if let Some(x) = number("wallpaper-zoom", &mut notes) {
        theme.wallpaper_zoom = if x >= 0.0 { 1.0 + x / 100.0 } else { 1.0 / (1.0 - x / 100.0) };
    }
    theme.wallpaper_size = number("wallpaper-size", &mut notes).map(|x| x / 100.0);
    if let Some(x) = number("wallpaper-blur", &mut notes) {
        theme.wallpaper_blur = x.round() as u32;
    }
    if let Some(x) = number("wallpaper-fade", &mut notes) {
        theme.wallpaper_fade = x / 100.0;
    }
    // Two numbers, across then down; each past -2000 or 2000 is that end.
    let (mine, theirs) = value("wallpaper-offset");
    let standard_offset = theirs.as_deref().and_then(two_numbers).map_or((0.0, 0.0), |(x, y)| (x.clamp(-MAX_OFFSET, MAX_OFFSET), y.clamp(-MAX_OFFSET, MAX_OFFSET)));
    theme.wallpaper_offset = match mine {
        Some((v, line)) => match two_numbers(&v) {
            Some((x, y)) => {
                let kept = (x.clamp(-MAX_OFFSET, MAX_OFFSET), y.clamp(-MAX_OFFSET, MAX_OFFSET));
                if kept != (x, y) {
                    notes.push(Note { line, text: format!("wallpaper-offset goes from -{MAX_OFFSET} to {MAX_OFFSET} each way. Using {} {}.", show(kept.0, None), show(kept.1, None)) });
                }
                kept
            }
            None => {
                notes.push(Note { line, text: format!("wallpaper-offset is two numbers, across then down, like 20 -10 (not \u{201c}{v}\u{201d}).") });
                standard_offset
            }
        },
        None => standard_offset,
    };
    if let Some(x) = number("scrollbar-width", &mut notes) {
        theme.scrollbar_w = x;
    }
    // The older Windows the skin looks like, and its icons (the era's unless it says).
    let era_of = |v: &str| match normalize_key(v).trim_start_matches("windows-").trim_start_matches("win") {
        "none" | "" => Some(Era::None),
        "98" | "95" | "me" => Some(Era::Win98),
        "2000" | "2k" | "classic" => Some(Era::Win2000),
        "xp" => Some(Era::Xp),
        "7" | "vista" | "aero" => Some(Era::Win7),
        _ => None,
    };
    let (mine, theirs) = value("era");
    theme.era = match mine {
        Some((v, line)) => era_of(&v).unwrap_or_else(|| {
            notes.push(Note { line, text: format!("era is none, 98, 2000, xp or 7 (not \u{201c}{v}\u{201d}).") });
            Era::None
        }),
        None => theirs.as_deref().and_then(era_of).unwrap_or_default(),
    };
    let icons_of = |v: &str| match normalize_key(v).as_str() {
        "windows" | "shell" | "today" | "modern" | "none" => Some(IconSet::Windows),
        "classic" | "98" | "2000" => Some(IconSet::Classic),
        "xp" | "luna" => Some(IconSet::Xp),
        "aero" | "7" | "vista" => Some(IconSet::Aero),
        _ => None,
    };
    let by_era = match theme.era {
        Era::None => IconSet::Windows,
        Era::Win98 | Era::Win2000 => IconSet::Classic,
        Era::Xp => IconSet::Xp,
        Era::Win7 => IconSet::Aero,
    };
    let (mine, theirs) = value("icons");
    theme.icons = match mine {
        Some((v, line)) => icons_of(&v).unwrap_or_else(|| {
            notes.push(Note { line, text: format!("icons is windows, classic, xp or aero (not \u{201c}{v}\u{201d}).") });
            by_era
        }),
        None => theirs.as_deref().and_then(icons_of).unwrap_or(by_era),
    };
    let (mine, theirs) = value("title-font");
    theme.title_font = font_list(&mine.map(|(v, _)| v).or(theirs).unwrap_or_default());

    let text_of = |key: &str, max: usize| own.get(key).map(|(v, _)| v.trim().chars().take(max).collect::<String>().trim().to_string()).filter(|v| !v.is_empty());
    let name = text_of("name", MAX_NAME).unwrap_or_else(|| fallback_name.chars().take(MAX_NAME).collect());
    let description = text_of("description", MAX_DESCRIPTION).unwrap_or_default();
    let twin = text_of("twin", MAX_NAME).unwrap_or_default();
    notes.sort_by_key(|n| n.line);
    notes.dedup();
    Parsed { theme, name, description, twin, notes }
}

/// The word a skin's `logo-text` puts in the logo's box: at most MAX_LOGO_TEXT letters. GLINT for
/// nothing.
pub fn logo_word(written: &str) -> String {
    let word: String = written.trim().chars().take(MAX_LOGO_TEXT).collect();
    let word = word.trim_end();
    if word.is_empty() {
        DEFAULT_LOGO_TEXT.to_string()
    } else {
        word.to_string()
    }
}

/// DIPs in a point (9 pt = 12 DIPs).
const PT: f32 = 96.0 / 72.0;

/// A size as the skin would write it (in points for font sizes).
fn show(x: f32, pt: Option<f32>) -> String {
    match pt {
        Some(p) => format!("{}pt", (x / p * 2.0).round() / 2.0),
        None => format!("{}", (x * 10.0).round() / 10.0),
    }
}

/// How far `wallpaper-offset` moves a wallpaper at most, each way (DIPs).
const MAX_OFFSET: f32 = 2000.0;

/// Two numbers ("20 -10", "20px, -10px"); None for anything else.
fn two_numbers(v: &str) -> Option<(f32, f32)> {
    let words: Vec<&str> = v.split([' ', ',']).filter(|w| !w.is_empty()).collect();
    match words.as_slice() {
        [x, y] => Some((parse_number(x, None)?, parse_number(y, None)?)),
        _ => None,
    }
}

/// Where a wallpaper sits, from `wallpaper-position`: across (0 its left, 1 its right) and up and
/// down (0 its top, 1 its bottom), each None when it isn't said. "right", "left bottom", "middle
/// top"...: across, then up and down - or the other way round ("top right" is "right top"). Or
/// two percentages, across then down ("30% 70%": 0% the left or top, 100% the right or bottom).
/// None when it isn't understood (a word it doesn't know, more than two, two the same way, or a
/// percentage past 0% to 100%).
fn wallpaper_position(v: &str) -> Option<(Option<f32>, Option<f32>)> {
    let words: Vec<String> = v.to_ascii_lowercase().split([' ', ',']).filter(|w| !w.is_empty()).map(str::to_string).collect();
    if words.len() > 2 {
        return None;
    }
    if words.iter().any(|w| w.ends_with('%')) {
        let part = |w: &str| w.strip_suffix('%')?.trim().parse::<f32>().ok().filter(|x| (0.0..=100.0).contains(x)).map(|x| x / 100.0);
        return match words.as_slice() {
            [x, y] => Some((Some(part(x)?), Some(part(y)?))),
            _ => None,
        };
    }
    let (mut across, mut down, mut either) = (None, None, 0);
    for w in &words {
        match w.as_str() {
            "left" | "right" if across.is_none() => across = Some(if w == "left" { 0.0 } else { 1.0 }),
            "top" | "bottom" if down.is_none() => down = Some(if w == "top" { 0.0 } else { 1.0 }),
            "middle" | "center" | "centre" => either += 1,
            _ => return None,
        }
    }
    // "middle" is whichever way isn't said yet (across first: "middle" alone is the middle across).
    if either > 0 && across.is_none() {
        across = Some(0.5);
        either -= 1;
    }
    if either > 0 && down.is_none() {
        down = Some(0.5);
    }
    Some((across, down))
}

fn parse_shape(v: &str) -> Option<Shape> {
    match normalize_key(v).as_str() {
        "slanted" | "slant" | "slanted-buttons" | "angled" => Some(Shape::Slanted),
        "straight" | "square" | "rect" | "flat" | "none" => Some(Shape::Straight),
        "rounded" | "round" => Some(Shape::Rounded),
        _ => None,
    }
}

/// Dark unless the background the skin gives is light (only when it doesn't say). A gradient
/// counts by its first colour.
fn guess_dark(own: &HashMap<String, (String, usize)>) -> bool {
    let first = |v: &str| {
        let part = split_commas(without_css_wrapper(v))[0].trim().to_string();
        // Without its position ("#f4f7fb 0%").
        let colour = match part.rsplit_once(char::is_whitespace) {
            Some((c, pct)) if pct.ends_with('%') => c.trim().to_string(),
            _ => part,
        };
        parse_hex(&colour).or_else(|| parse_rgb(&colour))
    };
    own.get("background").and_then(|(v, _)| first(v)).is_none_or(|c| c.lightness() < 0.5)
}

/// How the note about a wallpaper that isn't there starts.
const WALLPAPER_MISSING: &str = "Can't find the wallpaper";

/// A wallpaper's full path: a bare file name is looked for next to the skin (in `dir`, its file's
/// folder), then in the skins folder.
fn wallpaper_path(v: &str, dir: Option<&Path>) -> String {
    wallpaper_path_in(v, dir, &folder())
}

/// [`wallpaper_path`] with the skins folder being `skins`.
fn wallpaper_path_in(v: &str, dir: Option<&Path>, skins: &Path) -> String {
    if v.is_empty() {
        return String::new();
    }
    let p = std::path::Path::new(v);
    if p.is_absolute() || v.starts_with('\\') {
        return v.to_string();
    }
    // Next to the skin: another skin's folder may have a picture of the same name.
    if let Some(next_to) = dir.map(|d| d.join(v)).filter(|p| p.exists()) {
        return next_to.to_string_lossy().into_owned();
    }
    // Else in the skins folder itself, or in one of its folders (a skin kept in a folder of its
    // own, with its pictures).
    let here = skins.join(v);
    if here.exists() {
        return here.to_string_lossy().into_owned();
    }
    let inside = std::fs::read_dir(skins).into_iter().flatten().flatten().filter(|i| i.file_type().is_ok_and(|t| t.is_dir())).map(|i| i.path().join(v)).find(|p| p.exists());
    inside.unwrap_or(here).to_string_lossy().into_owned()
}

/// A list of fonts tidied: "\"A\", 'B',C" is "A, B, C" (at most MAX_FONTS, each name at most
/// MAX_FONT_NAME characters: every one is looked up when the skin is applied).
fn font_list(v: &str) -> String {
    v.split(',')
        .map(|f| f.trim().trim_matches(['"', '\'']).trim())
        .filter(|f| !f.is_empty() && f.chars().count() <= MAX_FONT_NAME)
        .take(MAX_FONTS)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A skin's font list: at most this many fonts, each name at most this long.
const MAX_FONTS: usize = 8;
const MAX_FONT_NAME: usize = 64;
/// A skin's name and description are cut at this many characters (they show in lists and menus).
const MAX_NAME: usize = 80;
const MAX_DESCRIPTION: usize = 400;

/// The theme of a built-in skin (read once, then kept).
pub fn built_in_theme(id: &str) -> Theme {
    static THEMES: OnceLock<Vec<(&'static str, Theme)>> = OnceLock::new();
    let all = THEMES.get_or_init(|| BUILT_IN.iter().map(|(id, text)| (*id, parse(text, id).theme)).collect());
    all.iter().find(|(i, _)| *i == id).or_else(|| all.first()).map(|(_, t)| t.clone()).unwrap_or_else(|| parse("", DEFAULT).theme)
}

/// Every colour in [`COLOURS`], in that order, as the skin with `text` makes it (as `parse` does:
/// names followed, what it leaves out from its base skin or from [`DERIVED`]). With each, what it
/// is written as when that is another colour's name ("accent", "accent 16%"), else None. For the
/// customiser, which shows every colour, including those that follow another one.
pub fn colours_of(text: &str) -> Vec<(Paint, Option<String>)> {
    let mut notes = Vec::new();
    let mut own: HashMap<String, (String, usize)> = HashMap::new();
    for l in read_lines(text, &mut notes) {
        if known_key(&l.key) {
            own.insert(l.key, (l.value, l.number));
        }
    }
    let dark = own.get("dark").and_then(|(v, _)| yes_no(v)).unwrap_or_else(|| guess_dark(&own));
    let base_text = BUILT_IN.iter().find(|(id, _)| *id == if dark { DEFAULT } else { DEFAULT_LIGHT }).map_or("", |(_, t)| t);
    let base: HashMap<String, String> = read_lines(base_text, &mut Vec::new()).into_iter().map(|l| (l.key, l.value)).collect();
    let written: Vec<Option<String>> = COLOURS
        .iter()
        .map(|(key, _)| {
            let v = own.get(*key).map(|(v, _)| v.clone()).or_else(|| base.get(*key).cloned()).or_else(|| DERIVED.iter().find(|(k, _)| k == key).map(|(_, v)| v.to_string()))?;
            let v = v.trim();
            // One name, maybe at a strength: "accent", "accent 16%".
            let name = match v.rsplit_once(char::is_whitespace) {
                Some((name, pct)) if pct.ends_with('%') && !v.contains(',') => name.trim(),
                _ => v,
            };
            let name = normalize_key(name);
            (name != *key && COLOURS.iter().any(|(k, _)| *k == name)).then(|| v.to_lowercase())
        })
        .collect();
    let mut r = Resolver { base, own, done: HashMap::new(), busy: HashSet::new(), notes: &mut notes };
    COLOURS.iter().zip(written).map(|((key, _), w)| (r.colour(key).unwrap_or(Paint::Flat(Color::hex(0x808080))), w)).collect()
}

// ---------------------------------------------------------------- changing a skin's text

/// The value of `key` in a skin's text (the line that wins, as written), if it sets it.
pub fn value_in(text: &str, key: &str) -> Option<String> {
    let key = normalize_key(key);
    read_lines(text, &mut Vec::new()).into_iter().rev().find(|l| l.key == key).map(|l| l.value)
}

/// `text` with `key` set to `value`. The line that sets it is changed in place (its note and every
/// other line stay as they are); a skin that doesn't set it yet gets a new line at the end.
pub fn with_value(text: &str, key: &str, value: &str) -> String {
    let key = normalize_key(key);
    let mut lines: Vec<String> = text.trim_start_matches('\u{feff}').lines().map(str::to_string).collect();
    let target = lines.iter().rposition(|l| without_note(l).and_then(split_line).is_some_and(|(k, _)| normalize_key(k) == key));
    let new_line = |old: &str| {
        let indent: String = old.chars().take_while(|c| c.is_whitespace()).collect();
        // The note at the end, with the spaces before it.
        let note = old.find(" //").map(|i| old[old[..i].trim_end().len()..].to_string()).unwrap_or_default();
        format!("{indent}{key}: {value}{note}")
    };
    match target {
        Some(i) => lines[i] = new_line(&lines[i]),
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            lines.push(format!("{key}: {value}"));
        }
    }
    let mut out = lines.join("\r\n");
    out.push_str("\r\n");
    out
}

/// The text of a new skin called `name`, made from `from` (another skin's text): its opening notes
/// are replaced by one saying where it came from.
pub fn copy_text(from: &str, name: &str, came_from: &str) -> String {
    let body: Vec<&str> = from.trim_start_matches('\u{feff}').lines().skip_while(|l| l.trim().is_empty() || l.trim_start().starts_with("//")).collect();
    let header = format!(
        "// {name} - made from {came_from}.\r\n// One setting per line: \"name: value\". Colours are #rrggbb (#rrggbbaa: see-through),\r\n// or a gradient: #fdfdfd, #c3cddb 45%, #8d9db8 (top to bottom; left to right on tall things).\r\n\r\n"
    );
    // A copy is a skin of its own: the original's other half isn't the copy's.
    let body: Vec<&str> = body.into_iter().filter(|l| without_note(l).and_then(split_line).is_none_or(|(k, _)| normalize_key(k) != "twin")).collect();
    let text = format!("{header}{}\r\n", body.join("\r\n"));
    let text = with_value(&text, "name", name);
    // (The line about the skin it came from would say the wrong thing now.)
    with_value(&text, "description", &format!("Made from {came_from}"))
}

// ---------------------------------------------------------------- the list of skins

/// One skin in the list: built in, or a file of the person's.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub dark: bool,
    /// Its light or dark other half as its `twin` line names it ("" for none).
    pub twin: String,
    pub text: String,
    /// The file (a person's own skin); None for a built-in one.
    pub file: Option<PathBuf>,
}

impl Entry {
    fn new(id: String, text: String, file: Option<PathBuf>, fallback_name: &str) -> Entry {
        let p = parse(&text, fallback_name);
        Entry { id, name: p.name, description: p.description, dark: p.theme.dark, twin: p.twin, text, file }
    }

    pub fn built_in(&self) -> bool {
        self.file.is_none()
    }
}

/// The skins folder (in the config folder in use now).
pub fn folder() -> PathBuf {
    settings::config_now().join(FOLDER)
}

/// A skin id as settings.json has it, with "" meaning the default skin.
pub fn id_or_default(id: &str) -> String {
    let t = id.trim();
    if t.is_empty() {
        DEFAULT.to_string()
    } else {
        t.to_string()
    }
}

/// The name a light version has: "X (Light)" for "X".
fn base_name(name: &str) -> &str {
    let t = name.trim();
    match t.len().checked_sub(" (light)".len()) {
        Some(cut) if t.is_char_boundary(cut) && t[cut..].eq_ignore_ascii_case(" (light)") => t[..cut].trim_end(),
        _ => t,
    }
}

/// Every skin there is: the built-in ones, then the person's own (as last read from the folder,
/// with the customiser's changes that aren't on disk yet).
pub struct Library {
    entries: Vec<Entry>,
    /// Bumped whenever the list or a skin's text changes (the Settings window and the customiser
    /// lay themselves out again).
    pub generation: u64,
    /// Edited, not handed to the file worker yet (the customiser's save comes a moment later):
    /// their text in memory wins over a listing.
    dirty: HashSet<String>,
    /// File jobs asked for and finished. A listing read while one was waiting may be out of date: it
    /// is dropped, and the folder is listed again once the worker is done.
    ops_queued: u64,
    ops_done: u64,
    list_again: bool,
    /// The listing asked for last: its number, and how many file jobs had been queued then.
    asked: u64,
    asked_at_ops: (u64, u64),
    hwnd: isize,
    watch: Option<Watch>,
    /// Hidden in the background: nothing is watched.
    paused: bool,
    /// What the file worker said last, for the customiser.
    said: Option<Result<String, String>>,
    /// The file name (lower case, without .glint) of every skin file the last listing saw - also
    /// those it left out (too big, too many): a new or renamed skin never takes one of their names.
    stems: HashSet<String>,
}

/// Results of background work, for the window to take (the listing; file jobs' outcomes).
static LISTED: Mutex<Option<(u64, Listing)>> = Mutex::new(None);
static WRITTEN: Mutex<Vec<Result<String, String>>> = Mutex::new(Vec::new());
/// Files the worker wouldn't write over (a skin file Glint didn't know was there), for the library
/// to move its skin away from (`take_written`), with the skin's own file when it was a rename.
static REFUSED: Mutex<Vec<(PathBuf, Option<PathBuf>)>> = Mutex::new(Vec::new());
/// Every such file, lower case, for as long as Glint runs: a later save to one is refused too, until
/// Glint itself makes or renames a file there.
static NOT_OURS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// A skin file the worker was to delete is still there (the person said no at Windows' question,
/// or Windows couldn't): the folder is listed again, so the skin is back in the list.
static KEPT: AtomicBool = AtomicBool::new(false);

impl Library {
    /// The built-in skins only (a person's own are added by `read_now` or a listing).
    pub fn new() -> Library {
        let entries = BUILT_IN.iter().map(|(id, text)| Entry::new(id.to_string(), text.to_string(), None, id)).collect();
        Library {
            entries,
            generation: 1,
            dirty: HashSet::new(),
            ops_queued: 0,
            ops_done: 0,
            list_again: false,
            asked: 0,
            asked_at_ops: (0, 0),
            hwnd: 0,
            watch: None,
            paused: false,
            said: None,
            stems: HashSet::new(),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The skin with id `id` (capitals don't matter, as in file names). A skin in a folder of its
    /// own is also found by the id Glint 1 gave it, "file:" and its file's name without the folder
    /// ("file:dark" for Pack\dark.glint) - the first such, as Glint 1 gave that id to the first.
    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| same_text(&e.id, id)).or_else(|| {
            let stem = id.get(FILE_PREFIX.len()..).filter(|s| id[..FILE_PREFIX.len()].eq_ignore_ascii_case(FILE_PREFIX) && !s.contains('\\'))?;
            self.entries
                .iter()
                .filter(|e| pack_of(&e.id).is_some() && stem_of(&e.id).is_some_and(|s| same_text(s, stem)))
                .min_by_key(|e| pack_of(&e.id).map(str::to_lowercase))
        })
    }

    /// Ids and names, built-in first.
    #[cfg(test)]
    pub fn names(&self) -> Vec<(String, String)> {
        self.entries.iter().map(|e| (e.id.clone(), e.name.clone())).collect()
    }

    /// At start, when the skin in use is one of the person's own: reads the folder now (it is
    /// needed for the first frame, and its light or dark version may be another file).
    pub fn read_now(&mut self, id: &str) {
        if id.starts_with(FILE_PREFIX) {
            let listing = read_folder(&folder());
            self.stems = listing.stems;
            self.take_own(listing.own);
        }
    }

    /// For tests: one of the person's skins, as if it had been listed.
    #[cfg(test)]
    pub fn add_for_tests(&mut self, id: &str, text: &str) {
        let mut own: Vec<Entry> = self.entries.iter().filter(|e| !e.built_in()).cloned().collect();
        own.push(Entry::new(id.to_string(), text.to_string(), Some(PathBuf::from(format!("C:\\skins\\{id}.glint"))), id));
        self.take_own(own);
    }

    /// The person's own skins from a listing, keeping what the customiser changed but hasn't saved.
    fn take_own(&mut self, listed: Vec<Entry>) {
        let mut own: Vec<Entry> = listed.into_iter().filter(|e| !self.dirty.contains(&e.id)).collect();
        for e in self.entries.iter().filter(|e| !e.built_in() && self.dirty.contains(&e.id)) {
            own.push(e.clone());
        }
        own.sort_by_key(|e| e.name.to_lowercase());
        let mut entries: Vec<Entry> = self.entries.iter().filter(|e| e.built_in()).cloned().collect();
        entries.extend(own);
        if entries != self.entries {
            self.entries = entries;
            self.generation += 1;
        }
    }

    /// The id of `id`'s light (`dark` false) or dark version: the skin its `twin` line names (by id
    /// or by name), or one whose `twin` line names it; else the skin of the same kind (built in, or
    /// the person's) called the same with or without " (Light)". `id` itself when there's none.
    pub fn twin(&self, id: &str, dark: bool) -> String {
        let Some(e) = self.find(id) else { return id.to_string() };
        if e.dark == dark {
            return e.id.clone();
        }
        let named = |o: &Entry, by: &str| !by.is_empty() && (self.find(by).is_some_and(|f| f.id == o.id) || same_text(o.name.trim(), by));
        if let Some(o) = self.entries.iter().find(|o| o.dark == dark && named(o, &e.twin)) {
            return o.id.clone();
        }
        if let Some(o) = self.entries.iter().find(|o| o.dark == dark && named(e, &o.twin)) {
            return o.id.clone();
        }
        let base = base_name(&e.name);
        let want = if dark { base.to_string() } else { format!("{base} (Light)") };
        self.entries
            .iter()
            .find(|o| o.built_in() == e.built_in() && o.dark == dark && o.name.trim().eq_ignore_ascii_case(&want))
            .map_or_else(|| e.id.clone(), |o| o.id.clone())
    }

    /// The look of skin `id`; a skin that isn't there (its file was deleted) looks like AMD Radeon,
    /// light or dark as `dark` says.
    pub fn theme(&self, id: &str, dark: bool) -> Theme {
        match self.find(id) {
            Some(e) if e.built_in() => built_in_theme(&e.id),
            Some(e) => parse_in(&e.text, &e.name, e.file.as_deref().and_then(Path::parent)).theme,
            None => built_in_theme(if dark { DEFAULT } else { DEFAULT_LIGHT }),
        }
    }

    /// The notes about skin `id`'s text (for the customiser). Read as [`Library::theme`] reads it,
    /// so a wallpaper next to the skin's file is found.
    pub fn notes(&self, id: &str) -> Vec<Note> {
        self.find(id).map(|e| parse_in(&e.text, &e.name, e.file.as_deref().and_then(Path::parent)).notes).unwrap_or_default()
    }

    // ------------------------------------------------------------ the person's own, changed

    /// A free name for a new skin, starting from `wanted` ("My skin", "My skin 2"...), whose file
    /// name is free too. A long name is cut to make room for its number: a file name keeps only
    /// MAX_STEM characters, so "... 2", "... 3" cut there would all be the same file.
    pub fn free_name(&self, wanted: &str) -> String {
        self.free_name_besides(wanted, &[])
    }

    /// [`Library::free_name`], keeping clear of the names in `also` too (skins about to be added).
    fn free_name_besides(&self, wanted: &str, also: &[String]) -> String {
        let wanted = wanted.trim();
        let wanted = if wanted.is_empty() { "My skin" } else { wanted };
        let taken = |name: &str| {
            let id = id_for(name);
            self.entries.iter().any(|e| same_text(e.name.trim(), name) || same_text(&e.id, &id))
                || self.stems.contains(&file_stem(name).to_lowercase())
                || also.iter().any(|a| same_text(a.trim(), name) || same_text(&id_for(a), &id))
        };
        if !taken(wanted) {
            return wanted.to_string();
        }
        let numbered = |n: usize| {
            let room = MAX_STEM.saturating_sub(n.to_string().len() + 1);
            format!("{} {n}", wanted.chars().take(room).collect::<String>().trim_end())
        };
        (2..).map(numbered).find(|n| !taken(n)).unwrap_or_else(|| wanted.to_string())
    }

    /// Adds a skin of the person's with this text (called `name`, a free one) and saves it.
    /// Returns its id.
    pub fn add(&mut self, name: &str, text: &str) -> String {
        self.add_in(&folder(), None, name, text)
    }

    /// [`Library::add`], its file in `dir`: the skins folder (`pack` None), or its folder `pack`.
    fn add_in(&mut self, dir: &Path, pack: Option<&str>, name: &str, text: &str) -> String {
        let id = id_in(pack, &file_stem(name));
        let file = dir.join(format!("{}.{EXTENSION}", file_stem(name)));
        let entry = Entry::new(id.clone(), text.to_string(), Some(file.clone()), name);
        let at = self.entries.iter().position(|e| !e.built_in() && e.name.to_lowercase() > entry.name.to_lowercase()).unwrap_or(self.entries.len());
        self.entries.insert(at, entry);
        self.generation += 1;
        self.stems.insert(file_stem(name).to_lowercase());
        // Written as a new file: if one of that name turned up meanwhile, it isn't replaced.
        if let Some(e) = self.entries.iter().find(|e| e.id == id) {
            let job = FileJob::Write { file, text: e.text.clone(), name: e.name.clone(), new: true };
            self.queue(job);
        }
        id
    }

    /// A new text for one of the person's skins (saved a moment later with `save`).
    pub fn set_text(&mut self, id: &str, text: &str) {
        let Some(e) = self.entries.iter_mut().find(|e| e.id == id && !e.built_in()) else { return };
        if e.text == text {
            return;
        }
        let p = parse(text, &e.name);
        e.text = text.to_string();
        e.name = p.name;
        e.description = p.description;
        e.dark = p.theme.dark;
        e.twin = p.twin;
        self.dirty.insert(id.to_string());
        self.generation += 1;
    }

    /// Whether the customiser has changes to this skin that aren't saved yet.
    pub fn unsaved(&self, id: &str) -> bool {
        self.dirty.contains(id)
    }

    /// Writes one of the person's skins to its file (on the file worker).
    pub fn save(&mut self, id: &str) {
        self.dirty.remove(id);
        let Some(e) = self.entries.iter().find(|e| e.id == id) else { return };
        let Some(file) = e.file.clone() else { return };
        self.queue(FileJob::Write { file, text: e.text.clone(), name: e.name.clone(), new: false });
    }

    /// Saves everything not saved yet (the customiser closing).
    pub fn save_all(&mut self) {
        for id in self.dirty.clone() {
            self.save(&id);
        }
    }

    /// Renames one of the person's skins: its name in the text and its file. Returns the new id.
    pub fn rename(&mut self, id: &str, new_name: &str) -> Option<String> {
        let new_name = new_name.trim();
        let at = self.entries.iter().position(|e| e.id == id && !e.built_in())?;
        if new_name.is_empty() || self.entries[at].name == new_name {
            return None;
        }
        let new_id = id_in(pack_of(id), &file_stem(new_name));
        let old_file = self.entries[at].file.clone()?;
        if !self.free_for(at, new_name) {
            return None; // the customiser picks free names, and says so
        }
        // (In the folder it's in: a skin in a folder of its own stays next to its pictures.)
        let new_file = old_file.with_file_name(format!("{}.{EXTENSION}", file_stem(new_name)));
        let text = with_value(&self.entries[at].text, "name", new_name);
        self.dirty.remove(id);
        let e = &mut self.entries[at];
        e.text = text.clone();
        e.name = new_name.to_string();
        e.id = new_id.clone();
        e.file = Some(new_file.clone());
        // Built-in ones first in their own order, then the person's by name.
        self.entries.sort_by_key(|e| (!e.built_in(), BUILT_IN.iter().position(|(b, _)| *b == e.id).unwrap_or(0), e.name.to_lowercase()));
        self.generation += 1;
        self.stems.insert(file_stem(new_name).to_lowercase());
        // One job: the file is renamed and written - or, when the new name turned out to be taken,
        // left as it was (nothing is written over the other file).
        self.queue(FileJob::Rename { from: old_file, to: new_file, text, name: new_name.to_string() });
        Some(new_id)
    }

    /// Whether the skin at `at` may be called `new_name`: no other skin has that file name (a change
    /// of capitals only is fine), nor a file the listing left out.
    fn free_for(&self, at: usize, new_name: &str) -> bool {
        let stem = file_stem(new_name);
        let new_id = id_in(pack_of(&self.entries[at].id), &stem);
        let same_stem = self.entries[at].file.as_ref().and_then(|f| f.file_stem()).is_some_and(|s| same_text(&s.to_string_lossy(), &stem));
        let other = self.entries.iter().enumerate().any(|(i, e)| i != at && same_text(&e.id, &new_id));
        !other && (same_stem || !self.stems.contains(&stem.to_lowercase()))
    }

    /// Whether one of the person's skins, `id`, may be called `new_name`: no other skin is called
    /// that, and its file name is free ([`Library::rename`]'s check). A change of capitals only is
    /// fine.
    pub fn may_rename(&self, id: &str, new_name: &str) -> bool {
        let new_name = new_name.trim();
        let Some(at) = self.entries.iter().position(|e| e.id == id && !e.built_in()) else { return false };
        let named = self.entries.iter().enumerate().any(|(i, e)| i != at && same_text(e.name.trim(), new_name));
        !named && self.free_for(at, new_name)
    }

    /// Whether deleting skin `id` puts its file in the Recycle Bin. A USB stick or a network drive
    /// has none: there the file is deleted for good.
    pub fn recycles(&self, id: &str) -> bool {
        self.find(id).and_then(|e| e.file.as_ref()).is_some_and(|f| fileops::has_recycle_bin(&f.to_string_lossy()))
    }

    /// Deletes one of the person's skins: its file goes to the Recycle Bin. Returns whether it does
    /// (`recycles`): false where there's none, the file being deleted for good.
    pub fn delete(&mut self, id: &str) -> bool {
        let recycled = self.recycles(id);
        let Some(at) = self.entries.iter().position(|e| e.id == id && !e.built_in()) else { return false };
        let e = self.entries.remove(at);
        self.dirty.remove(id);
        self.generation += 1;
        if let Some(file) = e.file {
            self.queue(FileJob::Delete { file, name: e.name });
        }
        recycled
    }

    /// Exports a skin's text to `to` (any file the person picked).
    pub fn export(&mut self, id: &str, to: PathBuf) {
        if let Some(e) = self.find(id) {
            let (text, name) = (e.text.clone(), e.name.clone());
            self.queue(FileJob::Export { file: to, text, name });
        }
    }

    /// Adds what IMPORT… brought in ([`prepare_import`]): the skin, and its other half when that
    /// came too, each with a free name (a number after it when the name is taken, as for a new
    /// skin), the two naming each other with `twin` when their names don't pair up by themselves.
    /// Returns the skin's id, what to say (what was imported, and what it needs that didn't come),
    /// and whether that is a problem.
    pub fn take_import(&mut self, import: Import) -> (String, String, bool) {
        let pair = import.skins.len() == 2;
        // (Light, dark) when the two are a pair.
        let light_dark = |names: &[String]| if import.skins[0].dark { (names[1].clone(), names[0].clone()) } else { (names[0].clone(), names[1].clone()) };
        let by_name = |(light, dark): (String, String)| {
            base_name(&light).eq_ignore_ascii_case(dark.trim()) && light.trim().eq_ignore_ascii_case(&format!("{} (Light)", base_name(&dark)))
        };
        let wanted: Vec<String> = import.skins.iter().map(|s| s.name.clone()).collect();
        let mut names: Vec<String> = Vec::new();
        if pair && import.skins[0].dark != import.skins[1].dark && by_name(light_dark(&wanted)) {
            // "X" and "X (Light)": the dark one's name first, and the light one's from it ("X 2"
            // and "X 2 (Light)" when "X" is taken), so they still pair up by name.
            let (light, dark) = light_dark(&wanted);
            let dark_name = self.free_name(&dark);
            let light_wanted = if dark_name == dark { light } else { format!("{dark_name} (Light)") };
            let light_name = self.free_name_besides(&light_wanted, std::slice::from_ref(&dark_name));
            names = if import.skins[0].dark { vec![dark_name, light_name] } else { vec![light_name, dark_name] };
        } else {
            for w in &wanted {
                let name = self.free_name_besides(w, &names);
                names.push(name);
            }
        }
        let pairs_by_name = pair && import.skins[0].dark != import.skins[1].dark && by_name(light_dark(&names));
        let mut ids = Vec::new();
        for (i, s) in import.skins.iter().enumerate() {
            let name = &names[i];
            let mut text = if *name == s.name && value_in(&s.text, "name").is_some() { s.text.clone() } else { with_value(&s.text, "name", name) };
            if pair {
                // Its other half by the name it has now (with a number, or under a file name the old
                // `twin` line didn't say).
                let other = &names[1 - i];
                let names_it = value_in(&text, "twin").map(|t| t.trim().eq_ignore_ascii_case(other));
                if names_it == Some(false) || (names_it.is_none() && !pairs_by_name) {
                    text = with_value(&text, "twin", other);
                }
            }
            ids.push(self.add_in(&import.dir, import.pack.as_deref(), name, &text));
        }
        let (Some(first), Some(id)) = (import.skins.first(), ids.first().cloned()) else { return (String::new(), "There was no skin to import".into(), true) };
        let mut said = format!("Imported {}", names[0]);
        if pair {
            said.push_str(&format!(" and {}", names[1]));
        }
        let mut problems = Vec::new();
        for (i, s) in import.skins.iter().enumerate() {
            let whose = |file: &str| if i == 0 { format!("its picture {file}") } else { format!("the picture {file} of {}", names[i]) };
            problems.extend(s.problems.iter().map(|m| match m {
                Missing::NotFound(file) => format!("{} wasn't found", whose(file)),
                Missing::NotCopied(file, why) => format!("{} couldn't be copied ({why})", whose(file)),
            }));
        }
        // An other half its `twin` line names that wasn't next to it - and isn't one of the skins
        // already (a built-in one, or one imported before).
        let twin = import.twin_missing.trim();
        let named = |o: &Entry| o.id.eq_ignore_ascii_case(twin) || o.name.trim().eq_ignore_ascii_case(twin);
        if !twin.is_empty() && !self.entries.iter().any(|o| o.dark != first.dark && named(o)) {
            let version = if first.dark { "light" } else { "dark" };
            problems.push(format!("its {version} version \u{201c}{twin}\u{201d} wasn't found"));
        }
        if !problems.is_empty() {
            said.push_str(&format!(", but {}", problems.join(", and ")));
        }
        // Lines skipped (a picture said to be missing already isn't said again).
        let skipped: usize = ids
            .iter()
            .zip(&import.skins)
            .map(|(id, s)| self.notes(id).iter().filter(|n| s.problems.is_empty() || !n.text.starts_with(WALLPAPER_MISSING)).count())
            .sum();
        if skipped > 0 {
            said.push_str(&format!(". Skipped {skipped} line(s): see Show as text"));
        }
        (id, said, !problems.is_empty() || skipped > 0)
    }

    fn queue(&mut self, job: FileJob) {
        self.ops_queued += 1;
        if !queue(self.hwnd, job) {
            // No thread could be made: it's as good as done (and said so).
            self.ops_done += 1;
            WRITTEN.lock().unwrap_or_else(PoisonError::into_inner).push(Err("Windows couldn't save the skin right now. Try again.".into()));
            post(self.hwnd, EVENT_WRITTEN);
        }
    }

    // ------------------------------------------------------------ the folder

    /// The window exists: watch the folder and list it in the background.
    pub fn window_ready(&mut self, hwnd: HWND) {
        self.hwnd = hwnd.0 as isize;
        self.resume();
    }

    /// Hidden in the background: nothing is watched.
    pub fn pause(&mut self) {
        self.paused = true;
        self.watch = None;
    }

    /// Back from the background (or the window is new): watch the folder again and list it, in case
    /// something changed meanwhile.
    pub fn resume(&mut self) {
        self.paused = false;
        self.watch_folder();
        self.refresh();
    }

    /// What the file worker said last ("Saved My skin", or a problem), once.
    pub fn take_said(&mut self) -> Option<Result<String, String>> {
        self.said.take()
    }

    /// Watches the folder if it isn't watched (it may not have existed before).
    pub fn watch_folder(&mut self) {
        if self.paused || self.hwnd == 0 || self.watch.as_ref().is_some_and(Watch::alive) {
            return;
        }
        self.watch = Watch::start(folder(), self.hwnd);
    }

    /// Lists the folder in the background; the result comes back as WM_APP_SKINS.
    pub fn refresh(&mut self) {
        if self.hwnd == 0 {
            return;
        }
        self.asked += 1;
        self.asked_at_ops = (self.ops_queued, self.ops_done);
        let (n, hwnd, dir) = (self.asked, self.hwnd, folder());
        let started = shell::background("skins: list", move || {
            let listing = read_folder(&dir);
            *LISTED.lock().unwrap_or_else(PoisonError::into_inner) = Some((n, listing));
            post(hwnd, EVENT_LISTED);
        });
        if !started {
            log::line("skins: no thread for listing the skins folder");
        }
    }

    /// Takes in a listing (WM_APP_SKINS, EVENT_LISTED). Returns whether anything changed.
    pub fn take_listing(&mut self) -> bool {
        let Some((n, listing)) = LISTED.lock().unwrap_or_else(PoisonError::into_inner).take() else { return false };
        if n != self.asked {
            return false; // an older listing; a newer one is on its way
        }
        // Read while a file job was waiting or running: may be out of date.
        let (queued, done) = self.asked_at_ops;
        if queued != done || self.ops_queued != queued {
            self.list_again = true;
            return false;
        }
        let before = self.generation;
        self.stems = listing.stems;
        self.take_own(listing.own);
        self.generation != before
    }

    /// Takes in what the file worker finished (WM_APP_SKINS, EVENT_WRITTEN): problems to tell, and
    /// skins that moved to a new name (old id, new id) because the file their name needed turned out
    /// to be another one's. The last thing the worker said is kept for the customiser (`take_said`).
    pub fn take_written(&mut self) -> (Vec<String>, Vec<(String, String)>) {
        let refused: Vec<(PathBuf, Option<PathBuf>)> = std::mem::take(&mut *REFUSED.lock().unwrap_or_else(PoisonError::into_inner));
        let moved = refused.iter().flat_map(|(file, from)| self.move_off(file, from.as_deref())).collect();
        let results: Vec<Result<String, String>> = std::mem::take(&mut *WRITTEN.lock().unwrap_or_else(PoisonError::into_inner));
        let mut problems = Vec::new();
        for r in results {
            self.ops_done += 1;
            match &r {
                Err(e) => problems.push(e.clone()),
                Ok(s) if s.is_empty() => continue,
                Ok(_) => {}
            }
            self.said = Some(r);
        }
        if KEPT.swap(false, Ordering::SeqCst) {
            self.list_again = true;
        }
        if self.ops_done >= self.ops_queued {
            self.ops_done = self.ops_queued;
            // The folder may have been made just now.
            self.watch_folder();
            if std::mem::take(&mut self.list_again) {
                self.refresh();
            }
        }
        (problems, moved)
    }

    /// The person's skins whose file is `file` - one Glint wouldn't write over - get a free name and
    /// a file of their own: `from` (the skin's file before a rename that was refused) renamed to it,
    /// or a new one. Returns (old id, new id) for each.
    fn move_off(&mut self, file: &Path, from: Option<&Path>) -> Vec<(String, String)> {
        if let Some(stem) = file.file_stem() {
            self.stems.insert(stem.to_string_lossy().to_lowercase());
        }
        let same = |e: &Entry| e.file.as_ref().is_some_and(|f| f.as_os_str().eq_ignore_ascii_case(file.as_os_str()));
        let mut moved = Vec::new();
        while let Some(at) = self.entries.iter().position(|e| !e.built_in() && same(e)) {
            let (old_id, old_name) = (self.entries[at].id.clone(), self.entries[at].name.clone());
            let name = self.free_name(&old_name);
            let (new_id, new_file) = (id_in(pack_of(&old_id), &file_stem(&name)), file.with_file_name(format!("{}.{EXTENSION}", file_stem(&name))));
            let text = with_value(&self.entries[at].text, "name", &name);
            let e = &mut self.entries[at];
            (e.id, e.name, e.text, e.file) = (new_id.clone(), name.clone(), text.clone(), Some(new_file.clone()));
            self.stems.insert(file_stem(&name).to_lowercase());
            if self.dirty.remove(&old_id) {
                self.dirty.insert(new_id.clone());
            }
            self.generation += 1;
            match from {
                Some(from) => self.queue(FileJob::Rename { from: from.to_path_buf(), to: new_file, text, name }),
                None => self.queue(FileJob::Write { file: new_file, text, name, new: true }),
            }
            moved.push((old_id, new_id));
        }
        moved
    }
}

/// The id of a person's skin called `name` (by the name its file gets) in the skins folder itself.
pub fn id_for(name: &str) -> String {
    id_in(None, &file_stem(name))
}

/// The id of the person's skin in file `stem`.glint: "file:stem" in the skins folder itself,
/// "file:Pack\stem" in its folder Pack - always, whatever other files there are, so the id saved
/// in settings stays the skin's.
fn id_in(pack: Option<&str>, stem: &str) -> String {
    match pack {
        Some(pack) => format!("{FILE_PREFIX}{pack}\\{stem}"),
        None => format!("{FILE_PREFIX}{stem}"),
    }
}

/// The folder a person's skin is in, from its id ("Pack" of "file:Pack\dark"); None in the skins
/// folder itself (and for a built-in one).
fn pack_of(id: &str) -> Option<&str> {
    id.strip_prefix(FILE_PREFIX)?.rsplit_once('\\').map(|(pack, _)| pack)
}

/// The file name (without .glint) in a person's skin's id ("dark" of "file:Pack\dark").
fn stem_of(id: &str) -> Option<&str> {
    let rest = id.strip_prefix(FILE_PREFIX)?;
    Some(rest.rsplit_once('\\').map_or(rest, |(_, stem)| stem))
}

/// Whether two names are the same but for capitals - in any letters, as Windows' file names are
/// ("Élan" is "élan").
fn same_text(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
}

/// The longest a skin's file name gets (without .glint), in characters.
const MAX_STEM: usize = 60;

/// A file name (without .glint) for a skin's name: the characters Windows doesn't allow in names
/// left out, not empty, not one of Windows' device names.
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c }).collect();
    let mut stem: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    stem = stem.trim_matches(['.', ' ']).chars().take(MAX_STEM).collect::<String>().trim_end_matches(['.', ' ']).to_string();
    if stem.is_empty() {
        stem = "My skin".into();
    }
    let upper = stem.to_uppercase();
    let device = ["CON", "PRN", "AUX", "NUL"].contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT")) && upper.len() == 4 && upper.as_bytes()[3].is_ascii_digit());
    if device {
        stem.push_str(" skin");
    }
    stem
}

/// Text from a file's bytes: UTF-8 (with or without its mark), UTF-16 (Notepad's "Unicode", and
/// "Unicode big endian"), or else the PC's own old code page (Notepad's "ANSI", what it saved before
/// Windows 10: an é in it isn't UTF-8).
pub fn decode(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| String::from_utf16_lossy(&rest.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect::<Vec<u16>>());
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes);
    }
    if std::str::from_utf8(bytes).is_err() && !bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        // SAFETY: both buffers are slices the call only reads and writes within.
        let len = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None) };
        let mut wide = vec![0u16; len.max(0) as usize];
        // SAFETY: as above.
        let len = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide)) };
        if len > 0 {
            wide.truncate(len as usize);
            return String::from_utf16_lossy(&wide);
        }
    }
    String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string()
}

/// Whether `path` is somewhere on the network: a \\server\share path, or one on a drive letter
/// that is a network share (asking Windows about the drive doesn't reach the server).
fn on_network(path: &str) -> bool {
    if path.starts_with(r"\\") || path.starts_with("//") || path.starts_with(r"\/") || path.starts_with(r"/\") {
        return true;
    }
    let b = path.as_bytes();
    if !(b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':') {
        return false;
    }
    let root: Vec<u16> = format!("{}:\\", b[0] as char).encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated root path that outlives the call.
    unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) == 4 } // DRIVE_REMOTE
}

/// What a listing of the skins folder found.
struct Listing {
    /// The person's skins, by name.
    own: Vec<Entry>,
    /// Every *.glint file's name (lower case, without .glint), also those left out of `own`.
    stems: HashSet<String>,
}

/// The person's skins in `dir`: every *.glint file (not too many, not too big), by name - also
/// those in its folders, one level down (a skin in a folder of its own, with its pictures).
/// A skin's id is "file:" and its file's name, with its folder's name for one in a folder
/// ("file:Pack\dark"): each stays a skin of its own (a dark.glint in two folders), and keeps its
/// id when other files come or go.
fn read_folder(dir: &Path) -> Listing {
    let mut found = Listing { own: Vec::new(), stems: HashSet::new() };
    let Ok(listing) = std::fs::read_dir(dir) else { return found };
    // Each with the name of the folder it's in (None: `dir` itself).
    let mut items: Vec<(Option<String>, std::fs::DirEntry)> = Vec::new();
    for item in listing.flatten() {
        if item.file_type().is_ok_and(|t| t.is_dir()) {
            let pack = item.file_name().to_string_lossy().into_owned();
            items.extend(std::fs::read_dir(item.path()).into_iter().flatten().flatten().map(|i| (Some(pack.clone()), i)));
        } else {
            items.push((None, item));
        }
    }
    items.sort_by_cached_key(|(pack, item)| (pack.as_ref().map(|p| p.to_lowercase()), item.file_name().to_string_lossy().to_lowercase()));
    for (pack, item) in items {
        let path = item.path();
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION)) {
            continue;
        }
        let Some(stem) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
        found.stems.insert(stem.to_lowercase());
        if found.own.len() >= MAX_OWN || !item.file_type().is_ok_and(|t| t.is_file()) || item.metadata().is_ok_and(|m| m.len() > MAX_FILE_BYTES) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let id = id_in(pack.as_deref(), &stem);
        found.own.push(Entry::new(id, decode(&bytes), Some(path), &stem));
    }
    found.own.sort_by_key(|e| e.name.to_lowercase());
    found
}

/// Wakes the main window about skins (safe from any thread; a stale handle just fails).
fn post(hwnd: isize, event: usize) {
    if hwnd == 0 {
        return;
    }
    // SAFETY: posting to a window by handle; if it's gone the message is simply dropped.
    unsafe {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_SKINS, WPARAM(event), LPARAM(0));
    }
}

// ---------------------------------------------------------------- importing

/// A skin file read for IMPORT…, with what it needs brought into the skins folder
/// ([`prepare_import`]); the library adds it ([`Library::take_import`]).
#[derive(Debug)]
pub struct Import {
    /// The folder its file goes in: the skins folder, a folder of its own made there (it came in
    /// one, with its pictures), or the folder it was picked in when that's in the skins folder.
    pub dir: PathBuf,
    /// `dir`'s name when it's one of the skins folder's folders (None: the skins folder itself).
    pub pack: Option<String>,
    /// The skin, then its light or dark other half when that was next to it.
    pub skins: Vec<Incoming>,
    /// The other half its `twin` line names when that wasn't next to it ("" otherwise): the
    /// library says so, unless it's one of the skins already.
    pub twin_missing: String,
}

/// One skin file of an [`Import`].
#[derive(Debug)]
pub struct Incoming {
    /// Its name (from its text, else its file's).
    pub name: String,
    pub dark: bool,
    /// Its text, its `wallpaper` line naming the picture brought in (under a new name when one of
    /// the old name was there already).
    pub text: String,
    pub problems: Vec<Missing>,
}

/// A picture a skin names that didn't come with it.
#[derive(Debug, PartialEq)]
pub enum Missing {
    /// Neither next to the skin nor in the skins folder (its file name).
    NotFound(String),
    /// There, but copying it failed (its file name, and why).
    NotCopied(String, String),
}

/// What a picture's file name ends with, for telling a skin's own folder (one with nothing but its
/// skins and pictures in it) from any other.
const PICTURES: [&str; 12] = ["jpg", "jpeg", "jfif", "png", "bmp", "gif", "webp", "tif", "tiff", "heic", "avif", "ico"];
/// Files a skin's own folder may have besides (a note to read, and what Windows leaves there).
const ALSO_IN_A_SKIN_FOLDER: [&str; 5] = ["txt", "md", "url", "ini", "db"];
/// A skin's own folder has at most this many files; at most this many skin files next to the one
/// picked are read when looking for its other half.
const MAX_SKIN_FOLDER_FILES: usize = 32;
const MAX_TWIN_CANDIDATES: usize = 64;

/// A skin file next to the one picked (or that one).
struct Neighbour {
    file: PathBuf,
    stem: String,
    name: String,
    dark: bool,
    twin: String,
    text: String,
}

impl Neighbour {
    fn read(file: &Path) -> Result<Neighbour, String> {
        let too_big = std::fs::metadata(file).is_ok_and(|m| m.len() > MAX_FILE_BYTES);
        if too_big {
            return Err(format!("{} is too big to be a skin", file.display()));
        }
        let bytes = std::fs::read(file).map_err(|e| format!("Couldn't read {}: {e}", file.display()))?;
        let text = decode(&bytes);
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported skin".into());
        let p = parse(&text, &stem);
        Ok(Neighbour { file: file.to_path_buf(), stem, name: p.name, dark: p.theme.dark, twin: p.twin, text })
    }

    /// Whether `by` (a `twin` line) names this one: by name or by id, as the library would.
    fn named(&self, by: &str) -> bool {
        let by = by.trim();
        !by.is_empty() && (self.name.trim().eq_ignore_ascii_case(by) || format!("{FILE_PREFIX}{}", self.stem).eq_ignore_ascii_case(by))
    }
}

/// Whether two paths are the same folder (or file).
fn same_place(a: &Path, b: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    real(a) == real(b)
}

/// Reads the skin file `file` for IMPORT… and brings what it needs into `skins` (the skins folder):
/// - the picture its `wallpaper` line names, when that's a file name (or a path from the skin's
///   folder) and the picture is next to it: copied in next to where the skin goes. A full path is
///   left as it is.
/// - its light or dark other half, when that's next to it (with that one's picture).
/// - when it's in a folder of its own (nothing there but it, its other half, pictures and a note),
///   a folder of that name is made in the skins folder for them (one level down, as skins are read).
///
/// Nothing is written over: a folder or picture of that name that is there already makes the new
/// one "name 2" (as a skin of a taken name gets a number) - unless the picture there is the very
/// same one, which then serves both. Picked in the skins folder (or one of its folders) already, the
/// skin's copy goes next to it and uses the pictures there. Touches the disk: call it on a
/// background thread. Err: the skin itself couldn't be read.
pub fn prepare_import(file: &Path, skins: &Path) -> Result<Import, String> {
    let me = Neighbour::read(file)?;
    let from = file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let twin = find_twin(&me, &from);
    let twin_missing = if twin.is_none() { me.twin.trim().to_string() } else { String::new() };
    let mine: Vec<PathBuf> = std::iter::once(&me).chain(twin.as_ref()).map(|n| n.file.clone()).collect();
    let inside = same_place(&from, skins) || from.parent().is_some_and(|p| same_place(p, skins));
    let _ = std::fs::create_dir_all(skins);
    let dir = if inside {
        from.clone()
    } else if own_folder(&from, &mine, [&me].into_iter().chain(twin.as_ref())) {
        let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        make_folder(skins, &name).unwrap_or_else(|| skins.to_path_buf())
    } else {
        skins.to_path_buf()
    };
    // Pictures brought in so far (where from, and the name each has now): the two halves may share
    // one.
    let mut brought: Vec<(PathBuf, String)> = Vec::new();
    let mut incoming = Vec::new();
    for n in std::iter::once(me).chain(twin) {
        let mut problems = Vec::new();
        let text = bring_picture(&n.text, &from, &dir, skins, &mut brought, &mut problems);
        incoming.push(Incoming { name: n.name, dark: n.dark, text, problems });
    }
    let pack = (!same_place(&dir, skins)).then(|| dir.file_name().map(|n| n.to_string_lossy().into_owned())).flatten();
    Ok(Import { dir, pack, skins: incoming, twin_missing })
}

/// The skin's `wallpaper` line as written, without its quotes ("" for none).
fn wallpaper_written(text: &str) -> String {
    value_in(text, "wallpaper").unwrap_or_default().trim().trim_matches(['"', '\'']).to_string()
}

/// The picture a skin's text names next to it (in `from`), when it names one by a file name or a
/// path from there and it's there.
fn picture_next_to(text: &str, from: &Path) -> Option<PathBuf> {
    let v = wallpaper_written(text);
    let relative = !v.is_empty() && !Path::new(&v).is_absolute() && !v.starts_with('\\');
    Some(from.join(&v)).filter(|p| relative && p.is_file())
}

/// The skin's light or dark other half next to it in `from`: the skin file its `twin` line names,
/// or one whose `twin` line names it, or else the one called the same with or without " (Light)"
/// (as `Library::twin` pairs them).
fn find_twin(me: &Neighbour, from: &Path) -> Option<Neighbour> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(from)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|i| i.file_type().is_ok_and(|t| t.is_file()))
        .map(|i| i.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION)) && !same_place(p, &me.file))
        .collect();
    files.sort_by_cached_key(|p| p.to_string_lossy().to_lowercase());
    let others: Vec<Neighbour> = files.iter().take(MAX_TWIN_CANDIDATES).filter_map(|f| Neighbour::read(f).ok()).filter(|o| o.dark != me.dark).collect();
    let base = base_name(&me.name);
    let want = if me.dark { format!("{base} (Light)") } else { base.to_string() };
    let at = others
        .iter()
        .position(|o| o.named(&me.twin))
        .or_else(|| others.iter().position(|o| me.named(&o.twin)))
        .or_else(|| others.iter().position(|o| o.name.trim().eq_ignore_ascii_case(&want)))?;
    others.into_iter().nth(at)
}

/// Whether `from` is a skin's own folder: in it, nothing but `mine` (the skin and its other half),
/// pictures - one of them a skin's - and a note or two; not too many files; not a drive.
fn own_folder<'a>(from: &Path, mine: &[PathBuf], skins: impl Iterator<Item = &'a Neighbour>) -> bool {
    if from.parent().is_none() || from.file_name().is_none() {
        return false;
    }
    let Ok(listing) = std::fs::read_dir(from) else { return false };
    let mut files = 0;
    for item in listing.flatten() {
        if item.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        files += 1;
        let path = item.path();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let fine = if ext == EXTENSION { mine.iter().any(|m| same_place(m, &path)) } else { PICTURES.contains(&ext.as_str()) || ALSO_IN_A_SKIN_FOLDER.contains(&ext.as_str()) };
        if !fine || files > MAX_SKIN_FOLDER_FILES {
            return false;
        }
    }
    let mut skins = skins;
    files > 0 && skins.any(|n| picture_next_to(&n.text, from).is_some_and(|p| p.parent().is_some_and(|d| same_place(d, from))))
}

/// `name` with a number after it, cut so a long one still has room for it ("name 2").
fn numbered(name: &str, n: usize) -> String {
    let room = MAX_STEM.saturating_sub(n.to_string().len() + 1);
    format!("{} {n}", name.chars().take(room).collect::<String>().trim_end())
}

/// Makes a new folder in `skins` called `name` - or "name 2", "name 3"... when that's taken.
/// None if it couldn't be made.
fn make_folder(skins: &Path, name: &str) -> Option<PathBuf> {
    let name = file_stem(name);
    for n in 1..1000 {
        let try_name = if n == 1 { name.clone() } else { numbered(&name, n) };
        let dir = skins.join(&try_name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Some(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                log::line(&format!("skins: couldn't make the folder {}: {e}", dir.display()));
                return None;
            }
        }
    }
    None
}

/// `text` with the picture its `wallpaper` line names next to it (in `from`) copied into `dir`, the
/// line naming the copy. A picture that's there and isn't found, or can't be copied, is put in
/// `problems` (the line is left as it is).
fn bring_picture(text: &str, from: &Path, dir: &Path, skins: &Path, brought: &mut Vec<(PathBuf, String)>, problems: &mut Vec<Missing>) -> String {
    let v = wallpaper_written(text);
    if v.is_empty() {
        return text.to_string();
    }
    let file_name = |p: &Path| p.file_name().map_or_else(|| v.clone(), |n| n.to_string_lossy().into_owned());
    let written = Path::new(&v);
    if written.is_absolute() || v.starts_with('\\') {
        // A full path is left as it is (a network one isn't looked for: the server may be away).
        if !v.starts_with(r"\\") && !written.exists() {
            problems.push(Missing::NotFound(file_name(written)));
        }
        return text.to_string();
    }
    let Some(picture) = picture_next_to(text, from) else {
        // Not next to it: maybe in the skins folder already (where a skin looks too).
        if !Path::new(&wallpaper_path_in(&v, Some(dir), skins)).exists() {
            problems.push(Missing::NotFound(file_name(written)));
        }
        return text.to_string();
    };
    if same_place(from, dir) {
        return text.to_string(); // the copy is next to the skin it's made from: so is the picture
    }
    let name = match brought.iter().find(|(p, _)| same_place(p, &picture)) {
        Some((_, name)) => name.clone(),
        None => match copy_picture(&picture, dir) {
            Ok(name) => {
                brought.push((picture.clone(), name.clone()));
                name
            }
            Err(e) => {
                problems.push(Missing::NotCopied(file_name(&picture), e.to_string()));
                return text.to_string();
            }
        },
    };
    if name == v {
        text.to_string()
    } else {
        with_value(text, "wallpaper", &name)
    }
}

/// Copies `picture` into `dir` under its own name - or "name 2.jpg"... when a different file has
/// that name (nothing is written over; the very same picture there already is used as it is).
/// Returns the name it has there.
fn copy_picture(picture: &Path, dir: &Path) -> std::io::Result<String> {
    let name = picture.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| std::io::Error::other("it has no file name"))?;
    let (stem, ext) = match picture.extension() {
        Some(e) => (picture.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), format!(".{}", e.to_string_lossy())),
        None => (name.clone(), String::new()),
    };
    for n in 1..1000 {
        let try_name = if n == 1 { name.clone() } else { format!("{}{ext}", numbered(&stem, n)) };
        let to = dir.join(&try_name);
        if to.exists() {
            if same_bytes(picture, &to) {
                return Ok(try_name);
            }
            continue;
        }
        match copy_new(picture, &to) {
            Ok(()) => return Ok(try_name),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("every name for it is taken"))
}

/// Copies `from` to `to`, a file that mustn't be there yet; a copy that fails is taken away again.
fn copy_new(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut source = std::fs::File::open(from)?;
    let len = source.metadata()?.len();
    let mut copy = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
    // (What a copy says it copied isn't reliable everywhere - under Wine it's 0 - so the copy itself
    // is measured.)
    let done = std::io::copy(&mut source, &mut copy).and_then(|_| copy.sync_all()).and_then(|()| {
        if std::fs::metadata(to)?.len() == len {
            Ok(())
        } else {
            Err(std::io::Error::other("it wasn't copied whole"))
        }
    });
    drop(copy);
    if done.is_err() {
        let _ = std::fs::remove_file(to);
    }
    done
}

/// Whether two files hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    if same_place(a, b) {
        return true;
    }
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    if ma.len() != mb.len() || !mb.is_file() {
        return false;
    }
    let (Ok(fa), Ok(fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else { return false };
    let (mut ra, mut rb) = (std::io::BufReader::new(fa), std::io::BufReader::new(fb));
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let Ok(n) = ra.read(&mut ba) else { return false };
        if n == 0 {
            return true;
        }
        if rb.read_exact(&mut bb[..n]).is_err() || ba[..n] != bb[..n] {
            return false;
        }
    }
}

// ---------------------------------------------------------------- the file worker

/// Something done to a skin file.
enum FileJob {
    /// Write the text (atomically: a new file, then renamed over the old one). `new`: a skin just
    /// made - a file already there (one Glint didn't know about) is never written over.
    Write { file: PathBuf, text: String, name: String, new: bool },
    /// A renamed skin: its file follows and gets the new text - unless the new name is taken.
    Rename { from: PathBuf, to: PathBuf, text: String, name: String },
    /// To the Recycle Bin.
    Delete { file: PathBuf, name: String },
    /// A copy somewhere else (Export…).
    Export { file: PathBuf, text: String, name: String },
}

/// The worker's queue, while its thread runs.
static WORKER: Mutex<Option<Sender<(isize, FileJob)>>> = Mutex::new(None);
/// The worker ends after this long without work (and starts again when there is some).
const WORKER_IDLE: Duration = Duration::from_secs(10);

/// Jobs handed to the worker and not finished yet (`wait_for_writes`).
static PENDING: AtomicU64 = AtomicU64::new(0);

/// Hands a job to the file worker (starting it if needed). False if no thread could be made.
fn queue(hwnd: isize, job: FileJob) -> bool {
    let mut worker = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
    PENDING.fetch_add(1, Ordering::SeqCst);
    let mut job = (hwnd, job);
    if let Some(tx) = worker.as_ref() {
        match tx.send(job) {
            Ok(()) => return true,
            Err(e) => job = e.0, // its thread has ended: start a new one
        }
    }
    let (tx, rx) = channel::<(isize, FileJob)>();
    let _ = tx.send(job);
    let started = shell::background("skins: files", move || {
        let _com = shell::ComGuard::init();
        let finish = |(hwnd, job): (isize, FileJob)| {
            let result = run_job(job);
            if let Err(e) = &result {
                log::error(e);
            }
            WRITTEN.lock().unwrap_or_else(PoisonError::into_inner).push(result);
            PENDING.fetch_sub(1, Ordering::SeqCst);
            post(hwnd, EVENT_WRITTEN);
        };
        loop {
            match rx.recv_timeout(WORKER_IDLE) {
                Ok(job) => finish(job),
                Err(RecvTimeoutError::Timeout) => {
                    // Nothing for a while: end - unless a job slipped in meanwhile. (Senders hold
                    // the same lock, so none can send to this thread once it has let go.)
                    let mut w = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
                    match rx.try_recv() {
                        Ok(job) => {
                            drop(w);
                            finish(job);
                        }
                        Err(_) => {
                            *w = None;
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    if started {
        *worker = Some(tx);
    } else {
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
    started
}

/// Waits (at most `max`) for skin files still being written: called when Glint ends, after its
/// window is gone, so a change made just before quitting isn't lost.
pub fn wait_for_writes(max: Duration) {
    let until = Instant::now() + max;
    while PENDING.load(Ordering::SeqCst) > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Does one file job. Ok: what was done; Err: what went wrong, in plain words.
fn run_job(job: FileJob) -> Result<String, String> {
    match job {
        FileJob::Write { file, text, name, new } => {
            if (new && file.exists()) || (!new && not_ours(&file)) {
                refuse(&file, None);
                return Err(format!("Couldn't save the skin \u{201c}{name}\u{201d}: {} is another file (it was left as it is)", file.display()));
            }
            write_atomic(&file, &text).map_err(|e| format!("Couldn't save the skin \u{201c}{name}\u{201d}: {e}"))?;
            ours_now(&file);
            Ok(format!("Saved {name}"))
        }
        FileJob::Export { file, text, name } => {
            write_atomic(&file, &text).map(|()| format!("Exported {name}")).map_err(|e| format!("Couldn't export \u{201c}{name}\u{201d}: {e}"))
        }
        FileJob::Rename { from, to, text, name } => {
            // (Any letters: Windows takes "élan" and "Élan" as one file, so that's a change of capitals.)
            let same_file = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
            if to.exists() && !same_file {
                refuse(&to, Some(&from));
                return Err(format!("Couldn't rename the skin to \u{201c}{name}\u{201d}: {} is another file (it was left as it is)", to.display()));
            }
            // (A skin never saved yet has no file to rename: writing makes it.)
            if from.exists() && from != to {
                std::fs::rename(&from, &to).map_err(|e| format!("Couldn't rename the skin's file: {e}"))?;
            }
            write_atomic(&to, &text).map_err(|e| format!("Couldn't save the skin \u{201c}{name}\u{201d}: {e}"))?;
            ours_now(&to);
            Ok(format!("Saved {name}"))
        }
        FileJob::Delete { file, name } => {
            if !file.exists() {
                return Ok(String::new());
            }
            // (Where there's no Recycle Bin, Windows deletes it for good: Glint says so.)
            let bin = fileops::has_recycle_bin(&file.to_string_lossy());
            let before = if bin { bin_items(&file) } else { None };
            let problem = match recycle(&file, bin) {
                // Where the Recycle Bin is switched off for the drive, Windows asked and deleted it
                // for good: the Recycle Bin holds no more than before.
                Ok(true) if bin && before.is_some_and(|n| bin_items(&file).is_some_and(|now| now <= n)) => return Ok(format!("Deleted {name}")),
                Ok(true) => return Ok(if bin { format!("{name} is in the Recycle Bin") } else { format!("Deleted {name}") }),
                Ok(false) => format!("{name} wasn't deleted"),
                Err(e) => format!("Couldn't delete the skin \u{201c}{name}\u{201d}: {e}"),
            };
            // (Still there: listed again, the skin is back in the list.)
            KEPT.store(true, Ordering::SeqCst);
            Err(problem)
        }
    }
}

/// A file Glint won't write over: the library moves its skin away from it (`REFUSED`), and later
/// saves to it are refused too (`NOT_OURS`).
fn refuse(file: &Path, from: Option<&Path>) {
    REFUSED.lock().unwrap_or_else(PoisonError::into_inner).push((file.to_path_buf(), from.map(Path::to_path_buf)));
    let key = file.to_string_lossy().to_lowercase();
    let mut not_ours = NOT_OURS.lock().unwrap_or_else(PoisonError::into_inner);
    if !not_ours.contains(&key) {
        not_ours.push(key);
    }
}

/// Whether `file` is one Glint refused to write over (and hasn't made its own since).
fn not_ours(file: &Path) -> bool {
    let key = file.to_string_lossy().to_lowercase();
    NOT_OURS.lock().unwrap_or_else(PoisonError::into_inner).contains(&key)
}

/// Glint made or renamed a skin file at `file` itself: saving there is fine from now on.
fn ours_now(file: &Path) {
    let key = file.to_string_lossy().to_lowercase();
    NOT_OURS.lock().unwrap_or_else(PoisonError::into_inner).retain(|k| *k != key);
}

/// Writes `text` to `file` atomically: a new file next to it, flushed, then renamed over it.
fn write_atomic(file: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    match written.and_then(|()| std::fs::rename(&tmp, file)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Sends a file to the Recycle Bin (Windows' own way, without asking: Glint asked already). On a
/// drive without one (a USB stick, a network drive) Windows deletes it for good. `bin`: Glint said
/// it goes to the Recycle Bin, so where it can't after all (the Recycle Bin is switched off for that
/// drive) Windows asks before it deletes it for good. Ok(false): the person said no there.
fn recycle(file: &Path, bin: bool) -> Result<bool, String> {
    let mut from: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(file.as_os_str()).collect();
    from.extend([0, 0]);
    let flags = FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI;
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (if bin { flags | FOF_WANTNUKEWARNING } else { flags }).0 as u16,
        ..Default::default()
    };
    // SAFETY: the struct and the double-NUL-terminated list live across the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    match code {
        // Stopped at Windows' question (ERROR_CANCELLED, DE_OPCANCELLED).
        _ if op.fAnyOperationsAborted.as_bool() || matches!(code, 1223 | 0x75) => Ok(false),
        0 => Ok(true),
        _ => Err(format!("Windows said {code:#x}")),
    }
}

/// How many things the Recycle Bin of `file`'s drive holds (None when Windows doesn't say).
fn bin_items(file: &Path) -> Option<i64> {
    let root: Vec<u16> = file.to_string_lossy().chars().take(3).collect::<String>().encode_utf16().chain([0]).collect();
    let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
    // SAFETY: a NUL-terminated root path and a struct with its size set, both alive for the call.
    unsafe { SHQueryRecycleBinW(PCWSTR(root.as_ptr()), &mut info) }.ok().map(|_| info.i64NumItems)
}

// ---------------------------------------------------------------- watching the folder

/// A Windows event object, closed when the last one holding it lets go.
struct Signal(HANDLE);

// SAFETY: an event handle may be used and closed from any thread.
unsafe impl Send for Signal {}
// SAFETY: as above; setting and waiting are thread-safe operations on the kernel object.
unsafe impl Sync for Signal {}

impl Drop for Signal {
    fn drop(&mut self) {
        // SAFETY: closes the event we created; nobody uses it after this (both holders are done).
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Watches the skins folder on a thread of its own, which sleeps until a file in it changes (a skin
/// edited in Notepad, copied in, deleted) and then tells the window (WM_APP_SKINS,
/// EVENT_CHANGED). Stops when dropped.
pub struct Watch {
    stop: Arc<Signal>,
    alive: Arc<AtomicBool>,
}

impl Watch {
    /// Starts watching `dir`. If it isn't there (no skin was saved yet) the thread ends at once
    /// and `alive` says so; the library starts a new watch after it saves something.
    fn start(dir: PathBuf, hwnd: isize) -> Option<Watch> {
        // SAFETY: a plain unnamed manual-reset event; the handle is owned by the Signal.
        let stop = Arc::new(Signal(unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.ok()?));
        let alive = Arc::new(AtomicBool::new(true));
        let (thread_stop, thread_alive) = (stop.clone(), alive.clone());
        let started = shell::background("skins: watch", move || {
            watch_thread(&dir, hwnd, &thread_stop);
            thread_alive.store(false, Ordering::SeqCst);
        });
        started.then_some(Watch { stop, alive })
    }

    /// Whether the folder is being watched (false once its thread ended).
    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: our own event, still open (the thread holds it too).
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
    }
}

/// How long the folder must be quiet before a change is reported (an editor saving writes a
/// file, renames and deletes in a burst), and the longest a burst is waited out.
const QUIET: u32 = 150;
const LONGEST_BURST: Duration = Duration::from_secs(1);

fn watch_thread(dir: &Path, hwnd: isize, stop: &Signal) {
    let path = HSTRING::from(dir.as_os_str());
    // The subfolders too: a skin can sit in a folder of its own.
    // SAFETY: `path` lives across the call; the handle is closed below.
    let Ok(change) = (unsafe { FindFirstChangeNotificationW(&path, true, FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE) })
    else {
        return;
    };
    let handles = [change, stop.0];
    // SAFETY: both handles stay open for the whole loop (the change handle is closed after it, the
    // event by its last holder).
    unsafe {
        'watching: loop {
            if WaitForMultipleObjects(&handles, false, INFINITE) != WAIT_OBJECT_0 {
                break;
            }
            let first = Instant::now();
            loop {
                if FindNextChangeNotification(change).is_err() {
                    break 'watching;
                }
                let w = WaitForMultipleObjects(&handles, false, QUIET);
                if w.0 == WAIT_OBJECT_0.0 + 1 {
                    break 'watching;
                }
                if w != WAIT_OBJECT_0 || first.elapsed() >= LONGEST_BURST {
                    break;
                }
            }
            post(hwnd, EVENT_CHANGED);
        }
        let _ = FindCloseChangeNotification(change);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_skins_read_cleanly_with_their_colours() {
        for (id, text) in BUILT_IN {
            let p = parse(text, id);
            assert!(p.notes.is_empty(), "{id}: {:?}", p.notes);
        }
        // AMD Radeon: layered charcoal, one red accent, slanted; and its light version.
        let dark = built_in_theme(DEFAULT);
        assert!(dark.dark && dark.menus_dark);
        assert_eq!(dark.window, Color::hex(0x0e0e10));
        assert_eq!(dark.titlebar, dark.window);
        assert_eq!(dark.input, dark.window);
        assert_eq!(dark.accent, Color::hex(0xe3262d));
        assert!((dark.accent_tint.a - 0.16).abs() < 1e-6 && dark.accent_tint.r == dark.accent.r);
        assert_eq!(dark.chip_hover, Color::hex(0x2c2c31));
        assert_eq!(dark.bar_track, Color::hex(0x2a2a2f));
        assert_eq!((dark.shape, dark.slant, dark.path_slant, dark.row_h), (Shape::Slanted, 8.0, 6.0, 28.0));
        assert_eq!((dark.tab_line_w, dark.toolbar_line_w, dark.font_size), (3.0, 2.0, 12.0));
        assert_eq!(dark.font, "Segoe UI Variable Text, Segoe UI");
        let light = built_in_theme(DEFAULT_LIGHT);
        assert!(!light.dark && !light.menus_dark);
        assert_eq!(light.input, Color::hex(0xffffff));
        assert!((light.accent_tint.a - 0.12).abs() < 1e-6);
        assert!((light.caption_pressed.a - 0.7).abs() < 1e-6);
        // NVIDIA: near-black, green, square.
        let nv = built_in_theme("nvidia");
        assert_eq!((nv.accent, nv.window, nv.shape), (Color::hex(0x76b900), Color::hex(0x0f0f0f), Shape::Straight));
        assert_eq!(nv.lean(8.0), 0.0);
        assert_eq!(nv.path_lean(), 0.0);
        assert_eq!(nv.tab_line, nv.accent);
    }

    #[test]
    fn the_logo_is_the_glint_box_unless_a_skin_names_another_word() {
        // The built-in skins have no word of their own: every one shows GLINT in its box.
        for (id, text) in BUILT_IN {
            assert_eq!(value_in(text, "logo-text"), None, "{id}");
            let t = built_in_theme(id);
            assert!(!t.chrome_logo() && t.logo_text == DEFAULT_LOGO_TEXT, "{id}");
        }
        // No word, or just spaces: GLINT.
        for text in ["name: Moss\n", "logo-text:   \n", "logo text = GLINT\n", "logo-text: \"\"  // none\n"] {
            let p = parse(text, "x");
            assert!(p.notes.is_empty(), "{text:?}: {:?}", p.notes);
            assert_eq!(p.theme.logo_text, DEFAULT_LOGO_TEXT, "{text:?}");
        }
        // Nothing after the colon: GLINT too (the chrome logo is the "Logo" setting, not a skin's).
        assert_eq!(parse("logo-text:\n", "x").theme.logo_text, DEFAULT_LOGO_TEXT);
        // Any other word: a box in the skin's colours with that word.
        let p = parse("logo-text: MOSS\nlogo: #3a7d44\nlogo-letters: #000000\n", "x");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert!(!p.theme.chrome_logo());
        assert_eq!(p.theme.logo_text, "MOSS");
        assert_eq!((p.theme.logo, p.theme.logo_ink), (Color::hex(0x3a7d44), Color::hex(0x000000)));
        assert_eq!(parse("logo-text: Glint Dark", "x").theme.logo_text, "Glint Dark");
        // What the customiser writes for a word typed in its Logo box.
        assert_eq!(logo_word("   "), DEFAULT_LOGO_TEXT);
        assert_eq!(logo_word(""), DEFAULT_LOGO_TEXT);
        assert_eq!(logo_word(" Moss "), "Moss");
        assert_eq!(logo_word("ABCDEFGHIJKLMNOP"), "ABCDEFGHIJKL");
        // Changing a skin's word in place, and clearing it.
        let old = "name: Mine\nlogo-text: GLINT\n";
        assert_eq!(parse(old, "x").theme.logo_text, DEFAULT_LOGO_TEXT);
        let worded = with_value(old, "logo-text", "MINE");
        assert_eq!(parse(&worded, "x").theme.logo_text, "MINE");
        let cleared = with_value(&worded, "logo-text", "");
        assert_eq!(parse(&cleared, "x").theme.logo_text, DEFAULT_LOGO_TEXT);
        assert!(parse(&cleared, "x").notes.is_empty());
    }

    #[test]
    fn a_skin_of_a_few_lines_takes_the_rest_from_amd_radeon() {
        let p = parse("name: Moss\naccent: #3a7d44\nbackground: #101410\n", "moss");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert_eq!(p.name, "Moss");
        let t = p.theme;
        assert!(t.dark, "guessed from the dark background");
        assert_eq!(t.accent, Color::hex(0x3a7d44));
        // What names other colours follows them.
        assert_eq!((t.titlebar, t.sidebar, t.statusbar, t.input), (t.window, t.window, t.window, t.window));
        assert_eq!((t.logo, t.tab_line, t.toolbar_line, t.select_edge), (t.accent, t.accent, t.accent, t.accent));
        assert_eq!(t.accent_tint, t.accent.with_alpha(0.16));
        assert_eq!(t.panel, Color::hex(0x1b1b1e));
        // A light background means a light skin, with AMD Radeon (Light) underneath.
        let light = parse("background: #f0f0f0", "x").theme;
        assert!(!light.dark && !light.menus_dark);
        assert_eq!(light.panel, Color::hex(0xffffff));
    }

    #[test]
    fn mistakes_are_noted_and_skipped() {
        let text = "// notes are fine\n\
                    # so is this\n\
                    accent: reddish\n\
                    wobble: 3\n\
                    just some words\n\
                    slant: 99\n\
                    row height: nine\n\
                    shape: hexagon\n\
                    background: line\n\
                    line: background\n\
                    font-size: 30pt\n\
                    logo text: SUPERCALIFRAGILISTIC\n";
        let p = parse(text, "oops");
        let lines: Vec<usize> = p.notes.iter().map(|n| n.line).collect();
        for want in [3, 4, 5, 6, 7, 8, 11, 12] {
            assert!(lines.contains(&want), "line {want} should have a note: {:?}", p.notes);
        }
        assert!(!lines.contains(&1) && !lines.contains(&2));
        let t = p.theme;
        assert_eq!(t.accent, Color::hex(0xe3262d), "a broken colour is the standard one");
        assert_eq!(t.slant, 20.0, "kept in range");
        assert_eq!(t.row_h, 28.0);
        assert_eq!(t.shape, Shape::Slanted);
        assert_eq!(t.font_size, 14.0 * PT);
        assert_eq!(t.logo_text.chars().count(), MAX_LOGO_TEXT);
        // A circle (background -> line -> background) falls back instead of hanging.
        assert!(t.window.a > 0.0);
    }

    #[test]
    fn colours_take_every_form() {
        let p = parse("accent: #abc\nbackground: #11223380\ntoolbar: rgb(10, 20, 30)\npanel: none\nhover: accent 50%\nline: 'Accent'\n", "x");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        let t = p.theme;
        assert_eq!(t.accent, Color::hex(0xaabbcc));
        assert_eq!(hex_of(t.window), "#11223380");
        assert_eq!(t.toolbar, Color::rgba(10, 20, 30, 1.0));
        assert_eq!(t.panel.a, 0.0);
        assert_eq!(t.hover, Color::hex(0xaabbcc).with_alpha(0.5));
        assert_eq!(t.line, t.accent);
        assert_eq!(hex_of(Color::hex(0x76b900)), "#76b900");
        assert_eq!(parse_hex("#76B900"), Some(Color::hex(0x76b900)));
        assert_eq!(parse_hex("76b900"), None);
        assert_eq!(parse_hex("#76b90"), None);
    }

    #[test]
    fn names_are_forgiving() {
        assert_eq!(normalize_key("  Title Bar "), "title-bar");
        assert_eq!(normalize_key("title_bar"), "title-bar");
        assert_eq!(normalize_key("WINDOW"), "background");
        assert_eq!(normalize_key("accent-tint"), "selection");
        let p = parse("Accent = #00ff00;\nFont-Family: \"Cascadia Code\", 'Consolas'\nfont size: 12px\nshape: Rounded\nradius: 4px\n", "x");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert_eq!(p.theme.accent, Color::hex(0x00ff00));
        assert_eq!(p.theme.font, "Cascadia Code, Consolas");
        assert_eq!(p.theme.font_size, 12.0);
        assert_eq!((p.theme.shape, p.theme.radius), (Shape::Rounded, 4.0));
        assert_eq!(p.theme.lean(8.0), 0.0);
        assert_eq!(p.theme.corner(30.0), 4.0);
        // Only "slant": the path's box follows in proportion.
        let s = parse("slant: 12", "x").theme;
        assert_eq!((s.slant, s.path_slant, s.lean(8.0), s.lean(6.0)), (12.0, 9.0, 12.0, 9.0));
    }

    #[test]
    fn changing_a_value_keeps_the_rest_of_the_text() {
        let text = "// my notes\r\nname: Mine\r\naccent: #e3262d   // red\r\n\r\ntext: #ffffff\r\n";
        let changed = with_value(text, "Accent", "#76b900");
        assert_eq!(changed, "// my notes\r\nname: Mine\r\naccent: #76b900   // red\r\n\r\ntext: #ffffff\r\n");
        assert_eq!(value_in(&changed, "accent").as_deref(), Some("#76b900"));
        let added = with_value(text, "shape", "straight");
        assert!(added.ends_with("text: #ffffff\r\nshape: straight\r\n"));
        assert_eq!(value_in(&added, "SHAPE").as_deref(), Some("straight"));
        // Two lines for the same thing: the one that counts (the last) changes.
        let twice = with_value("accent: #111111\naccent: #222222\n", "accent", "#333333");
        assert_eq!(twice, "accent: #111111\r\naccent: #333333\r\n");
        // A copy gets its own name and a new note at the top.
        let copy = copy_text(BUILT_IN[2].1, "My skin", "NVIDIA");
        let p = parse(&copy, "x");
        assert_eq!(p.name, "My skin");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert_eq!(p.theme, built_in_theme("nvidia"));
        assert!(copy.starts_with("// My skin - made from NVIDIA"));
    }

    #[test]
    fn light_and_dark_versions_pair_up() {
        let mut lib = Library::new();
        assert_eq!(lib.twin(DEFAULT, false), DEFAULT_LIGHT);
        assert_eq!(lib.twin(DEFAULT_LIGHT, true), DEFAULT);
        assert_eq!(lib.twin("nvidia", false), "nvidia-light");
        assert_eq!(lib.twin("nvidia-light", false), "nvidia-light");
        assert_eq!(lib.twin("file:gone", true), "file:gone");
        // The person's own: by name, only among their own.
        let own = vec![
            Entry::new("file:Moss".into(), "name: Moss\ndark: true\n".into(), Some(PathBuf::from("C:\\s\\Moss.glint")), "Moss"),
            Entry::new("file:moss light".into(), "name: Moss (Light)\ndark: false\n".into(), Some(PathBuf::from("C:\\s\\moss light.glint")), "x"),
            Entry::new("file:Alone".into(), "name: Alone\n".into(), Some(PathBuf::from("C:\\s\\Alone.glint")), "Alone"),
            Entry::new("file:fake".into(), "name: NVIDIA (Light)\ndark: false\n".into(), Some(PathBuf::from("C:\\s\\fake.glint")), "x"),
        ];
        lib.take_own(own);
        assert_eq!(lib.twin("file:Moss", false), "file:moss light");
        assert_eq!(lib.twin("file:moss light", true), "file:Moss");
        assert_eq!(lib.twin("file:Alone", false), "file:Alone");
        assert_eq!(lib.twin("nvidia", false), "nvidia-light", "a built-in pairs with a built-in");
        // Built-ins first, then the person's by name.
        let names: Vec<String> = lib.names().into_iter().map(|(_, n)| n).collect();
        assert_eq!(names, ["AMD Radeon", "AMD Radeon (Light)", "NVIDIA", "NVIDIA (Light)", "Chrome", "Gunmetal", "Alone", "Moss", "Moss (Light)", "NVIDIA (Light)"]);
        assert_eq!(lib.free_name("Moss"), "Moss 2");
        assert_eq!(lib.free_name("New"), "New");
    }

    #[test]
    fn ids_and_file_names() {
        assert_eq!(id_or_default(""), DEFAULT);
        assert_eq!(id_or_default(" "), DEFAULT);
        assert_eq!(id_or_default("nvidia"), "nvidia");
        assert_eq!(id_or_default("file:Mine"), "file:Mine");
        assert_eq!(file_stem("My: skin?"), "My skin");
        assert_eq!(file_stem(" ..con "), "con skin");
        assert_eq!(file_stem("COM1"), "COM1 skin");
        assert_eq!(file_stem("///"), "My skin");
        assert_eq!(id_for("Night / Day"), "file:Night Day");
        assert_eq!(base_name("Moss (Light)"), "Moss");
        assert_eq!(base_name("Moss (light)"), "Moss");
        assert_eq!(base_name("Moss"), "Moss");
        assert_eq!(decode(b"\xef\xbb\xbfname: x"), "name: x");
        assert_eq!(decode(&[0xFF, 0xFE, b'a', 0, b':', 0]), "a:");
        assert_eq!(decode(&[0xFE, 0xFF, 0, b'a', 0, b':']), "a:");
    }

    #[test]
    fn a_long_name_still_finds_a_free_one() {
        // A file name keeps 60 characters: each copy's number has to fit in them (or "... 2",
        // "... 3" are all the same file, none is ever free, and Glint hangs looking).
        let mut lib = Library::new();
        let long = "A skin whose name is far too long to fit in a file name of its own";
        let mut ids = HashSet::new();
        for n in 1..=12 {
            let name = lib.free_name(long);
            assert!(n == 1 || name.ends_with(&format!(" {n}")), "{name}");
            assert!(ids.insert(id_for(&name).to_lowercase()), "{name} is taken");
            lib.add_for_tests(&id_for(&name), &format!("name: {name}\n"));
        }
    }

    #[test]
    fn files_are_written_atomically_and_listed() {
        let dir = std::env::temp_dir().join(format!("glint-skin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("Mine.glint");
        write_atomic(&file, "name: Mine\naccent: #123456\n").unwrap();
        write_atomic(&file, "name: Mine\naccent: #654321\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a skin").unwrap();
        std::fs::write(dir.join("Other.GLINT"), "accent: #00ff00").unwrap();
        // A skin in a folder of its own, next to its picture.
        std::fs::create_dir_all(dir.join("Pink")).unwrap();
        std::fs::write(dir.join("Pink").join("Pink.glint"), "wallpaper: pink.png").unwrap();
        std::fs::write(dir.join("Pink").join("pink.png"), "not really a picture").unwrap();
        // Another of the same file name in a folder of its own, with its own picture of that name.
        std::fs::create_dir_all(dir.join("Rose")).unwrap();
        std::fs::write(dir.join("Rose").join("Pink.glint"), "name: Rose\nwallpaper: pink.png").unwrap();
        std::fs::write(dir.join("Rose").join("pink.png"), "not really a picture").unwrap();
        let listing = read_folder(&dir);
        let listed = &listing.own;
        let ids: Vec<&str> = listed.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["file:Mine", "file:Other", "file:Pink\\Pink", "file:Rose\\Pink"]);
        let mut lib = Library::new();
        lib.take_own(listed.clone());
        assert_eq!(lib.find("file:Rose\\Pink").map(|e| e.name.as_str()), Some("Rose"));
        assert_eq!(lib.theme("file:Pink", true).wallpaper, dir.join("Pink").join("pink.png").to_string_lossy());
        // Its picture next to it is found for the customiser's notes too.
        assert!(lib.notes("file:Pink").is_empty() && lib.notes("file:Rose\\Pink").is_empty(), "{:?}", lib.notes("file:Pink"));
        assert_eq!(lib.theme("file:Rose\\Pink", true).wallpaper, dir.join("Rose").join("pink.png").to_string_lossy());
        assert!(listed[0].text.contains("#654321"));
        assert_eq!(listed[1].name, "Other", "no name: the file's name");
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hostile_skin_never_stops_glint() {
        // Odd bytes, odd numbers, odd lengths: every one is read without a panic or a hang, and every
        // size stays in its range.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces = [
            "accent", "background", "slant", "row-height", "font-size", "radius", "font", "name", "logo-text", ":", "=", " ", "%", "#", "//", "\n",
            "NaN", "inf", "-inf", "1e39", "-0", "999999999999", "rgb(", ")", ",", "accent 50%", "accent NaN%", "\u{feff}", "\u{1f600}", "\"", "'", "{", "}",
            "title-bar", "#fff", "45%", "linear-gradient(", "to bottom", "buttons", "liquid", "twin", "scrollbar-width", "tabs", "pills",
        ];
        for _ in 0..400 {
            let mut text = String::new();
            for _ in 0..(next() % 60) {
                text.push_str(pieces[(next() % pieces.len() as u64) as usize]);
            }
            let bytes: Vec<u8> = (0..(next() % 40)).map(|_| next() as u8).collect();
            text.push_str(&decode(&bytes));
            let t = parse(&text, "fuzz").theme;
            assert!((0.0..=20.0).contains(&t.slant) && (0.0..=16.0).contains(&t.radius) && (20.0..=48.0).contains(&t.row_h), "{text:?}");
            assert!(t.font_size.is_finite() && t.accent.a.is_finite() && t.hover.a.is_finite() && t.accent_tint.a.is_finite(), "{text:?}");
            assert!(t.titlebar.r.is_finite() && (4.0..=18.0).contains(&t.scrollbar_w), "{text:?}");
            if let Some(g) = t.titlebar_paint.gradient() {
                assert!(g.stops().windows(2).all(|s| s[0].at <= s[1].at) && g.stops().len() <= MAX_STOPS, "{text:?}");
            }
            let _ = with_value(&text, "accent", "#123456");
        }
        // Huge values are cut down to size.
        let long = format!("name: {}\ndescription: {}\nfont: {}\nselection: accent NaN%\n", "n".repeat(10_000), "d".repeat(10_000), "F, ".repeat(5_000));
        let p = parse(&long, "x");
        assert!(p.name.chars().count() <= MAX_NAME && p.description.chars().count() <= MAX_DESCRIPTION);
        assert!(p.theme.font.split(", ").count() <= MAX_FONTS);
        assert!(p.theme.accent_tint.a.is_finite(), "a strength that isn't a number is the standard one");
        assert!(p.notes.iter().any(|n| n.line == 4));
    }

    /// Every colour, shape and size of a theme that existed before paints and styles, as text.
    fn old_fields(t: &Theme) -> String {
        let h = hex_of;
        format!(
            "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {:?} {} {} {} {} {} {} {} {} {}",
            t.dark, h(t.window), h(t.titlebar), h(t.toolbar), h(t.panel), h(t.sidebar), h(t.statusbar), h(t.input), h(t.chip), h(t.chip_hover),
            h(t.text), h(t.text_strong), h(t.muted), h(t.disabled), h(t.accent), h(t.accent_tint), h(t.select_edge), h(t.on_accent), h(t.hover),
            h(t.button_hover), h(t.button_pressed), h(t.crumb_hover), h(t.line), h(t.scroll_thumb), h(t.scroll_thumb_hover), h(t.bar_track),
            h(t.logo), h(t.logo_ink), h(t.tab_line), h(t.toolbar_line), h(t.caption_hover), h(t.caption_pressed), h(t.close_hover),
            t.shape, t.menus_dark, t.slant, t.path_slant, t.radius, t.row_h, t.tab_line_w, t.toolbar_line_w, t.font, t.font_size
        )
    }

    #[test]
    fn the_older_skins_look_exactly_as_they_did() {
        // As the four skins read before gradients, the new colours and the styles existed.
        let before = [
            ("amd-radeon", "true #0e0e10 #0e0e10 #151517 #1b1b1e #0e0e10 #0e0e10 #0e0e10 #232327 #2c2c31 #e8e8ea #ffffff #8d8d96 #4a4a50 #e3262d #e3262d29 #e3262d #ffffff #202024 #232327 #2c2c31 #1b1b1e #2a2a2f #3a3a40 #4d4d55 #2a2a2f #e3262d #ffffff #e3262d #e3262d #232327 #2c2c31 #e3262d Slanted true 8 6 6 28 3 2 Segoe UI Variable Text, Segoe UI 12"),
            ("amd-radeon-light", "false #e9e9ec #e9e9ec #f5f5f7 #ffffff #e9e9ec #e9e9ec #ffffff #e6e6ea #dcdce1 #1d1d1f #1d1d1f #6b6b73 #b8b8be #e3262d #e3262d1f #e3262d #ffffff #f3f3f5 #e6e6ea #dcdce1 #ededf0 #e0e0e4 #c6c6cc #a9a9b0 #dcdce1 #e3262d #ffffff #e3262d #e3262d #dcdce1 #dcdce1b3 #e3262d Slanted false 8 6 6 28 3 2 Segoe UI Variable Text, Segoe UI 12"),
            ("nvidia", "true #0f0f0f #0f0f0f #1a1a1a #222222 #0f0f0f #0f0f0f #0f0f0f #2b2b2b #363636 #ffffff #ffffff #a3a3a3 #505050 #76b900 #76b90033 #76b900 #0f0f0f #2a2a2a #2b2b2b #363636 #222222 #333333 #3d3d3d #575757 #333333 #76b900 #0f0f0f #76b900 #2b2b2b #2b2b2b #363636 #76b900 Straight true 0 0 2 28 3 1 NVIDIA Sans, Segoe UI Variable Text, Segoe UI 12"),
            ("nvidia-light", "false #e6e6e6 #e6e6e6 #f4f4f4 #ffffff #e6e6e6 #e6e6e6 #ffffff #e2e2e2 #d4d4d4 #1a1a1a #000000 #666666 #b3b3b3 #76b900 #76b9002e #76b900 #0f0f0f #f0f0f0 #e2e2e2 #d4d4d4 #ececec #dcdcdc #c4c4c4 #a6a6a6 #d4d4d4 #76b900 #0f0f0f #76b900 #dcdcdc #d4d4d4 #d4d4d4b3 #76b900 Straight false 0 0 2 28 3 1 NVIDIA Sans, Segoe UI Variable Text, Segoe UI 12"),
        ];
        for (id, want) in before {
            let t = built_in_theme(id);
            assert_eq!(old_fields(&t), want, "{id}");
            // Every surface is its old colour, flat.
            for (paint, colour) in [
                (t.window_paint, t.window),
                (t.titlebar_paint, t.titlebar),
                (t.toolbar_paint, t.toolbar),
                (t.panel_paint, t.panel),
                (t.sidebar_paint, t.sidebar),
                (t.statusbar_paint, t.statusbar),
                (t.input_paint, t.input),
                (t.chip_paint, t.chip),
                (t.chip_hover_paint, t.chip_hover),
                (t.accent_paint, t.accent),
                (t.accent_tint_paint, t.accent_tint),
                (t.hover_paint, t.hover),
                (t.scroll_thumb_paint, t.scroll_thumb),
                (t.scroll_thumb_hover_paint, t.scroll_thumb_hover),
                (t.bar_track_paint, t.bar_track),
                (t.logo_paint, t.logo),
            ] {
                assert_eq!(paint, Paint::Flat(colour), "{id}");
            }
            // The newer colours follow the ones the parts were drawn with before them.
            assert_eq!((t.tab, t.tab_active), (Paint::CLEAR, t.toolbar_paint), "{id}");
            assert_eq!((t.tab_text, t.tab_active_text, t.tab_border.a, t.highlight.a), (t.muted, t.text, 0.0, 0.0), "{id}");
            assert_eq!((t.sidebar_panel, t.panel_header, t.panel_header_text), (t.panel_paint, t.toolbar_paint, t.muted), "{id}");
            assert_eq!((t.sidebar_selection, t.sidebar_selection_text), (t.accent_tint_paint, t.text_strong), "{id}");
            assert_eq!((t.list_header, t.list_header_text, t.column_line.a, t.status_text), (t.panel_paint, t.muted, 0.0, t.muted), "{id}");
            assert_eq!(t.status_line.a, 0.0, "{id}");
            assert_eq!((t.selection_text, t.selection_details, t.input_border.a, t.button_text), (t.text_strong, t.text, 0.0, t.text), "{id}");
            assert_eq!((t.marble, t.marble_close, t.usage_fill), (Paint::Flat(t.muted), Paint::Flat(t.close_hover), t.accent_paint), "{id}");
            assert!(t.scrollbar_track.is_clear() && t.scrollbar_border.a == 0.0 && t.scrollbar_grip_lines == t.muted, "{id}");
            // And the styles are the ones Glint always had.
            assert_eq!((t.buttons, t.caption_buttons, t.tabs, t.sidebar_style), (ButtonStyle::Flat, CaptionStyle::Symbols, TabStyle::Attached, SidebarStyle::Plain), "{id}");
            assert_eq!((t.scrollbar_grip, t.scrollbar_w, t.scrollbar_margin()), (false, size::SCROLLBAR_W, size::SCROLLBAR_MARGIN), "{id}");
            // No older Windows: today's icons, titles and menus as they were drawn before.
            assert_eq!((t.era, t.icons, t.title_font.as_str()), (Era::None, IconSet::Windows, ""), "{id}");
            assert_eq!((t.title_text, t.title_text_inactive, t.title_shadow.a, t.titlebar_inactive_paint), (t.text, t.muted, 0.0, t.titlebar_paint), "{id}");
            assert!(t.bevel_highlight.a == 0.0 && t.bevel_light.a == 0.0 && t.bevel_shadow.a == 0.0 && t.bevel_dark.a == 0.0, "{id}");
            assert_eq!((t.menu.a, t.sidebar_text), (0.0, t.text), "{id}");
        }
        assert_eq!(Theme::dark(), built_in_theme(DEFAULT));
        assert_eq!(Theme::light(), built_in_theme(DEFAULT_LIGHT));
        // A skin of the person's that names none of the newer things looks like its base skin did.
        let mine = parse("name: Mine\naccent: #3a7d44\ntoolbar: #202020\n", "x").theme;
        assert_eq!((mine.tab_active, mine.usage_fill, mine.logo_paint), (Paint::Flat(Color::hex(0x202020)), Paint::Flat(Color::hex(0x3a7d44)), Paint::Flat(Color::hex(0x3a7d44))));
    }

    /// The stops of a paint, as (position in %, colour) - rounded so they compare plainly.
    fn stops(p: Paint) -> Vec<(u32, String)> {
        match p {
            Paint::Flat(c) => vec![(0, hex_of(c))],
            Paint::Gradient(g) => g.stops().iter().map(|s| ((s.at * 100.0).round() as u32, hex_of(s.color))).collect(),
        }
    }

    #[test]
    fn colours_can_be_gradients() {
        let text = "title-bar: #fdfdfd, #c3cddb 45%, #8d9db8 55%, #dfe6f0\n\
                    toolbar: #000, #888, #fff\n\
                    panel: #000, #111 20%, #222, #333, #444 80%, #555\n\
                    sidebar: linear-gradient(180deg, #ffffff 0%, #000000 100%);\n\
                    status-bar: rgb(1, 2, 3), #ffffff\n\
                    input: accent 50% 30%, #ffffff\n\
                    hover: #000000 60%, #ffffff 20%\n\
                    accent: #102030\n\
                    button: title-bar\n\
                    selection: toolbar 50%\n\
                    tab-active: #ffffff, title-bar\n";
        let p = parse(text, "x");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        let t = p.theme;
        assert_eq!(stops(t.titlebar_paint), [(0, "#fdfdfd".into()), (45, "#c3cddb".into()), (55, "#8d9db8".into()), (100, "#dfe6f0".into())]);
        // Without a %: spread evenly between their neighbours.
        assert_eq!(stops(t.toolbar_paint).iter().map(|s| s.0).collect::<Vec<_>>(), [0, 50, 100]);
        assert_eq!(stops(t.panel_paint).iter().map(|s| s.0).collect::<Vec<_>>(), [0, 20, 40, 60, 80, 100]);
        // CSS's own way of writing it, its direction left out.
        assert_eq!(stops(t.sidebar_paint), [(0, "#ffffff".into()), (100, "#000000".into())]);
        assert_eq!(stops(t.statusbar_paint)[0], (0, "#010203".into()));
        // A strength, then the position.
        assert_eq!(stops(t.input_paint)[0], (30, "#10203080".into()));
        // A position before an earlier one moves up to it.
        assert_eq!(stops(t.hover_paint).iter().map(|s| s.0).collect::<Vec<_>>(), [60, 60]);
        // A name stands for the whole gradient (at a strength too); inside a gradient, for its one colour.
        assert_eq!(t.chip_paint, t.titlebar_paint);
        assert_eq!(t.accent_tint_paint, t.toolbar_paint.with_strength(0.5));
        assert_eq!(stops(t.tab_active)[1], (100, hex_of(t.titlebar_paint.base())));
        // The colour that stands for a gradient: its average along its length.
        assert_eq!(t.titlebar, t.titlebar_paint.base());
        assert_eq!(hex_of(t.toolbar), "#848484", "a quarter black, half #888, a quarter white");
        assert_eq!(hex_of(Paint::even(&[Color::hex(0), Color::hex(0xffffff)]).base()), "#808080");
        assert_eq!(Paint::from(Color::hex(0x123456)).base(), Color::hex(0x123456));
        // A gradient on the background is light or dark by its first colour.
        assert!(!parse("background: #f4f7fb, #cfd8e6", "x").theme.dark);
        assert!(parse("background: #101010 0%, #f0f0f0", "x").theme.dark);
    }

    #[test]
    fn bad_gradient_colours_are_noted_and_fall_back_to_a_flat_colour() {
        let text = "title-bar: #fdfdfd, wobble, #000000\n\
                    toolbar: #ffffff, #000000 5x%\n\
                    panel: wobble, nope\n\
                    hover: #ffffff,,#000000\n\
                    sidebar: #000, #111, #222, #333, #444, #555, #666, #777, #888, #999\n";
        let p = parse(text, "x");
        let lines: Vec<usize> = p.notes.iter().map(|n| n.line).collect();
        assert_eq!(lines, [1, 2, 3, 4, 5], "{:?}", p.notes);
        let t = p.theme;
        assert_eq!(t.titlebar_paint, Paint::Flat(Color::hex(0xfdfdfd)), "its first good colour, flat");
        assert!(p.notes[0].text.contains("wobble") && p.notes[0].text.contains("#fdfdfd"), "{}", p.notes[0].text);
        assert_eq!(t.toolbar_paint, Paint::Flat(Color::hex(0xffffff)));
        assert!(p.notes[1].text.contains("5x%"), "{}", p.notes[1].text);
        assert_eq!(t.panel_paint, built_in_theme(DEFAULT).panel_paint, "no good colour at all: the standard one");
        assert_eq!(t.hover_paint, Paint::Flat(Color::hex(0xffffff)));
        assert_eq!(stops(t.sidebar_paint).len(), MAX_STOPS, "at most 8 colours, the rest left out");
        // Styles that aren't one of their words are noted too.
        let p = parse("buttons: wobbly\ntabs: pill\nsidebar-style: PANELS\ncaption-buttons: 3\nscrollbar-grip: maybe\nscrollbar-width: 40\n", "x");
        let lines: Vec<usize> = p.notes.iter().map(|n| n.line).collect();
        assert_eq!(lines, [1, 4, 5, 6], "{:?}", p.notes);
        assert_eq!((p.theme.buttons, p.theme.tabs, p.theme.sidebar_style, p.theme.caption_buttons), (ButtonStyle::Flat, TabStyle::Pills, SidebarStyle::Panels, CaptionStyle::Symbols));
        assert_eq!((p.theme.scrollbar_grip, p.theme.scrollbar_w), (false, 18.0));
        // How see-through 7's glass is: a number or a percentage, at most 90.
        assert_eq!(parse("see-through: 35%\n", "x").theme.see_through, 0.35);
        let p = parse("see-through: 120\n", "x");
        assert_eq!((p.theme.see_through, p.notes.len()), (0.9, 1));
        assert_eq!(parse("era: 7\n", "x").theme.see_through, 0.0, "solid unless the skin says");
        // The wallpaper's zoom, and where it sits across and up and down.
        let t = parse("wallpaper-zoom: 50\nwallpaper-position: left bottom\n", "x").theme;
        assert_eq!((t.wallpaper_zoom, t.wallpaper_at, t.wallpaper_at_y), (1.5, 0.0, Some(1.0)));
        let t = parse("wallpaper-position: middle\n", "x").theme;
        assert_eq!((t.wallpaper_zoom, t.wallpaper_at, t.wallpaper_at_y), (1.0, 0.5, None));
        assert_eq!(parse("wallpaper-position: top\n", "x").theme.wallpaper_at_y, Some(0.0));
        // Up and down first is fine too.
        for (v, at) in [("top right", (1.0, Some(0.0))), ("right top", (1.0, Some(0.0))), ("bottom left", (0.0, Some(1.0))), ("middle right", (1.0, Some(0.5)))] {
            let t = parse(&format!("wallpaper-position: {v}\n"), "x").theme;
            assert_eq!((t.wallpaper_at, t.wallpaper_at_y), at, "{v}");
        }
        assert_eq!(parse("wallpaper-zoom: -100\n", "x").theme.wallpaper_zoom, 0.5);
        assert_eq!(parse("wallpaper-zoom: 900\n", "x").theme.wallpaper_zoom, 6.0);
    }

    /// The skins shared as files (next to the source, not built in) read cleanly too, each with its
    /// era. Skipped where the folder isn't there.
    #[test]
    fn the_shared_skin_files_read_cleanly() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("glint-test").join("skins");
        let Ok(listing) = std::fs::read_dir(&dir) else { return };
        let mut read = 0;
        for file in listing.flatten() {
            let text = std::fs::read_to_string(file.path()).unwrap();
            let name = file.file_name().to_string_lossy().into_owned();
            let p = parse_in(&text, &name, Some(&dir));
            assert!(p.notes.is_empty(), "{name}: {:?}", p.notes);
            // The older Windows looks each have an era; the picture skins have a wallpaper instead.
            assert!(value_in(&text, "era").is_some() || value_in(&text, "wallpaper").is_some(), "{name} has no era");
            read += 1;
        }
        assert!(read >= 6);
    }

    #[test]
    fn chrome_and_gunmetal_are_liquid_chrome_and_a_pair() {
        for (id, dark) in [("chrome", false), ("gunmetal", true)] {
            let text = BUILT_IN.iter().find(|(i, _)| *i == id).unwrap().1;
            let p = parse(text, id);
            assert!(p.notes.is_empty(), "{id}: {:?}", p.notes);
            let t = p.theme;
            assert_eq!(t.dark, dark, "{id}");
            assert_eq!((t.buttons, t.caption_buttons, t.tabs, t.sidebar_style), (ButtonStyle::Liquid, CaptionStyle::Marbles, TabStyle::Pills, SidebarStyle::Panels), "{id}");
            assert!(t.scrollbar_grip && t.scrollbar_w > size::SCROLLBAR_W && !t.scrollbar_track.is_clear(), "{id}");
            assert_eq!((t.shape, t.radius, t.font.as_str(), t.font_size), (Shape::Rounded, 10.0, "Verdana, Tahoma, Segoe UI", 8.0 * PT), "{id}");
            for paint in [t.titlebar_paint, t.toolbar_paint, t.sidebar_paint, t.chip_paint, t.marble, t.marble_close, t.scroll_thumb_paint, t.scrollbar_track, t.list_header, t.panel_header] {
                assert!(paint.gradient().is_some(), "{id}: {paint:?}");
            }
            assert_eq!(t.select_edge.a, 0.0, "{id}: no edge on selected rows");
        }
        let chrome = built_in_theme("chrome");
        assert_eq!(stops(chrome.titlebar_paint), [(0, "#fdfdfd".into()), (45, "#c3cddb".into()), (55, "#8d9db8".into()), (100, "#dfe6f0".into())]);
        assert_eq!((chrome.accent, chrome.text, chrome.button_text), (Color::hex(0x5a82c8), Color::hex(0x112233), Color::hex(0xffffff)));
        let lib = Library::new();
        let chrome = lib.find("chrome").unwrap();
        assert_eq!((chrome.name.as_str(), chrome.description.as_str()), ("Chrome", "Year 2000 chrome: shiny silver-blue, liquid buttons"));
        let gunmetal = lib.find("gunmetal").unwrap();
        assert_eq!((gunmetal.name.as_str(), gunmetal.description.as_str()), ("Gunmetal", "Year 2000 chrome in dark gunmetal, liquid buttons"));
    }

    #[test]
    fn twins_named_in_the_skin_pair_up() {
        let mut lib = Library::new();
        assert_eq!(lib.twin("chrome", true), "gunmetal");
        assert_eq!(lib.twin("gunmetal", false), "chrome");
        assert_eq!(lib.twin("chrome", false), "chrome");
        assert_eq!(lib.twin(DEFAULT, false), DEFAULT_LIGHT, "names still pair up");
        // The person's own: by id or by name, and one named by the other half works both ways.
        lib.add_for_tests("file:Dusk", "name: Dusk\ndark: true\ntwin: Morning glow\n");
        lib.add_for_tests("file:Morning glow", "name: Morning glow\ndark: false\n");
        lib.add_for_tests("file:Night", "name: Night\ndark: true\ntwin: chrome\n");
        lib.add_for_tests("file:Wrong", "name: Wrong\ndark: true\ntwin: gunmetal\n");
        assert_eq!(lib.twin("file:Dusk", false), "file:Morning glow");
        assert_eq!(lib.twin("file:Morning glow", true), "file:Dusk");
        assert_eq!(lib.twin("file:Night", false), "chrome");
        assert_eq!(lib.twin("file:Wrong", false), "file:Wrong", "a twin that's dark too isn't the light version");
        // A copy is a skin of its own: it doesn't take the original's other half.
        let copy = copy_text(BUILT_IN[4].1, "My chrome", "Chrome");
        let p = parse(&copy, "x");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert_eq!((p.twin.as_str(), p.name.as_str()), ("", "My chrome"));
        assert_eq!(p.theme, built_in_theme("chrome"));
    }

    #[test]
    fn new_and_renamed_skins_never_write_over_another_file() {
        let dir = std::env::temp_dir().join(format!("glint-skin-safe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let theirs = dir.join("Taken.glint");
        std::fs::write(&theirs, "name: Someone else's\n").unwrap();
        // A new skin whose file turned out to be there already: refused, the file kept.
        let new = run_job(FileJob::Write { file: theirs.clone(), text: "name: Mine\n".into(), name: "Mine".into(), new: true });
        assert!(new.is_err());
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "name: Someone else's\n");
        // A rename onto it: refused, both files as they were, nothing written.
        let mine = dir.join("Mine.glint");
        std::fs::write(&mine, "name: Mine\n").unwrap();
        let renamed = run_job(FileJob::Rename { from: mine.clone(), to: theirs.clone(), text: "name: Taken\n".into(), name: "Taken".into() });
        assert!(renamed.is_err());
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "name: Someone else's\n");
        assert_eq!(std::fs::read_to_string(&mine).unwrap(), "name: Mine\n");
        // A rename to a free name moves the file and writes the new text; only the capitals: too.
        let free = dir.join("Sunset.glint");
        assert!(run_job(FileJob::Rename { from: mine.clone(), to: free.clone(), text: "name: Sunset\n".into(), name: "Sunset".into() }).is_ok());
        assert!(!mine.exists());
        assert_eq!(std::fs::read_to_string(&free).unwrap(), "name: Sunset\n");
        let caps = dir.join("SUNSET.glint");
        assert!(run_job(FileJob::Rename { from: free.clone(), to: caps.clone(), text: "name: SUNSET\n".into(), name: "SUNSET".into() }).is_ok());
        assert_eq!(std::fs::read_to_string(&caps).unwrap(), "name: SUNSET\n");
        // Capitals in any letters: "élan" to "Élan" is the same file, not a taken name.
        let elan = dir.join("élan.glint");
        if std::fs::write(&elan, "name: élan\n").is_ok() {
            let big = dir.join("Élan.glint");
            assert!(run_job(FileJob::Rename { from: elan.clone(), to: big.clone(), text: "name: Élan\n".into(), name: "Élan".into() }).is_ok());
            assert_eq!(std::fs::read_to_string(&big).unwrap(), "name: Élan\n");
        }
        // A listing knows the names of files it left out, and new names keep clear of them.
        std::fs::write(dir.join("Huge.glint"), vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
        let listing = read_folder(&dir);
        assert!(listing.stems.contains("huge") && !listing.own.iter().any(|e| e.id == "file:Huge"));
        let mut lib = Library::new();
        lib.stems = listing.stems;
        lib.take_own(listing.own);
        assert_eq!(lib.free_name("Huge"), "Huge 2");
        assert_eq!(lib.free_name("Taken"), "Taken 2");
        // Renaming one's own skin: to its own name in other capitals yes, onto a file left out or
        // another skin's no.
        let at = lib.entries().iter().position(|e| e.name == "SUNSET").unwrap();
        assert!(lib.free_for(at, "Sunset") && lib.free_for(at, "Dawn"));
        assert!(!lib.free_for(at, "Huge") && !lib.free_for(at, "taken"));
        assert_eq!(lib.rename(&lib.entries()[at].id.clone(), "Huge"), None, "refused before anything is queued");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_colour_is_listed_as_the_skin_makes_it() {
        let at = |key: &str| COLOURS.iter().position(|(k, _)| *k == key).unwrap();
        // The same colours as the look the skin makes.
        let chrome = BUILT_IN.iter().find(|(id, _)| *id == "chrome").unwrap().1;
        let all = colours_of(chrome);
        let theme = built_in_theme("chrome");
        assert_eq!(all.len(), COLOURS.len());
        assert_eq!(all[at("title-bar")], (theme.titlebar_paint, None));
        assert_eq!(all[at("marble-close")].0, theme.marble_close);
        assert_eq!(all[at("accent")].0, theme.accent_paint);
        // A colour that names another follows it, and says so; one left out follows its base
        // skin's line or the colour it came after.
        assert_eq!(all[at("tab-line")], (theme.accent_paint, Some("accent".to_string())));
        let mine = colours_of("dark: true\naccent: #102030\nselection: Accent 20%\n");
        assert_eq!(mine[at("selection")].1.as_deref(), Some("accent 20%"));
        assert_eq!(mine[at("selection")].0, Paint::Flat(Color::hex(0x102030).with_alpha(0.2)));
        assert_eq!(mine[at("title-bar")].1.as_deref(), Some("background"));
        assert_eq!(mine[at("tab-active")].1.as_deref(), Some("toolbar"));
        assert_eq!(mine[at("accent")].1, None);
        // Gradients and plain colours don't follow anything.
        assert_eq!(mine[at("panel")].1, None);
    }

    /// The lines of a skin's notes.
    fn noted(p: &Parsed) -> Vec<usize> {
        p.notes.iter().map(|n| n.line).collect()
    }

    #[test]
    fn wallpaper_fits_and_positions_that_arent_understood_are_noted_and_fall_back() {
        for (word, fit) in [("fill", WallpaperFit::Fill), ("whole", WallpaperFit::Whole), ("Stretch", WallpaperFit::Stretch), ("TILE", WallpaperFit::Tile), ("actual", WallpaperFit::Actual)] {
            let p = parse(&format!("wallpaper-fit: {word}\n"), "x");
            assert_eq!((p.theme.wallpaper_fit, p.notes.len()), (fit, 0), "{word}");
        }
        for bad in ["wobble", "cover", "", "fill whole", "7"] {
            let p = parse(&format!("name: x\nwallpaper-fit: {bad}\n"), "x");
            assert_eq!(noted(&p), [2], "{bad:?}: {:?}", p.notes);
            assert_eq!(p.theme.wallpaper_fit, WallpaperFit::Fill, "{bad:?}: the standard fit");
            assert!(p.notes[0].text.contains("fill, whole, stretch, tile or actual"), "{}", p.notes[0].text);
        }
        // Every word pair, either way round, and the percentages.
        let words = [("left", 0.0), ("middle", 0.5), ("right", 1.0)];
        let downs = [("top", 0.0), ("middle", 0.5), ("bottom", 1.0)];
        for (a, x) in words {
            for (d, y) in downs {
                for v in [format!("{a} {d}"), format!("{d} {a}")] {
                    let p = parse(&format!("wallpaper-position: {v}\n"), "x");
                    assert_eq!((p.theme.wallpaper_at, p.theme.wallpaper_at_y, p.notes.len()), (x, Some(y), 0), "{v}");
                }
            }
        }
        for (v, at) in [("30% 70%", (0.3, Some(0.7))), ("0% 100%", (0.0, Some(1.0))), ("100%, 0%", (1.0, Some(0.0))), ("bottom", (1.0, Some(1.0))), ("centre", (0.5, None))] {
            let p = parse(&format!("wallpaper-position: {v}\n"), "x");
            assert_eq!((p.theme.wallpaper_at, p.theme.wallpaper_at_y, p.notes.len()), (at.0, at.1, 0), "{v}");
        }
        for bad in ["sideways", "left wobble", "left right", "top bottom", "left top middle", "150% 20%", "-10% 20%", "30%", "30% left", "x% 20%", "left left"] {
            let p = parse(&format!("wallpaper-position: {bad}\n"), "x");
            assert_eq!(noted(&p), [1], "{bad}: {:?}", p.notes);
            assert_eq!((p.theme.wallpaper_at, p.theme.wallpaper_at_y), (1.0, None), "{bad}: the standard place");
        }
    }

    #[test]
    fn wallpaper_numbers_past_their_ends_or_not_numbers_are_noted() {
        // (value, what it is then, noted)
        let opacity = |v: &str| {
            let p = parse(&format!("wallpaper-opacity: {v}\n"), "x");
            (p.theme.wallpaper_opacity, p.notes.len())
        };
        assert_eq!(opacity("0"), (0.0, 0));
        assert_eq!(opacity("100"), (1.0, 0));
        assert_eq!(opacity("150"), (1.0, 1));
        assert_eq!(opacity("-5"), (0.0, 1));
        assert_eq!(opacity("lots"), (0.4, 1), "the standard 40");
        assert_eq!(opacity(""), (0.4, 1));
        let zoom = |v: &str| {
            let p = parse(&format!("wallpaper-zoom: {v}\n"), "x");
            (p.theme.wallpaper_zoom, p.notes.len())
        };
        assert_eq!(zoom("-500"), (1.0 / 6.0, 0));
        assert_eq!(zoom("-900"), (1.0 / 6.0, 1));
        assert_eq!(zoom("big"), (1.0, 1));
        assert_eq!(zoom("NaN"), (1.0, 1));
        let size = |v: &str| {
            let p = parse(&format!("wallpaper-size: {v}\n"), "x");
            (p.theme.wallpaper_size, p.notes.len())
        };
        assert_eq!(parse("", "x").theme.wallpaper_size, None, "not set: the fit decides");
        assert_eq!(size("50%"), (Some(0.5), 0));
        assert_eq!(size("400"), (Some(4.0), 0));
        assert_eq!(size("1%"), (Some(0.01), 0));
        assert_eq!(size("0"), (Some(0.01), 1));
        assert_eq!(size("500%"), (Some(4.0), 1));
        assert_eq!(size("-20%"), (Some(0.01), 1));
        assert_eq!(size("half"), (None, 1));
        let blur = |v: &str| {
            let p = parse(&format!("wallpaper-blur: {v}\n"), "x");
            (p.theme.wallpaper_blur, p.notes.len())
        };
        assert_eq!(blur("0"), (0, 0));
        assert_eq!(blur("50"), (50, 0));
        assert_eq!(blur("7.6"), (8, 0));
        assert_eq!(blur("51"), (50, 1));
        assert_eq!(blur("-3"), (0, 1));
        assert_eq!(blur("soft"), (0, 1));
        let fade = |v: &str| {
            let p = parse(&format!("wallpaper-fade: {v}\n"), "x");
            (p.theme.wallpaper_fade, p.notes.len())
        };
        assert_eq!(fade("0"), (0.0, 0));
        assert_eq!(fade("50"), (0.5, 0));
        assert_eq!(fade("10%"), (0.1, 0));
        assert_eq!(fade("60"), (0.5, 1));
        assert_eq!(fade("-1"), (0.0, 1));
        assert_eq!(fade("edges"), (0.0, 1));
        let offset = |v: &str| {
            let p = parse(&format!("wallpaper-offset: {v}\n"), "x");
            (p.theme.wallpaper_offset, p.notes.len())
        };
        assert_eq!(parse("", "x").theme.wallpaper_offset, (0.0, 0.0));
        assert_eq!(offset("20 -10"), ((20.0, -10.0), 0));
        assert_eq!(offset("20px, -10px"), ((20.0, -10.0), 0));
        assert_eq!(offset("0 0"), ((0.0, 0.0), 0));
        assert_eq!(offset("2000 -2000"), ((2000.0, -2000.0), 0));
        assert_eq!(offset("3000 -2500"), ((2000.0, -2000.0), 1));
        for bad in ["20", "1 2 3", "a b", "", "inf 0", "20 down"] {
            assert_eq!(offset(bad), ((0.0, 0.0), 1), "{bad:?}");
        }
        // Each note is on its own line, and the skin still reads the rest.
        let p = parse("wallpaper-blur: 99\naccent: #ff0000\nwallpaper-fade: x\n", "x");
        assert_eq!(noted(&p), [1, 3]);
        assert_eq!(p.theme.accent, Color::hex(0xff0000));
        // What a number can be, as the hint line says it.
        assert_eq!(range_of("slant").as_deref(), Some("0 to 20"));
        assert_eq!(range_of("font-size").as_deref(), Some("8pt to 14pt"));
        assert_eq!(range_of("wallpaper-zoom").as_deref(), Some("-500 to 500"));
        assert_eq!(range_of("accent"), None);
        // Every number setting is a setting.
        assert!(NUMBERS.iter().all(|(k, ..)| known_key(k)));
    }

    #[test]
    fn a_wallpaper_that_isnt_there_is_noted() {
        let dir = std::env::temp_dir().join(format!("glint-wallpaper-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let here = dir.join("here.png");
        std::fs::write(&here, "not really a picture").unwrap();
        let gone = dir.join("gone.png");
        // A full path: there, or not.
        let p = parse(&format!("wallpaper: {}\n", here.display()), "x");
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        let p = parse(&format!("name: x\nwallpaper: \"{}\"\n", gone.display()), "x");
        assert_eq!(noted(&p), [2], "{:?}", p.notes);
        assert!(p.notes[0].text.contains("gone.png"), "{}", p.notes[0].text);
        assert_eq!(p.theme.wallpaper, gone.to_string_lossy(), "the skin still reads, with its path");
        // A file name: next to the skin, then the skins folder.
        let p = parse_in("wallpaper: here.png\n", "x", Some(&dir));
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        let missing = format!("glint-no-such-picture-{}.png", std::process::id());
        let p = parse_in(&format!("wallpaper: {missing}\n"), "x", Some(&dir));
        assert_eq!(noted(&p), [1]);
        assert!(p.notes[0].text.contains("next to the skin"), "{}", p.notes[0].text);
        // A network path isn't looked for (a server that's away would hold the window up), and
        // no wallpaper is nothing to look for.
        assert!(parse("wallpaper: \\\\nowhere-glint\\share\\a.png\n", "x").notes.is_empty());
        assert!(parse("wallpaper:\n", "x").notes.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_setting_written_twice_is_noted_on_the_line_that_wins() {
        // The same key twice, notes and blank lines between: the later one wins, and says so.
        let p = parse("name: x\nwallpaper-blur: 4\n\n// a note\n  wallpaper-blur: 8 // the one\n", "x");
        assert_eq!(noted(&p), [5], "{:?}", p.notes);
        assert_eq!(p.notes[0].text, "wallpaper-blur is already set on line 2 (this one wins).");
        assert_eq!(p.theme, parse("name: x\nwallpaper-blur: 8\n", "x").theme, "the last one still wins");
        // Under another of its names, or in other capitals and spellings: the same setting.
        let p = parse("window: #101010\nbackground: #202020\n", "x");
        assert_eq!(noted(&p), [2]);
        assert!(p.notes[0].text.contains("background is already set on line 1"), "{}", p.notes[0].text);
        assert_eq!(p.theme.window, Color::hex(0x202020));
        let p = parse("Title Bar: #101010\ntitle_bar: #202020\nTITLEBAR = #303030\n", "x");
        assert_eq!(noted(&p), [2, 3], "{:?}", p.notes);
        assert!(p.notes[0].text.contains("line 1") && p.notes[1].text.contains("line 2"), "{:?}", p.notes);
        assert_eq!(p.theme.titlebar, Color::hex(0x303030));
        // Settings Glint doesn't know aren't counted twice (they're noted as such), and different
        // settings aren't the same one.
        let p = parse("sparkle: 1\nsparkle: 2\naccent: #ff0000\ntext: #ff0000\n", "x");
        assert_eq!(noted(&p), [1, 2], "{:?}", p.notes);
        assert!(p.notes.iter().all(|n| n.text.contains("isn't a skin setting")));
    }

    // ------------------------------------------------------------ importing

    /// A folder for an import test (made empty): the skins folder in it, and a folder to import from.
    fn import_test(case: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("glint-import-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (skins, from) = (base.join("skins"), base.join("Downloads"));
        std::fs::create_dir_all(&skins).unwrap();
        std::fs::create_dir_all(&from).unwrap();
        (base, skins, from)
    }

    /// The library as Glint has it with `skins` as the skins folder.
    fn library_of(skins: &Path) -> Library {
        let mut lib = Library::new();
        let listing = read_folder(skins);
        lib.stems = listing.stems;
        lib.take_own(listing.own);
        lib
    }

    /// IMPORT… of `file`, all the way: brought in, added, its files written.
    fn import(lib: &mut Library, file: &Path, skins: &Path) -> (String, String, bool) {
        let said = lib.take_import(prepare_import(file, skins).unwrap());
        wait_for_writes(Duration::from_secs(10));
        said
    }

    /// An emoji for names - or, under Wine without a UTF-8 locale (where such names can't be made),
    /// a plain sign.
    fn flower(dir: &Path) -> &'static str {
        if crate::search::odd_names_work(dir) {
            "\u{1f338}"
        } else {
            "+"
        }
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|i| i.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn an_imported_skin_brings_its_picture() {
        let (base, skins, from) = import_test("picture");
        // A folder with other things in it (not the skin's own): the picture goes next to the skin.
        std::fs::write(from.join("letter.docx"), "not a skin's").unwrap();
        std::fs::write(from.join("sakura.jpg"), "a picture").unwrap();
        let file = from.join("Blue Cardigan.glint");
        std::fs::write(&file, "name: Blue Cardigan\ndark: true\nwallpaper: \"sakura.jpg\"\n").unwrap();
        let mut lib = library_of(&skins);
        let (id, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!((id.as_str(), said.as_str(), problem), ("file:Blue Cardigan", "Imported Blue Cardigan", false));
        assert_eq!(files_in(&skins), ["Blue Cardigan.glint", "sakura.jpg"]);
        assert_eq!(std::fs::read_to_string(skins.join("sakura.jpg")).unwrap(), "a picture");
        assert_eq!(lib.theme(&id, true).wallpaper, skins.join("sakura.jpg").to_string_lossy());
        assert!(lib.notes(&id).is_empty(), "{:?}", lib.notes(&id));
        // Again: a skin of its own with a number, the very same picture serving both.
        let (id2, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!((id2.as_str(), said.as_str(), problem), ("file:Blue Cardigan 2", "Imported Blue Cardigan 2", false));
        assert_eq!(files_in(&skins), ["Blue Cardigan 2.glint", "Blue Cardigan.glint", "sakura.jpg"]);
        // A different picture of that name: never written over, the new one numbered.
        std::fs::write(from.join("sakura.jpg"), "another picture").unwrap();
        let (id3, _, problem) = import(&mut lib, &file, &skins);
        assert!(!problem);
        assert_eq!(std::fs::read_to_string(skins.join("sakura.jpg")).unwrap(), "a picture");
        assert_eq!(std::fs::read_to_string(skins.join("sakura 2.jpg")).unwrap(), "another picture");
        let text = std::fs::read_to_string(skins.join("Blue Cardigan 3.glint")).unwrap();
        assert_eq!(value_in(&text, "wallpaper").as_deref(), Some("sakura 2.jpg"), "{text}");
        assert_eq!(lib.theme(&id3, true).wallpaper, skins.join("sakura 2.jpg").to_string_lossy());
        assert_eq!(lib.theme(&id, true).wallpaper, skins.join("sakura.jpg").to_string_lossy(), "the first one keeps its own");
        // A path from the skin's folder: the picture comes too, the line naming it there.
        std::fs::create_dir_all(from.join("pics")).unwrap();
        std::fs::write(from.join("pics").join("moss.png"), "moss").unwrap();
        let moss = from.join("Moss.glint");
        std::fs::write(&moss, "name: Moss\nwallpaper: pics\\moss.png\n").unwrap();
        let (id, said, _) = import(&mut lib, &moss, &skins);
        assert_eq!(said, "Imported Moss");
        assert_eq!(value_in(&lib.find(&id).unwrap().text, "wallpaper").as_deref(), Some("moss.png"));
        assert_eq!(std::fs::read_to_string(skins.join("moss.png")).unwrap(), "moss");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_skin_in_its_own_folder_comes_as_a_folder_with_its_other_half() {
        let (base, skins, from) = import_test("folder");
        let f = flower(&base);
        let own = from.join(format!("{f} Blue #1 Cardigan"));
        std::fs::create_dir_all(&own).unwrap();
        let night = format!("sakura #1 {f}.jpg");
        std::fs::write(own.join(&night), "night").unwrap();
        std::fs::write(own.join("day.png"), "day").unwrap();
        std::fs::write(own.join("readme.txt"), "made by someone").unwrap();
        let dark = own.join(format!("{f} Blue #1.glint"));
        std::fs::write(&dark, format!("name: {f} Blue #1\ndark: true\nwallpaper: {night}\n")).unwrap();
        // Its light half, with a picture of its own, named by the name.
        std::fs::write(own.join(format!("{f} Blue #1 (Light).glint")), format!("name: {f} Blue #1 (Light)\ndark: false\nwallpaper: day.png\n")).unwrap();
        let mut lib = library_of(&skins);
        let (id, said, problem) = import(&mut lib, &dark, &skins);
        assert_eq!(said, format!("Imported {f} Blue #1 and {f} Blue #1 (Light)"));
        assert!(!problem);
        let folder = skins.join(format!("{f} Blue #1 Cardigan"));
        assert_eq!(files_in(&skins), [format!("{f} Blue #1 Cardigan")]);
        let mut expected = vec!["day.png".to_string(), night.clone(), format!("{f} Blue #1 (Light).glint"), format!("{f} Blue #1.glint")];
        expected.sort();
        assert_eq!(files_in(&folder), expected);
        let light = lib.twin(&id, false);
        // (Its id has its folder's name, as the listing gives it.)
        assert_eq!(light, format!("file:{f} Blue #1 Cardigan\\{f} Blue #1 (Light)"));
        assert_eq!(id, format!("file:{f} Blue #1 Cardigan\\{f} Blue #1"));
        assert_eq!(lib.twin(&light, true), id);
        assert_eq!(lib.theme(&id, true).wallpaper, folder.join(&night).to_string_lossy());
        assert_eq!(lib.theme(&light, false).wallpaper, folder.join("day.png").to_string_lossy());
        // Listed as Glint reads the folder, the two are there with their pictures.
        let listed = library_of(&skins);
        assert!(listed.find(&id).is_some_and(|e| e.id == id), "the same id as listed");
        assert_eq!(listed.twin(&id, false), light);
        assert_eq!(listed.theme(&light, false).wallpaper, folder.join("day.png").to_string_lossy());
        // Again: a folder of its own with a number, the two still a pair.
        let (id2, said, _) = import(&mut lib, &dark, &skins);
        assert_eq!(said, format!("Imported {f} Blue #1 2 and {f} Blue #1 2 (Light)"));
        let folder2 = skins.join(format!("{f} Blue #1 Cardigan 2"));
        assert_eq!(files_in(&folder2).len(), 4);
        assert_eq!(lib.twin(&id2, false), format!("file:{f} Blue #1 Cardigan 2\\{f} Blue #1 2 (Light)"));
        assert_eq!(lib.twin(&id, false), light, "the first pair stays a pair");
        // Picking the light half brings the dark one.
        let (id3, said, _) = import(&mut lib, &own.join(format!("{f} Blue #1 (Light).glint")), &skins);
        assert_eq!(said, format!("Imported {f} Blue #1 3 (Light) and {f} Blue #1 3"));
        assert_eq!(lib.twin(&id3, true), format!("file:{f} Blue #1 Cardigan 3\\{f} Blue #1 3"));
        // Listed again, every id is as it was given.
        let listed = library_of(&skins);
        for i in [&id, &id2, &id3] {
            assert!(listed.find(i).is_some_and(|e| e.id == *i), "{i}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_twin_named_by_its_twin_line_comes_too_and_still_pairs_with_numbers() {
        let (base, skins, from) = import_test("twin-line");
        std::fs::write(from.join("letter.docx"), "x").unwrap();
        // The two share a picture: it's copied once.
        std::fs::write(from.join("shared.png"), "shared").unwrap();
        let night = from.join("night.glint");
        std::fs::write(&night, "name: Night\ndark: true\ntwin: file:morning\nwallpaper: shared.png\n").unwrap();
        std::fs::write(from.join("morning.glint"), "name: Morning\ndark: false\nwallpaper: shared.png\n").unwrap();
        std::fs::write(from.join("other.glint"), "name: Other\ndark: false\n").unwrap();
        let mut lib = library_of(&skins);
        let (id, said, problem) = import(&mut lib, &night, &skins);
        assert_eq!((said.as_str(), problem), ("Imported Night and Morning", false));
        // Its file is called after its name now: the `twin` line says the name.
        assert_eq!(lib.twin(&id, false), "file:Morning");
        assert_eq!(lib.twin("file:Morning", true), id);
        assert_eq!(files_in(&skins), ["Morning.glint", "Night.glint", "shared.png"]);
        let (id2, said, _) = import(&mut lib, &night, &skins);
        assert_eq!(said, "Imported Night 2 and Morning 2");
        assert_eq!(lib.twin(&id2, false), "file:Morning 2");
        assert_eq!(lib.twin("file:Morning 2", true), id2);
        assert_eq!(lib.twin(&id, false), "file:Morning");
        assert_eq!(files_in(&skins).iter().filter(|f| f.ends_with(".png")).count(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn what_an_imported_skin_needs_and_lacks_is_said() {
        let (base, skins, from) = import_test("missing");
        std::fs::write(from.join("letter.docx"), "x").unwrap();
        let mut lib = library_of(&skins);
        // Its picture isn't there: imported, and said so (not again as a skipped line).
        let gone = format!("glint-gone-{}.jpg", std::process::id());
        let file = from.join("Lonely.glint");
        std::fs::write(&file, format!("name: Lonely\nwallpaper: {gone}\n")).unwrap();
        let (id, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!(said, format!("Imported Lonely, but its picture {gone} wasn't found"));
        assert!(problem);
        assert_eq!(value_in(&lib.find(&id).unwrap().text, "wallpaper").as_deref(), Some(gone.as_str()), "the line stays, for the picture put there later");
        // Its other half isn't there either.
        let file = from.join("Dusk.glint");
        std::fs::write(&file, format!("name: Dusk\ndark: true\ntwin: Dusk morning\nwallpaper: {gone}\nsparkle: 1\n")).unwrap();
        let (_, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!(said, format!("Imported Dusk, but its picture {gone} wasn't found, and its light version \u{201c}Dusk morning\u{201d} wasn't found. Skipped 1 line(s): see Show as text"));
        assert!(problem);
        // An other half that is one of the skins already (a built-in one) is fine.
        let file = from.join("Steel.glint");
        std::fs::write(&file, "name: Steel\ndark: true\ntwin: chrome\n").unwrap();
        let (id, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!((said.as_str(), problem), ("Imported Steel", false));
        assert_eq!(lib.twin(&id, false), "chrome");
        // A full path: left as it is when it's there (nothing copied), said when it isn't.
        let pic = base.join("far away.png");
        std::fs::write(&pic, "far").unwrap();
        let file = from.join("Far.glint");
        std::fs::write(&file, format!("name: Far\nwallpaper: {}\n", pic.display())).unwrap();
        let before = files_in(&skins);
        let (id, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!((said.as_str(), problem), ("Imported Far", false));
        assert_eq!(lib.theme(&id, true).wallpaper, pic.to_string_lossy());
        assert_eq!(files_in(&skins).len(), before.len() + 1, "only the skin");
        let file = from.join("Farther.glint");
        std::fs::write(&file, format!("name: Farther\nwallpaper: {}\n", base.join("nowhere.png").display())).unwrap();
        let (_, said, _) = import(&mut lib, &file, &skins);
        assert_eq!(said, "Imported Farther, but its picture nowhere.png wasn't found");
        // A picture put in the skins folder before is found there: nothing to say.
        std::fs::write(skins.join("already.png"), "here").unwrap();
        let file = from.join("Ready.glint");
        std::fs::write(&file, "name: Ready\nwallpaper: already.png\n").unwrap();
        assert_eq!(import(&mut lib, &file, &skins).1, "Imported Ready");
        // A skin that can't be read isn't imported.
        assert!(prepare_import(&from.join("no such.glint"), &skins).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_skin_imported_from_the_skins_folder_is_copied_next_to_itself() {
        let (base, skins, _) = import_test("inside");
        std::fs::write(skins.join("sakura.jpg"), "a picture").unwrap();
        let file = skins.join("Blue Cardigan.glint");
        std::fs::write(&file, "name: Blue Cardigan\ndark: true\nwallpaper: sakura.jpg\n").unwrap();
        let mut lib = library_of(&skins);
        let (id, said, problem) = import(&mut lib, &file, &skins);
        assert_eq!((id.as_str(), said.as_str(), problem), ("file:Blue Cardigan 2", "Imported Blue Cardigan 2", false));
        assert_eq!(files_in(&skins), ["Blue Cardigan 2.glint", "Blue Cardigan.glint", "sakura.jpg"]);
        assert_eq!(lib.theme(&id, true).wallpaper, skins.join("sakura.jpg").to_string_lossy());
        // In a folder of its own there (with its other half): the copies go next to them.
        let pack = skins.join("Pack");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("leaf.png"), "leaf").unwrap();
        std::fs::write(pack.join("Leaf.glint"), "name: Leaf\ndark: true\nwallpaper: leaf.png\n").unwrap();
        std::fs::write(pack.join("Leaf (Light).glint"), "name: Leaf (Light)\ndark: false\nwallpaper: leaf.png\n").unwrap();
        let mut lib = library_of(&skins);
        let (id, said, _) = import(&mut lib, &pack.join("Leaf.glint"), &skins);
        assert_eq!(said, "Imported Leaf 2 and Leaf 2 (Light)");
        assert_eq!(files_in(&skins), ["Blue Cardigan 2.glint", "Blue Cardigan.glint", "Pack", "sakura.jpg"]);
        assert_eq!(files_in(&pack), ["Leaf (Light).glint", "Leaf 2 (Light).glint", "Leaf 2.glint", "Leaf.glint", "leaf.png"]);
        assert_eq!(id, "file:Pack\\Leaf 2");
        assert_eq!(lib.twin(&id, false), "file:Pack\\Leaf 2 (Light)");
        assert!(library_of(&skins).find(&id).is_some_and(|e| e.id == id), "listed under the same id");
        assert_eq!(lib.theme(&id, true).wallpaper, pack.join("leaf.png").to_string_lossy());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn long_names_are_imported_and_numbered_within_bounds() {
        let (base, skins, from) = import_test("long");
        std::fs::write(from.join("letter.docx"), "x").unwrap();
        let long = flower(&base).repeat(3) + &"a".repeat(140);
        let picture = format!("{long}.jpg");
        std::fs::write(from.join(&picture), "one").unwrap();
        let file = from.join(format!("{long}.glint"));
        std::fs::write(&file, format!("wallpaper: {picture}\n")).unwrap();
        let mut lib = library_of(&skins);
        let (id, said, problem) = import(&mut lib, &file, &skins);
        assert!(!problem, "{said}");
        assert_eq!(std::fs::read_to_string(skins.join(&picture)).unwrap(), "one");
        assert_eq!(lib.theme(&id, true).wallpaper, skins.join(&picture).to_string_lossy());
        // Again with another picture of that name: numbered, cut to make room.
        std::fs::write(from.join(&picture), "two").unwrap();
        let (id2, said, problem) = import(&mut lib, &file, &skins);
        assert!(!problem, "{said}");
        assert_ne!(id2, id);
        let numbered: Vec<String> = files_in(&skins).into_iter().filter(|f| f.ends_with(" 2.jpg")).collect();
        assert_eq!(numbered.len(), 1, "{:?}", files_in(&skins));
        assert!(numbered[0].chars().count() <= MAX_STEM + 4, "{}", numbered[0]);
        assert_eq!(std::fs::read_to_string(skins.join(&numbered[0])).unwrap(), "two");
        assert_eq!(lib.theme(&id2, true).wallpaper, skins.join(&numbered[0]).to_string_lossy());
        let _ = std::fs::remove_dir_all(&base);
    }

    // ------------------------------------------------------------ ids that stay

    /// A skin in a folder keeps its id when another skin of that file name comes or goes (two
    /// packs with a dark.glint each, one deleted or renamed): the one in use stays in use. The ids
    /// Glint 1 saved still find their skin.
    #[test]
    fn a_skins_id_stays_when_other_skins_come_and_go() {
        let (base, skins, _) = import_test("ids");
        let f = flower(&base);
        for pack in ["Win7", "XP"] {
            std::fs::create_dir_all(skins.join(pack)).unwrap();
            std::fs::write(skins.join(pack).join("dark.glint"), format!("name: {pack} dark\ndark: true\naccent: #123456\n")).unwrap();
        }
        let lib = library_of(&skins);
        let ids: Vec<String> = lib.entries().iter().filter(|e| !e.built_in()).map(|e| e.id.clone()).collect();
        assert_eq!(ids, ["file:Win7\\dark", "file:XP\\dark"]);
        // Glint 1 gave the first of them the plain id: it still finds that one.
        assert_eq!(lib.find("file:dark").map(|e| e.id.as_str()), Some("file:Win7\\dark"));
        assert_eq!(lib.find("FILE:DARK").map(|e| e.id.as_str()), Some("file:Win7\\dark"));
        assert_eq!(lib.find("file:xp\\DARK").map(|e| e.id.as_str()), Some("file:XP\\dark"));
        // The one in use (XP's) after Win7's is deleted, then after a dark.glint turns up in the
        // skins folder itself and in another pack: still XP's, by the same id.
        std::fs::remove_dir_all(skins.join("Win7")).unwrap();
        let lib = library_of(&skins);
        assert_eq!(lib.find("file:XP\\dark").map(|e| e.id.as_str()), Some("file:XP\\dark"));
        assert_eq!(lib.twin("file:XP\\dark", true), "file:XP\\dark");
        std::fs::write(skins.join("dark.glint"), "name: Plain dark\ndark: true\n").unwrap();
        std::fs::create_dir_all(skins.join("Aero")).unwrap();
        std::fs::write(skins.join("Aero").join("dark.glint"), "name: Aero dark\ndark: true\n").unwrap();
        let lib = library_of(&skins);
        assert_eq!(lib.find("file:XP\\dark").map(|e| e.name.as_str()), Some("XP dark"));
        assert_eq!(lib.find("file:dark").map(|e| e.name.as_str()), Some("Plain dark"), "the skins folder's own first");
        assert_eq!(lib.find("file:Aero\\dark").map(|e| e.name.as_str()), Some("Aero dark"));
        // A skin alone in a folder of its own (as Glint 1 gave "file:Pink"), with odd names too.
        let odd = format!("{f} Pack");
        std::fs::create_dir_all(skins.join(&odd)).unwrap();
        std::fs::write(skins.join(&odd).join(format!("{f}.glint")), "name: Odd\n").unwrap();
        let lib = library_of(&skins);
        let odd_id = format!("file:{odd}\\{f}");
        assert_eq!(lib.find(&format!("file:{f}")).map(|e| e.id.clone()), Some(odd_id.clone()));
        // The light-or-dark switch at start gives the id as listed (what settings then saves).
        assert_eq!(lib.twin(&format!("file:{f}"), true), odd_id);
        assert_eq!(lib.twin(&format!("file:{f}"), false), odd_id, "no other half: itself");
        // An id of nothing finds nothing; a built-in id isn't mistaken for a file's.
        assert!(lib.find("file:nothing").is_none() && lib.find("file:").is_none() && lib.find("file").is_none() && lib.find("").is_none());
        assert!(lib.find(DEFAULT).is_some_and(|e| e.built_in()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Renaming a skin in a folder keeps it in that folder and under that folder's id: listed
    /// again, it's the same id. A skin moved off a taken file name (`move_off`) keeps its folder too.
    #[test]
    fn a_renamed_skin_in_a_folder_keeps_its_folder_in_its_id() {
        let (base, skins, _) = import_test("rename-pack");
        std::fs::create_dir_all(skins.join("XP")).unwrap();
        std::fs::write(skins.join("XP").join("dark.glint"), "name: dark\ndark: true\n").unwrap();
        std::fs::write(skins.join("dark.glint"), "name: Plain\ndark: true\n").unwrap();
        let mut lib = library_of(&skins);
        // Its own name in other capitals is fine, though another file is called dark too.
        assert!(lib.may_rename("file:XP\\dark", "DARK"));
        assert!(!lib.may_rename("file:XP\\dark", "plain"), "another skin's name");
        let new_id = lib.rename("file:XP\\dark", "Night").unwrap();
        assert_eq!(new_id, "file:XP\\Night");
        wait_for_writes(Duration::from_secs(10));
        lib.take_written();
        assert!(skins.join("XP").join("Night.glint").is_file());
        let listed = library_of(&skins);
        assert!(listed.find(&new_id).is_some_and(|e| e.id == new_id && e.name == "Night"));
        assert_eq!(listed.find("file:dark").map(|e| e.name.as_str()), Some("Plain"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Changing only capitals of a name - in any letters, "élan" to "Élan" - isn't refused as a
    /// name that's taken; another skin's name, in other capitals, is.
    #[test]
    fn a_change_of_capitals_in_any_letters_is_the_same_name() {
        // As listed (the file names known), without touching the disk.
        let mut lib = Library::new();
        let entry = |stem: &str| Entry::new(format!("file:{stem}"), format!("name: {stem}\n"), Some(PathBuf::from(format!("C:\\skins\\{stem}.glint"))), stem);
        lib.take_own(vec![entry("élan"), entry("Ωmega")]);
        lib.stems = ["élan", "ωmega"].into_iter().map(String::from).collect();
        let id = "file:élan".to_string();
        assert_eq!(lib.find("FILE:ÉLAN").map(|e| e.id.clone()), Some(id.clone()));
        assert!(lib.may_rename(&id, "Élan") && lib.may_rename(&id, "ÉLAN") && lib.may_rename(&id, "Elan"));
        assert!(!lib.may_rename(&id, "ωMEGA") && !lib.may_rename(&id, "  ΩMEGA "), "the other skin's, in other capitals");
        assert_eq!(lib.free_name("ÉLAN"), "ÉLAN 2", "a new skin keeps clear of it");
        assert_eq!(lib.free_name("ωmega"), "ωmega 2");
        let at = lib.entries().iter().position(|e| e.id == id).unwrap();
        assert!(lib.free_for(at, "Élan"), "rename's own check agrees");
        // Renamed for real, where Windows takes such names.
        let (base, skins, _) = import_test("caps");
        if !crate::search::odd_names_work(&base) {
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        std::fs::write(skins.join("élan.glint"), "name: élan\n").unwrap();
        let mut lib = library_of(&skins);
        let id = lib.entries().iter().find(|e| e.name == "élan").map(|e| e.id.clone()).unwrap();
        let new_id = lib.rename(&id, "Élan").expect("renamed");
        wait_for_writes(Duration::from_secs(10));
        lib.take_written();
        assert_eq!(lib.find(&new_id).map(|e| e.name.as_str()), Some("Élan"));
        // Done again (no change): nothing.
        assert_eq!(lib.rename(&new_id, "Élan"), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A wallpaper on the network isn't looked for while a skin is read (a server or a mapped
    /// drive that's away would hold up the window); one on this computer that isn't there is
    /// still noted.
    #[test]
    fn a_wallpaper_on_the_network_is_never_looked_for() {
        for p in [r"\\server\share\bg.jpg", "//server/share/bg.jpg", r"\\?\UNC\server\share\bg.jpg", r"\/server\bg.jpg"] {
            assert!(on_network(p), "{p}");
        }
        for p in [r"C:\Pictures\bg.jpg", "c:/bg.jpg", "bg.jpg", "", r"\bg.jpg", "\u{1f338}:\\x"] {
            assert!(!on_network(p), "{p}");
        }
        let missing = |text: &str| parse(text, "x").notes.iter().any(|n| n.text.starts_with(WALLPAPER_MISSING));
        assert!(!missing("wallpaper: \\\\server\\share\\bg.jpg\n"));
        assert!(!missing("wallpaper: \"\\\\server\\share\\\u{1f338}.jpg\"\n"));
        let gone = std::env::temp_dir().join(format!("glint-no-such-{}", std::process::id())).join("\u{1f338} bg.jpg");
        assert!(missing(&format!("wallpaper: {}\n", gone.display())), "a missing picture here is still noted");
        assert!(missing("wallpaper: Q:\\nope.jpg\n") || on_network("Q:\\"), "a drive that isn't there: noted");
    }
}
