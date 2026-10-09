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
//! - The rest is in [`OTHERS`]: the name, light or dark, the shapes (straight / slanted / rounded,
//!   how far things lean, how round corners are), the row height, the font and its size, and a
//!   word for the logo (without one, the title bars show the chrome wordmark: logo.rs).
//! - Whatever a skin leaves out comes from AMD Radeon (a dark skin) or AMD Radeon (Light) (a light
//!   one), so a skin can be three lines. A line Glint doesn't understand is skipped with a note (the
//!   customiser shows them); nothing in a skin can stop Glint from starting.
//!
//! **Where skins are.** The built-in ones (AMD Radeon, NVIDIA and their "(Light)" versions) are
//! compiled into glint.exe from `skins/*.glint`, in the same format. A person's own skins are
//! `*.glint` files in the `skins` folder of the config folder (settings.rs), made by the customiser
//! (skinwin.rs) or by hand. Ids in settings.json: `amd-radeon`, `amd-radeon-light`, `nvidia`,
//! `nvidia-light`, and `file:<file name without .glint>`.
//!
//! **Light and dark.** A skin's light version is the skin with the same name and " (Light)" after
//! it (and `dark: false`); the light/dark choice picks between the two (`Library::twin`).
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
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::log;
use crate::settings;
use crate::shell;
use crate::theme::{size, Color, Shape, Theme};

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

/// The built-in skins, in the order they're listed: id and text.
const BUILT_IN: [(&str, &str); 4] = [
    (DEFAULT, include_str!("../skins/amd-radeon.glint")),
    (DEFAULT_LIGHT, include_str!("../skins/amd-radeon-light.glint")),
    ("nvidia", include_str!("../skins/nvidia.glint")),
    ("nvidia-light", include_str!("../skins/nvidia-light.glint")),
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
];

/// Everything else a skin can set, with what it does.
pub const OTHERS: &[(&str, &str)] = &[
    ("name", "Name in Settings; its light version adds \" (Light)\""),
    ("description", "One line about it"),
    ("author", "Who made it"),
    ("dark", "true or false"),
    ("menus", "Right-click menus: dark or light"),
    ("shape", "slanted, straight or rounded"),
    ("slant", "How far buttons lean, 0 to 20"),
    ("path-slant", "How far the current folder's box leans, 0 to 20"),
    ("radius", "Corner roundness, 0 to 16"),
    ("row-height", "File list row height, 20 to 48"),
    ("tab-line-width", "Current tab's line, 0 to 6"),
    ("toolbar-line-width", "Toolbar's line, 0 to 6"),
    ("font", "Fonts to try, best first"),
    ("font-size", "8pt to 14pt (or pixels: 12px)"),
    ("logo-text", "The word in the logo's box, at most 12 letters"),
];

/// Other names people may write for a setting (CSS habits, the code's names).
const ALIASES: [(&str, &str); 10] = [
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

fn known_key(key: &str) -> bool {
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
    } else if let Some(n) = v.strip_suffix("px") {
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
    done: HashMap<String, Color>,
    busy: HashSet<String>,
    notes: &'a mut Vec<Note>,
}

impl Resolver<'_> {
    /// A colour expression: "#hex", "none", "rgb(..)", a colour's name, each maybe with "NN%".
    fn expression(&mut self, v: &str) -> Result<Color, String> {
        let v = v.trim();
        let (term, strength) = match v.rsplit_once(char::is_whitespace) {
            Some((t, pct)) if pct.ends_with('%') => {
                let p = pct.trim_end_matches('%').trim().parse::<f32>().ok().filter(|p| p.is_finite()).ok_or_else(|| format!("\u{201c}{pct}\u{201d} isn't a strength like 16%"))?;
                (t.trim(), Some((p / 100.0).clamp(0.0, 1.0)))
            }
            _ => (v, None),
        };
        let lower = term.to_lowercase();
        let color = if lower == "none" || lower == "transparent" {
            Color::rgba(0, 0, 0, 0.0)
        } else if let Some(c) = parse_hex(term).or_else(|| parse_rgb(term)) {
            c
        } else {
            let name = normalize_key(term);
            if !COLOURS.iter().any(|(k, _)| *k == name) {
                return Err(format!("\u{201c}{term}\u{201d} isn't a colour: write it like #e3262d, or another colour's name"));
            }
            self.colour(&name).ok_or_else(|| format!("\u{201c}{name}\u{201d} refers back to itself"))?
        };
        Ok(match strength {
            Some(s) => color.with_alpha(color.a * s),
            None => color,
        })
    }

    /// The colour called `key` (None while it's being worked out: a circle).
    fn colour(&mut self, key: &str) -> Option<Color> {
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
                Err(why) => self.notes.push(Note { line, text: format!("{why}. Using the standard {key} instead.") }),
            }
        }
        if result.is_none() {
            if let Some(v) = self.base.get(key).cloned() {
                result = self.expression(&v).ok();
            }
        }
        self.busy.remove(key);
        // (Never reached with the built-in base skins, which name every colour; the tests check.)
        let c = result.unwrap_or(Color::hex(0x808080));
        self.done.insert(key.to_string(), c);
        Some(c)
    }
}

/// Reads a skin. Never fails: what can't be understood is noted and taken from the base skin.
/// `fallback_name` is the name when the text has none (the file's name).
pub fn parse(text: &str, fallback_name: &str) -> Parsed {
    let mut notes = Vec::new();
    let lines = read_lines(text, &mut notes);
    let mut own: HashMap<String, (String, usize)> = HashMap::new();
    for l in &lines {
        if !known_key(&l.key) {
            notes.push(Note { line: l.number, text: format!("\u{201c}{}\u{201d} isn't a skin setting. Skipped.", l.key) });
            continue;
        }
        own.insert(l.key.clone(), (l.value.clone(), l.number));
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
    let mut c = |key: &str| r.colour(key).unwrap_or(Color::hex(0x808080));
    let window = c("background");
    let mut theme = Theme {
        dark,
        window,
        titlebar: c("title-bar"),
        toolbar: c("toolbar"),
        panel: c("panel"),
        sidebar: c("sidebar"),
        statusbar: c("status-bar"),
        input: c("input"),
        chip: c("button"),
        chip_hover: c("button-hover"),
        text: c("text"),
        text_strong: c("text-bright"),
        muted: c("muted"),
        disabled: c("disabled"),
        accent: c("accent"),
        accent_tint: c("selection"),
        select_edge: c("selection-edge"),
        on_accent: c("accent-text"),
        hover: c("hover"),
        button_hover: c("tool-hover"),
        button_pressed: c("tool-pressed"),
        crumb_hover: c("path-hover"),
        line: c("line"),
        scroll_thumb: c("scrollbar"),
        scroll_thumb_hover: c("scrollbar-hover"),
        bar_track: c("usage-bar"),
        logo: c("logo"),
        logo_ink: c("logo-letters"),
        tab_line: c("tab-line"),
        toolbar_line: c("toolbar-line"),
        caption_hover: c("caption-hover"),
        caption_pressed: c("caption-pressed"),
        close_hover: c("close-hover"),
        menus_dark: dark,
        shape: Shape::Slanted,
        slant: 8.0,
        path_slant: 6.0,
        radius: 6.0,
        row_h: size::ROW_H,
        tab_line_w: 3.0,
        toolbar_line_w: 2.0,
        font: String::new(),
        font_size: size::FONT,
        logo_text: DEFAULT_LOGO_TEXT.into(),
        logo_style: crate::settings::LogoStyle::Box,
    };

    // The rest: as the skin says, else as the base says.
    let value = |key: &str| -> (Option<(String, usize)>, Option<String>) { (own.get(key).cloned(), base.get(key).cloned()) };
    let number = |key: &str, min: f32, max: f32, pt: Option<f32>, notes: &mut Vec<Note>| -> Option<f32> {
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
    if let Some(x) = number("slant", 0.0, 20.0, None, &mut notes) {
        theme.slant = x;
    }
    // A skin that sets only "slant" gets a path box in proportion (AMD Radeon's 8 and 6).
    let path_default = if own.contains_key("slant") && !own.contains_key("path-slant") { Some(theme.slant * 0.75) } else { None };
    match path_default {
        Some(x) => theme.path_slant = x,
        None => {
            if let Some(x) = number("path-slant", 0.0, 20.0, None, &mut notes) {
                theme.path_slant = x;
            }
        }
    }
    if let Some(x) = number("radius", 0.0, 16.0, None, &mut notes) {
        theme.radius = x;
    }
    if let Some(x) = number("row-height", 20.0, 48.0, None, &mut notes) {
        theme.row_h = x.round();
    }
    if let Some(x) = number("tab-line-width", 0.0, 6.0, None, &mut notes) {
        theme.tab_line_w = x;
    }
    if let Some(x) = number("toolbar-line-width", 0.0, 6.0, None, &mut notes) {
        theme.toolbar_line_w = x;
    }
    if let Some(x) = number("font-size", 8.0 * PT, 14.0 * PT, Some(PT), &mut notes) {
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

    let text_of = |key: &str, max: usize| own.get(key).map(|(v, _)| v.trim().chars().take(max).collect::<String>().trim().to_string()).filter(|v| !v.is_empty());
    let name = text_of("name", MAX_NAME).unwrap_or_else(|| fallback_name.chars().take(MAX_NAME).collect());
    let description = text_of("description", MAX_DESCRIPTION).unwrap_or_default();
    notes.sort_by_key(|n| n.line);
    notes.dedup();
    Parsed { theme, name, description, notes }
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

fn parse_shape(v: &str) -> Option<Shape> {
    match normalize_key(v).as_str() {
        "slanted" | "slant" | "slanted-buttons" | "angled" => Some(Shape::Slanted),
        "straight" | "square" | "rect" | "flat" | "none" => Some(Shape::Straight),
        "rounded" | "round" => Some(Shape::Rounded),
        _ => None,
    }
}

/// Dark unless the background the skin gives is light (only when it doesn't say).
fn guess_dark(own: &HashMap<String, (String, usize)>) -> bool {
    own.get("background").and_then(|(v, _)| parse_hex(v).or_else(|| parse_rgb(v))).is_none_or(|c| c.lightness() < 0.5)
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
        "// {name} - made from {came_from}.\r\n// One setting per line: \"name: value\". Colours are #rrggbb (#rrggbbaa: see-through).\r\n\r\n"
    );
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
    pub text: String,
    /// The file (a person's own skin); None for a built-in one.
    pub file: Option<PathBuf>,
}

impl Entry {
    fn new(id: String, text: String, file: Option<PathBuf>, fallback_name: &str) -> Entry {
        let p = parse(&text, fallback_name);
        Entry { id, name: p.name, description: p.description, dark: p.theme.dark, text, file }
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

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id.eq_ignore_ascii_case(id))
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

    /// The id of `id`'s light (`dark` false) or dark version: the skin of the same kind (built in,
    /// or the person's) called the same with or without " (Light)". `id` itself when there's none.
    pub fn twin(&self, id: &str, dark: bool) -> String {
        let Some(e) = self.find(id) else { return id.to_string() };
        if e.dark == dark {
            return e.id.clone();
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
            Some(e) => parse(&e.text, &e.name).theme,
            None => built_in_theme(if dark { DEFAULT } else { DEFAULT_LIGHT }),
        }
    }

    /// The notes about skin `id`'s text (for the customiser).
    pub fn notes(&self, id: &str) -> Vec<Note> {
        self.find(id).map(|e| parse(&e.text, &e.name).notes).unwrap_or_default()
    }

    // ------------------------------------------------------------ the person's own, changed

    /// A free name for a new skin, starting from `wanted` ("My skin", "My skin 2"...), whose file
    /// name is free too.
    pub fn free_name(&self, wanted: &str) -> String {
        let wanted = wanted.trim();
        let wanted = if wanted.is_empty() { "My skin" } else { wanted };
        let taken = |name: &str| {
            let id = id_for(name);
            self.entries.iter().any(|e| e.name.trim().eq_ignore_ascii_case(name) || e.id.eq_ignore_ascii_case(&id)) || self.stems.contains(&file_stem(name).to_lowercase())
        };
        if !taken(wanted) {
            return wanted.to_string();
        }
        (2..).map(|n| format!("{wanted} {n}")).find(|n| !taken(n)).unwrap_or_else(|| wanted.to_string())
    }

    /// Adds a skin of the person's with this text (called `name`, a free one) and saves it.
    /// Returns its id.
    pub fn add(&mut self, name: &str, text: &str) -> String {
        let id = id_for(name);
        let file = folder().join(format!("{}.{EXTENSION}", file_stem(name)));
        let entry = Entry::new(id.clone(), text.to_string(), Some(file), name);
        let at = self.entries.iter().position(|e| !e.built_in() && e.name.to_lowercase() > entry.name.to_lowercase()).unwrap_or(self.entries.len());
        self.entries.insert(at, entry);
        self.generation += 1;
        self.stems.insert(file_stem(name).to_lowercase());
        // Written as a new file: if one of that name turned up meanwhile, it isn't replaced.
        if let Some(e) = self.entries.iter().find(|e| e.id == id) {
            let job = FileJob::Write { file: folder().join(format!("{}.{EXTENSION}", file_stem(name))), text: e.text.clone(), name: e.name.clone(), new: true };
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
        let new_id = id_for(new_name);
        let old_file = self.entries[at].file.clone()?;
        if !self.free_for(at, new_name) {
            return None; // the customiser picks free names, and says so
        }
        let new_file = folder().join(format!("{}.{EXTENSION}", file_stem(new_name)));
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
        let (new_id, stem) = (id_for(new_name), file_stem(new_name));
        let same_stem = self.entries[at].file.as_ref().and_then(|f| f.file_stem()).is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(&stem));
        let other = self.entries.iter().enumerate().any(|(i, e)| i != at && e.id.eq_ignore_ascii_case(&new_id));
        !other && (same_stem || !self.stems.contains(&stem.to_lowercase()))
    }

    /// Deletes one of the person's skins: its file goes to the Recycle Bin.
    pub fn delete(&mut self, id: &str) {
        let Some(at) = self.entries.iter().position(|e| e.id == id && !e.built_in()) else { return };
        let e = self.entries.remove(at);
        self.dirty.remove(id);
        self.generation += 1;
        if let Some(file) = e.file {
            self.queue(FileJob::Delete { file, name: e.name });
        }
    }

    /// Exports a skin's text to `to` (any file the person picked).
    pub fn export(&mut self, id: &str, to: PathBuf) {
        if let Some(e) = self.find(id) {
            let (text, name) = (e.text.clone(), e.name.clone());
            self.queue(FileJob::Export { file: to, text, name });
        }
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
            let (new_id, new_file) = (id_for(&name), folder().join(format!("{}.{EXTENSION}", file_stem(&name))));
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

/// The id of a person's skin called `name` (by the name its file gets).
pub fn id_for(name: &str) -> String {
    format!("{FILE_PREFIX}{}", file_stem(name))
}

/// A file name (without .glint) for a skin's name: the characters Windows doesn't allow in names
/// left out, not empty, not one of Windows' device names.
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c }).collect();
    let mut stem: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    stem = stem.trim_matches(['.', ' ']).chars().take(60).collect::<String>().trim_end_matches(['.', ' ']).to_string();
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

/// Text from a file's bytes: UTF-8 (with or without its mark) or UTF-16 (Notepad's "Unicode").
pub fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).trim_start_matches('\u{feff}').to_string()
}

/// What a listing of the skins folder found.
struct Listing {
    /// The person's skins, by name.
    own: Vec<Entry>,
    /// Every *.glint file's name (lower case, without .glint), also those left out of `own`.
    stems: HashSet<String>,
}

/// The person's skins in `dir`: every *.glint file (not too many, not too big), by name.
fn read_folder(dir: &Path) -> Listing {
    let mut found = Listing { own: Vec::new(), stems: HashSet::new() };
    let Ok(listing) = std::fs::read_dir(dir) else { return found };
    for item in listing.flatten() {
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
        found.own.push(Entry::new(format!("{FILE_PREFIX}{stem}"), decode(&bytes), Some(path), &stem));
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
            let same_file = from.to_string_lossy().eq_ignore_ascii_case(&to.to_string_lossy());
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
            recycle(&file).map(|()| format!("{name} is in the Recycle Bin")).map_err(|e| format!("Couldn't delete the skin \u{201c}{name}\u{201d}: {e}"))
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

/// Sends a file to the Recycle Bin (Windows' own way, without asking: Glint asked already).
fn recycle(file: &Path) -> Result<(), String> {
    let mut from: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(file.as_os_str()).collect();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    // SAFETY: the struct and the double-NUL-terminated list live across the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code == 0 && !op.fAnyOperationsAborted.as_bool() {
        Ok(())
    } else {
        Err(format!("Windows said {code:#x}"))
    }
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
    // SAFETY: `path` lives across the call; the handle is closed below.
    let Ok(change) = (unsafe { FindFirstChangeNotificationW(&path, false, FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE) })
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
        assert_eq!(names, ["AMD Radeon", "AMD Radeon (Light)", "NVIDIA", "NVIDIA (Light)", "Alone", "Moss", "Moss (Light)", "NVIDIA (Light)"]);
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
        let listing = read_folder(&dir);
        let listed = &listing.own;
        let ids: Vec<&str> = listed.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["file:Mine", "file:Other"]);
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
}
