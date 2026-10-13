//! What the skin customiser's text view knows about a skin's text (codebox.rs): which settings
//! fit where the cursor is, what a line does, and how its words are coloured.
//!
//! - Typing a setting's name → the settings the skin doesn't set yet, each with what it does;
//!   taking one writes "name: " and goes on to its values.
//! - After "name:" → what it can be: a colour's other colours to follow (or none), a style's few
//!   words, true / false, a few usual numbers or fonts.
//! - The cursor's line: what that setting does, in faint words after it.

use std::collections::HashSet;

use crate::codebox::Lang;
use crate::complete::{self, Completion, Suggestion};
use crate::skin::{self, COLOURS, DERIVED, OTHERS};
use crate::theme::{Color, Theme};

pub struct SkinLang;

/// The words a setting can be, with a few words about each.
fn values_for(key: &str) -> &'static [(&'static str, &'static str)] {
    const YES_NO: &[(&str, &str)] = &[("true", "On"), ("false", "Off")];
    match key {
        "dark" => &[("true", "A dark skin"), ("false", "A light skin")],
        "scrollbar-grip" | "bevels" | "title-bar-across" => YES_NO,
        "menus" => &[("dark", "Dark right-click menus"), ("light", "Light right-click menus")],
        "shape" => &[("slanted", "Buttons lean"), ("straight", "Square buttons"), ("rounded", "Round corners")],
        "buttons" => &[("flat", "Plain buttons"), ("liquid", "Glossy glass marbles and capsules")],
        "caption-buttons" => &[("symbols", "Plain signs"), ("marbles", "Round glass buttons"), ("tiles", "Square tiles")],
        "tabs" => &[("attached", "The current tab joins the toolbar"), ("pills", "Rounded tabs")],
        "sidebar-style" => &[("plain", "One list"), ("panels", "Each group in a rounded panel")],
        "era" => &[("none", "Today's Glint"), ("98", "Windows 98"), ("2000", "Windows 2000"), ("xp", "Windows XP"), ("7", "Windows 7")],
        "icons" => &[("windows", "Today's icons"), ("classic", "Windows 98 / 2000"), ("xp", "Windows XP"), ("aero", "Windows 7")],
        "wallpaper-position" => &[
            ("left", "Keep the left side in view"),
            ("middle", "Keep the middle in view"),
            ("right", "Keep the right side in view"),
            ("right bottom", "The bottom right corner"),
            ("middle middle", "The very middle"),
        ],
        "wallpaper-zoom" => &[("0", "Its usual size"), ("50", "Zoomed in a little"), ("100", "Twice as big"), ("-50", "Zoomed out a little"), ("-100", "Half as big")],
        "wallpaper-fit" => &[("fill", "Fills the list (cropped)"), ("whole", "All of it, at its side")],
        "font" | "title-font" => &[
            ("Segoe UI", "Windows' own"),
            ("Segoe UI Variable", "Windows 11's"),
            ("Tahoma", "Windows 2000 / XP"),
            ("Verdana", "Wide and clear"),
            ("Arial", "Plain"),
            ("Trebuchet MS", "XP's titles"),
            ("Calibri", "Soft"),
            ("Consolas", "Monospaced"),
        ],
        "font-size" => &[("8pt", "Small"), ("9pt", "Standard"), ("10pt", "Bigger"), ("11pt", "Big"), ("12pt", "Bigger still"), ("14pt", "Biggest")],
        "slant" | "path-slant" => &[("0", "Upright"), ("4", "A little"), ("8", "AMD Radeon's"), ("12", "More"), ("20", "The most")],
        "radius" => &[("0", "Square"), ("4", "Slightly round"), ("8", "Round"), ("16", "Roundest")],
        "row-height" => &[("24", "Tight"), ("28", "Standard"), ("32", "Roomy"), ("40", "Big")],
        "sidebar-row-height" => &[("22", "Tight"), ("28", "Standard"), ("32", "Roomy")],
        "tab-line-width" | "toolbar-line-width" => &[("0", "No line"), ("1", "Thin"), ("2", "Standard"), ("3", "Thick")],
        "scrollbar-width" => &[("6", "Thin"), ("8", "Standard"), ("12", "Wide"), ("17", "Older Windows'")],
        "see-through" => &[("0", "None"), ("30", "A little"), ("60", "Half"), ("90", "Most")],
        "wallpaper-opacity" => &[("20", "Faint"), ("40", "Standard"), ("70", "Strong"), ("100", "Full")],
        _ => &[],
    }
}

fn is_colour(key: &str) -> bool {
    COLOURS.iter().any(|(k, _)| *k == key)
}

/// What a setting does.
fn about(key: &str) -> Option<String> {
    if let Some((_, d)) = COLOURS.iter().find(|(k, _)| *k == key) {
        let follows = DERIVED.iter().find(|(k, _)| *k == key).map(|(_, base)| *base);
        return Some(match follows {
            Some("none") => format!("{d} (none when left out)"),
            Some(base) => format!("{d} (like {base} when left out)"),
            None => d.to_string(),
        });
    }
    OTHERS.iter().find(|(k, _)| *k == key).map(|(_, d)| d.to_string())
}

/// Where a line's note starts ("//", "#" or ";" first, or " // " later on), in bytes.
fn note_at(row: &str) -> Option<usize> {
    let lead = row.len() - row.trim_start().len();
    let t = &row[lead..];
    if t.starts_with("//") || t.starts_with('#') || t.starts_with(';') {
        return Some(lead);
    }
    let b = row.as_bytes();
    (1..b.len().saturating_sub(1)).find(|&i| b[i] == b'/' && b[i + 1] == b'/' && b[i - 1].is_ascii_whitespace())
}

/// The setting's name and value on a line (without its note), with where the colon is.
fn split(row: &str) -> Option<(String, &str, usize)> {
    let code = &row[..note_at(row).unwrap_or(row.len())];
    let at = code.find([':', '='])?;
    let key = skin::normalize_key(code[..at].trim().trim_start_matches('{'));
    (!key.is_empty()).then(|| (key, code[at + 1..].trim(), at))
}

/// The settings a text already sets (by name).
fn set_in(text: &str, but_line: usize) -> HashSet<String> {
    text.split('\n').enumerate().filter(|(i, _)| *i != but_line).filter_map(|(_, l)| split(l).map(|(k, _, _)| k)).collect()
}

impl Lang for SkinLang {
    fn complete(&self, text: &str, caret: usize) -> Completion {
        let caret = caret.min(text.len());
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let before = &text[line_start..caret];
        let none = Completion { start: caret, end: caret, ..Completion::default() };
        if note_at(before).is_some() {
            return none;
        }
        let Some(colon) = before.find([':', '=']) else {
            // A setting's name being typed.
            let typed = before.trim_start();
            if typed.is_empty() || typed.contains(char::is_whitespace) {
                return none;
            }
            let line = text[..line_start].matches('\n').count();
            let taken = set_in(text, line);
            let items = COLOURS
                .iter()
                .chain(OTHERS.iter())
                .filter(|(k, _)| !taken.contains(*k))
                .map(|(k, _)| (k.to_string(), Suggestion { insert: format!("{k}: "), label: k.to_string(), hint: about(k).unwrap_or_default() }))
                .collect();
            return Completion { start: caret - typed.len(), end: caret, items: complete::rank(items, typed), wants: None };
        };
        // Its value.
        let key = skin::normalize_key(before[..colon].trim());
        let value = &before[colon + 1..];
        let typed = value.trim_start();
        if typed.starts_with('#') || typed.contains(',') {
            return none;
        }
        let items: Vec<(String, Suggestion)> = if is_colour(&key) {
            COLOURS
                .iter()
                .filter(|(k, _)| *k != key)
                .map(|(k, d)| (*k, *d))
                .chain(std::iter::once(("none", "No colour")))
                .map(|(k, d)| (k.to_string(), Suggestion { insert: k.to_string(), label: k.to_string(), hint: if k == "none" { d.to_string() } else { format!("Same as {k}: {}", d.to_lowercase()) } }))
                .collect()
        } else {
            values_for(&key).iter().map(|(v, d)| (v.to_string(), Suggestion { insert: v.to_string(), label: v.to_string(), hint: d.to_string() })).collect()
        };
        Completion { start: caret - typed.len(), end: caret, items: complete::rank(items, typed), wants: None }
    }

    fn explain(&self, row: &str) -> Option<String> {
        let (key, value, _) = split(row)?;
        let said = about(&key)?;
        if is_colour(&key) && COLOURS.iter().any(|(k, _)| skin::normalize_key(value) == *k) {
            return Some(format!("{said} \u{00b7} same as {}", skin::normalize_key(value)));
        }
        Some(said)
    }

    fn ink(&self, row: &str, theme: &Theme) -> Vec<(usize, usize, Color)> {
        let note = note_at(row);
        let code = &row[..note.unwrap_or(row.len())];
        let mut out = Vec::new();
        if let Some(at) = code.find([':', '=']) {
            out.push((0, at, theme.accent));
            out.push((at, at + 1, theme.muted));
            let value = &code[at + 1..];
            let lead = value.len() - value.trim_start().len();
            let v = value.trim();
            if !v.is_empty() {
                let key = skin::normalize_key(code[..at].trim());
                // A colour followed, or a style's word: stronger than plain text.
                let word = !v.starts_with('#') && (is_colour(&key) || values_for(&key).iter().any(|(w, _)| w.eq_ignore_ascii_case(v)));
                if word {
                    out.push((at + 1 + lead, at + 1 + lead + v.len(), theme.text_strong));
                }
            }
        }
        if let Some(n) = note {
            out.push((n, row.len(), theme.muted));
        }
        out
    }

    fn swatch(&self, row: &str) -> Option<Color> {
        let (key, value, _) = split(row)?;
        if !is_colour(&key) {
            return None;
        }
        let first = value.split(',').next()?.split_whitespace().next()?;
        skin::parse_hex(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(c: &Completion) -> Vec<String> {
        c.items.iter().map(|s| s.label.clone()).collect()
    }

    #[test]
    fn names_then_values_are_suggested() {
        let l = SkinLang;
        let text = "name: Mine\nacc";
        let c = l.complete(text, text.len());
        assert_eq!(c.items.first().map(|s| s.insert.as_str()), Some("accent: "));
        // A setting already in the skin isn't offered again.
        let text = "accent: #fff\nacc";
        assert!(!labels(&l.complete(text, text.len())).contains(&"accent".to_string()));
        let text = "shape: ro";
        assert_eq!(labels(&l.complete(text, text.len())), ["rounded"]);
        let text = "tab-line: ";
        let c = l.complete(text, text.len());
        assert!(labels(&c).contains(&"accent".to_string()) && !labels(&c).contains(&"tab-line".to_string()));
        // No list for a hex colour or a note.
        assert!(l.complete("accent: #f", 10).items.is_empty());
        assert!(l.complete("// acc", 6).items.is_empty());
    }

    #[test]
    fn lines_are_explained_and_coloured() {
        let l = SkinLang;
        assert_eq!(l.explain("accent: #ff0000").as_deref(), Some("Lines, selected rows' edge, switches"));
        assert!(l.explain("tab-line: accent").is_some_and(|s| s.ends_with("same as accent")));
        assert_eq!(l.explain("nonsense: 3"), None);
        assert_eq!(l.swatch("accent: #ff0000"), skin::parse_hex("#ff0000"));
        assert_eq!(l.swatch("shape: rounded"), None);
    }
}
