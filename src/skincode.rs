//! What the skin customiser's text view knows about a skin's text (codebox.rs): which settings
//! fit where the cursor is, what a line does, and how its words are coloured.
//!
//! - Typing a setting's name → the settings the skin doesn't set yet, each with what it does;
//!   taking one writes "name: " and goes on to its values.
//! - After "name:" → what it can be: a colour's other colours to follow (or none), a style's few
//!   words, true / false, a few usual numbers or fonts.
//! - The cursor's line: what that setting does, in faint words after it.

use std::collections::HashSet;

use crate::codebox::{Lang, PROBLEM_RED};
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
        // Every corner and edge, then a side alone.
        "wallpaper-position" => &[
            ("left top", "The top left corner"),
            ("middle top", "The middle of the top"),
            ("right top", "The top right corner"),
            ("left middle", "The middle of the left side"),
            ("middle", "The very middle"),
            ("right middle", "The middle of the right side"),
            ("left bottom", "The bottom left corner"),
            ("middle bottom", "The middle of the bottom"),
            ("right bottom", "The bottom right corner"),
            ("left", "Keep the left side in view"),
            ("right", "Keep the right side in view"),
            ("50% 50%", "Across, then down, as percentages"),
        ],
        "wallpaper-zoom" => &[("0", "Its usual size"), ("50", "Zoomed in a little"), ("100", "Twice as big"), ("-50", "Zoomed out a little"), ("-100", "Half as big")],
        "wallpaper-fit" => &[
            ("fill", "Fills the list (cropped)"),
            ("whole", "All of it, at its side"),
            ("stretch", "Pulled to the list's shape"),
            ("tile", "Repeated, at its own size"),
            ("actual", "Once, at its own size"),
        ],
        "wallpaper-size" => &[("25%", "A quarter of the list's width"), ("50%", "Half the list's width"), ("100%", "As wide as the list")],
        "wallpaper-offset" => &[("0 0", "Where it's placed"), ("20 0", "20 to the right"), ("-20 0", "20 to the left"), ("0 20", "20 down"), ("0 -20", "20 up")],
        "wallpaper-blur" => &[("0", "Sharp"), ("4", "A little"), ("12", "Soft"), ("30", "Very soft")],
        "wallpaper-fade" => &[("0", "No fade"), ("10", "Soft edges"), ("25", "A wide fade"), ("50", "Fades from the middle")],
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

/// What a setting does (a number's with what it can be: "0 to 20").
fn about(key: &str) -> Option<String> {
    if let Some((_, d)) = COLOURS.iter().find(|(k, _)| *k == key) {
        let follows = DERIVED.iter().find(|(k, _)| *k == key).map(|(_, base)| *base);
        return Some(match follows {
            Some("none") => format!("{d} (none when left out)"),
            Some(base) => format!("{d} (like {base} when left out)"),
            None => d.to_string(),
        });
    }
    let (_, d) = OTHERS.iter().find(|(k, _)| *k == key)?;
    Some(match skin::range_of(key) {
        Some(range) if !d.contains(&range) => format!("{d} ({range})"),
        _ => d.to_string(),
    })
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

/// The settings line `line` could be: those the rest of the text doesn't set yet, each with what it
/// does. Taking one writes "name: ".
fn names_for(text: &str, line: usize) -> Vec<(String, Suggestion)> {
    let taken = set_in(text, line);
    COLOURS
        .iter()
        .chain(OTHERS.iter())
        .filter(|(k, _)| !taken.contains(*k))
        .map(|(k, _)| (k.to_string(), Suggestion { insert: format!("{k}: "), label: k.to_string(), hint: about(k).unwrap_or_default() }))
        .collect()
}

impl Lang for SkinLang {
    fn complete_asked(&self, text: &str, caret: usize) -> Completion {
        let caret = caret.min(text.len());
        let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let line_end = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
        let before = &text[line_start..caret];
        // After the colon (its values), in a note, or after a second word: as when typing.
        if note_at(before).is_some() || before.contains([':', '=']) {
            return self.complete(text, caret);
        }
        let typed = before.trim_start();
        if typed.contains(char::is_whitespace) {
            return self.complete(text, caret);
        }
        let rest = &text[caret..line_end];
        // Spaces after the cursor on a line with nothing before it are stepped over.
        let skip = if typed.is_empty() { rest.len() - rest.trim_start().len() } else { 0 };
        let word = &rest[skip..];
        let code_end = note_at(word).unwrap_or(word.len());
        if typed.is_empty() && code_end == 0 && !word.is_empty() {
            // The line is a note.
            return Completion { start: caret, end: caret, ..Completion::default() };
        }
        // The rest of the name after the cursor, and its colon, are replaced too: taking a setting
        // changes the line's name instead of writing a second one into it.
        let code = &word[..code_end];
        let span = match code.find([':', '=']) {
            Some(at) => {
                let after = &code[at + 1..];
                at + 1 + after.len() - after.trim_start().len()
            }
            None => code.find(char::is_whitespace).unwrap_or(code.len()),
        };
        let line = text[..line_start].matches('\n').count();
        let start = caret + skip - typed.len();
        Completion { start, end: caret + skip + span, items: complete::rank(names_for(text, line), typed), wants: None }
    }

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
            let items = names_for(text, line);
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
            // A name Glint doesn't know is red, as the problem it is (the skin skips the line).
            let key = skin::normalize_key(code[..at].trim().trim_start_matches('{'));
            out.push((0, at, if skin::known_key(&key) { theme.accent } else { PROBLEM_RED }));
            out.push((at, at + 1, theme.muted));
            let value = &code[at + 1..];
            let lead = value.len() - value.trim_start().len();
            let v = value.trim();
            if !v.is_empty() {
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

    #[test]
    fn every_wallpaper_position_and_yes_or_no_is_offered() {
        let l = SkinLang;
        let text = "wallpaper-position: ";
        let offered = labels(&l.complete(text, text.len()));
        for a in ["left", "middle", "right"] {
            for d in ["top", "middle", "bottom"] {
                let v = if a == d { a.to_string() } else { format!("{a} {d}") };
                assert!(offered.contains(&v), "{v} isn't offered: {offered:?}");
                // What's offered is understood, without a note.
                let p = skin::parse(&format!("wallpaper-position: {v}"), "x");
                assert!(p.notes.is_empty(), "{v}: {:?}", p.notes);
            }
        }
        // Typing narrows it down.
        let text = "wallpaper-position: right b";
        assert_eq!(labels(&l.complete(text, text.len())), ["right bottom"]);
        // Every setting that's true or false offers both.
        for (key, said) in OTHERS {
            if said.contains("true or false") {
                let text = format!("{key}: ");
                let offered = labels(&l.complete(&text, text.len()));
                assert!(offered.contains(&"true".to_string()) && offered.contains(&"false".to_string()), "{key}: {offered:?}");
            }
        }
        // Every word a setting offers is one the skin understands.
        for (key, _) in OTHERS {
            for (v, _) in values_for(key) {
                let p = skin::parse(&format!("{key}: {v}"), "x");
                assert!(p.notes.is_empty(), "{key}: {v}: {:?}", p.notes);
            }
        }
    }

    #[test]
    fn numbers_say_what_they_can_be() {
        let l = SkinLang;
        for (key, _) in OTHERS {
            if let Some(range) = skin::range_of(key) {
                let said = l.explain(&format!("{key}: 1")).unwrap_or_default();
                assert_eq!(said.matches(range.as_str()).count(), 1, "{key}: {said}");
            }
        }
        assert!(l.explain("wallpaper-zoom: 50").is_some_and(|s| s.contains("-500 to 500")));
        assert!(l.explain("font-size: 9pt").is_some_and(|s| s.contains("8pt to 14pt")));
    }

    #[test]
    fn a_name_glint_doesnt_know_is_red() {
        let l = SkinLang;
        let theme = Theme::dark();
        let first = |row: &str| l.ink(row, &theme).first().map(|p| (p.0, p.1, p.2));
        assert_eq!(first("wobble: 3"), Some((0, 6, PROBLEM_RED)));
        assert_eq!(first(": 3"), Some((0, 0, PROBLEM_RED)));
        assert_eq!(first("wallpaper-wobble: 3 // a note"), Some((0, 16, PROBLEM_RED)));
        // Known names, other names for them, and CSS habits stay in the accent colour.
        for row in ["accent: #fff", "titlebar: #000", "Title Bar = #000", "{accent: #fff", "wallpaper-fade: 10"] {
            assert_eq!(first(row).map(|p| p.2), Some(theme.accent), "{row}");
        }
        // A note, or a name still being typed, has no name to colour.
        assert!(l.ink("// wobble: 3", &theme).iter().all(|p| p.2 != PROBLEM_RED));
        assert!(l.ink("wobble", &theme).is_empty());
    }

    #[test]
    fn ctrl_space_lists_what_fits_anywhere() {
        let l = SkinLang;
        // An empty text, and past its end: every setting.
        for caret in [0, 5] {
            let c = l.complete_asked("", caret);
            assert_eq!((c.start, c.end), (0, 0));
            assert_eq!(c.items.len(), COLOURS.len() + OTHERS.len());
        }
        // A blank line between two settings: those not set yet.
        let text = "accent: #fff\n\ntext: #000";
        let c = l.complete_asked(text, 13);
        assert_eq!((c.start, c.end), (13, 13));
        let offered = labels(&c);
        assert!(!offered.contains(&"accent".to_string()) && !offered.contains(&"text".to_string()) && offered.contains(&"panel".to_string()));
        // The start of a line with a setting: taking another changes its name (and the colon
        // isn't doubled).
        let text = "accent: #fff";
        let c = l.complete_asked(text, 0);
        assert_eq!((c.start, c.end), (0, 8));
        assert!(labels(&c).contains(&"accent".to_string()), "the line's own name is offered too");
        // Spaces before the name are stepped over.
        let c = l.complete_asked("   accent: #fff", 0);
        assert_eq!((c.start, c.end), (3, 11));
        // In the middle of a name: the whole of it.
        let c = l.complete_asked("accent: #fff", 3);
        assert_eq!((c.start, c.end), (0, 8));
        assert_eq!(labels(&c).first().map(String::as_str), Some("accent"), "\"acc\" typed");
        // The end of a line: its value's words, as when typing.
        let text = "shape: ";
        assert_eq!(labels(&l.complete_asked(text, text.len())), ["slanted", "straight", "rounded"]);
        let text = "accent: #fff";
        assert!(l.complete_asked(text, text.len()).items.is_empty());
        // A note: nothing.
        assert!(l.complete_asked("// accent", 0).items.is_empty());
        assert!(l.complete_asked("  // accent", 1).items.is_empty());
        assert!(l.complete_asked("// accent", 9).items.is_empty());
        // Letters past the first 128, and a cursor at each place in them: no panic.
        let text = "é: ü\nnäme";
        for caret in (0..=text.len()).filter(|&i| text.is_char_boundary(i)) {
            let c = l.complete_asked(text, caret);
            assert!(c.start <= c.end && c.end <= text.len() && text.is_char_boundary(c.start) && text.is_char_boundary(c.end), "{caret}");
        }
    }
}
