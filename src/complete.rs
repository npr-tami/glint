//! Suggestions while a sort order is typed, like Minecraft's command box: what fits where the
//! cursor is.
//!
//! - after `in ` → folders: the user's places (Downloads, Documents…), pinned folders, the open
//!   tabs' folders, the subfolders of the folder on screen and the drives; a path being typed
//!   (`C:\Us`) → the real folders in it, listed off the UI thread ([`Lister`]);
//! - after `-` or at the start of a word → the rule words, each with a few words about it;
//! - after `-bigger_than_` / `-smaller_than_` → sizes; after `-updated_` / `-older_than_` → times;
//! - after a rule and `_` → `_first`, `_last`, and `_by_day`… on date orders.
//!
//! Matching: the ones that start with what's typed first, then the ones that contain it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::folder;
use crate::sortrecipe::{self, EXACT_RULES, GROUP_RULES, ORDER_RULES, PLACE_NAMES};

/// One suggestion: what replaces the word being typed, what the list shows, and a few words about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub insert: String,
    pub label: String,
    pub hint: String,
}

/// The suggestions for the cursor's place: they replace `text[start..end]` (`end` is the cursor).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completion {
    pub start: usize,
    pub end: usize,
    pub items: Vec<Suggestion>,
    /// A folder whose subfolders would be suggested but aren't listed yet (ask the [`Lister`]).
    pub wants: Option<String>,
}

impl Completion {
    /// The rest of the first suggestion after what's typed (the grey text after the cursor), when
    /// it starts with what's typed.
    pub fn ghost(&self, text: &str) -> Option<String> {
        let typed = text.get(self.start..self.end)?;
        let first = self.items.first()?;
        let head = first.insert.get(..typed.len())?;
        (head.eq_ignore_ascii_case(typed) && first.insert.len() > typed.len()).then(|| first.insert[typed.len()..].to_string())
    }
}

/// A folder that can follow "in": its name, its path, and where it comes from ("Quick access").
#[derive(Clone, Debug)]
pub struct Place {
    pub name: String,
    pub path: String,
    pub hint: String,
}

/// Common file types offered as rules.
const TYPES: &[&str] = &["png", "jpg", "pdf", "exe", "zip", "mp4", "mp3", "txt", "docx", "xlsx", "msi", "iso", "psd", "svg", "gif", "json"];
const SIZES: &[&str] = &["1mb", "10mb", "100mb", "1gb", "500kb"];
const TIMES: &[&str] = &["2_hours", "3_days", "7_days", "2_weeks", "1_month"];
const AGES: &[&str] = &["1_month", "6_months", "1_year", "2_years"];

fn is_sep(c: char) -> bool {
    c.is_whitespace() || c == ',' || c == ';'
}

/// Puts `items` in matching order: the ones whose `key` starts with `typed` first, then the ones
/// that contain it; the rest are dropped, and so is one that is exactly what's typed.
fn rank(items: Vec<(String, Suggestion)>, typed: &str) -> Vec<Suggestion> {
    let typed = typed.to_lowercase();
    let (mut first, mut then) = (Vec::new(), Vec::new());
    let mut seen = HashSet::new();
    for (key, s) in items {
        let key = key.to_lowercase();
        if !seen.insert(s.insert.to_lowercase()) || key == typed {
            continue;
        }
        if key.starts_with(&typed) {
            first.push(s);
        } else if !typed.is_empty() && key.contains(&typed) {
            then.push(s);
        }
    }
    first.extend(then);
    first
}

/// Whether what follows "in " is a path being typed rather than a place's name.
fn looks_like_path(s: &str) -> bool {
    s.contains('\\') || s.starts_with(['%', '~']) || s.as_bytes().get(1) == Some(&b':')
}

/// The suggestions at `caret` in `text`. `places` are the folders for "in"; `listed` gives a
/// folder's subfolders when they've been listed (None: not yet).
pub fn complete(text: &str, caret: usize, places: &[Place], listed: &dyn Fn(&str) -> Option<Arc<Vec<String>>>) -> Completion {
    let caret = caret.min(text.len());
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..caret];
    let lead = before.len() - before.trim_start().len();
    let trimmed = &before[lead..];
    let none = Completion { start: caret, end: caret, ..Completion::default() };

    // "in " → folders
    if trimmed.len() >= 3 && trimmed[..3].eq_ignore_ascii_case("in ") {
        let typed = trimmed[3..].trim_start();
        if typed.contains(':') && !looks_like_path(typed) || typed.ends_with(':') {
            return none;
        }
        let start = caret - typed.len();
        let mut done = Completion { start, end: caret, ..Completion::default() };
        if looks_like_path(typed) && typed.contains('\\') {
            let cut = typed.rfind('\\').map_or(0, |i| i + 1);
            let (dir, partial) = typed.split_at(cut);
            let real = folder::normalize(dir);
            match listed(&real) {
                Some(names) => {
                    let items = names.iter().map(|n| (n.clone(), Suggestion { insert: format!("{dir}{n}\\"), label: n.clone(), hint: "folder".into() })).collect();
                    done.items = rank(items, partial);
                    // "C:\Users\me\" itself, finished with the colon.
                    if partial.is_empty() && !names.is_empty() {
                        done.items.push(Suggestion { insert: format!("{}:", typed.trim_end_matches('\\')), label: ":".into(), hint: "this folder".into() });
                    }
                }
                None => done.wants = Some(real),
            }
            return done;
        }
        let mut items = Vec::new();
        for p in places {
            let known = PLACE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(&p.name)) && p.hint == "Quick access";
            let drive = p.path.len() <= 3 && p.path.ends_with('\\');
            let insert = if known {
                format!("{}:", p.name)
            } else if drive {
                p.path.clone()
            } else {
                format!("{}:", p.path)
            };
            let s = Suggestion { insert, label: p.name.clone(), hint: if known || drive { p.hint.clone() } else { format!("{} \u{00b7} {}", p.hint, p.path) } };
            items.push((p.name.clone(), s.clone()));
            if looks_like_path(typed) {
                items.push((p.path.clone(), s));
            }
        }
        done.items = rank(items, typed);
        return done;
    }

    // The word being typed (from the last space to the cursor).
    let word_start = before.rfind(is_sep).map_or(0, |i| i + before[i..].chars().next().map_or(1, char::len_utf8));
    let word = &before[word_start..];
    let start = line_start + word_start;
    let prev: Vec<String> = before[..word_start].split(is_sep).filter(|t| !t.is_empty()).map(|t| t.trim_start_matches('-').to_lowercase()).collect();
    let dash = if word.starts_with('-') { "-" } else { "" };
    let norm = word.trim_start_matches('-').to_lowercase().replace('-', "_");
    let mut done = Completion { start, end: caret, ..Completion::default() };

    // "bigger than " (with spaces) → sizes on their own.
    let last2: Vec<&str> = prev.iter().rev().take(2).map(String::as_str).collect();
    if last2.len() == 2 && last2[0] == "than" && matches!(last2[1], "bigger" | "larger" | "smaller") && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        let items = SIZES.iter().map(|s| (s.to_string(), Suggestion { insert: s.to_string(), label: s.to_string(), hint: "a size".into() })).collect();
        done.items = rank(items, &norm);
        return done;
    }
    if word.is_empty() {
        return none;
    }
    let mut items: Vec<(String, Suggestion)> = Vec::new();
    // -bigger_than_… → sizes; -updated_… / -older_than_… → times.
    for (base, list, hint) in [("bigger_than_", SIZES, "a size"), ("larger_than_", SIZES, "a size"), ("smaller_than_", SIZES, "a size"), ("older_than_", AGES, "how old"), ("updated_", TIMES, "how recent"), ("added_", TIMES, "how recent")] {
        if let Some(partial) = norm.strip_prefix(base) {
            let items = list.iter().map(|s| (s.to_string(), Suggestion { insert: format!("{dash}{base}{s}"), label: format!("{base}{s}"), hint: hint.into() })).collect();
            done.items = rank(items, partial);
            if !done.items.is_empty() || base != "updated_" && base != "added_" {
                return done;
            }
        }
    }
    // A rule and "_" → _first, _last, _by_day…
    if let Some(cut) = norm.rfind('_') {
        let (base, partial) = (&norm[..cut], &norm[cut..]);
        let order = sortrecipe::is_order_word(base);
        let group = !order && !base.is_empty() && sortrecipe::Recipe::parse(&format!("-{base}")).messages.is_empty();
        if order || group {
            let mut ends: Vec<(&str, &str)> = if order { vec![("_first", "this end first"), ("_last", "turned around")] } else { vec![("_first", "at the very top"), ("_last", "at the very bottom")] };
            let date = order && ["newest", "oldest", "date", "modified", "latest", "created"].iter().any(|d| base.starts_with(d));
            if date && !base.contains("_by_") {
                ends.extend([("_by_day", "a day counts as one"), ("_by_week", "a week counts as one"), ("_by_month", "a month counts as one"), ("_by_year", "a year counts as one")]);
            }
            let suffixes = ends.into_iter().filter(|(e, _)| !base.ends_with(e)).map(|(e, h)| (e.to_string(), Suggestion { insert: format!("{dash}{base}{e}"), label: format!("{base}{e}"), hint: h.into() })).collect();
            items.extend(rank(suffixes, partial).into_iter().map(|s| (s.label.clone(), s)));
        }
    }
    // Rule words.
    for (rule, hint) in GROUP_RULES.iter().chain(ORDER_RULES).chain(EXACT_RULES) {
        let w = &rule[1..];
        items.push((w.to_string(), Suggestion { insert: format!("{dash}{w}"), label: rule.to_string(), hint: hint.to_string() }));
    }
    for t in TYPES {
        items.push((t.to_string(), Suggestion { insert: format!("{dash}{t}"), label: format!("-{t}"), hint: format!(".{t} files") }));
    }
    // "in" at the start of a line: rules for one folder.
    if prev.is_empty() && dash.is_empty() {
        items.push(("in".into(), Suggestion { insert: "in ".into(), label: "in".into(), hint: "rules for one folder".into() }));
    }
    done.items = rank(items, &norm);
    done
}

/// Lists folders' subfolders on a thread of its own, so typing never waits for the disk; the
/// window hears with a posted message and asks again.
#[derive(Default)]
pub struct Lister {
    done: Arc<Mutex<HashMap<String, Arc<Vec<String>>>>>,
    asked: HashSet<String>,
}

impl Lister {
    /// The subfolders of `dir` (normalised) when they've been listed.
    pub fn get(&self, dir: &str) -> Option<Arc<Vec<String>>> {
        self.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&dir.to_lowercase()).cloned()
    }

    /// Lists `dir` in the background (once), then posts `msg` to `hwnd`.
    pub fn ask(&mut self, dir: &str, hwnd: HWND, msg: u32) {
        let key = dir.to_lowercase();
        if dir.is_empty() || !self.asked.insert(key.clone()) {
            return;
        }
        let (done, dir, target) = (self.done.clone(), dir.to_string(), hwnd.0 as isize);
        crate::shell::background("glint-sort-folders", move || {
            let _com = crate::shell::ComGuard::init();
            let read = folder::read(&dir);
            let mut names: Vec<String> = read.entries.iter().filter(|e| e.is_container() && !e.is_hidden()).map(|e| e.name.clone()).collect();
            names.sort_by(|a, b| folder::natural_cmp(&a.to_lowercase(), &b.to_lowercase()));
            names.truncate(500);
            let mut map = done.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if map.len() > 200 {
                map.clear();
            }
            map.insert(key, Arc::new(names));
            drop(map);
            // SAFETY: posting to a window by handle; if it's gone the message is dropped.
            unsafe {
                let _ = PostMessageW(Some(HWND(target as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places() -> Vec<Place> {
        let p = |name: &str, path: &str, hint: &str| Place { name: name.into(), path: path.into(), hint: hint.into() };
        vec![
            p("Home", "C:\\Users\\me", "Quick access"),
            p("Documents", "C:\\Users\\me\\Documents", "Quick access"),
            p("Downloads", "C:\\Users\\me\\Downloads", "Quick access"),
            p("Work", "D:\\Work", "Pinned"),
            p("docs", "C:\\src\\docs", "Subfolder"),
            p("Local Disk (C:)", "C:\\", "Drive"),
        ]
    }

    fn at_end(text: &str) -> Completion {
        let listed = |dir: &str| (dir == "C:\\").then(|| Arc::new(vec!["Users".to_string(), "Windows".to_string(), "Program Files".to_string()]));
        complete(text, text.len(), &places(), &listed)
    }

    fn labels(c: &Completion) -> Vec<&str> {
        c.items.iter().map(|s| s.label.as_str()).collect()
    }

    #[test]
    fn folders_after_in() {
        let c = at_end("-a_to_z\nin do");
        assert_eq!(labels(&c)[..3], ["Documents", "Downloads", "docs"]);
        assert_eq!(c.items[0].insert, "Documents:");
        assert_eq!(c.items[2].insert, "C:\\src\\docs:");
        assert_eq!(c.ghost("-a_to_z\nin do").as_deref(), Some("cuments:"));
        assert_eq!(&"-a_to_z\nin do"[c.start..c.end], "do");
        // A path: the real folders in it (listed in the background).
        let c = at_end("in C:\\Us");
        assert_eq!(labels(&c), ["Users"]);
        assert_eq!(c.items[0].insert, "C:\\Users\\");
        assert_eq!(at_end("in D:\\Work\\").wants.as_deref(), Some("D:\\Work"));
        assert!(at_end("in Downloads:").items.is_empty());
    }

    #[test]
    fn rule_words_with_hints() {
        let c = at_end("-new");
        assert_eq!(c.items[0].insert, "-newest");
        assert!(!c.items[0].hint.is_empty());
        assert_eq!(c.ghost("-new").as_deref(), Some("est"));
        assert!(labels(&c).contains(&"-newest_added"));
        // No dash: the word as it's written.
        assert_eq!(at_end("folder_first\nbigg").items[0].insert, "biggest");
        // Contains comes after starts-with.
        let c = at_end("-age");
        assert!(c.items.iter().any(|s| s.insert == "-image"));
        assert!(at_end("-folder ").items.is_empty(), "nothing after a space");
        assert!(at_end("-zzzz").items.is_empty(), "no list when nothing matches");
        assert!(at_end("i").items.iter().any(|s| s.insert == "in "));
    }

    #[test]
    fn sizes_and_endings() {
        let c = at_end("-bigger_than_1");
        assert_eq!(labels(&c)[..2], ["bigger_than_1mb", "bigger_than_10mb"]);
        assert_eq!(c.items[0].insert, "-bigger_than_1mb");
        assert_eq!(at_end("bigger than ").items[0].insert, "1mb");
        let c = at_end("-newest_");
        assert_eq!(labels(&c)[..3], ["newest_first", "newest_last", "newest_by_day"]);
        let c = at_end("-image_l");
        assert_eq!(c.items[0].insert, "-image_last");
        assert_eq!(c.items[0].hint, "at the very bottom");
        assert!(!labels(&at_end("-image_")).contains(&"image_by_day"), "_by_ only on date orders");
    }
}
