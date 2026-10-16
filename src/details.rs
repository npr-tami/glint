//! The details panel (Alt+P, "ShowDetails", off by default): on the right of the
//! file list, what is selected - a preview (the picture Explorer would show, the first lines of a
//! text file, or a big icon), its name, chips saying what it is and how big, its dates and folder,
//! and the Open with…, Copy path and Open buttons.
//!
//! - Nothing selected: the folder on screen (its icon, name, how many items, where it is).
//! - One item: its thumbnail (thumbs.rs) or text, and what peek.rs finds out about it off the UI
//!   thread (picture size, a song's or video's length, a folder's item count). A drive's free space
//!   comes from the drive list, which already knows it.
//! - One file also has a Fingerprint row: Show works out its SHA-256 with Windows' own cryptography
//!   (CNG), on a thread of its own, in percent for a big file. Then Copy copies the code, and Check
//!   compares it with a code copied from a website (for an MD5 or SHA-1 code, the file's own of that
//!   kind is worked out to compare). Selecting something else stops it and forgets it. Not in the
//!   Recycle Bin or Home, nor inside a .zip or .7z.
//! - Several: a little stack of cards with the count, how many files and folders, and the files' size.
//!
//! The panel never waits: each frame shows what is there, and the thumbnail service, peek.rs and
//! the fingerprint's thread repaint the window when more arrives. The layout of the box and the
//! buttons doesn't depend on what's shown, so nothing jumps while arrowing through files.
//!
//! The same pieces (what is selected, what to say about it, the name, chips and rows) are used by
//! the Space preview (preview.rs) for files that are neither pictures nor text.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Security::Cryptography::{
    BCryptCloseAlgorithmProvider, BCryptCreateHash, BCryptDestroyHash, BCryptFinishHash, BCryptHashData, BCryptOpenAlgorithmProvider,
    BCRYPT_ALG_HANDLE, BCRYPT_HASH_HANDLE, BCRYPT_MD5_ALGORITHM, BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS, BCRYPT_SHA1_ALGORITHM, BCRYPT_SHA256_ALGORITHM,
};

use crate::app::{AppState, Tab};
use crate::folder::{self, Entry, EntryKind};
use crate::peek::{self, Answer, Ask, Facts, FactsCache, Peeker, TextLimits, What};
use crate::pathbar::{fill_box, fill_box_paint, outline_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::shell;
use crate::theme::{ButtonStyle, Color, Theme};
use crate::thumbs::{self, Picture};
use crate::ui::{Command, Ctx, Effect, MouseButton, MouseEvent, Point, Rect};

/// This PC and the Network as places the shell knows (for their icons).
pub const THIS_PC_ID: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
const NETWORK_ID: &str = "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";

/// The panel's sizes, in DIPs.
pub mod dims {
    /// The space round everything inside the panel.
    pub const PAD: f32 = 14.0;
    pub const LABEL_FONT: f32 = 10.5;
    pub const NAME_FONT: f32 = 13.0;
    pub const NAME_LINE: f32 = 18.0;
    pub const CHIP_FONT: f32 = 10.5;
    pub const CHIP_H: f32 = 20.0;
    pub const CHIP_PAD: f32 = 9.0;
    pub const CHIP_GAP: f32 = 6.0;
    pub const ROW_H: f32 = 26.0;
    pub const OPEN_H: f32 = 34.0;
    pub const BUTTON_H: f32 = 30.0;
    /// How far the Open button's right edge leans.
    pub const SLANT: f32 = 10.0;
    pub const MONO_FONT: f32 = 11.0;
    pub const MONO_LINE: f32 = 15.0;
    /// Icons are drawn at most this big in the box.
    pub const ICON_MAX: f32 = 128.0;
}

// ---------------------------------------------------------------- what is selected

/// What is selected in the tab on screen, as the panel and the preview see it.
#[derive(Clone, Debug)]
pub enum Sel {
    /// Nothing: the folder itself ("" = This PC), and how many items it shows.
    Folder { path: String, items: usize },
    One(Box<One>),
    Many { count: usize, files: usize, folders: usize, bytes: u64 },
}

/// One item and what the panel needs to know about it.
#[derive(Clone, Debug)]
pub struct One {
    pub entry: Entry,
    /// Its index in the tab's listing.
    pub index: usize,
    /// Its full path (in the Recycle Bin: where it is kept now).
    pub path: String,
    /// What the shell is asked for its picture (a network drive: the Network place, since a lost
    /// one could keep the shell waiting).
    pub shell_path: String,
    /// What Copy path copies (in the Recycle Bin: where it was deleted from).
    pub copy: String,
    /// Where it is (search results) or was (the Recycle Bin).
    pub location: Option<String>,
    /// A Recycle Bin item: its date is when it was deleted.
    pub deleted: bool,
    /// Tells this version of the item apart (path, date, size): facts and pictures are kept by it.
    pub key: String,
    pub version: u64,
}

impl One {
    pub fn is_file(&self) -> bool {
        self.entry.kind == EntryKind::File
    }

    /// The question for peek.rs about this item.
    pub fn ask(&self, text: Option<TextLimits>, picture: Option<(u32, u64)>, show_hidden: bool, show_system: bool) -> Ask {
        let what = match self.entry.kind {
            EntryKind::File => What::File,
            EntryKind::Folder => What::Folder,
            EntryKind::Drive => What::Drive,
        };
        Ask {
            key: self.key.clone(),
            path: self.path.clone(),
            what,
            ext: self.entry.ext.clone(),
            attributes: self.entry.attributes,
            size: self.entry.size,
            show_hidden,
            show_system,
            text,
            picture,
        }
    }
}

/// The item at `index` of the tab's listing (None if it isn't shown).
pub fn one_at(tab: &Tab, index: usize) -> Option<One> {
    let e = tab.folder.entries.get(index)?;
    tab.row_of(index)?;
    let path = e.path_in(&tab.folder.path);
    let bin = tab.folder.bin.as_ref().filter(|b| b.items.len() == tab.folder.entries.len()).and_then(|b| b.items.get(index));
    let shell_path = if e.kind == EntryKind::Drive && &*e.type_name == "Network Drive" { NETWORK_ID.to_string() } else { path.clone() };
    let copy = bin.map(|b| b.original.clone()).filter(|o| !o.is_empty()).unwrap_or_else(|| path.clone());
    let location = match bin {
        Some(b) => Some(b.location.clone()),
        None if tab.is_searching() => Some(folder::result_folder(e).to_string()),
        None => None,
    }
    .filter(|l| !l.is_empty());
    let mut version = thumbs::version_of(e.modified, e.size);
    if e.kind == EntryKind::Drive {
        version ^= e.free; // a drive's free space read again
    }
    // A folder's count depends on whether hidden items show.
    let shown = if e.kind == EntryKind::Folder { u64::from(tab.show_hidden) + 2 * u64::from(tab.show_system) } else { 0 };
    let key = format!("{}|{version}|{shown}", path.to_lowercase());
    Some(One { entry: e.clone(), index, path, shell_path, copy, location, deleted: bin.is_some(), key, version })
}

/// What is selected in the tab on screen.
pub fn selection(app: &AppState) -> Sel {
    let tab = app.tab();
    let chosen = tab.selected_entries();
    match chosen.as_slice() {
        [] => Sel::Folder { path: tab.folder.path.clone(), items: tab.view.len() },
        [one] => one_at(tab, *one).map_or_else(|| Sel::Folder { path: tab.folder.path.clone(), items: tab.view.len() }, |o| Sel::One(Box::new(o))),
        many => {
            let (mut files, mut folders, mut bytes) = (0, 0, 0u64);
            for &i in many {
                let e = &tab.folder.entries[i];
                if e.is_container() {
                    folders += 1;
                } else {
                    files += 1;
                    bytes += e.size;
                }
            }
            Sel::Many { count: many.len(), files, folders, bytes }
        }
    }
}

/// The one item the Space preview shows: the one with the keyboard cursor if it's selected, else
/// the first selected (None when nothing is selected).
pub fn focused(app: &AppState) -> Option<One> {
    let tab = app.tab();
    let index = tab.focus.filter(|&f| tab.is_selected(f)).or_else(|| tab.selected_entries().first().copied())?;
    one_at(tab, index)
}

// ---------------------------------------------------------------- what to say about it

/// One line of details: a label on the left, its value on the right.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: &'static str,
    pub value: String,
    /// A path: cut in the middle when too long ("C:\Users\…\Photos").
    pub path: bool,
}

/// What the panel says about the selection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    pub name: String,
    /// The small boxes under the name: what it is and how big. What peek.rs finds out comes last,
    /// so the chips before it don't move when it arrives.
    pub chips: Vec<String>,
    pub rows: Vec<Row>,
    /// A drive's usage bar (0 to 1).
    pub bar: Option<f32>,
}

/// A file's kind in a word: its extension in capitals (PNG, MP4…), or its type name when it has none.
fn kind_text(e: &Entry) -> String {
    if !e.ext.is_empty() && e.ext.chars().count() <= 16 && !e.ext.chars().any(char::is_whitespace) {
        e.ext.to_uppercase()
    } else {
        e.type_name.to_string()
    }
}

/// "1 file", "3 files", "10,000 items".
fn count(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", folder::format_count(n))
    }
}

/// How long something plays (100-nanosecond steps): "3:07", or "1:02:45" from an hour on.
pub fn duration_text(ticks: u64) -> String {
    let secs = ticks / 10_000_000;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// What to say about the selection, with what peek.rs found out so far.
pub fn info(sel: &Sel, facts: Option<&Facts>, app: &AppState) -> Info {
    let sizes = app.settings.size_format;
    let dates = app.settings.date_format;
    let mut out = Info::default();
    match sel {
        Sel::Folder { path, items } => {
            out.name = app.display_name(path);
            out.chips.push(count(*items, "item", "items"));
            if !path.is_empty() && !crate::folder::is_place(path) {
                out.rows.push(Row { label: "Location", value: path.clone(), path: true });
            }
        }
        Sel::Many { count: n, files, folders, bytes } => {
            out.name = format!("{} items selected", folder::format_count(*n));
            // (The name already says how many; the size is of the files only, so it sits next to them.)
            if *files > 0 {
                out.chips.push(count(*files, "file", "files"));
                out.chips.push(folder::format_size_as(*bytes, sizes));
            }
            if *folders > 0 {
                out.chips.push(count(*folders, "folder", "folders"));
            }
        }
        Sel::One(one) => {
            let e = &one.entry;
            out.name = e.name.clone();
            match e.kind {
                EntryKind::Drive => {
                    out.chips.push(e.type_name.to_string());
                    if e.size > 0 {
                        out.chips.push(format!("{} free", folder::format_size(e.free)));
                        out.rows.push(Row { label: "Capacity", value: folder::format_size(e.size), path: false });
                        out.bar = Some((1.0 - e.free as f64 / e.size as f64).clamp(0.0, 1.0) as f32);
                    }
                    return out;
                }
                EntryKind::Folder => {
                    out.chips.push("Folder".into());
                    if let Some((n, more)) = facts.and_then(|f| f.items) {
                        out.chips.push(if more { format!("{}+ items", folder::format_count(n)) } else { count(n, "item", "items") });
                    }
                }
                EntryKind::File => {
                    out.chips.push(kind_text(e));
                    out.chips.push(folder::format_size_as(e.size, sizes));
                    if let Some((w, h)) = facts.and_then(|f| f.dims) {
                        out.chips.push(format!("{w} \u{d7} {h}"));
                    }
                    if let Some(t) = facts.and_then(|f| f.duration) {
                        out.chips.push(duration_text(t));
                    }
                }
            }
            let modified = folder::format_date_as(e.modified, dates);
            if !modified.is_empty() {
                out.rows.push(Row { label: if one.deleted { "Deleted" } else { "Modified" }, value: modified, path: false });
            }
            let created = folder::format_date_as(e.created, dates);
            if !created.is_empty() {
                out.rows.push(Row { label: "Created", value: created, path: false });
            }
            // Search results and the Recycle Bin say where the item is (or was); otherwise the folder's name.
            match &one.location {
                Some(l) => out.rows.push(Row { label: "Location", value: l.clone(), path: true }),
                None => {
                    if let Some(parent) = folder::parent(&one.path).filter(|p| !p.is_empty()) {
                        out.rows.push(Row { label: "Folder", value: app.display_name(&parent), path: false });
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- fitting text

/// Splits a name over at most two lines: the first as full as fits, broken
/// after a space, dash or dot near its end (before a dot, so an extension moves down whole); the
/// second ends in "…" when it's still too long, keeping the extension. `measure` says how wide a
/// text is.
pub fn wrap_name(text: &str, width: f32, measure: &mut dyn FnMut(&str) -> f32) -> (String, Option<String>) {
    let chars: Vec<char> = text.chars().collect();
    if width <= 0.0 || chars.len() < 2 || measure(text) <= width {
        return (text.to_string(), None);
    }
    let s = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
    // The most characters that fit on the first line.
    let (mut lo, mut hi) = (1usize, chars.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&s(0, mid)) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut cut = lo;
    const BREAKS: [char; 7] = [' ', '-', '_', '.', ',', ')', ']'];
    if let Some(soft) = chars[..cut].iter().rposition(|c| BREAKS.contains(c)) {
        if soft >= cut / 2 && soft > 0 && soft + 1 < chars.len() {
            cut = if chars[soft] == '.' { soft } else { soft + 1 };
        }
    }
    let one = s(0, cut).trim_end().to_string();
    let rest = s(cut, chars.len()).trim_start().to_string();
    if rest.is_empty() {
        return (one, None);
    }
    if measure(&rest) <= width {
        return (one, Some(rest));
    }
    let rest_chars: Vec<char> = rest.chars().collect();
    let ext_at = rest_chars.iter().rposition(|&c| c == '.').filter(|&i| i > 0 && rest_chars.len() - i <= 8).unwrap_or(rest_chars.len());
    let ext: String = rest_chars[ext_at..].iter().collect();
    let body = &rest_chars[..ext_at];
    let (mut lo, mut hi) = (0usize, body.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let trial = format!("{}\u{2026}{ext}", body[..mid].iter().collect::<String>().trim_end());
        if measure(&trial) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    (one, Some(format!("{}\u{2026}{ext}", body[..lo].iter().collect::<String>().trim_end())))
}

/// A path cut in the middle so it fits ("C:\Users\…\Photos\2026"): the drive (or server) stays,
/// then as much of the end as fits.
pub fn fit_path(text: &str, width: f32, measure: &mut dyn FnMut(&str) -> f32) -> String {
    if measure(text) <= width {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // The start that stays: "C:\" or "\\server\".
    let head_len = if text.starts_with("\\\\") {
        chars.iter().enumerate().skip(2).find(|(_, &c)| c == '\\').map_or(2, |(i, _)| i + 1)
    } else {
        chars.iter().position(|&c| c == '\\').map_or(0, |i| i + 1)
    };
    let head: String = chars[..head_len].iter().collect();
    let (mut lo, mut hi) = (0usize, chars.len() - head_len);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let tail: String = chars[chars.len() - mid..].iter().collect();
        if measure(&format!("{head}\u{2026}{tail}")) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let tail: String = chars[chars.len() - lo..].iter().collect();
    format!("{head}\u{2026}{tail}")
}

/// A fingerprint cut in the middle so it fits ("e3b0c442…7852b855"): as much of its start and its
/// end as fit, the same of each.
fn fit_code(code: &str, width: f32, measure: &mut dyn FnMut(&str) -> f32) -> String {
    if measure(code) <= width {
        return code.to_string();
    }
    let chars: Vec<char> = code.chars().collect();
    let cut = |n: usize| format!("{}\u{2026}{}", chars[..n].iter().collect::<String>(), chars[chars.len() - n..].iter().collect::<String>());
    let (mut lo, mut hi) = (0usize, chars.len() / 2);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&cut(mid)) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// Chips side by side, going on to another line when the next one doesn't fit; one wider than the
/// line is cut to its width. Returns each chip's box.
pub fn place_chips(chips: &[String], x: f32, y: f32, w: f32, measure: &mut dyn FnMut(&str) -> f32) -> Vec<Rect> {
    let mut out = Vec::with_capacity(chips.len());
    let (mut cx, mut cy) = (x, y);
    for c in chips {
        let cw = (measure(c) + 2.0 * dims::CHIP_PAD).ceil().min(w).max(0.0);
        if cx > x && cx + cw > x + w {
            cx = x;
            cy += dims::CHIP_H + dims::CHIP_GAP;
        }
        out.push(Rect::new(cx, cy, cw, dims::CHIP_H));
        cx += cw + dims::CHIP_GAP;
    }
    out
}

// ---------------------------------------------------------------- drawing pieces

fn chip_style(theme: &Theme) -> TextStyle {
    TextStyle::new(theme.text).size(dims::CHIP_FONT).align(Align::Center)
}

fn name_style(theme: &Theme) -> TextStyle {
    TextStyle::new(theme.text_strong).size(dims::NAME_FONT).semibold()
}

/// The name on one or two lines at `y`; returns how tall it was.
pub fn draw_name(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32, name: &str) -> f32 {
    let style = name_style(theme);
    let (one, two) = wrap_name(name, w, &mut |t| r.measure(t, &style));
    r.text(&one, Rect::new(x, y, w, dims::NAME_LINE), &style);
    match two {
        Some(t) => {
            r.text(&t, Rect::new(x, y + dims::NAME_LINE, w, dims::NAME_LINE), &style);
            2.0 * dims::NAME_LINE
        }
        None => dims::NAME_LINE,
    }
}

/// The chips at `y`; returns where they end (at least one line of chips, so the rows below don't
/// move when the chips peek.rs adds arrive).
pub fn draw_chips(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32, chips: &[String]) -> f32 {
    let style = chip_style(theme);
    let boxes = place_chips(chips, x, y, w, &mut |t| r.measure(t, &style));
    let mut bottom = y + dims::CHIP_H;
    for (text, b) in chips.iter().zip(&boxes) {
        fill_box(r, theme, *b, 0.0, theme.chip);
        r.text(text, b.inset(4.0, 0.0), &style);
        bottom = bottom.max(b.bottom());
    }
    bottom
}

/// A row's hairline and its label on the left, at `y`; returns where its value may start.
fn draw_row_label(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32, label: &str) -> f32 {
    let style = TextStyle::new(theme.muted);
    r.fill_rect(Rect::new(x, y, w, 1.0), theme.line);
    let label_w = r.measure(label, &style);
    r.text(label, Rect::new(x, y, w, dims::ROW_H), &style);
    (x + label_w + 12.0).min(x + w)
}

/// The rows from `y` down, stopping above `limit`; a hairline above each, the label on the left and
/// the value on the right. Returns where they end.
pub fn draw_rows(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32, limit: f32, rows: &[Row]) -> f32 {
    let value_style = TextStyle::new(theme.text).align(Align::Right);
    let mut y = y;
    for row in rows {
        if y + dims::ROW_H > limit {
            break;
        }
        let vx = draw_row_label(r, theme, x, y, w, row.label);
        let vw = (x + w - vx).max(0.0);
        let value = if row.path { fit_path(&row.value, vw, &mut |t| r.measure(t, &value_style)) } else { row.value.clone() };
        r.text(&value, Rect::new(vx, y, vw, dims::ROW_H), &value_style);
        y += dims::ROW_H;
    }
    y
}

/// A drive's usage bar: round ends in a rounded skin, with the path box's edge if the skin has one.
pub fn draw_bar(r: &mut Renderer, theme: &Theme, rect: Rect, used: f32) {
    let radius = theme.corner(rect.h);
    r.fill_paint(rect, radius, &theme.bar_track_paint);
    let w = rect.w * used.clamp(0.0, 1.0);
    if w > 0.0 {
        r.fill_paint(Rect::new(rect.x, rect.y, w.max(2.0 * radius).min(rect.w), rect.h), radius, &theme.usage_fill);
    }
    if radius > 0.0 {
        r.stroke_round_rect(rect.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, theme.input_border);
    }
}

/// Name, chips, usage bar and rows from the top of `area`; returns where they end.
pub fn draw_info(r: &mut Renderer, theme: &Theme, area: Rect, info: &Info) -> f32 {
    let mut y = area.y;
    y += draw_name(r, theme, area.x, y, area.w, &info.name) + 8.0;
    y = draw_chips(r, theme, area.x, y, area.w, &info.chips) + 10.0;
    if let Some(used) = info.bar {
        draw_bar(r, theme, Rect::new(area.x, y, area.w, 4.0), used);
        y += 14.0;
    }
    draw_rows(r, theme, area.x, y, area.w, area.bottom(), &info.rows)
}

/// A picture centred in `area`, shrunk to fit and keeping its shape; never blown up past its own
/// size (its pixels at this scale). Icons are drawn at most `icon_max` DIPs.
pub fn draw_fitted(r: &mut Renderer, pic: &std::sync::Arc<Picture>, area: Rect, scale: f32, icon_max: f32) -> bool {
    let (pw, ph) = (pic.width() as f32 / scale, pic.height() as f32 / scale);
    let mut k = (area.w / pw).min(area.h / ph).min(1.0);
    if pic.is_icon() {
        k = k.min(icon_max / pw.max(ph));
    }
    let (w, h) = ((pw * k).max(1.0), (ph * k).max(1.0));
    r.picture(pic, area.center(w, h), None, 1.0, true)
}

/// The first lines of a text file, as many as fit.
pub fn draw_lines(r: &mut Renderer, theme: &Theme, area: Rect, text: &peek::Text) {
    if text.lines.is_empty() && !text.cut {
        r.text("Empty file", area, &TextStyle::new(theme.muted).align(Align::Center));
        return;
    }
    let inner = area.inset(8.0, 6.0);
    let color = blend(theme.text, theme.window, 0.15);
    let mut y = inner.y;
    // (Long lines are cut at the box's edge.)
    r.push_clip(inner);
    for line in &text.lines {
        if y + dims::MONO_LINE > inner.bottom() {
            break;
        }
        if !line.is_empty() {
            r.mono_text(line, Rect::new(inner.x, y, inner.w, dims::MONO_LINE), dims::MONO_FONT, color);
        }
        y += dims::MONO_LINE;
    }
    r.pop_clip();
}

/// `a` moved `t` of the way towards `b`.
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    Color { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: a.a + (b.a - a.a) * t }
}

/// Several items: a little stack of cards with the count on the front one.
fn draw_stack(r: &mut Renderer, theme: &Theme, area: Rect, n: usize) {
    let big = TextStyle::new(theme.text_strong).size(24.0).bold().align(Align::Center);
    let cw = (area.w * 2.0 / 5.0).min(112.0);
    let ch = cw * 3.0 / 4.0;
    let step = 7.0;
    let label = folder::format_count(n);
    if cw < 24.0 || ch + step * 2.0 > area.h {
        r.text(&label, area, &big);
        return;
    }
    let front = Rect::new(area.x + (area.w - cw) / 2.0 + step, area.y + (area.h - ch) / 2.0 + step, cw, ch);
    for i in (0..3).rev() {
        let card = Rect::new(front.x - i as f32 * step, front.y - i as f32 * step, cw, ch);
        fill_box(r, theme, card, 0.0, blend(theme.window, theme.text, 0.05 + (2 - i) as f32 * 0.03));
        outline_box(r, theme, card, 0.0, 1.0, theme.line);
    }
    r.text(&label, front, &big);
}

/// A flat button in the skin's shape: lighter under the mouse, darker while pressed.
#[allow(clippy::too_many_arguments)]
fn button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, hover: bool, pressed: bool, accent: bool, text_color: Option<Color>) {
    if rect.is_empty() {
        return;
    }
    let style = TextStyle::new(text_color.unwrap_or(if accent { theme.on_accent } else { theme.text })).semibold().align(Align::Center);
    if theme.buttons == ButtonStyle::Liquid {
        // Glossy capsules, as SORT, VIEW and SETTINGS: the accent's glass for Open, the button's
        // for the others; white words with a dark shadow.
        // (A light accent is taken darker, so the white word on it stays easy to read.)
        let lit = theme.accent_paint.base().lightness() > 0.55;
        let glass = if accent && lit {
            theme.accent_paint.darker(0.3)
        } else if accent {
            theme.accent_paint
        } else if hover {
            theme.chip_hover_paint
        } else {
            theme.chip_paint
        };
        let glass = if pressed { glass.darker(0.12) } else if hover && accent { glass.lighter(0.12) } else { glass };
        let capsule = rect.inset(0.0, 2.0);
        r.liquid_capsule(capsule, &glass);
        let ink = text_color.unwrap_or(theme.button_text);
        let style = TextStyle::new(ink).bold().align(Align::Center);
        let shadow = glass.last().darker(0.5).with_alpha(0.8);
        r.text(label, Rect::new(capsule.x + 6.0, capsule.y + 1.0, (capsule.w - 12.0).max(0.0), capsule.h), &style.color(shadow));
        r.text(label, capsule.inset(6.0, 0.0), &style);
        return;
    }
    if accent {
        let fill = if pressed { blend(theme.accent, Color::hex(0), 0.18) } else if hover { blend(theme.accent, Color::hex(0xffffff), 0.1) } else { theme.accent };
        // In a slanted skin the right edge is cut at an angle (like AMD's Radeon software);
        // straight or rounded otherwise, as the skin says.
        let cut = theme.lean(dims::SLANT).min(rect.w / 2.0);
        if cut > 0.0 {
            r.fill_polygon(&[Point::new(rect.x, rect.y), Point::new(rect.right(), rect.y), Point::new(rect.right() - cut, rect.bottom()), Point::new(rect.x, rect.bottom())], fill);
        } else {
            fill_box(r, theme, rect, 0.0, fill);
        }
    } else {
        let fill = if pressed { theme.button_pressed.into() } else if hover { theme.chip_hover_paint } else { theme.chip_paint };
        fill_box_paint(r, theme, rect, 0.0, &fill);
        outline_box(r, theme, rect, 0.0, 1.0, theme.line);
    }
    r.text(label, rect.inset(6.0, 0.0), &style);
}

// ---------------------------------------------------------------- fingerprints

/// From this size a file is big: how far along working out its code is shows in percent.
const BIG: u64 = 8 << 20;
/// A file is read this much at a time.
const CHUNK: usize = 1 << 20;
/// While a big file is read, the window is repainted this often (for the percentage).
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// What the status bar says after Copy (the button says "Copied" while it shows).
const PRINT_COPIED: &str = "Fingerprint copied";

/// The kinds of fingerprint. Show works out the SHA-256; MD5 and SHA-1 (which older download pages
/// give) only to compare with a copied code of that kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Md5,
    Sha1,
    Sha256,
}

impl Kind {
    /// The kind of a code this many hex digits long: 32 MD5, 40 SHA-1, 64 SHA-256.
    fn of_digits(n: usize) -> Option<Kind> {
        match n {
            32 => Some(Kind::Md5),
            40 => Some(Kind::Sha1),
            64 => Some(Kind::Sha256),
            _ => None,
        }
    }

    /// How many bytes its code has (two hex digits each).
    fn bytes(self) -> usize {
        match self {
            Kind::Md5 => 16,
            Kind::Sha1 => 20,
            Kind::Sha256 => 32,
        }
    }

    /// Its name in Windows' cryptography.
    fn algorithm(self) -> PCWSTR {
        match self {
            Kind::Md5 => BCRYPT_MD5_ALGORITHM,
            Kind::Sha1 => BCRYPT_SHA1_ALGORITHM,
            Kind::Sha256 => BCRYPT_SHA256_ALGORITHM,
        }
    }
}

/// A code being worked out with Windows' own cryptography (CNG), fed a file a chunk at a time.
struct Hasher {
    alg: BCRYPT_ALG_HANDLE,
    hash: BCRYPT_HASH_HANDLE,
    kind: Kind,
}

impl Hasher {
    fn new(kind: Kind) -> Option<Hasher> {
        let (mut alg, mut hash) = (BCRYPT_ALG_HANDLE::default(), BCRYPT_HASH_HANDLE::default());
        // SAFETY: the calls write the handles; the provider is closed again when the hash can't be
        // made (and in Drop otherwise). Given no buffer, CNG keeps the hash's memory itself.
        unsafe {
            if BCryptOpenAlgorithmProvider(&mut alg, kind.algorithm(), PCWSTR::null(), BCRYPT_OPEN_ALGORITHM_PROVIDER_FLAGS(0)).is_err() {
                return None;
            }
            if BCryptCreateHash(alg, &mut hash, None, None, 0).is_err() {
                let _ = BCryptCloseAlgorithmProvider(alg, 0);
                return None;
            }
        }
        Some(Hasher { alg, hash, kind })
    }

    /// Feeds it more of the file.
    fn add(&mut self, data: &[u8]) -> bool {
        // SAFETY: a live hash, and bytes CNG only reads during the call.
        data.chunks(CHUNK).all(|part| unsafe { BCryptHashData(self.hash, part, 0) }.is_ok())
    }

    /// The code, in lower-case hex.
    fn finish(self) -> Option<String> {
        let mut code = vec![0u8; self.kind.bytes()];
        // SAFETY: a live hash, and room for exactly its code.
        let done = unsafe { BCryptFinishHash(self.hash, &mut code, 0) }.is_ok();
        done.then(|| code.iter().map(|b| format!("{b:02x}")).collect())
    }
}

impl Drop for Hasher {
    fn drop(&mut self) {
        // SAFETY: both handles were made in `new`, and are let go of here, once.
        unsafe {
            let _ = BCryptDestroyHash(self.hash);
            let _ = BCryptCloseAlgorithmProvider(self.alg, 0);
        }
    }
}

/// The `kind` code of the file at `path`, read a chunk at a time (`read` is told how much each
/// time). None if it can't be read, or once `stop` is set.
fn file_code(path: &str, kind: Kind, stop: &AtomicBool, read: &mut dyn FnMut(u64)) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Hasher::new(kind)?;
    let mut chunk = vec![0u8; CHUNK];
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match file.read(&mut chunk) {
            Ok(0) => return hasher.finish(),
            Ok(n) => {
                if !hasher.add(&chunk[..n]) {
                    return None;
                }
                read(n as u64);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// Working out one code of one file, on a thread of its own.
struct Job {
    kind: Kind,
    /// The file's size: the percentage is of this.
    total: u64,
    /// How much of it has been read.
    done: AtomicU64,
    /// Nobody wants the code any more: the thread stops at its next chunk.
    stop: AtomicBool,
    /// Once it's done, the code (None: the file couldn't be read).
    end: OnceLock<Option<String>>,
}

impl Job {
    /// Starts working out the `kind` code of the file at `path` (`size` bytes). `hwnd` is repainted
    /// as it goes along (for a big file) and when it's done.
    fn start(path: &str, size: u64, kind: Kind, hwnd: HWND) -> Running {
        let job = Arc::new(Job { kind, total: size, done: AtomicU64::new(0), stop: AtomicBool::new(false), end: OnceLock::new() });
        let (mine, path, window) = (job.clone(), path.to_string(), AtomicIsize::new(hwnd.0 as isize));
        let started = shell::background("fingerprint", move || {
            let mut shown = Instant::now();
            let code = file_code(&path, mine.kind, &mine.stop, &mut |n| {
                mine.done.fetch_add(n, Ordering::Relaxed);
                if mine.total >= BIG && shown.elapsed() >= PROGRESS_EVERY {
                    shown = Instant::now();
                    thumbs::repaint(&window);
                }
            });
            let _ = mine.end.set(code);
            thumbs::repaint(&window);
        });
        if !started {
            let _ = job.end.set(None); // no thread to be had: as if the file couldn't be read
        }
        Running(job)
    }

    /// How far along it is, in percent, for a big file (a small one is done in a moment).
    fn percent(&self) -> Option<u64> {
        (self.total >= BIG).then(|| (self.done.load(Ordering::Relaxed).saturating_mul(100) / self.total).min(99))
    }

    /// "Working it out… 45%": what is being done and, for a big file, how far along it is.
    fn progress(&self, doing: &str) -> String {
        match self.percent() {
            Some(n) => format!("{doing}\u{2026} {n}%"),
            None => format!("{doing}\u{2026}"),
        }
    }
}

/// The panel's hold on a job: letting go of it stops the job.
struct Running(Arc<Job>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}

/// A code being worked out, or what came of it.
enum Step {
    Working(Running),
    Done(String),
    /// The file couldn't be read.
    Failed,
}

/// What Check found out.
enum Check {
    /// The copied code is an MD5 or SHA-1: the file's own of that kind is being worked out.
    Working { copied: String, job: Running },
    Said(Verdict),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Matches,
    Differs,
    /// There was no code on the clipboard.
    NoCode,
    /// The file couldn't be read again (for its MD5 or SHA-1).
    Unreadable,
}

impl Verdict {
    /// What the panel says, in which colour.
    fn text(self, theme: &Theme) -> (&'static str, Color) {
        match self {
            Verdict::Matches => ("\u{2713} Matches the copied code", if theme.dark { Color::hex(0x3dd68c) } else { Color::hex(0x1a7f37) }),
            Verdict::Differs => ("\u{2717} Doesn't match the copied code", crate::codebox::PROBLEM_RED),
            Verdict::NoCode => ("Copy the code from the website first", theme.muted),
            Verdict::Unreadable => ("Couldn't read this file", theme.muted),
        }
    }
}

/// The fingerprint of the one file selected, from Show until something else is selected.
struct Print {
    /// The file (`One::key`), and its path and size.
    key: String,
    path: String,
    size: u64,
    sha256: Step,
    check: Option<Check>,
    /// The MD5 and SHA-1 codes Check worked out: checking again doesn't read the file again.
    others: Vec<(Kind, String)>,
    /// Copy was pressed (it says "Copied" while the note shows).
    copied: bool,
}

impl Print {
    /// Show was pressed for `one`: its SHA-256 is worked out by `start`.
    fn new(one: &One, start: &mut dyn FnMut(Kind) -> Running) -> Print {
        let sha256 = Step::Working(start(Kind::Sha256));
        Print { key: one.key.clone(), path: one.path.clone(), size: one.entry.size, sha256, check: None, others: Vec::new(), copied: false }
    }

    /// The SHA-256, once it's worked out.
    fn code(&self) -> Option<&str> {
        match &self.sha256 {
            Step::Done(code) => Some(code),
            _ => None,
        }
    }

    /// Takes in what its jobs have finished.
    fn poll(&mut self) {
        if let Step::Working(job) = &self.sha256 {
            if let Some(end) = job.0.end.get() {
                self.sha256 = end.clone().map_or(Step::Failed, Step::Done);
            }
        }
        if let Some(Check::Working { copied, job }) = &self.check {
            if let Some(end) = job.0.end.get() {
                let verdict = match end {
                    Some(code) => {
                        self.others.push((job.0.kind, code.clone()));
                        if code == copied {
                            Verdict::Matches
                        } else {
                            Verdict::Differs
                        }
                    }
                    None => Verdict::Unreadable,
                };
                self.check = Some(Check::Said(verdict));
            }
        }
    }

    /// Check was pressed with this on the clipboard (`copied_code`'s answer): it's compared with
    /// the SHA-256, or with the file's MD5 or SHA-1, which `start` works out when it isn't known yet.
    fn check(&mut self, copied: Option<String>, start: &mut dyn FnMut(Kind) -> Running) {
        let Step::Done(sha256) = &self.sha256 else { return };
        let Some(copied) = copied else {
            self.check = Some(Check::Said(Verdict::NoCode));
            return;
        };
        let kind = Kind::of_digits(copied.len()).unwrap_or(Kind::Sha256);
        let ours = if kind == Kind::Sha256 { Some(sha256) } else { self.others.iter().find(|(k, _)| *k == kind).map(|(_, code)| code) };
        self.check = Some(match (ours, self.check.take()) {
            (Some(ours), _) => Check::Said(if *ours == copied { Verdict::Matches } else { Verdict::Differs }),
            // That kind is being worked out already: it's compared with this code when it's done.
            (None, Some(Check::Working { job, .. })) if job.0.kind == kind => Check::Working { copied, job },
            (None, _) => Check::Working { copied, job: start(kind) },
        });
    }
}

/// The code in copied text, the way Check compares it: in lower case, without the spaces or dashes
/// it may be written with ("SHA-256: 9F86D081 884C7D65…" gives "9f86d081884c7d65…"). That is the
/// longest run of hex digits (spaces and dashes between them allowed) as long as an MD5, SHA-1 or
/// SHA-256 code, with a digit in it (so words like "faded" aren't taken for one), and not a whole
/// code joined to a piece of another length beside it (an MD5 and a date, or a file's name that's
/// all hex digits, isn't a SHA-1). None if there's no such code.
fn copied_code(text: &str) -> Option<String> {
    // (A code is short: a whole page copied by mistake is only looked at so far.)
    let mut end = text.len().min(100_000);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    // The best so far: how long, in how many pieces, and the code.
    let mut best: Option<(usize, usize, String)> = None;
    for part in text[..end].split(|c: char| !(c.is_ascii_hexdigit() || c.is_whitespace() || c == '-')) {
        let words: Vec<&str> = part.split(|c: char| c.is_whitespace() || c == '-').filter(|w| !w.is_empty()).collect();
        for first in 0..words.len() {
            let mut len = 0;
            for (i, w) in words[first..].iter().enumerate() {
                len += w.len();
                if len > 64 {
                    break;
                }
                let run = &words[first..=first + i];
                let better = best.as_ref().is_none_or(|(l, pieces, _)| len > *l || (len == *l && run.len() < *pieces));
                let joined = run.iter().any(|w| Kind::of_digits(w.len()).is_some()) && run.iter().any(|w| w.len() != run[0].len());
                if better && !joined && Kind::of_digits(len).is_some() && run.iter().any(|w| w.bytes().any(|b| b.is_ascii_digit())) {
                    best = Some((len, run.len(), run.concat().to_ascii_lowercase()));
                }
            }
        }
    }
    best.map(|(_, _, code)| code)
}

// ---------------------------------------------------------------- the panel

/// What's under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    None,
    Open,
    OpenWith,
    Copy,
    /// The preview box (a double-click opens).
    Box,
    /// The Fingerprint's buttons: Show, Copy and Check.
    Show,
    CopyPrint,
    CheckPrint,
}

/// Where the parts that never move sit: the label, the preview box and the buttons.
struct Frame {
    label: Rect,
    preview: Rect,
    open_with: Rect,
    copy: Rect,
    open: Rect,
}

pub struct DetailsPanel {
    rect: Rect,
    peeker: Peeker,
    facts: FactsCache,
    /// The key last asked of peek.rs.
    asked: String,
    hover: Hit,
    pressed: Hit,
    /// Copy path was pressed for this item (it says "Copied" while the note shows).
    copied: Option<String>,
    /// The selected file's fingerprint, once Show was pressed.
    print: Option<Print>,
    /// The Fingerprint's buttons where the last frame drew them, and the item (`One::key`) they
    /// were for.
    print_buttons: (String, Vec<(Hit, Rect)>),
    /// Whether the item at this path is inside a .zip or .7z (`in_archive`).
    archived: (String, bool),
}

impl Default for DetailsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl DetailsPanel {
    pub fn new() -> DetailsPanel {
        DetailsPanel {
            rect: Rect::default(),
            peeker: Peeker::new("details panel"),
            facts: FactsCache::new(64),
            asked: String::new(),
            hover: Hit::None,
            pressed: Hit::None,
            copied: None,
            print: None,
            print_buttons: (String::new(), Vec::new()),
            archived: (String::new(), false),
        }
    }

    pub fn layout(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The panel was hidden: what was still waiting is forgotten (a fingerprint too: it stops).
    pub fn hidden(&mut self) {
        self.peeker.cancel();
        self.asked.clear();
        self.hover = Hit::None;
        self.pressed = Hit::None;
        self.print = None;
        self.print_buttons.1.clear();
    }

    fn frame(&self, open_with: bool) -> Frame {
        let r = self.rect;
        let pad = dims::PAD;
        let w = (r.w - 2.0 * pad).max(0.0);
        let label = Rect::new(r.x + pad, r.y + 12.0, w, 14.0);
        let top = label.bottom() + 8.0;
        let open = Rect::new(r.x + pad, r.bottom() - pad - dims::OPEN_H, w, dims::OPEN_H);
        // Above Open: Open with… and Copy path side by side for a file, Copy path across for the rest.
        let row_y = open.y - 8.0 - dims::BUTTON_H;
        let half = ((w - 6.0) / 2.0).max(0.0);
        let open_with_rect = Rect::new(r.x + pad, row_y, half, dims::BUTTON_H);
        let copy = if open_with { Rect::new(r.x + pad + w - half, row_y, half, dims::BUTTON_H) } else { Rect::new(r.x + pad, row_y, w, dims::BUTTON_H) };
        // The box is 16:10 but leaves room for two lines of name, a line of chips, three rows and a
        // file's Fingerprint (three lines). It has the same size for every item, so it doesn't jump
        // while arrowing through files.
        let room = copy.y - top - (12.0 + 2.0 * dims::NAME_LINE + 8.0 + dims::CHIP_H + 10.0 + 6.0 * dims::ROW_H + 10.0);
        let ideal = w * 10.0 / 16.0;
        let h = ideal.min(room).max(ideal.min(72.0)).max(0.0);
        Frame { label, preview: Rect::new(r.x + pad, top, w, h), open_with: open_with_rect, copy, open }
    }

    /// Which buttons there are for this selection: (Open, Open with…, Copy path).
    fn buttons(sel: &Sel) -> (bool, bool, bool) {
        match sel {
            Sel::One(one) => (true, one.is_file(), !one.copy.is_empty()),
            _ => (false, false, false),
        }
    }

    fn hit(&self, p: Point, app: &AppState) -> Hit {
        let sel = selection(app);
        // The Fingerprint's buttons, where they were drawn for this item.
        if let Sel::One(one) = &sel {
            if let Some((h, _)) = self.print_buttons.1.iter().find(|(_, b)| b.contains(p)).filter(|_| self.print_buttons.0 == one.key) {
                return *h;
            }
        }
        let (open, open_with, copy) = Self::buttons(&sel);
        let f = self.frame(open_with);
        if open && f.open.contains(p) {
            Hit::Open
        } else if open_with && f.open_with.contains(p) {
            Hit::OpenWith
        } else if copy && f.copy.contains(p) {
            Hit::Copy
        } else if open && f.preview.contains(p) {
            Hit::Box
        } else {
            Hit::None
        }
    }

    /// Takes in what peek.rs found out.
    fn take_answers(&mut self) {
        for a in self.peeker.take() {
            if let Answer::Facts { key, facts } = a {
                self.facts.insert(key, facts);
            }
        }
    }

    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        let rect = self.rect;
        if rect.is_empty() {
            return;
        }
        self.take_answers();
        r.fill_paint(rect, 0.0, &theme.panel_paint);
        r.push_clip(rect);
        let sel = selection(app);
        let (can_open, open_with, can_copy) = Self::buttons(&sel);
        let f = self.frame(open_with);
        r.text("Details", f.label, &TextStyle::new(theme.muted).size(dims::LABEL_FONT).semibold());

        // Ask peek.rs about a new item (facts and the first lines of text are kept by key).
        let one = match &sel {
            Sel::One(one) => Some(one),
            _ => None,
        };
        if let Some(one) = one {
            if self.facts.get(&one.key).is_none() && self.asked != one.key {
                // (Asked again next time when no thread could be had for it.)
                if self.peeker.ask(one.ask(Some(peek::PANEL_TEXT), None, app.settings.show_hidden, app.settings.show_system), ctx.hwnd) {
                    self.asked = one.key.clone();
                }
            }
        }
        // A fingerprint is of the one file it was worked out for: anything else selected forgets it
        // (and stops working it out).
        let printable = one.filter(|o| self.can_print(o, app));
        if self.print.as_ref().is_some_and(|p| printable.is_none_or(|o| o.key != p.key)) {
            self.print = None;
        }
        if let Some(p) = &mut self.print {
            p.poll();
        }
        let facts = one.and_then(|o| self.facts.get(&o.key));

        // The preview, the name and the rows stop above the buttons, however short the panel gets.
        let buttons_top = if can_copy { f.copy.y } else { f.open.y } - 10.0;
        r.push_clip(Rect::new(rect.x, rect.y, rect.w, (buttons_top - rect.y).max(0.0)));
        self.paint_preview(r, theme, &sel, facts, f.preview, ctx.scale);
        let info = info(&sel, facts, app);
        let rows = Rect::new(f.preview.x, f.preview.bottom() + 12.0, f.preview.w, (buttons_top - f.preview.bottom() - 12.0).max(0.0));
        let end = draw_info(r, theme, rows, &info);
        // One file: its Fingerprint under the rows.
        match printable {
            Some(o) => self.paint_print(r, theme, app, &o.key, Rect::new(rows.x, end, rows.w, (rows.bottom() - end).max(0.0))),
            None => self.print_buttons.1.clear(),
        }
        r.pop_clip();

        if open_with {
            let (hover, pressed) = self.look(Hit::OpenWith);
            button(r, theme, f.open_with, "Open with\u{2026}", hover, pressed, false, None);
        }
        if can_copy {
            let copied = one.is_some_and(|o| self.copied.as_deref() == Some(o.key.as_str())) && app.note() == Some("Path copied");
            let (hover, pressed) = self.look(Hit::Copy);
            button(r, theme, f.copy, if copied { "Copied" } else { "Copy path" }, hover, pressed, false, copied.then_some(theme.accent));
        }
        // Open only for one item (Enter opens several). Its place stays empty otherwise.
        if can_open {
            let (hover, pressed) = self.look(Hit::Open);
            button(r, theme, f.open, "Open", hover, pressed, true, None);
        }
        r.pop_clip();
    }

    /// How a button looks: (under the mouse, pressed).
    fn look(&self, h: Hit) -> (bool, bool) {
        (self.hover == h, self.pressed == h && self.hover == h)
    }

    /// Whether the item has a Fingerprint row: a file, but not one in the Recycle Bin or Home, nor
    /// inside a .zip or .7z.
    fn can_print(&mut self, one: &One, app: &AppState) -> bool {
        one.is_file() && !one.deleted && !folder::is_place(&app.tab().folder.path) && !self.in_archive(&one.path)
    }

    /// Whether the item at `path` is inside a .zip or .7z (found out once for each item: it may ask
    /// the disk).
    fn in_archive(&mut self, path: &str) -> bool {
        if self.archived.0 != path {
            let inside = folder::parent(path).is_some_and(|p| crate::archive::split(&p).is_some());
            self.archived = (path.to_string(), inside);
        }
        self.archived.1
    }

    /// The Fingerprint under the rows (`area` starts where they end): Show; then how far along it
    /// is; then the code, cut to fit, with Copy and Check under it and what Check found under
    /// those. Only the lines that fit are drawn, and the buttons drawn are kept for `hit`.
    fn paint_print(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, key: &str, area: Rect) {
        let mut buttons = Vec::new();
        let (x, w) = (area.x, area.w);
        let line = |n: f32| area.y + n * dims::ROW_H;
        let fits = |n: f32| line(n + 1.0) <= area.bottom();
        // The buttons are as wide as the longest word they show, so none moves when Copy says "Copied".
        let word = TextStyle::new(theme.text).bold();
        let bw = ["Show", "Copied", "Check"].iter().map(|t| r.measure(t, &word)).fold(0.0, f32::max).ceil() + 24.0;
        // A button in line `n`, `slot` buttons in from the right.
        let at = |n: f32, slot: f32| Rect::new(x + w - bw - slot * (bw + 6.0), line(n) + 3.0, bw, dims::ROW_H - 5.0);
        if fits(0.0) {
            let vx = draw_row_label(r, theme, x, line(0.0), w, "Fingerprint");
            let value = Rect::new(vx, line(0.0), (x + w - vx).max(0.0), dims::ROW_H);
            let muted = TextStyle::new(theme.muted).align(Align::Right);
            match self.print.as_ref().map(|p| &p.sha256) {
                None => {
                    let (hover, pressed) = self.look(Hit::Show);
                    button(r, theme, at(0.0, 0.0), "Show", hover, pressed, false, None);
                    buttons.push((Hit::Show, at(0.0, 0.0)));
                }
                Some(Step::Working(job)) => r.text(&job.0.progress("Working it out"), value, &muted),
                Some(Step::Failed) => r.text("Couldn't read this file", value, &muted),
                Some(Step::Done(code)) => {
                    let style = TextStyle::new(theme.text).align(Align::Right);
                    let shown = fit_code(code, value.w, &mut |t| r.measure(t, &style));
                    r.text(&shown, value, &style);
                }
            }
        }
        if let Some(p) = self.print.as_ref().filter(|p| p.code().is_some()) {
            if fits(1.0) {
                let copied = p.copied && app.note() == Some(PRINT_COPIED);
                let (hover, pressed) = self.look(Hit::CopyPrint);
                button(r, theme, at(1.0, 1.0), if copied { "Copied" } else { "Copy" }, hover, pressed, false, copied.then_some(theme.accent));
                let (hover, pressed) = self.look(Hit::CheckPrint);
                button(r, theme, at(1.0, 0.0), "Check", hover, pressed, false, None);
                buttons.extend([(Hit::CopyPrint, at(1.0, 1.0)), (Hit::CheckPrint, at(1.0, 0.0))]);
            }
            let said = match &p.check {
                Some(Check::Said(v)) => {
                    let (text, color) = v.text(theme);
                    Some((text.to_string(), color))
                }
                Some(Check::Working { job, .. }) => Some((job.0.progress("Checking"), theme.muted)),
                None => None,
            };
            if let Some((text, color)) = said.filter(|_| fits(2.0)) {
                r.text(&text, Rect::new(x, line(2.0), w, dims::ROW_H), &TextStyle::new(color).align(Align::Right));
            }
        }
        self.print_buttons = (key.to_string(), buttons);
    }

    fn paint_preview(&self, r: &mut Renderer, theme: &Theme, sel: &Sel, facts: Option<&Facts>, area: Rect, scale: f32) {
        if area.h <= 0.0 {
            return;
        }
        fill_box_paint(r, theme, area, 0.0, &theme.window_paint);
        let px = (area.w.max(area.h) * scale).ceil() as u32;
        let icon_max = (area.h * 0.75).min(dims::ICON_MAX);
        match sel {
            Sel::Many { count, .. } => draw_stack(r, theme, area, *count),
            Sel::Folder { path, .. } => {
                let place = if path.is_empty() { THIS_PC_ID } else { path.as_str() };
                if let Some(pic) = thumbs::request(place, (icon_max * scale) as u32) {
                    draw_fitted(r, &pic, area, scale, icon_max);
                }
            }
            Sel::One(one) => {
                // Text by its name: its lines (no picture asked for).
                if let Some(text) = facts.and_then(|f| f.text.as_ref()).filter(|_| facts.is_some_and(|f| f.text_by_name)) {
                    draw_lines(r, theme, area, text);
                    return;
                }
                if one.is_file() && peek::is_text_file(&one.entry.ext) && facts.is_none() {
                    return; // its lines are on their way
                }
                let pic = thumbs::request_versioned(&one.shell_path, one.version, px);
                match pic {
                    Some(p) if !p.is_icon() => {
                        draw_fitted(r, &p, area, scale, icon_max);
                    }
                    other => {
                        // No real thumbnail: small files that turn out to be text show their lines
                        // (README, .gitignore, .svg...); everything else its icon.
                        if let Some(text) = facts.and_then(|f| f.text.as_ref()) {
                            draw_lines(r, theme, area, text);
                        } else if let Some(p) = other {
                            draw_fitted(r, &p, area, scale, icon_max);
                        }
                    }
                }
            }
        }
    }

    pub fn mouse_move(&mut self, p: Point, app: &AppState) -> Effect {
        let h = self.hit(p, app);
        if h != self.hover {
            self.hover = h;
            return Effect::Redraw;
        }
        Effect::None
    }

    pub fn mouse_leave(&mut self) -> Effect {
        if self.hover != Hit::None {
            self.hover = Hit::None;
            return Effect::Redraw;
        }
        Effect::None
    }

    pub fn mouse_down(&mut self, e: &MouseEvent, app: &AppState) -> Effect {
        if e.button != MouseButton::Left {
            return Effect::None;
        }
        self.pressed = match self.hit(e.pos, app) {
            Hit::Box => Hit::None,
            h => h,
        };
        self.hover = self.hit(e.pos, app);
        Effect::Redraw
    }

    pub fn mouse_up(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        if e.button != MouseButton::Left || self.pressed == Hit::None {
            return Effect::None;
        }
        let was = std::mem::replace(&mut self.pressed, Hit::None);
        if self.hit(e.pos, app) != was {
            return Effect::Redraw; // let go somewhere else: nothing happens
        }
        match was {
            Hit::Open => Effect::Command(Command::OpenSelected),
            Hit::OpenWith => Effect::Command(Command::OpenWith),
            Hit::Copy => {
                if let Sel::One(one) = selection(app) {
                    // The item's own path as it is (the list's Ctrl+Shift+C quotes paths with spaces).
                    if shell::copy_text(ctx.hwnd, &one.copy) {
                        app.say("Path copied");
                        self.copied = Some(one.key);
                    } else {
                        app.say("Couldn't copy (the clipboard is busy)");
                    }
                }
                Effect::Redraw
            }
            Hit::Show => {
                if let Sel::One(one) = selection(app) {
                    if self.can_print(&one, app) {
                        self.print = Some(Print::new(&one, &mut |kind| Job::start(&one.path, one.entry.size, kind, ctx.hwnd)));
                    }
                }
                Effect::Redraw
            }
            Hit::CopyPrint => {
                if let Some(p) = &mut self.print {
                    if let Some(code) = p.code() {
                        // The whole code, in lower case (what download pages show).
                        if shell::copy_text(ctx.hwnd, code) {
                            app.say(PRINT_COPIED);
                            p.copied = true;
                        } else {
                            app.say("Couldn't copy (the clipboard is busy)");
                        }
                    }
                }
                Effect::Redraw
            }
            Hit::CheckPrint => {
                if let Some(p) = &mut self.print {
                    let copied = crate::codebox::clipboard_text(ctx.hwnd).as_deref().and_then(copied_code);
                    let (path, size) = (p.path.clone(), p.size);
                    p.check(copied, &mut |kind| Job::start(&path, size, kind, ctx.hwnd));
                }
                Effect::Redraw
            }
            _ => Effect::Redraw,
        }
    }

    /// A double-click on the preview box opens the item.
    pub fn double_click(&mut self, e: &MouseEvent, app: &AppState) -> Effect {
        if e.button == MouseButton::Left && self.hit(e.pos, app) == Hit::Box {
            self.pressed = Hit::None;
            return Effect::Command(Command::OpenSelected);
        }
        Effect::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character is one unit wide.
    fn chars(t: &str) -> f32 {
        t.chars().count() as f32
    }

    #[test]
    fn short_names_stay_on_one_line() {
        assert_eq!(wrap_name("notes.txt", 20.0, &mut chars), ("notes.txt".to_string(), None));
    }

    #[test]
    fn long_names_break_at_a_good_place() {
        // Broken after the space near the end of what fits.
        let (one, two) = wrap_name("holiday photos from the beach.jpg", 20.0, &mut chars);
        assert_eq!(one, "holiday photos from");
        assert_eq!(two.as_deref(), Some("the beach.jpg"));
        // Before a dot, so the extension moves down whole.
        let (one, two) = wrap_name("abcdefghijklmnop.config", 18.0, &mut chars);
        assert_eq!(one, "abcdefghijklmnop");
        assert_eq!(two.as_deref(), Some(".config"));
    }

    #[test]
    fn a_very_long_name_keeps_its_extension() {
        let name = format!("{}.pdf", "x".repeat(60));
        let (one, two) = wrap_name(&name, 20.0, &mut chars);
        assert_eq!(chars(&one), 20.0);
        let two = two.unwrap();
        assert!(two.ends_with("\u{2026}.pdf"), "{two}");
        assert!(chars(&two) <= 20.0);
    }

    #[test]
    fn paths_are_cut_in_the_middle() {
        assert_eq!(fit_path("C:\\a\\b", 50.0, &mut chars), "C:\\a\\b");
        let fitted = fit_path("C:\\Users\\YourName\\Pictures\\2026\\Holiday", 20.0, &mut chars);
        assert!(fitted.starts_with("C:\\\u{2026}"), "{fitted}");
        assert!(fitted.ends_with("Holiday"));
        assert!(chars(&fitted) <= 20.0);
        let unc = fit_path("\\\\server\\share\\deep\\folder\\here", 22.0, &mut chars);
        assert!(unc.starts_with("\\\\server\\\u{2026}"), "{unc}");
    }

    #[test]
    fn chips_go_on_to_the_next_line() {
        let chips: Vec<String> = ["JPG", "3.1 MB", "4000 x 3000"].iter().map(|s| s.to_string()).collect();
        // Chips are their text + 2 x 9 wide; 60 units hold the first two (21 + 6 + 24), not the third.
        let boxes = place_chips(&chips, 0.0, 0.0, 60.0, &mut chars);
        assert_eq!(boxes[0].y, 0.0);
        assert_eq!(boxes[1].y, 0.0);
        assert_eq!(boxes[2].y, dims::CHIP_H + dims::CHIP_GAP);
        assert_eq!(boxes[2].x, 0.0);
        // One wider than the line is cut to it.
        let wide = place_chips(&["a very long chip text indeed".to_string()], 0.0, 0.0, 10.0, &mut chars);
        assert_eq!(wide[0].w, 10.0);
    }

    #[test]
    fn lengths_read_like_a_clock() {
        assert_eq!(duration_text(0), "0:00");
        assert_eq!(duration_text(187 * 10_000_000), "3:07");
        assert_eq!(duration_text(3765 * 10_000_000), "1:02:45");
    }

    #[test]
    fn what_the_panel_says() {
        let app = AppState::for_tests(crate::settings::Settings::default());
        let e = Entry {
            name: "photo.jpg".into(),
            lower: "photo.jpg".into(),
            kind: EntryKind::File,
            size: 2048,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: "jpg".into(),
            type_name: std::sync::Arc::from("JPEG Image"),
            target: None,
            free: 0,
        };
        let one = One { entry: e, index: 0, path: "C:\\pics\\photo.jpg".into(), shell_path: "C:\\pics\\photo.jpg".into(), copy: "C:\\pics\\photo.jpg".into(), location: None, deleted: false, key: "k".into(), version: 0 };
        let facts = Facts { dims: Some((4000, 3000)), duration: None, ..Facts::default() };
        let i = info(&Sel::One(Box::new(one.clone())), Some(&facts), &app);
        assert_eq!(i.name, "photo.jpg");
        assert_eq!(i.chips, vec!["JPG", "2 KB", "4000 \u{d7} 3000"]);
        assert_eq!(i.rows, vec![Row { label: "Folder", value: "pics".into(), path: false }]);
        // Before peek.rs answers: no picture size yet, the rest is there.
        assert_eq!(info(&Sel::One(Box::new(one)), None, &app).chips, vec!["JPG", "2 KB"]);
        let many = info(&Sel::Many { count: 3, files: 2, folders: 1, bytes: 4096 }, None, &app);
        assert_eq!(many.name, "3 items selected");
        assert_eq!(many.chips, vec!["2 files", "4 KB", "1 folder"]);
        let place = info(&Sel::Folder { path: String::new(), items: 1 }, None, &app);
        assert_eq!((place.name.as_str(), place.chips.clone(), place.rows.len()), ("This PC", vec!["1 item".to_string()], 0));
    }

    // ---------------------------------------------------------------- fingerprints

    const SHA256_EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const SHA1_EMPTY: &str = "da39a3ee5e6b4b0d3255bfef95601890afd80709";
    const SHA1_ABC: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";
    const MD5_EMPTY: &str = "d41d8cd98f00b204e9800998ecf8427e";
    const MD5_ABC: &str = "900150983cd24fb0d6963f7d28e17f72";

    /// What Windows works out for these bytes.
    fn code_of(data: &[u8], kind: Kind) -> String {
        let mut hasher = Hasher::new(kind).expect("Windows has the algorithm");
        assert!(hasher.add(data));
        hasher.finish().expect("the code")
    }

    /// The code of "abc" of each kind.
    fn abc(kind: Kind) -> &'static str {
        match kind {
            Kind::Md5 => MD5_ABC,
            Kind::Sha1 => SHA1_ABC,
            Kind::Sha256 => SHA256_ABC,
        }
    }

    /// A job that's done already, with this code (None: the file couldn't be read).
    fn finished(kind: Kind, code: Option<&str>) -> Running {
        let job = Job { kind, total: 3, done: AtomicU64::new(3), stop: AtomicBool::new(false), end: OnceLock::new() };
        let _ = job.end.set(code.map(str::to_string));
        Running(Arc::new(job))
    }

    fn a_file() -> One {
        let entry = Entry {
            name: "abc.txt".into(),
            lower: "abc.txt".into(),
            kind: EntryKind::File,
            size: 3,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: "txt".into(),
            type_name: std::sync::Arc::from("Text Document"),
            target: None,
            free: 0,
        };
        One { entry, index: 0, path: "C:\\abc.txt".into(), shell_path: "C:\\abc.txt".into(), copy: "C:\\abc.txt".into(), location: None, deleted: false, key: "k".into(), version: 0 }
    }

    #[test]
    fn windows_works_out_the_codes() {
        assert_eq!(code_of(b"", Kind::Sha256), SHA256_EMPTY);
        assert_eq!(code_of(b"abc", Kind::Sha256), SHA256_ABC);
        assert_eq!(code_of(b"", Kind::Sha1), SHA1_EMPTY);
        assert_eq!(code_of(b"abc", Kind::Sha1), SHA1_ABC);
        assert_eq!(code_of(b"", Kind::Md5), MD5_EMPTY);
        assert_eq!(code_of(b"abc", Kind::Md5), MD5_ABC);
        // Fed in pieces, as a file is: the same code.
        let mut hasher = Hasher::new(Kind::Sha256).unwrap();
        assert!(hasher.add(b"a") && hasher.add(b"") && hasher.add(b"bc"));
        assert_eq!(hasher.finish().as_deref(), Some(SHA256_ABC));
    }

    #[test]
    fn a_code_says_its_kind_by_its_length() {
        assert_eq!(Kind::of_digits(MD5_ABC.len()), Some(Kind::Md5));
        assert_eq!(Kind::of_digits(SHA1_ABC.len()), Some(Kind::Sha1));
        assert_eq!(Kind::of_digits(SHA256_ABC.len()), Some(Kind::Sha256));
        assert_eq!(Kind::of_digits(0), None);
        assert_eq!(Kind::of_digits(63), None);
        assert_eq!(Kind::of_digits(128), None, "SHA-512 isn't one of them");
        for kind in [Kind::Md5, Kind::Sha1, Kind::Sha256] {
            assert_eq!(Kind::of_digits(2 * kind.bytes()), Some(kind));
        }
    }

    #[test]
    fn copied_text_is_cleaned_to_its_code() {
        let some = |code: &str| Some(code.to_string());
        // As it is, or in capitals.
        assert_eq!(copied_code(SHA256_ABC), some(SHA256_ABC));
        assert_eq!(copied_code(&SHA256_ABC.to_uppercase()), some(SHA256_ABC));
        // With a name before it, in groups, with a new line after it.
        assert_eq!(copied_code("SHA256: ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad\r\n"), some(SHA256_ABC));
        assert_eq!(copied_code("SHA-256 checksum: BA7816BF-8F01CFEA-414140DE-5DAE2223-B00361A3-96177A9C-B410FF61-F20015AD"), some(SHA256_ABC));
        // As sha256sum writes it, with the file's name after it.
        assert_eq!(copied_code(&format!("{SHA256_ABC}  glint-setup.exe")), some(SHA256_ABC));
        // MD5 and SHA-1 codes, with spaces round them or dashes in them.
        assert_eq!(copied_code(&format!("  {MD5_ABC}\t")), some(MD5_ABC));
        assert_eq!(copied_code("a9993e36-4706816a-ba3e2571-7850c26c-9cd0d89d"), some(SHA1_ABC));
        // A page that gives several: the longest.
        assert_eq!(copied_code(&format!("MD5: {MD5_ABC}\nSHA-1: {SHA1_ABC}\nSHA-256: {SHA256_ABC}")), some(SHA256_ABC));
        // A code with a file's name of hex digits, or a date, beside it: the code alone.
        assert_eq!(copied_code(&format!("{MD5_ABC}  1a2b3c4d.tmp")), some(MD5_ABC));
        assert_eq!(copied_code(&format!("{MD5_ABC}\t2024-01-15")), some(MD5_ABC));
        // No code: nothing, words, a code cut short, or one too long.
        assert_eq!(copied_code(""), None);
        assert_eq!(copied_code("Copy the code from the website"), None);
        assert_eq!(copied_code(&"faded".repeat(8)), None, "hex letters without a digit are words");
        assert_eq!(copied_code(&SHA256_ABC[..63]), None);
        assert_eq!(copied_code(&format!("{SHA256_ABC}{SHA256_ABC}")), None);
    }

    #[test]
    fn long_codes_are_cut_in_the_middle() {
        assert_eq!(fit_code("abc", 10.0, &mut chars), "abc");
        assert_eq!(fit_code("0123456789abcdef", 9.0, &mut chars), "0123\u{2026}cdef");
        assert_eq!(fit_code(SHA256_ABC, 21.0, &mut chars), "ba7816bf8f\u{2026}61f20015ad");
        assert_eq!(fit_code("0123456789", 0.0, &mut chars), "\u{2026}");
    }

    #[test]
    fn a_file_is_read_a_chunk_at_a_time_and_can_be_stopped() {
        let dir = std::env::temp_dir().join(format!("glint-fingerprint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.bin");
        let data: Vec<u8> = (0..2 * CHUNK + 12_345).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let path = file.to_string_lossy().into_owned();
        let mut read = Vec::new();
        assert_eq!(file_code(&path, Kind::Sha256, &AtomicBool::new(false), &mut |n| read.push(n)), Some(code_of(&data, Kind::Sha256)));
        assert!(read.len() >= 3, "{read:?}");
        assert_eq!(read.iter().sum::<u64>(), data.len() as u64);
        // Stopped, or not there: no code.
        assert_eq!(file_code(&path, Kind::Sha256, &AtomicBool::new(true), &mut |_| {}), None);
        assert_eq!(file_code(&format!("{path}.gone"), Kind::Md5, &AtomicBool::new(false), &mut |_| {}), None);

        // On a thread of its own; letting go of it stops it.
        std::fs::write(&file, b"abc").unwrap();
        let running = Job::start(&path, 3, Kind::Sha1, HWND::default());
        let start = Instant::now();
        while running.0.end.get().is_none() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(running.0.end.get(), Some(&Some(SHA1_ABC.to_string())));
        let job = running.0.clone();
        drop(running);
        assert!(job.stop.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh test folder with the process id in its name.
    fn fresh_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("glint-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn code_in(path: &std::path::Path, kind: Kind) -> Option<String> {
        file_code(&path.to_string_lossy(), kind, &AtomicBool::new(false), &mut |_| {})
    }

    #[test]
    fn same_content_gives_the_same_fingerprint_and_other_content_does_not() {
        let dir = fresh_dir("print-same");
        let data: Vec<u8> = (0..CHUNK + 100).map(|i| (i % 253) as u8).collect();
        let mut other = data.clone();
        *other.last_mut().unwrap() ^= 1; // one bit, in the very last byte
        let mut first = data.clone();
        first[0] ^= 1; // and in the very first
        for (name, bytes) in [("a.bin", &data), ("b copy.bin", &data), ("c.bin", &other), ("d.bin", &first)] {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        for kind in [Kind::Md5, Kind::Sha1, Kind::Sha256] {
            let a = code_in(&dir.join("a.bin"), kind).unwrap();
            assert_eq!(a.len(), kind.bytes() * 2);
            // The name doesn't matter, the content does.
            assert_eq!(code_in(&dir.join("b copy.bin"), kind), Some(a.clone()));
            assert_ne!(code_in(&dir.join("c.bin"), kind), Some(a.clone()));
            assert_ne!(code_in(&dir.join("d.bin"), kind), Some(a));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sizes_around_the_chunk_give_the_same_code_as_the_whole_in_memory() {
        let dir = fresh_dir("print-sizes");
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK, 2 * CHUNK + 1] {
            let data: Vec<u8> = (0..len).map(|i| (i * 31 % 247) as u8).collect();
            let file = dir.join(format!("f{len}.bin"));
            std::fs::write(&file, &data).unwrap();
            let mut total = 0;
            let code = file_code(&file.to_string_lossy(), Kind::Sha256, &AtomicBool::new(false), &mut |n| total += n);
            assert_eq!(code, Some(code_of(&data, Kind::Sha256)), "{len} bytes");
            assert_eq!(total, len as u64, "{len} bytes are all counted");
        }
        // A zero-byte file has the well-known empty codes.
        let empty = dir.join("f0.bin");
        assert_eq!(code_in(&empty, Kind::Sha256).as_deref(), Some(SHA256_EMPTY));
        assert_eq!(code_in(&empty, Kind::Sha1).as_deref(), Some(SHA1_EMPTY));
        // One byte more or less than a chunk is not the same file.
        assert_ne!(code_in(&dir.join(format!("f{}.bin", CHUNK)), Kind::Sha256), code_in(&dir.join(format!("f{}.bin", CHUNK + 1)), Kind::Sha256));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_gone_locked_or_a_folder_has_no_code() {
        let dir = fresh_dir("print-gone");
        let file = dir.join("x.bin");
        std::fs::write(&file, b"abc").unwrap();
        assert_eq!(code_in(&file, Kind::Sha1).as_deref(), Some(SHA1_ABC));
        // Deleted before it's read.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(code_in(&file, Kind::Sha1), None);
        // A folder is not a file.
        assert_eq!(code_in(&dir, Kind::Sha256), None);
        // Locked by another program (nobody may share it): no code, no panic.
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::write(&file, b"abc").unwrap();
            let _lock = std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&file).unwrap();
            assert_eq!(code_in(&file, Kind::Sha256), None);
        }
        // Odd names: # and dots (accents and emoji fail only under Wine here).
        let odd = dir.join("my file #1 a.b.bin");
        std::fs::write(&odd, b"abc").unwrap();
        assert_eq!(code_in(&odd, Kind::Sha256).as_deref(), Some(SHA256_ABC));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_changes_while_it_is_read_still_gives_a_code_and_stopping_midway_gives_none() {
        let dir = fresh_dir("print-change");
        let file = dir.join("grow.bin");
        std::fs::write(&file, vec![7u8; 3 * CHUNK]).unwrap();
        let path = file.to_string_lossy().into_owned();
        // It grows after the first chunk was read: no crash, a code comes out.
        let mut seen = 0u64;
        let mut grown = false;
        let code = file_code(&path, Kind::Sha256, &AtomicBool::new(false), &mut |n| {
            seen += n;
            if !grown {
                grown = true;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
                f.write_all(&[1u8; 10]).unwrap();
            }
        });
        assert!(code.is_some());
        assert!(seen >= 3 * CHUNK as u64);
        // Stopped after the first chunk (the window was closed): no code.
        std::fs::write(&file, vec![7u8; 3 * CHUNK]).unwrap();
        let stop = AtomicBool::new(false);
        let code = file_code(&path, Kind::Md5, &stop, &mut |_| stop.store(true, Ordering::Relaxed));
        assert_eq!(code, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn big_files_say_how_far_along_they_are() {
        let job = Job { kind: Kind::Sha256, total: 4 * BIG, done: AtomicU64::new(BIG + BIG / 2), stop: AtomicBool::new(false), end: OnceLock::new() };
        assert_eq!(job.progress("Working it out"), "Working it out\u{2026} 37%");
        job.done.store(5 * BIG, Ordering::Relaxed); // (it grew meanwhile)
        assert_eq!(job.percent(), Some(99));
        let small = Job { total: BIG - 1, ..job };
        assert_eq!(small.progress("Checking"), "Checking\u{2026}");
    }

    #[test]
    fn check_compares_with_the_copied_code() {
        let mut started = Vec::new();
        let mut start = |kind: Kind| {
            started.push(kind);
            finished(kind, Some(abc(kind)))
        };
        let mut p = Print::new(&a_file(), &mut start);
        assert_eq!(p.code(), None);
        p.poll();
        assert_eq!(p.code(), Some(SHA256_ABC));
        let said = |p: &Print| match &p.check {
            Some(Check::Said(v)) => Some(*v),
            _ => None,
        };
        // Nothing copied; the same SHA-256 (as `copied_code` gives it); another one.
        p.check(None, &mut start);
        assert_eq!(said(&p), Some(Verdict::NoCode));
        p.check(copied_code(&SHA256_ABC.to_uppercase()), &mut start);
        assert_eq!(said(&p), Some(Verdict::Matches));
        p.check(Some(SHA256_EMPTY.into()), &mut start);
        assert_eq!(said(&p), Some(Verdict::Differs));
        // An MD5: the file's own is worked out to compare, and then known.
        p.check(Some(MD5_ABC.into()), &mut start);
        assert!(matches!(p.check, Some(Check::Working { .. })));
        p.poll();
        assert_eq!(said(&p), Some(Verdict::Matches));
        p.check(Some(MD5_EMPTY.into()), &mut start);
        assert_eq!(said(&p), Some(Verdict::Differs));
        // A SHA-1 that doesn't match.
        p.check(Some(SHA1_EMPTY.into()), &mut start);
        p.poll();
        assert_eq!(said(&p), Some(Verdict::Differs));
        assert_eq!(started, vec![Kind::Sha256, Kind::Md5, Kind::Sha1], "each read once");

        // A file that can't be read: no code, and nothing to check.
        let mut gone = Print::new(&a_file(), &mut |kind| finished(kind, None));
        gone.poll();
        assert!(matches!(gone.sha256, Step::Failed));
        gone.check(Some(SHA256_ABC.into()), &mut |_| unreachable!());
        assert!(gone.check.is_none());
        // ...or that can't be read again for its MD5.
        let mut once = Print::new(&a_file(), &mut |kind| finished(kind, Some(abc(kind))));
        once.poll();
        once.check(Some(MD5_ABC.into()), &mut |kind| finished(kind, None));
        once.poll();
        assert_eq!(said(&once), Some(Verdict::Unreadable));
    }

    #[test]
    fn what_check_says() {
        let theme = Theme::dark();
        assert_eq!(Verdict::Matches.text(&theme).0, "\u{2713} Matches the copied code");
        assert_eq!(Verdict::Differs.text(&theme), ("\u{2717} Doesn't match the copied code", crate::codebox::PROBLEM_RED));
        assert_eq!(Verdict::NoCode.text(&theme).0, "Copy the code from the website first");
        // The green is darker on a light skin, so it stays easy to read.
        assert_ne!(Verdict::Matches.text(&theme).1, Verdict::Matches.text(&Theme::light()).1);
    }
}
