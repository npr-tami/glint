//! The details panel (Alt+P, "ShowDetails", off by default): on the right of the
//! file list, what is selected - a preview (the picture Explorer would show, the first lines of a
//! text file, or a big icon), its name, chips saying what it is and how big, its dates and folder,
//! and the Open with…, Copy path and Open buttons.
//!
//! - Nothing selected: the folder on screen (its icon, name, how many items, where it is).
//! - One item: its thumbnail (thumbs.rs) or text, and what peek.rs finds out about it off the UI
//!   thread (picture size, a song's or video's length, a folder's item count). A drive's free space
//!   comes from the drive list, which already knows it.
//! - Several: a little stack of cards with the count, how many files and folders, and the files' size.
//!
//! The panel never waits: each frame shows what is there, and the thumbnail service and peek.rs
//! repaint the window when more arrives. The layout of the box and the buttons doesn't depend on
//! what's shown, so nothing jumps while arrowing through files.
//!
//! The same pieces (what is selected, what to say about it, the name, chips and rows) are used by
//! the Space preview (preview.rs) for files that are neither pictures nor text.

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

/// The rows from `y` down, stopping above `limit`; a hairline above each, the label on the left and
/// the value on the right. Returns where they end.
pub fn draw_rows(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32, limit: f32, rows: &[Row]) -> f32 {
    let label_style = TextStyle::new(theme.muted);
    let value_style = TextStyle::new(theme.text).align(Align::Right);
    let mut y = y;
    for row in rows {
        if y + dims::ROW_H > limit {
            break;
        }
        r.fill_rect(Rect::new(x, y, w, 1.0), theme.line);
        let label_w = r.measure(row.label, &label_style);
        r.text(row.label, Rect::new(x, y, w, dims::ROW_H), &label_style);
        let vx = (x + label_w + 12.0).min(x + w);
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
}

impl Default for DetailsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl DetailsPanel {
    pub fn new() -> DetailsPanel {
        DetailsPanel { rect: Rect::default(), peeker: Peeker::new("details panel"), facts: FactsCache::new(64), asked: String::new(), hover: Hit::None, pressed: Hit::None, copied: None }
    }

    pub fn layout(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The panel was hidden: what was still waiting is forgotten.
    pub fn hidden(&mut self) {
        self.peeker.cancel();
        self.asked.clear();
        self.hover = Hit::None;
        self.pressed = Hit::None;
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
        // The box is 16:10 but leaves room for two lines of name, a line of chips and three rows. It
        // has the same size for every item, so it doesn't jump while arrowing through files.
        let room = copy.y - top - (12.0 + 2.0 * dims::NAME_LINE + 8.0 + dims::CHIP_H + 10.0 + 3.0 * dims::ROW_H + 10.0);
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
                self.peeker.ask(one.ask(Some(peek::PANEL_TEXT), None, app.settings.show_hidden, app.settings.show_system), ctx.hwnd);
                self.asked = one.key.clone();
            }
        }
        let facts = one.and_then(|o| self.facts.get(&o.key));

        // The preview, the name and the rows stop above the buttons, however short the panel gets.
        let buttons_top = if can_copy { f.copy.y } else { f.open.y } - 10.0;
        r.push_clip(Rect::new(rect.x, rect.y, rect.w, (buttons_top - rect.y).max(0.0)));
        self.paint_preview(r, theme, &sel, facts, f.preview, ctx.scale);
        let info = info(&sel, facts, app);
        draw_info(r, theme, Rect::new(f.preview.x, f.preview.bottom() + 12.0, f.preview.w, (buttons_top - f.preview.bottom() - 12.0).max(0.0)), &info);
        r.pop_clip();

        let look = |h: Hit| (self.hover == h, self.pressed == h && self.hover == h);
        if open_with {
            let (hover, pressed) = look(Hit::OpenWith);
            button(r, theme, f.open_with, "Open with\u{2026}", hover, pressed, false, None);
        }
        if can_copy {
            let copied = one.is_some_and(|o| self.copied.as_deref() == Some(o.key.as_str())) && app.note() == Some("Path copied");
            let (hover, pressed) = look(Hit::Copy);
            button(r, theme, f.copy, if copied { "Copied" } else { "Copy path" }, hover, pressed, false, copied.then_some(theme.accent));
        }
        // Open only for one item (Enter opens several). Its place stays empty otherwise.
        if can_open {
            let (hover, pressed) = look(Hit::Open);
            button(r, theme, f.open, "Open", hover, pressed, true, None);
        }
        r.pop_clip();
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
        let fitted = fit_path("C:\\Users\\pandora\\Pictures\\2026\\Holiday", 20.0, &mut chars);
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
}
