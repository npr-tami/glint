//! Suggestions while a sort order is typed, like Minecraft's command box: what fits where the
//! cursor is.
//!
//! - after `in ` → folders: the user's places (Downloads, Documents…), pinned folders, the open
//!   tabs' folders, the subfolders of the folder on screen and the drives; a path being typed
//!   (`C:\Us`) → the real folders in it, listed off the UI thread ([`Lister`]);
//! - after `-` or at the start of a word → the rule words, each with a few words about it, then
//!   every file type Glint knows (after `-.`, written with the dot: `-.png`);
//! - at the start of a line → `in` and `in = (` too, or `)` inside a folder's rules not closed yet;
//! - after `-bigger_than_` / `-smaller_than_` → sizes; after `-updated_` / `-older_than_` → times;
//! - after a rule and `_` → `_first`, `_last`, and `_by_day`… on date orders.
//!
//! Ctrl+Space ([`complete_asked`]) gives them before anything's typed too.
//!
//! Matching: the ones that start with what's typed first, then the ones that contain it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::folder;
use crate::sortrecipe::{self, EXACT_RULES, FOLDER_RULES, GROUP_RULES, MORE_RULES, ORDER_RULES, PLACE_NAMES, VIEW_RULES};

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

/// The file types people sort by most, offered first (then every other type Glint knows).
const TYPES: &[&str] = &["png", "jpg", "pdf", "exe", "zip", "mp4", "mp3", "txt", "docx", "xlsx", "msi", "iso", "psd", "svg", "gif", "json"];

const SIZES: &[&str] = &["1mb", "10mb", "100mb", "1gb", "500kb"];
const TIMES: &[&str] = &["2_hours", "3_days", "7_days", "2_weeks", "1_month"];
const AGES: &[&str] = &["1_month", "6_months", "1_year", "2_years"];

fn is_sep(c: char) -> bool {
    c.is_whitespace() || c == ',' || c == ';'
}

/// Puts `items` in matching order: the ones whose `key` starts with `typed` first, then the ones
/// that contain it; the rest are dropped, and so is one that is exactly what's typed.
pub fn rank(items: Vec<(String, Suggestion)>, typed: &str) -> Vec<Suggestion> {
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

/// Whether the line starting at `line_start` is inside an "in = (" (its folders and rules), going by
/// the lines above it.
fn in_multi(text: &str, line_start: usize) -> bool {
    for line in text[..line_start].lines().rev() {
        let code = sortrecipe::strip_note(line).trim();
        if code.starts_with(')') {
            return false;
        }
        if let Some(rest) = code.get(..3).filter(|h| h.eq_ignore_ascii_case("in ")).map(|_| code[3..].trim_start()) {
            return rest.starts_with('=');
        }
    }
    false
}

/// Whether the line starting at `line_start` is inside a folder's rules whose ")" never comes:
/// the lines above it open them ("in Downloads (", "in = ("), and the lines below it don't close
/// them before the next "in".
fn in_open_block(text: &str, line_start: usize) -> bool {
    let is_in = |code: &str| code.get(..3).is_some_and(|h| h.eq_ignore_ascii_case("in ") || h.eq_ignore_ascii_case("in\t"));
    let mut open = false;
    for line in text[..line_start].lines() {
        let code = sortrecipe::strip_note(line).trim();
        if code.starts_with([')', '}', ']']) {
            open = false;
        } else if code == "(" || is_in(code) {
            open = code.ends_with(['(', '{', '[']);
        }
    }
    if !open {
        return false;
    }
    let below = text[line_start..].split_once('\n').map_or("", |(_, rest)| rest);
    for line in below.lines() {
        let code = sortrecipe::strip_note(line).trim();
        if code.starts_with([')', '}', ']']) {
            return false;
        }
        if is_in(code) {
            break;
        }
    }
    true
}

/// The places as folders of an "in = (": their full path goes in.
fn place_paths(places: &[Place]) -> Vec<(String, Suggestion)> {
    places
        .iter()
        .filter(|p| !p.path.is_empty())
        .map(|p| (p.name.clone(), Suggestion { insert: sortrecipe::quote_path(&p.path), label: p.name.clone(), hint: format!("{} \u{00b7} {}", p.hint, p.path) }))
        .collect()
}

/// A subfolder picked on a folder's own line: its path with a "\" to go on, or the whole path in
/// quotes when a # in it would start a note ("D:\Pics\Photos #2").
fn subfolder(dir: &str, name: &str) -> String {
    let path = format!("{dir}{name}");
    let quoted = sortrecipe::quote_path(&path);
    if quoted.len() == path.len() {
        path + "\\"
    } else {
        quoted
    }
}

/// Whether what follows "in " is a path being typed rather than a place's name.
fn looks_like_path(s: &str) -> bool {
    s.contains('\\') || s.starts_with(['%', '~']) || s.as_bytes().get(1) == Some(&b':')
}

/// The suggestions at `caret` in `text`, as it's typed. `places` are the folders for "in";
/// `listed` gives a folder's subfolders when they've been listed (None: not yet).
pub fn complete(text: &str, caret: usize, places: &[Place], listed: &dyn Fn(&str) -> Option<Arc<Vec<String>>>) -> Completion {
    suggestions(text, caret, places, listed, false)
}

/// Ctrl+Space: the suggestions at `caret`, even where nothing's typed yet (at the start of a line,
/// after a space, in an empty box): everything that fits there.
pub fn complete_asked(text: &str, caret: usize, places: &[Place], listed: &dyn Fn(&str) -> Option<Arc<Vec<String>>>) -> Completion {
    suggestions(text, caret, places, listed, true)
}

/// The suggestions at `caret`; `asked`: with Ctrl+Space, so before a word is started too.
fn suggestions(text: &str, caret: usize, places: &[Place], listed: &dyn Fn(&str) -> Option<Arc<Vec<String>>>, asked: bool) -> Completion {
    let caret = (0..=caret.min(text.len())).rev().find(|&c| text.is_char_boundary(c)).unwrap_or(0);
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1);
    let before = &text[line_start..caret];
    let lead = before.len() - before.trim_start().len();
    let trimmed = &before[lead..];
    let none = Completion { start: caret, end: caret, ..Completion::default() };

    // "in " → folders (`get`: the third byte can be inside a letter, as in "señor")
    if trimmed.get(..3).is_some_and(|head| head.eq_ignore_ascii_case("in ")) {
        let typed = trimmed[3..].trim_start();
        if typed.contains(':') && !looks_like_path(typed) || typed.ends_with([':', '(']) {
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
                    // "C:\Users\me\" itself, with its rules to follow in ( ). (A drive keeps its
                    // backslash: "C:\ (".)
                    if partial.is_empty() && !names.is_empty() {
                        let this = typed.trim_end_matches('\\');
                        let this = if this.len() == 2 && this.ends_with(':') { &typed[..3] } else { this };
                        done.items.push(Suggestion { insert: format!("{this} ("), label: "(".into(), hint: "this folder".into() });
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
                format!("{} (", p.name)
            } else if drive {
                p.path.clone()
            } else {
                format!("{} (", p.path)
            };
            let s = Suggestion { insert, label: p.name.clone(), hint: if known || drive { p.hint.clone() } else { format!("{} \u{00b7} {}", p.hint, p.path) } };
            items.push((p.name.clone(), s.clone()));
            if looks_like_path(typed) {
                items.push((p.path.clone(), s));
            }
        }
        // "in = (": several folders, the same rules (after the folders: they're what's wanted most).
        items.push(("= (".into(), Suggestion { insert: "= (".into(), label: "= (".into(), hint: "the same rules for several folders".into() }));
        done.items = rank(items, typed);
        return done;
    }

    // A path on a line of its own (a folder of "in = ("): its subfolders.
    if trimmed.contains('\\') && (trimmed.starts_with(['~', '%', '\\']) || trimmed.as_bytes().get(1) == Some(&b':')) {
        let cut = trimmed.rfind('\\').map_or(0, |i| i + 1);
        let (dir, partial) = trimmed.split_at(cut);
        let mut done = Completion { start: caret - trimmed.len(), end: caret, ..Completion::default() };
        match listed(&folder::normalize(dir)) {
            Some(names) => {
                let items = names.iter().map(|n| (n.clone(), Suggestion { insert: subfolder(dir, n), label: n.clone(), hint: "folder".into() })).collect();
                done.items = rank(items, partial);
            }
            None => done.wants = Some(folder::normalize(dir)),
        }
        return done;
    }

    // A folder's name on a line of its own in "in = (": the folder, as its full path. (With a space,
    // the whole line is the name; without, rules are suggested too, below.)
    let in_folders = in_multi(text, line_start) && !trimmed.starts_with(['-', '\u{2013}', '\u{2014}', '#']);
    let multi = in_folders && !trimmed.is_empty();
    if multi && trimmed.contains(' ') {
        let items = rank(place_paths(places), trimmed);
        if !items.is_empty() {
            return Completion { start: caret - trimmed.len(), end: caret, items, ..Completion::default() };
        }
    }

    // Nothing in a note (after a #): it's plain words.
    if sortrecipe::strip_note(before).len() < before.len() {
        return none;
    }
    // The word being typed (from the last space to the cursor).
    let word_start = before.rfind(is_sep).map_or(0, |i| i + before[i..].chars().next().map_or(1, char::len_utf8));
    let word = &before[word_start..];
    let start = line_start + word_start;
    // (The word can go on after the cursor: the suggestion replaces what's before it.)
    let word_end = caret + text[caret..].find(is_sep).unwrap_or(text.len() - caret);
    let prev: Vec<String> = before[..word_start].split(is_sep).filter(|t| !t.is_empty()).map(|t| t.trim_start_matches('-').to_lowercase()).collect();
    let dash = if word.starts_with('-') { "-" } else { "" };
    let norm = word.trim_start_matches('-').to_lowercase().replace('-', "_");
    let mut done = Completion { start, end: caret, ..Completion::default() };

    // "bigger than " (with spaces) → sizes on their own.
    let last2: Vec<&str> = prev.iter().rev().take(2).map(String::as_str).collect();
    if last2.len() == 2 && last2[0] == "than" && matches!(last2[1], "bigger" | "larger" | "smaller") && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
        let items = SIZES.iter().map(|s| (s.to_string(), Suggestion { insert: s.to_string(), label: s.to_string(), hint: "a size".into() })).collect();
        done.items = rank(items, &norm);
        return without_repeats(done, text, word_end);
    }
    if word.is_empty() && !asked {
        return none;
    }
    let mut items: Vec<(String, Suggestion)> = Vec::new();
    // -bigger_than_… → sizes; -updated_… / -older_than_… → times.
    for (base, list, hint) in [("bigger_than_", SIZES, "a size"), ("larger_than_", SIZES, "a size"), ("smaller_than_", SIZES, "a size"), ("older_than_", AGES, "how old"), ("updated_", TIMES, "how recent"), ("added_", TIMES, "how recent")] {
        if let Some(partial) = norm.strip_prefix(base) {
            let items = list.iter().map(|s| (s.to_string(), Suggestion { insert: format!("{dash}{base}{s}"), label: format!("{base}{s}"), hint: hint.into() })).collect();
            done.items = rank(items, partial);
            if !done.items.is_empty() || base != "updated_" && base != "added_" {
                return without_repeats(done, text, word_end);
            }
        }
    }
    // A rule and "_" → _first, _last, _by_day… (After the rule words that go on from what's
    // typed when the rule is another way of writing one: "name_" is -name_has_… more likely than
    // -a_to_z written as -name_first.)
    let mut endings: Vec<(String, Suggestion)> = Vec::new();
    let mut endings_last = false;
    if let Some(cut) = norm.rfind('_') {
        let (base, partial) = (&norm[..cut], &norm[cut..]);
        let order = sortrecipe::is_order_word(base);
        let group = !order && !base.is_empty() && !sortrecipe::is_view_word(base) && sortrecipe::Recipe::parse(&format!("-{base}")).messages.is_empty();
        if order || group {
            let mut ends: Vec<(&str, &str)> = if order { vec![("_first", "this end first"), ("_last", "turned around")] } else { vec![("_first", "at the very top"), ("_last", "at the very bottom")] };
            let date = order && ["newest", "oldest", "date", "modified", "latest", "created"].iter().any(|d| base.starts_with(d));
            if date && !base.contains("_by_") {
                ends.extend([("_by_day", "a day counts as one"), ("_by_week", "a week counts as one"), ("_by_month", "a month counts as one"), ("_by_year", "a year counts as one")]);
            }
            let suffixes = ends.into_iter().filter(|(e, _)| !base.ends_with(e)).map(|(e, h)| (e.to_string(), Suggestion { insert: format!("{dash}{base}{e}"), label: format!("{base}{e}"), hint: h.into() })).collect();
            endings.extend(rank(suffixes, partial).into_iter().map(|s| (s.label.clone(), s)));
            let main = GROUP_RULES.iter().chain(ORDER_RULES).any(|(r, _)| r[1..] == *base) || sortrecipe::known_type(base);
            endings_last = !main;
        }
    }
    if !endings_last {
        items.append(&mut endings);
    }
    // In a folder's rules not closed yet, at the start of a line: the ")" that closes them, first.
    let line_start_word = prev.is_empty() && dash.is_empty();
    let open_block = line_start_word && in_open_block(text, line_start);
    if open_block {
        items.push((")".into(), Suggestion { insert: ")".into(), label: ")".into(), hint: "the end of this folder's rules".into() }));
    }
    // Rule words (-file, which the guide leaves out, right after -folder), then other ways to
    // write them.
    let rule = |rule: &str, hint: &str| {
        let w = &rule[1..];
        (w.to_string(), Suggestion { insert: format!("{dash}{w}"), label: rule.to_string(), hint: hint.to_string() })
    };
    for (r, hint) in GROUP_RULES.iter().chain(ORDER_RULES).chain(EXACT_RULES).chain(VIEW_RULES).chain(FOLDER_RULES) {
        items.push(rule(r, hint));
        if *r == "-folder" {
            items.push(rule("-file", "files (not folders)"));
        }
    }
    items.extend(MORE_RULES.iter().map(|(r, hint)| rule(r, hint)));
    items.append(&mut endings);
    // "in" at the start of a line: rules for one folder, or "in = (" for several (not inside a
    // folder's rules: those end with ")" first).
    if line_start_word && !open_block {
        items.push(("in".into(), Suggestion { insert: "in ".into(), label: "in".into(), hint: "rules for one folder".into() }));
        items.push(("in = (".into(), Suggestion { insert: "in = (".into(), label: "in = (".into(), hint: "the same rules for several folders".into() }));
    }
    // File types: the common ones, then every other one Glint knows. After "-." with the dot.
    let dot = if norm.starts_with('.') { "." } else { "" };
    for t in TYPES.iter().copied().chain(sortrecipe::file_types()) {
        items.push((format!("{dot}{t}"), Suggestion { insert: format!("{dash}{dot}{t}"), label: format!("-{dot}{t}"), hint: format!(".{t} files") }));
    }
    // In "in = (": the folders too, first (its lines name folders before their rules), after the ")".
    if in_folders && prev.is_empty() {
        let at = items.iter().position(|(key, _)| key == ")").map_or(0, |i| i + 1);
        items.splice(at..at, place_paths(places));
    }
    done.items = rank(items, &norm);
    without_repeats(done, text, word_end)
}

/// The suggestions without the rules that wouldn't fit where the word is: a group the rules there
/// have already (on another line of the folder's block, or of the rules for every folder, or on
/// this line), which would be left out as a repeat, or one a group above takes all of.
/// `word_end`: where the word being replaced ends.
fn without_repeats(mut done: Completion, text: &str, word_end: usize) -> Completion {
    if done.items.is_empty() {
        return done;
    }
    if let Some(here) = sortrecipe::beside(text, done.start, word_end) {
        done.items.retain(|s| here.fits(&s.insert));
    }
    done
}

/// Lists folders' subfolders on a thread of its own, so typing never waits for the disk; the
/// window hears with a posted message and asks again.
#[derive(Default)]
pub struct Lister {
    done: Arc<Mutex<Cache<Arc<Vec<String>>>>>,
    /// The folders asked about, with how often `done` had been cleared then.
    asked: HashMap<String, usize>,
    /// Whether folders are there (the folders the rules name), looked at the same way.
    found: Arc<Mutex<Cache<bool>>>,
    looked: HashMap<String, usize>,
}

/// What the background threads found, emptied when it grows too big.
#[derive(Default)]
struct Cache<T> {
    map: HashMap<String, T>,
    clears: usize,
}

impl<T> Cache<T> {
    fn insert(&mut self, key: String, value: T, most: usize) {
        if self.map.len() > most {
            self.map.clear();
            self.clears += 1;
        }
        self.map.insert(key, value);
    }
}

/// Whether `key` needs asking about: never asked, or its answer was cleared away since. (One still
/// on its way when the cache was cleared may be asked for twice, which does no harm.)
fn needs_asking<T>(asked: &mut HashMap<String, usize>, cache: &Mutex<Cache<T>>, key: &str) -> bool {
    let cache = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.map.contains_key(key) || asked.get(key) == Some(&cache.clears) {
        return false;
    }
    asked.insert(key.to_string(), cache.clears);
    true
}

impl Lister {
    /// The subfolders of `dir` (normalised) when they've been listed.
    pub fn get(&self, dir: &str) -> Option<Arc<Vec<String>>> {
        self.done.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map.get(&dir.to_lowercase()).cloned()
    }

    /// Whether the folder `path` (normalised) is there, once it's been looked at.
    pub fn exists(&self, path: &str) -> Option<bool> {
        self.found.lock().unwrap_or_else(std::sync::PoisonError::into_inner).map.get(&path.to_lowercase()).copied()
    }

    /// Looks whether the folder `path` is there in the background (once: a network folder can take
    /// a while to answer), then posts `msg` to `hwnd`.
    pub fn look(&mut self, path: &str, hwnd: HWND, msg: u32) {
        let key = path.to_lowercase();
        if path.is_empty() || !needs_asking(&mut self.looked, &self.found, &key) {
            return;
        }
        let (found, path, target) = (self.found.clone(), path.to_string(), hwnd.0 as isize);
        crate::shell::background("glint-sort-folder-there", move || {
            let there = std::path::Path::new(&path).is_dir();
            found.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, there, 500);
            // SAFETY: posting to a window by handle; if it's gone the message is dropped.
            unsafe {
                let _ = PostMessageW(Some(HWND(target as *mut _)), msg, WPARAM(0), LPARAM(0));
            }
        });
    }

    /// Lists `dir` in the background (once), then posts `msg` to `hwnd`.
    pub fn ask(&mut self, dir: &str, hwnd: HWND, msg: u32) {
        let key = dir.to_lowercase();
        if dir.is_empty() || !needs_asking(&mut self.asked, &self.done, &key) {
            return;
        }
        let (done, dir, target) = (self.done.clone(), dir.to_string(), hwnd.0 as isize);
        crate::shell::background("glint-sort-folders", move || {
            let _com = crate::shell::ComGuard::init();
            let read = folder::read(&dir);
            let mut names: Vec<String> = read.entries.iter().filter(|e| e.is_container() && !e.is_hidden()).map(|e| e.name.clone()).collect();
            names.sort_by(|a, b| folder::natural_cmp(&a.to_lowercase(), &b.to_lowercase()));
            names.truncate(500);
            done.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key, Arc::new(names), 200);
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

    /// A folder is asked about once, and again only after its answer was cleared away.
    #[test]
    fn asks_again_after_a_clear() {
        let cache = Mutex::new(Cache::default());
        let mut asked = HashMap::new();
        assert!(needs_asking(&mut asked, &cache, "c:\\a"));
        assert!(!needs_asking(&mut asked, &cache, "c:\\a")); // (still on its way)
        cache.lock().unwrap().insert("c:\\a".into(), true, 1);
        assert!(!needs_asking(&mut asked, &cache, "c:\\a"));
        cache.lock().unwrap().insert("c:\\b".into(), true, 1);
        cache.lock().unwrap().insert("c:\\c".into(), true, 1); // clears a and b
        assert!(needs_asking(&mut asked, &cache, "c:\\a"));
        assert!(!needs_asking(&mut asked, &cache, "c:\\a"));
    }

    #[test]
    fn views_are_suggested() {
        let c = at_end("in Downloads (\n-larg");
        assert_eq!(c.items[0].insert, "-large_icons");
        assert!(labels(&at_end("-ext")).contains(&"-extra_large_icons"));
        // A view takes no _first / _last.
        assert!(!labels(&at_end("-large_icons_")).iter().any(|l| l.ends_with("_first")));
    }

    #[test]
    fn folders_after_in() {
        let c = at_end("-a_to_z\nin do");
        assert_eq!(labels(&c)[..3], ["Documents", "Downloads", "docs"]);
        assert_eq!(c.items[0].insert, "Documents (");
        assert_eq!(c.items[2].insert, "C:\\src\\docs (");
        assert_eq!(c.ghost("-a_to_z\nin do").as_deref(), Some("cuments ("));
        assert_eq!(&"-a_to_z\nin do"[c.start..c.end], "do");
        // A path: the real folders in it (listed in the background).
        let c = at_end("in C:\\Us");
        assert_eq!(labels(&c), ["Users"]);
        assert_eq!(c.items[0].insert, "C:\\Users\\");
        assert_eq!(at_end("in D:\\Work\\").wants.as_deref(), Some("D:\\Work"));
        assert!(at_end("in Downloads:").items.is_empty());
        assert!(at_end("in Downloads (").items.is_empty());
        // A folder itself: its rules follow in brackets.
        let this = |text: &str| at_end(text).items.into_iter().find(|s| s.label == "(").map(|s| s.insert);
        assert_eq!(this("in C:\\").as_deref(), Some("C:\\ ("));
    }

    #[test]
    fn folders_on_lines_of_their_own() {
        // The folders of "in = (", one per line.
        let c = at_end("in = (\nC:\\Us");
        assert_eq!(labels(&c), ["Users"]);
        assert_eq!(c.items[0].insert, "C:\\Users\\");
        assert_eq!(at_end("in = (\n  D:\\Work\\").wants.as_deref(), Some("D:\\Work"));
        assert!(labels(&at_end("in = (\nC:\\x\n-new")).contains(&"-newest"), "rules after them as usual");
        // A folder's name: its full path.
        let c = at_end("in = (\nC:\\x\ndown");
        assert_eq!(c.items[0].insert, "C:\\Users\\me\\Downloads");
        assert_eq!(c.ghost("in = (\nC:\\x\ndown").as_deref(), None, "the line is replaced, not added to");
        assert!(at_end("in = (\n-large_icons\n)\ndown").items.iter().all(|s| !s.insert.contains('\\')), "not after the )");
        assert!(at_end("down").items.iter().all(|s| !s.insert.contains('\\')), "not outside \"in = (\"");
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
    fn and_subfolders_is_suggested() {
        assert!(labels(&at_end("in C:\\x (\nand_s")).contains(&"-and_subfolders"));
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

    fn asked(text: &str, caret: usize) -> Completion {
        complete_asked(text, caret, &places(), &|_: &str| None)
    }

    fn inserts(c: &Completion) -> Vec<&str> {
        c.items.iter().map(|s| s.insert.as_str()).collect()
    }

    #[test]
    fn other_ways_to_write_rules_are_suggested() {
        // -file (which the guide leaves out), right after -folder.
        let c = at_end("-f");
        let at = |w: &str| inserts(&c).iter().position(|i| *i == w);
        assert_eq!(at("-file"), at("-folder").map(|i| i + 1));
        assert!(inserts(&at_end("fi")).contains(&"file"), "without the dash, as written");
        // The name rules, in place of -name_first (which is -a_to_z written another way).
        assert_eq!(at_end("-name_h").items[0].insert, "-name_has_");
        assert_eq!(at_end("-name_s").items[0].insert, "-name_starts_with_");
        assert_eq!(at_end("-name_e").items[0].insert, "-name_ends_with_");
        assert_eq!(at_end("-name_c").items[0].insert, "-name_contains_");
        let c = at_end("-name_");
        assert!(c.items[0].insert.starts_with("-name_has_") && c.items[1].insert.starts_with("-name_starts_with_"), "{:?}", labels(&c));
        assert!(labels(&c).contains(&"name_first"), "still there, after them");
        // -larger_than_, and its sizes.
        assert!(inserts(&at_end("-larg")).contains(&"-larger_than_"));
        assert_eq!(at_end("-larger_than_1").items[0].insert, "-larger_than_1mb");
        assert!(inserts(&at_end("-add")).contains(&"-added_3_days"));
        // Other words for the kinds, which the rules take as they are.
        assert_eq!(at_end("-mus").items[0].insert, "-music");
        assert_eq!(at_end("-pho").items[0].insert, "-photos");
        assert_eq!(at_end("-mov").items[0].insert, "-movies");
        assert_eq!(at_end("alpha").items[0].insert, "alphabetical");
        // The main words still come first.
        assert_eq!(at_end("-a").items[0].insert, "-audio");
        assert_eq!(at_end("-new").items[0].insert, "-newest");
    }

    /// Only rules that fit are offered: not one naming a group the rules there have already (it
    /// would be left out as a repeat), nor one a group above takes all of.
    #[test]
    fn rules_already_there_are_not_offered() {
        let c = at_end("-pic");
        assert_eq!(c.items[0].insert, "-pictures", "{:?}", labels(&c));
        for text in ["-image -newest\n-pic", "-photos\n\n-folder\n-pic", "-pictures_last -pic"] {
            let c = at_end(text);
            assert!(!inserts(&c).iter().any(|i| ["-pictures", "-photos", "-image", "-png"].contains(i)), "{text:?}: {:?}", inserts(&c));
        }
        // Below the word, too (that line would be the one left out), and Ctrl+Space before anything's typed.
        let text = "-pi\n-image";
        assert!(!inserts(&complete(text, 3, &places(), &|_: &str| None)).contains(&"-pictures"));
        let text = "-video\n\n-image";
        let c = asked(text, 7);
        assert!(!labels(&c).contains(&"-image") && !labels(&c).contains(&"-video") && labels(&c).contains(&"-audio"), "{:?}", labels(&c));
        // In a folder's own rules, only its own lines count.
        assert_eq!(at_end("-image\nin C:\\x (\n-pic").items[0].insert, "-pictures");
        assert!(!inserts(&at_end("in C:\\x (\n-image\n-pic")).contains(&"-pictures"));
        // The word going on after the cursor is the one replaced: it isn't counted as a line.
        let text = "-video\n-pictures";
        assert_eq!(complete(text, 10, &places(), &|_: &str| None).items[0].insert, "-pictures");
        // The endings of a group already there aren't offered either; an order rule's are.
        assert!(at_end("-image\n-image_").items.is_empty());
        assert!(!at_end("-newest\n-newest_").items.is_empty());
        // What's offered and "Did you mean" agree.
        let r = sortrecipe::Recipe::parse("-pic");
        assert_eq!(r.messages[0].fix.as_deref(), Some(at_end("-pic").items[0].insert.as_str()));
    }

    #[test]
    fn every_file_type_is_suggested() {
        let all = at_end("-");
        let offered: HashSet<&str> = inserts(&all).into_iter().collect();
        for t in sortrecipe::file_types() {
            assert!(offered.contains(format!("-{t}").as_str()), "-{t}");
        }
        assert!(sortrecipe::file_types().len() > 200);
        // The common ones before the rest.
        let c = at_end("-ps");
        let at = |w: &str| inserts(&c).iter().position(|i| *i == w).unwrap_or_else(|| panic!("{w}"));
        assert!(at("-psd") < at("-ps1"));
        assert!(inserts(&at_end("-blen")).contains(&"-blend"));
        assert!(inserts(&at_end("-torr")).contains(&"-torrent"));
        // With the dot, written with the dot (and nothing else).
        let c = at_end("-.pn");
        assert_eq!(c.items[0].insert, "-.png");
        assert!(c.items.iter().all(|s| s.insert.starts_with("-.")));
        assert!(inserts(&at_end(".bl")).contains(&".blend"));
        assert!(at_end("-.zzzz").items.is_empty());
    }

    #[test]
    fn in_and_brackets_at_the_start_of_a_line() {
        // "in = (" beside "in".
        let c = at_end("i");
        assert!(inserts(&c).contains(&"in ") && inserts(&c).contains(&"in = ("));
        assert!(inserts(&at_end("in")).contains(&"in = ("), "\"in\" typed: \"in = (\" goes on from it");
        assert!(inserts(&at_end("in ")).contains(&"= ("), "after \"in \" too, after the folders");
        assert_eq!(at_end("in ").items.last().map(|s| s.insert.as_str()), Some("= ("));
        assert_eq!(inserts(&at_end("in =")), ["= ("]);
        assert!(at_end("in = (").items.is_empty());
        // Not after another word, nor with a dash.
        let has_in = |c: &Completion| inserts(c).iter().any(|i| matches!(*i, "in " | "in = (" | "-in " | "-in = ("));
        assert!(!has_in(&at_end("-folder i")));
        assert!(!has_in(&at_end("-i")));
        // Not inside a folder's rules not closed yet: their ")" comes first.
        assert!(!has_in(&at_end("in Downloads (\ni")));
        assert!(inserts(&at_end("in Downloads (\n)\ni")).contains(&"in "), "after the )");
        // The ")" (Ctrl+Space on an empty line in rules not closed yet).
        assert_eq!(asked("in Downloads (\n-newest\n", 23).items[0].insert, ")");
        assert_eq!(asked("in Downloads (\n  ", 17).items[0].insert, ")", "indented");
        assert_eq!(asked("in Downloads\n(\n", 15).items[0].insert, ")", "the ( on a line of its own");
        let none = |text: &str, caret: usize| !inserts(&asked(text, caret)).contains(&")");
        assert!(none("", 0), "no rules to close");
        assert!(none("in Downloads (\n\n)", 15), "closed below already");
        assert!(none("in Downloads (\n-a_to_z\n)\n", 25), "closed above");
        assert!(none("in Downloads (-a_to_z)\n", 23), "on one line");
        assert!(none("in Downloads:\n", 14), "the older kind, without brackets");
        assert!(none("in Downloads (\n-newest ", 23), "not after a word");
        assert!(!inserts(&at_end("in Downloads (\n-")).contains(&")"), "not with a dash");
        // In "in = (": the ")" first, then the folders.
        let c = asked("in = (\n", 7);
        assert_eq!(c.items[0].insert, ")");
        assert_eq!(c.items[1].insert, "C:\\Users\\me");
    }

    #[test]
    fn ctrl_space_lists_what_fits() {
        // An empty box: the rules, "in" and "in = (" — and typed, nothing until a letter.
        let c = asked("", 0);
        assert_eq!((c.start, c.end), (0, 0));
        assert_eq!(c.items[0].insert, "folder");
        assert!(inserts(&c).contains(&"in ") && inserts(&c).contains(&"in = ("));
        assert!(!inserts(&c).contains(&")"));
        assert!(at_end("").items.is_empty());
        // At the start of a line.
        let text = "-newest\n";
        let c = asked(text, text.len());
        assert_eq!((c.start, c.end), (8, 8));
        assert_eq!(c.items[0].insert, "folder");
        assert!(inserts(&c).contains(&"in "));
        assert!(at_end(text).items.is_empty());
        // At the end of a line, after a space: rules, no "in".
        let text = "-folder ";
        let c = asked(text, text.len());
        assert!(inserts(&c).contains(&"a_to_z"));
        assert!(!inserts(&c).iter().any(|i| matches!(*i, "in " | "in = (")));
        // At the end of a word: the same as typing it.
        assert_eq!(asked("-new", 4), at_end("-new"));
        // In the middle of a line, between words.
        let c = asked("-folder  -newest", 8);
        assert_eq!((c.start, c.end), (8, 8));
        assert!(!c.items.is_empty());
        // After "in ": the folders.
        assert_eq!(asked("in ", 3).items[0].label, "Home");
        // Nothing in a note, asked or typed.
        assert!(asked("# my notes ", 11).items.is_empty());
        assert!(at_end("-a_to_z # new").items.is_empty());
        // A cursor past the end, or inside a letter, is taken back to one.
        assert_eq!(asked("", 5), asked("", 0));
        assert_eq!(asked("é", 1).start, 0);
    }

    /// Every word the list offers reads without a mistake (and so is coloured as a word Glint
    /// knows): the rule words, the file types, the endings, the sizes and the times.
    #[test]
    fn every_word_offered_reads() {
        let check = |word: &str| {
            // Inside a folder's rules, where every rule (-and_subfolders too) goes.
            let text = format!("in C:\\x (\n{word}\n)");
            let r = sortrecipe::Recipe::parse(&text);
            assert!(!r.has_errors(), "{word}: {:?}", r.messages);
            assert!(
                sortrecipe::is_order_word(word) || sortrecipe::is_view_word(word) || sortrecipe::is_folder_word(word) || sortrecipe::is_group_word(word),
                "{word} isn't coloured as a rule"
            );
        };
        // A word that needs something after it, with something after it.
        let finished = |w: &str| -> String {
            if !w.ends_with('_') {
                w.to_string()
            } else if w.contains("name_") {
                format!("{w}invoice")
            } else if w.contains("older_than_") {
                format!("{w}1_year")
            } else if w.contains("than_") {
                format!("{w}100mb")
            } else {
                format!("{w}3_days")
            }
        };
        let structure = ["in ", "in = (", ")"];
        let mut n = 0;
        for c in [asked("", 0), at_end("-"), at_end("-."), asked("-folder ", 8)] {
            for s in &c.items {
                if !structure.contains(&s.insert.as_str()) {
                    check(&finished(&s.insert));
                    n += 1;
                }
            }
        }
        assert!(n > 600, "{n}");
        // The endings, sizes and times.
        for stem in ["-bigger_than_", "-larger_than_", "-smaller_than_", "-older_than_", "-updated_", "-added_", "-newest_", "-oldest_added_", "-image_", "-png_", "-music_", "-name_"] {
            let c = at_end(stem);
            assert!(!c.items.is_empty(), "{stem}");
            for s in &c.items {
                check(&finished(&s.insert));
            }
        }
    }

    #[test]
    fn letters_of_more_than_one_byte() {
        // "ñ" takes the third and fourth bytes, "€" the second to fourth: "in " is looked for in
        // the first three.
        assert!(at_end("señor").items.is_empty());
        assert!(at_end("a€").items.is_empty());
        assert!(at_end("in Dö").items.is_empty());
    }
}
