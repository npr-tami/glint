//! What the form box (form.rs) is about: saving the open tabs as a set, and renaming many files at
//! once. Each form says what its boxes are, what the preview shows while typing, and what happens on
//! yes.
//!
//! **Tab sets** ("Save tabs as a set…" on a tab's right-click menu) keep the tabs' folders, sorting and
//! views under a name in settings.json ("TabSets"). Opening one opens its tabs after the ones already
//! open (nothing is closed); saving under a name that's taken replaces that set.
//!
//! **Renaming many** (F2 with several items selected, or right-click > Rename all…): a new name with
//! `#` for the number ("Holiday #" makes "Holiday 1", "Holiday 2"…; "###" makes "001"), and/or text
//! to find and what to put instead (it's looked for in the new name, once the new name is made).
//! Extensions stay as they are. The preview lists every new name (it scrolls), and says so when two
//! would get the same name or a name is taken by another item; nothing happens until it's all fine.
//! It's one step for Ctrl+Z.

use super::files::AfterJob;
use super::AppState;
use crate::fileops::{check_name, Job, NameCheck};
use crate::folder::{file_name, join, parent};
use crate::settings::{SavedTab, TabSet, MAX_TAB_SETS};

/// A form to fill in (form.rs draws it).
pub struct Form {
    /// Tells forms apart (a new one starts with its boxes filled afresh).
    pub id: u64,
    pub title: String,
    pub fields: Vec<Field>,
    /// The yes button's label ("SAVE").
    pub yes: &'static str,
    /// A two-way switch above the boxes (ZIP / 7Z), when the form has one.
    pub choice: Option<Choice>,
    /// A slider under the boxes (Split into), when the form has one.
    pub slider: Option<Slider>,
    /// A second switch under the boxes (Split by: FILES / SIZE): side 1 swaps the slider for the
    /// sizes. Its side comes after the slider's number in the values.
    pub by: Option<Choice>,
    /// Sizes to pick from (Each file: 25 MB...) and a box to type your own. The one picked comes
    /// next in the values (the own box is the number after the last chip), then the own box's text.
    pub sizes: Option<Sizes>,
    kind: Kind,
}

/// A form's sizes to pick from (see `Form::sizes`).
pub struct Sizes {
    pub label: &'static str,
    pub chips: &'static [&'static str],
    pub start: usize,
    /// The own box's unit, shown in it ("MB"), and its faint text when empty.
    pub unit: &'static str,
    pub own_hint: &'static str,
}

/// A form's slider: its label, the lowest and highest number, and the one it starts on. The number
/// picked comes last in the form's values ("3").
pub struct Slider {
    pub label: &'static str,
    pub min: u32,
    pub max: u32,
    pub start: u32,
    /// What the number counts, one and more ("file", "files"), shown after it.
    pub units: [&'static str; 2],
}

/// A form's two-way switch: its label, its two sides, and which is on at first. What's picked comes
/// after the boxes' text in the form's values ("0" or "1").
pub struct Choice {
    pub label: &'static str,
    pub sides: [&'static str; 2],
    pub start: usize,
}

/// One text box of a form.
pub struct Field {
    pub label: &'static str,
    /// Faint text in the empty box.
    pub hint: &'static str,
    /// What's in it at first.
    pub start: String,
    /// A faint line under the box saying how to fill it in ("" for none).
    pub note: &'static str,
    /// Shows dots instead of what's typed (a password).
    pub secret: bool,
}

/// What a form's typing would do, for its preview.
#[derive(Default)]
pub struct Preview {
    /// Lines that always show, above the list.
    pub lines: Vec<String>,
    /// Each item that changes, its name now and its new one ("a.jpg", "Holiday 1.jpg"): form.rs
    /// shows them a line each, a few at a time, and scrolls through the rest.
    pub list: Vec<(String, String)>,
    /// Why it can't be done as typed (shown in the accent colour; yes does nothing).
    pub problem: Option<String>,
    /// Something to know that doesn't stop yes ("Wrong password. Try again."), in the accent colour.
    pub alert: Option<String>,
    /// Something would happen (otherwise yes does nothing, without a problem to say).
    pub ready: bool,
}

enum Kind {
    SaveTabs,
    /// The items to rename: their full paths, in the order they're shown (the numbers follow it),
    /// and whether each is a folder (folders have no extension to keep).
    RenameMany { paths: Vec<String>, folders: Vec<bool> },
    /// The password for an archive, then what wanted it is done again.
    Password { ask: crate::archive::Ask, retry: Retry },
    /// Compress…: these items into a new zip or 7z, with a password or not, in parts or not.
    Pack { paths: Vec<String> },
}

/// What to do again once an archive's password is typed.
pub(super) enum Retry {
    /// Read the tab with this id again (it was looking into the archive).
    Reload(u64),
    /// Take these items out of the archive again, and then this (open, copy...).
    TakeOut { archive: String, items: Vec<String>, then: super::Then },
    /// Run this job again (Extract here...).
    Job(Box<Job>, AfterJob),
    /// Nothing to do again by itself (a drag): say how to go on.
    Say(&'static str),
}

impl AppState {
    fn open_form(&mut self, title: String, fields: Vec<Field>, yes: &'static str, kind: Kind) {
        self.next_question += 1;
        self.form = Some(Form { id: self.next_question, title, fields, yes, choice: None, slider: None, by: None, sizes: None, kind });
    }

    /// Asks for the password `ask` wants (the archive's name in the title; "Wrong password" under
    /// the box when the one typed was wrong), then does `retry`. Another form already open stays:
    /// then the error already on the status bar has to do.
    pub(super) fn ask_password(&mut self, ask: crate::archive::Ask, retry: Retry) {
        if self.form.is_some() || self.question.is_some() {
            return;
        }
        let title = format!("\u{201c}{}\u{201d} has a password", ask.name);
        let fields = vec![Field { label: "Password", hint: "", start: String::new(), note: "", secret: true }];
        self.open_form(title, fields, "OPEN", Kind::Password { ask, retry });
    }

    /// A drag out of a locked archive stopped for its password: asked for, then it's dragged again
    /// by hand.
    pub fn ask_password_to_drag(&mut self, ask: crate::archive::Ask) {
        self.ask_password(ask, Retry::Say("Now drag it out again."));
    }

    /// Right-click > Compress…: zip or 7z, a password (none when it's left empty; its SHOW button
    /// lets you read what you typed), and how many parts of the same size (1: one file).
    pub(super) fn ask_pack(&mut self) {
        if !self.files_here() {
            return;
        }
        let paths = self.chosen();
        if paths.is_empty() {
            return self.say("Select what to compress first.");
        }
        let fields = vec![Field { label: "Password", hint: "", start: String::new(), note: "Leave empty for no password.", secret: true }];
        self.open_form("Compress".into(), fields, "COMPRESS", Kind::Pack { paths });
        if let Some(f) = self.form.as_mut() {
            f.choice = Some(Choice { label: "Type", sides: ["ZIP", "7Z"], start: 0 });
            f.slider = Some(Slider { label: "Split into", min: 1, max: MOST_PARTS, start: 1, units: ["file", "files"] });
            f.by = Some(Choice { label: "Split by", sides: ["FILES", "SIZE"], start: 0 });
            f.sizes = Some(Sizes { label: "Each file", chips: &SIZE_NAMES, start: 3, unit: "MB", own_hint: "Own" });
        }
    }

    /// "Save tabs as a set…": asks for a name.
    pub(super) fn ask_tab_set_name(&mut self) {
        let start = self.tab_set_suggestion();
        self.open_form("Save these tabs as a set".into(), vec![Field { label: "Name", hint: "Work", start, note: "", secret: false }], "SAVE", Kind::SaveTabs);
    }

    /// The name offered for a new set: "Set 1", "Set 2"…
    fn tab_set_suggestion(&self) -> String {
        (1..).map(|n| format!("Set {n}")).find(|n| !self.settings.tab_sets.iter().any(|s| s.name.eq_ignore_ascii_case(n))).unwrap_or_default()
    }

    /// F2 with several items selected, or right-click > Rename all…
    pub(super) fn ask_rename_many(&mut self) {
        if !self.files_here() {
            return;
        }
        let t = self.tab();
        let chosen = t.selected_entries();
        if chosen.len() < 2 {
            return;
        }
        let paths: Vec<String> = chosen.iter().map(|&i| t.folder.entries[i].path_in(&t.folder.path)).collect();
        let folders = chosen.iter().map(|&i| t.folder.entries[i].is_container()).collect();
        let title = format!("Rename {} items", paths.len());
        let fields = vec![
            Field { label: "New name", hint: "Holiday #", start: String::new(), note: "Use # for sequential numbers, for example Photo # \u{2192} Photo 1, Photo 2.", secret: false },
            Field { label: "Find", hint: "", start: String::new(), note: "", secret: false },
            Field { label: "Replace with", hint: "", start: String::new(), note: THEN_FIND, secret: false },
        ];
        self.open_form(title, fields, "RENAME", Kind::RenameMany { paths, folders });
    }

    /// What the form would do with what's typed now (form.rs shows it as you type).
    pub fn form_preview(&self, values: &[String]) -> Preview {
        let Some(form) = &self.form else { return Preview::default() };
        let value = |i: usize| values.get(i).map(String::as_str).unwrap_or("");
        match &form.kind {
            Kind::SaveTabs => {
                let name = value(0).trim();
                let folders: Vec<String> = self.tabs.iter().map(|t| self.display_name(&t.path)).collect();
                let mut lines = vec![format!("{} tabs: {}", folders.len(), folders.join(", "))];
                if self.settings.tab_sets.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
                    lines.push(format!("Replaces the set called \u{201c}{name}\u{201d}"));
                }
                Preview { lines, ready: !name.is_empty(), ..Default::default() }
            }
            Kind::RenameMany { paths, folders } => {
                let names: Vec<&str> = paths.iter().map(|p| file_name(p)).collect();
                // (Said only while it's so: a new name typed without a #.)
                let mut lines: Vec<String> = Vec::new();
                if numbers_added(value(0), names.len()) {
                    lines.push(NUMBER_ADDED.into());
                }
                match new_names(&names, folders, value(0), value(1), value(2)) {
                    Err(problem) => Preview { lines, problem: Some(problem), ..Default::default() },
                    Ok(new) => {
                        let list: Vec<(String, String)> = names.iter().zip(&new).filter(|(a, b)| **a != b.as_str()).map(|(a, b)| (a.to_string(), b.clone())).collect();
                        let problem = self.taken_name(paths, &new);
                        if list.is_empty() {
                            lines.push("Nothing changes yet".into());
                        }
                        let ready = !list.is_empty();
                        Preview { lines, list, problem, ready, alert: None }
                    }
                }
            }
            Kind::Password { ask, .. } => Preview { ready: !value(0).is_empty(), alert: ask.wrong.then(|| "Wrong password. Try again.".to_string()), ..Default::default() },
            Kind::Pack { paths } => {
                let name = format!("{}{}", crate::archive::pack_name(paths), pack_format(value(1)).ext());
                let line = match pack_split(value(2), value(3), value(4), value(5)) {
                    Ok(crate::fileops::Split::Files(1)) => name,
                    Ok(crate::fileops::Split::Files(n)) => format!("{name}.001 to {name}.{n:03}"),
                    Ok(crate::fileops::Split::Size(_)) => format!("{name}.001, .002\u{2026} each up to {}", size_name(value(4), value(5))),
                    Err(problem) => return Preview { lines: vec![name], problem, ..Default::default() },
                };
                Preview { lines: vec![line], ready: true, ..Default::default() }
            }
        }
    }

    /// A new name that another item in the folder already has (one that isn't being renamed too).
    fn taken_name(&self, paths: &[String], new: &[String]) -> Option<String> {
        let t = self.tab();
        if t.is_searching() {
            return None; // (the results' folders aren't listed here; the renames never replace anything)
        }
        let renamed: std::collections::HashSet<String> = paths.iter().map(|p| file_name(p).to_lowercase()).collect();
        let others: std::collections::HashSet<&str> = t.folder.entries.iter().map(|e| e.lower.as_str()).filter(|n| !renamed.contains(*n)).collect();
        new.iter().find(|n| others.contains(n.to_lowercase().as_str())).map(|n| format!("There's already an item called \u{201c}{n}\u{201d} here"))
    }

    /// The form's yes (`values`) or no (None). True when the form is done and closes.
    pub fn form_answer(&mut self, values: Option<Vec<String>>) -> bool {
        let Some(values) = values else {
            // CANCEL after a wrong password: what was said about it goes, so it doesn't stay up
            // as if Glint were still trying (the list says the archive has a password instead).
            if let Some(Form { kind: Kind::Password { ask, .. }, .. }) = self.form.take() {
                let wrong = format!("Wrong password for \u{201c}{}\u{201d}.", ask.name);
                if self.note() == Some(wrong.as_str()) {
                    self.note = None;
                }
                let locked = format!("\u{201c}{}\u{201d} has a password.", ask.name);
                for t in &mut self.tabs {
                    if t.folder.error.as_deref() == Some(wrong.as_str()) {
                        t.folder.error = Some(locked.clone());
                    }
                }
            }
            self.form = None;
            return true;
        };
        if !self.form_preview(&values).ready || self.form_preview(&values).problem.is_some() {
            return false;
        }
        let Some(form) = self.form.take() else { return true };
        let value = |i: usize| values.get(i).map(String::as_str).unwrap_or("");
        match form.kind {
            Kind::Password { ask, retry } => {
                crate::archive::set_password(&ask.archive, value(0));
                match retry {
                    Retry::Reload(tab_id) => {
                        if let Some(i) = self.tabs.iter().position(|t| t.id == tab_id) {
                            self.start_load(i);
                        }
                    }
                    Retry::TakeOut { archive, items, then } => {
                        let started = self.extract_then(archive, items, then);
                        if let super::Then::Copy(_) = then {
                            self.taking_out = started;
                        }
                    }
                    Retry::Job(job, after) => {
                        self.start_job(*job, after);
                    }
                    Retry::Say(what) => self.say(what),
                }
            }
            Kind::Pack { paths } => {
                let after = AfterJob::new(self.tab().id);
                let password = (!value(0).is_empty()).then(|| crate::fileops::Secret(value(0).to_string()));
                let split = pack_split(value(2), value(3), value(4), value(5)).unwrap_or(crate::fileops::Split::Files(1));
                self.start_job(Job::Compress { paths, format: pack_format(value(1)), password, split }, after);
            }
            Kind::SaveTabs => self.save_tab_set(value(0).trim()),
            Kind::RenameMany { paths, folders } => {
                let names: Vec<&str> = paths.iter().map(|p| file_name(p)).collect();
                if let Ok(new) = new_names(&names, &folders, value(0), value(1), value(2)) {
                    let pairs: Vec<(String, String)> = paths
                        .iter()
                        .zip(new)
                        .filter(|(p, n)| file_name(p) != n.as_str())
                        .map(|(p, n)| (p.clone(), join(&parent(p).unwrap_or_default(), &n)))
                        .collect();
                    let after = AfterJob::new(self.tab().id);
                    self.start_job(Job::RenameMany { pairs }, after);
                }
            }
        }
        true
    }

    fn save_tab_set(&mut self, name: &str) {
        // (A set keeps no Back and Forward places, a pinned tab's either: it opens fresh tabs.)
        let tabs: Vec<SavedTab> = self.snapshot().tabs.into_iter().map(|t| SavedTab { back: Vec::new(), forward: Vec::new(), ..t }).collect();
        let sets = &mut self.settings.tab_sets;
        sets.retain(|s| !s.name.eq_ignore_ascii_case(name));
        sets.push(TabSet { name: name.to_string(), tabs });
        let extra = sets.len().saturating_sub(MAX_TAB_SETS);
        sets.drain(..extra);
        self.say(format!("Saved the tabs as \u{201c}{name}\u{201d}."));
    }

    /// Opens the tabs of set `i` after the ones open now, and shows the first of them.
    pub(super) fn open_tab_set(&mut self, i: usize) {
        let Some(set) = self.settings.tab_sets.get(i).cloned() else { return };
        let mut first = None;
        for saved in &set.tabs {
            let mut tab = self.make_tab(saved.path.clone());
            tab.sort = crate::settings::sort_key(saved.sort_column);
            tab.descending = saved.sort_descending;
            if let Some(v) = saved.view {
                tab.set_own_view(v);
            }
            let len = self.tabs.len();
            let at = self.insert_tab(len, tab);
            first.get_or_insert(at);
        }
        if let Some(at) = first {
            self.active = at;
            self.start_load(at);
        }
        self.say(format!("Opened \u{201c}{}\u{201d}.", set.name));
    }

    pub(super) fn delete_tab_set(&mut self, i: usize) {
        if i < self.settings.tab_sets.len() {
            let set = self.settings.tab_sets.remove(i);
            self.say(format!("Removed the set \u{201c}{}\u{201d}.", set.name));
        }
    }
}

/// The most parts Compress… cuts an archive into.
const MOST_PARTS: u32 = 10;

/// Compress…'s sizes to pick from, and what each is in MB (1 MB = 1024 × 1024 bytes). "4 GB" is
/// a little under 4 GB, so a part fits a FAT32 USB stick.
const SIZE_NAMES: [&str; 4] = ["25 MB", "100 MB", "700 MB", "4 GB"];
const SIZE_MB: [u64; 4] = [25, 100, 700, 4092];
const MB: u64 = 1024 * 1024;

/// How Compress… cuts the archive, from its values: `by` "0" (FILES) takes the slider's `count`;
/// "1" (SIZE) takes the size chip `chip`, or the own box's `own` MB when that's picked. Err: why
/// it can't be done yet (None: nothing to say, the own box is just empty).
fn pack_split(count: &str, by: &str, chip: &str, own: &str) -> Result<crate::fileops::Split, Option<String>> {
    use crate::fileops::Split;
    if by != "1" {
        return Ok(Split::Files(pack_parts(count)));
    }
    let chip: usize = chip.parse().unwrap_or(0);
    if let Some(mb) = SIZE_MB.get(chip) {
        return Ok(Split::Size(mb * MB));
    }
    let own = own.trim();
    if own.is_empty() {
        return Err(None);
    }
    match own.parse::<u64>() {
        Ok(mb) if mb > 0 && mb <= 1024 * 1024 => Ok(Split::Size(mb * MB)),
        _ => Err(Some("Type the size in MB, like 50.".into())),
    }
}

/// The size picked, in words ("4 GB", "50 MB").
fn size_name(chip: &str, own: &str) -> String {
    let chip: usize = chip.parse().unwrap_or(0);
    SIZE_NAMES.get(chip).map_or_else(|| format!("{} MB", own.trim()), |n| n.to_string())
}

/// How many parts Compress…'s slider picks (1 to `MOST_PARTS`).
fn pack_parts(value: &str) -> u32 {
    value.parse::<u32>().unwrap_or(1).clamp(1, MOST_PARTS)
}

/// The kind of archive the switch of Compress… picks ("0" zip, "1" 7z).
fn pack_format(side: &str) -> crate::archive::Format {
    if side == "1" {
        crate::archive::Format::SevenZ
    } else {
        crate::archive::Format::Zip
    }
}

/// Said under the new name while a new name without `#` gets numbers anyway (`numbers_added`).
pub const NUMBER_ADDED: &str = "A number is added automatically when renaming multiple items.";
/// Said under Replace with: how it goes with the new name (as `new_names` does it).
const THEN_FIND: &str = "Find and Replace with are applied after New name.";

/// Whether a new name typed as `pattern` gets a number put after it for `count` items: it has no
/// `#` of its own and there's more than one item (they can't all have the same name).
pub fn numbers_added(pattern: &str, count: usize) -> bool {
    let pattern = pattern.trim();
    !pattern.is_empty() && !pattern.contains('#') && count > 1
}

/// The new names for `names` (one item each; `folders` says which are folders): `pattern` with `#`
/// for the number (empty keeps each name), then `find` replaced with `replace` (ignoring capitals).
/// Extensions of files stay as they are. Err: why it can't be done.
pub fn new_names(names: &[&str], folders: &[bool], pattern: &str, find: &str, replace: &str) -> Result<Vec<String>, String> {
    // "Holiday" for several items: they're numbered after it ("Holiday 1").
    let numbers = numbers_added(pattern, names.len());
    let pattern = pattern.trim();
    let pattern = if numbers { format!("{pattern} #") } else { pattern.to_string() };
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for (n, (&name, &folder)) in names.iter().zip(folders).enumerate() {
        // (Found in the name itself: lower case can have another length, "x.\u{212A}".)
        let ext = match name.rfind('.') {
            Some(i) if i > 0 && !folder => &name[i..],
            _ => "",
        };
        let stem = &name[..name.len() - ext.len()];
        let mut new = if pattern.is_empty() { stem.to_string() } else { numbered(&pattern, n + 1) };
        if !find.is_empty() {
            new = replace_ignoring_case(&new, find, replace);
        }
        if new.trim().is_empty() {
            return Err(format!("\u{201c}{name}\u{201d} would have no name"));
        }
        let full = format!("{new}{ext}");
        match check_name(&full) {
            NameCheck::Good(good) => out.push(good),
            NameCheck::Empty => return Err(format!("\u{201c}{name}\u{201d} would have no name")),
            NameCheck::Bad(why) => return Err(why),
        }
    }
    // Two items can't end up with the same name.
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = out.iter().zip(names).find(|(n, _)| !seen.insert(n.to_lowercase())).map(|(n, _)| n) {
        return Err(format!("Two items would both be called \u{201c}{dup}\u{201d}"));
    }
    Ok(out)
}

/// `pattern` with each run of `#` made the number `n`, padded with zeros to the run's length.
fn numbered(pattern: &str, n: usize) -> String {
    let mut out = String::new();
    let mut run = 0;
    let flush = |out: &mut String, run: &mut usize| {
        if *run > 0 {
            out.push_str(&format!("{n:0width$}", width = *run));
            *run = 0;
        }
    };
    for c in pattern.chars() {
        if c == '#' {
            run += 1;
        } else {
            flush(&mut out, &mut run);
            out.push(c);
        }
    }
    flush(&mut out, &mut run);
    out
}

/// `text` with every `find` (in any capitals) made `replace`.
fn replace_ignoring_case(text: &str, find: &str, replace: &str) -> String {
    // Lower-casing changes the length of a few rare letters (the Kelvin sign is "k"), and then a
    // place in the lower-case text isn't the same place in `text`: match exactly instead. (Each
    // letter is checked, as one that grows and one that shrinks keep the total length.)
    let same_places = |s: &str| s.chars().all(|c| c.to_lowercase().map(char::len_utf8).eq([c.len_utf8()]));
    if !same_places(text) || !same_places(find) {
        return text.replace(find, replace);
    }
    let (lower, needle) = (text.to_lowercase(), find.to_lowercase());
    let mut out = String::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find(&needle) {
        out.push_str(&text[at..at + i]);
        out.push_str(replace);
        at += i + needle.len();
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn many_names_get_numbers_and_keep_their_extensions() {
        let names = ["IMG_001.jpg", "IMG_002.JPG", "notes"];
        let folders = [false, false, true];
        assert_eq!(new_names(&names, &folders, "Holiday #", "", "").unwrap(), vec!["Holiday 1.jpg", "Holiday 2.JPG", "Holiday 3"]);
        assert_eq!(new_names(&names, &folders, "Holiday", "", "").unwrap(), vec!["Holiday 1.jpg", "Holiday 2.JPG", "Holiday 3"]);
        assert_eq!(new_names(&names, &folders, "pic ###", "", "").unwrap(), vec!["pic 001.jpg", "pic 002.JPG", "pic 003"]);
        // Find and replace, in any capitals, on the name only.
        assert_eq!(new_names(&names, &folders, "", "img_", "Trip ").unwrap(), vec!["Trip 001.jpg", "Trip 002.JPG", "notes"]);
        assert_eq!(new_names(&["a.jpg.jpg"], &[false], "", "jpg", "x").unwrap(), vec!["a.x.jpg"]);
        // A letter whose lower case is longer beside one whose lower case is shorter: no panic.
        assert_eq!(replace_ignoring_case("\u{130}\u{212B}", "\u{e5}", "y"), "\u{130}\u{212B}");
        // An extension whose lower case is shorter (the Kelvin sign is "k"): kept as it is, no panic.
        assert_eq!(new_names(&["x.\u{212A}", "\u{e9}.\u{130}"], &[false, false], "y", "", "").unwrap(), vec!["y 1.\u{212A}", "y 2.\u{130}"]);
    }

    #[test]
    fn bad_and_clashing_names_are_refused() {
        let folders = [false, false];
        assert!(new_names(&["a.txt", "b.txt"], &folders, "", "a", "b").unwrap_err().contains("both"));
        assert!(new_names(&["a.txt", "b.txt"], &folders, "x?", "", "").is_err());
        assert!(new_names(&["a.txt"], &[false], "", "a", "").is_err(), "no name left");
    }

    #[test]
    fn numbers_grow_past_their_zeros_and_never_clash() {
        let names: Vec<String> = (0..120).map(|n| format!("IMG_{n:04}.jpg")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let files = vec![false; names.len()];
        let new = new_names(&names, &files, "pic ##", "", "").unwrap();
        assert_eq!((new[0].as_str(), new[8].as_str(), new[9].as_str(), new[98].as_str(), new[99].as_str(), new[119].as_str()), ("pic 01.jpg", "pic 09.jpg", "pic 10.jpg", "pic 99.jpg", "pic 100.jpg", "pic 120.jpg"));
        let new = new_names(&names[..12], &files[..12], "pic #", "", "").unwrap();
        assert_eq!((new[8].as_str(), new[9].as_str(), new[11].as_str()), ("pic 9.jpg", "pic 10.jpg", "pic 12.jpg"));
        // Each run of # gets the number; a # already in a name is only text.
        assert_eq!(new_names(&["a.mp3", "b.mp3"], &[false, false], "# of ###", "", "").unwrap(), ["1 of 001.mp3", "2 of 002.mp3"]);
        assert_eq!(new_names(&["Track #1.mp3", "Track #2.mp3"], &[false, false], "", "#", "No. ").unwrap(), ["Track No. 1.mp3", "Track No. 2.mp3"]);
    }

    #[test]
    fn names_windows_would_refuse_or_change_are_caught_first() {
        let two = [false, false];
        // Reserved device names, also behind an extension or made by Find and replace.
        assert!(new_names(&["a.txt", "b.txt"], &two, "COM#", "", "").unwrap_err().contains("Windows keeps for itself"));
        assert!(new_names(&["icon.txt", "b.txt"], &two, "", "i", "").unwrap_err().contains("\u{201c}con.txt\u{201d}"));
        assert!(new_names(&["a.txt", "lpt9 b.txt"], &two, "", " b", "").is_err());
        assert!(new_names(&["a", "b"], &[true, true], "", "a", "NUL").is_err(), "the folder a would be NUL");
        assert_eq!(new_names(&["a", "b"], &[true, true], "NUL", "", "").unwrap(), ["NUL 1", "NUL 2"], "only the bare name is the device's");
        // Nothing left, or only dots and spaces: no name.
        assert!(new_names(&["a.txt", "b.txt"], &two, "", "a", " ").unwrap_err().contains("would have no name"));
        assert!(new_names(&["x", "y"], &[true, true], "", "x", "...").unwrap_err().contains("would have no name"));
        // Characters a name can't have, and names over 255 characters.
        assert!(new_names(&["a.txt", "b.txt"], &two, "", "a", "a/b").is_err());
        assert!(new_names(&["a.txt", "b.txt"], &two, &"x".repeat(250), "", "").unwrap_err().contains("too long"));
        // Dots and spaces at the end go, as Windows would take them off anyway.
        assert_eq!(new_names(&["x", "y"], &[true, true], "Trip #. ", "", "").unwrap(), ["Trip 1", "Trip 2"]);
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &two, "  Trip #  ", "", "").unwrap(), ["Trip 1.jpg", "Trip 2.jpg"]);
        // Two that would differ only in capitals clash too (Windows sees one name).
        assert!(new_names(&["Same.txt", "x.TXT"], &two, "", "x", "same").unwrap_err().contains("both"));
    }

    #[test]
    fn extensions_stay_and_folders_and_dot_files_have_none() {
        let names = ["archive.tar.gz", ".gitignore", "v1.2", "README", "photo.JPEG"];
        let folders = [false, false, true, false, false];
        let new = new_names(&names, &folders, "Trip #", "", "").unwrap();
        assert_eq!(new, ["Trip 1.gz", "Trip 2", "Trip 3", "Trip 4", "Trip 5.JPEG"]);
        // Accents and emoji in the new name.
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &[false, false], "\u{c9}t\u{e9} \u{1f389} #", "", "").unwrap(), ["\u{c9}t\u{e9} \u{1f389} 1.jpg", "\u{c9}t\u{e9} \u{1f389} 2.jpg"]);
        // Find ignores capitals in accented letters too; a change of capitals only is a change.
        assert_eq!(new_names(&["\u{e9}t\u{e9}.jpg", "b.jpg"], &[false, false], "", "\u{c9}T\u{c9}", "Summer").unwrap(), ["Summer.jpg", "b.jpg"]);
        assert_eq!(new_names(&["a.txt", "b.txt"], &[false, false], "", "a", "A").unwrap(), ["A.txt", "b.txt"]);
    }

    /// A tab at C:\x listing these items (folders end with \), with `chosen` selected.
    fn app_with(items: &[&str], chosen: &[&str]) -> AppState {
        use crate::folder::{extension, Entry, EntryKind, Folder};
        let entry = |n: &&str| {
            let name = n.trim_end_matches('\\');
            let kind = if n.ends_with('\\') { EntryKind::Folder } else { EntryKind::File };
            Entry { name: name.into(), lower: name.to_lowercase(), kind, size: 1, modified: 0, created: 0, attributes: 0x20, ext: extension(name), type_name: "File".into(), target: None, free: 0 }
        };
        let mut a = AppState::for_tests(crate::settings::Settings::default());
        a.tabs[0].path = "C:\\x".into();
        super::super::apply_listing(&mut a.tabs[0], Folder { path: "C:\\x".into(), entries: items.iter().map(entry).collect(), ..Default::default() });
        let t = a.tab_mut();
        t.selected = t.folder.entries.iter().map(|e| chosen.contains(&e.name.as_str())).collect();
        a
    }

    /// A change as the form's list shows it.
    fn line(change: &(String, String)) -> String {
        format!("{}  \u{2192}  {}", change.0, change.1)
    }

    fn typed(new_name: &str, find: &str, replace: &str) -> Vec<String> {
        vec![new_name.into(), find.into(), replace.into()]
    }

    #[test]
    fn a_new_name_another_item_has_is_named_and_nothing_happens() {
        let mut a = app_with(&["holiday 2.JPG", "x.jpg", "y.jpg", "z.jpg"], &["x.jpg", "y.jpg", "z.jpg"]);
        a.ask_rename_many();
        assert_eq!(a.form.as_ref().map(|f| f.title.as_str()), Some("Rename 3 items"));
        let p = a.form_preview(&typed("Holiday #", "", ""));
        assert_eq!(p.problem.as_deref(), Some("There's already an item called \u{201c}Holiday 2.jpg\u{201d} here"), "in any capitals");
        assert!(!a.form_answer(Some(typed("Holiday #", "", ""))), "the form stays open: nothing is renamed");
        assert!(a.form.is_some() && a.history.next_undo().is_none());
        // One keeps its name while another would get it.
        assert!(a.form_preview(&typed("", "z", "y")).problem.unwrap().contains("both"));
        let p = a.form_preview(&typed("", "", ""));
        assert!(!p.ready && p.lines == ["Nothing changes yet"]);
    }

    #[test]
    fn names_handed_round_among_the_items_are_fine_and_the_preview_counts_the_rest() {
        let names: Vec<String> = (1..=10).map(|n| format!("{n}.png")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut a = app_with(&names, &names);
        // Largest first: 10 becomes 1 and 1 becomes 10 - names taken only by items renamed too.
        let t = a.tab_mut();
        t.descending = true;
        t.rebuild_view();
        a.ask_rename_many();
        let p = a.form_preview(&typed("#", "", ""));
        assert!(p.ready && p.problem.is_none(), "{:?}", p.problem);
        assert_eq!((p.list.len(), line(&p.list[0]).as_str(), line(&p.list[9]).as_str()), (10, "10.png  \u{2192}  1.png", "1.png  \u{2192}  10.png"));
        assert!(p.lines.is_empty(), "every name is in the list, none counted as \u{201c}more\u{201d}");
    }

    #[test]
    fn the_number_note_shows_only_while_a_number_is_really_added() {
        let mut a = app_with(&["a.jpg", "b.jpg", "c.jpg"], &["a.jpg", "b.jpg", "c.jpg"]);
        a.ask_rename_many();
        let note = |p: &Preview| p.lines.iter().any(|l| l == NUMBER_ADDED);
        // No #: numbers are added, and it says so.
        let p = a.form_preview(&typed("Photo", "", ""));
        assert!(note(&p));
        assert_eq!(line(&p.list[0]), "a.jpg  \u{2192}  Photo 1.jpg");
        assert!(note(&a.form_preview(&typed("  Photo  ", "", ""))));
        // With a #, nothing is added beyond what it asks for; empty (or only spaces) changes nothing.
        for typed_name in ["Photo #", "Photo ###", "#", "", "   "] {
            assert!(!note(&a.form_preview(&typed(typed_name, "", ""))), "{typed_name:?}");
        }
        // Only Find and Replace with: no numbers.
        let p = a.form_preview(&typed("", "a", "z"));
        assert!(!note(&p) && p.list == [("a.jpg".to_string(), "z.jpg".to_string())]);
        // It's true even while the name can't be used (it says why too).
        let p = a.form_preview(&typed("x?", "", ""));
        assert!(note(&p) && p.problem.is_some() && p.list.is_empty());
        // One item alone never gets a number (the function the form and the renaming share).
        assert!(!numbers_added("Photo", 1) && numbers_added("Photo", 2) && !numbers_added("Photo #", 2));
        assert_eq!(new_names(&["a.jpg"], &[false], "Photo", "", "").unwrap(), ["Photo.jpg"]);
    }

    #[test]
    fn the_forms_words_say_how_to_number_and_in_which_order_things_happen() {
        let mut a = app_with(&["a.jpg", "b.jpg"], &["a.jpg", "b.jpg"]);
        a.ask_rename_many();
        let f = a.form.as_ref().unwrap();
        let notes: Vec<&str> = f.fields.iter().map(|f| f.note).collect();
        assert_eq!(notes, ["Use # for sequential numbers, for example Photo # \u{2192} Photo 1, Photo 2.", "", "Find and Replace with are applied after New name."]);
        // ...which is what happens: Find looks in the new name (with its number), not the old one.
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &[false, false], "Photo #", "Photo", "Trip").unwrap(), ["Trip 1.jpg", "Trip 2.jpg"]);
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &[false, false], "Photo #", "a", "z").unwrap(), ["Photo 1.jpg", "Photo 2.jpg"], "the old names' a is gone");
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &[false, false], "Trip #", "#", "No. ").unwrap(), ["Trip 1.jpg", "Trip 2.jpg"], "the # is a number by then");
        assert_eq!(new_names(&["a.jpg", "b.jpg"], &[false, false], "Trip", "1", "one").unwrap(), ["Trip one.jpg", "Trip 2.jpg"], "the added number too");
    }

    #[test]
    fn find_and_replace_with_empty_boxes() {
        let two = [false, false];
        // Find empty: Replace with is ignored.
        assert_eq!(new_names(&["a.txt", "b.txt"], &two, "", "", "zzz").unwrap(), ["a.txt", "b.txt"]);
        assert_eq!(new_names(&["a.txt", "b.txt"], &two, "N", "", "zzz").unwrap(), ["N 1.txt", "N 2.txt"]);
        // Replace with empty: what's found is taken out.
        assert_eq!(new_names(&["ab.txt", "cb.txt"], &two, "", "B", "").unwrap(), ["a.txt", "c.txt"]);
        let mut a = app_with(&["a.txt", "b.txt"], &["a.txt", "b.txt"]);
        a.ask_rename_many();
        let p = a.form_preview(&typed("", "", "zzz"));
        assert!(!p.ready && p.list.is_empty() && p.lines == ["Nothing changes yet"]);
    }

    #[test]
    fn odd_names_are_numbered_and_listed_whole() {
        let names = ["\u{1f389} party #1.jpg", "\u{130}stanbul.png", "a\u{f1}o.txt", "#.md"];
        let mut a = app_with(&names, &names);
        a.ask_rename_many();
        let p = a.form_preview(&typed("\u{130}\u{f1} \u{1f389} #", "", ""));
        assert!(p.ready && p.problem.is_none(), "{:?}", p.problem);
        // (The list follows the order on screen: by name.)
        let wanted: Vec<String> = a.tab().view.iter().map(|&i| a.tab().folder.entries[i as usize].name.clone()).collect();
        assert_eq!(p.list.len(), 4);
        for (n, (pair, old)) in p.list.iter().zip(&wanted).enumerate() {
            let ext = old.rfind('.').map(|i| &old[i..]).unwrap_or("");
            assert_eq!(line(pair), format!("{old}  \u{2192}  \u{130}\u{f1} \u{1f389} {}{ext}", n + 1));
        }
        // A # in the old names is just a letter there.
        assert_eq!(new_names(&["#1.txt", "#2.txt"], &[false, false], "", "#", "No ").unwrap(), ["No 1.txt", "No 2.txt"]);
        // \u{130} (whose small letter is longer) is still found.
        assert_eq!(new_names(&["\u{130}x.txt", "y.txt"], &[false, false], "", "\u{130}", "I").unwrap(), ["Ix.txt", "y.txt"]);
    }

    #[test]
    fn very_long_names_are_listed_or_refused_with_why() {
        let long = format!("{}.txt", "n".repeat(240));
        let names = [long.as_str(), "b.txt"];
        let mut a = app_with(&names, &names);
        a.ask_rename_many();
        let p = a.form_preview(&typed("", "n", "m"));
        assert!(p.ready && p.problem.is_none(), "{:?}", p.problem);
        assert_eq!(p.list, [(long.clone(), format!("{}.txt", "m".repeat(240)))]);
        // 300 letters can't be a name: it says so, and the note about the number still shows.
        let p = a.form_preview(&typed(&"\u{1f389}".repeat(300), "", ""));
        assert!(p.problem.unwrap().contains("too long") && p.list.is_empty() && !p.ready);
        assert!(p.lines.iter().any(|l| l == NUMBER_ADDED));
    }

    #[test]
    fn five_thousand_items_are_all_in_the_preview_list() {
        let names: Vec<String> = (0..5000).map(|n| format!("IMG_{n:04}.jpg")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut a = app_with(&names, &names);
        a.ask_rename_many();
        let started = std::time::Instant::now();
        let p = a.form_preview(&typed("Trip #", "", ""));
        assert!(p.ready && p.problem.is_none(), "{:?}", p.problem);
        assert_eq!(p.list.len(), 5000);
        assert_eq!(line(&p.list[0]), "IMG_0000.jpg  \u{2192}  Trip 1.jpg");
        assert_eq!(line(&p.list[4999]), "IMG_4999.jpg  \u{2192}  Trip 5000.jpg");
        assert!(p.lines.is_empty() && !p.list.iter().any(|l| line(l).contains("more")));
        // It's worked out on each frame drawn while the form is open: it must stay quick.
        assert!(started.elapsed() < std::time::Duration::from_secs(1), "{:?}", started.elapsed());
        // Asking twice gives the same.
        assert_eq!(a.form_preview(&typed("Trip #", "", "")).list, p.list);
    }

    /// A fresh folder under the temp folder for a test's real files, removed when it ends.
    struct Scratch(String);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let dir = std::env::temp_dir().join(format!("glint-forms-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir.to_string_lossy().into_owned())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The app with its tab showing the real folder `dir`.
    fn app_in(dir: &str) -> AppState {
        let mut a = AppState::for_tests(crate::settings::Settings::default());
        a.hidden = true; // (no folder watch in a test)
        a.tabs[0].path = dir.to_string();
        super::super::apply_listing(&mut a.tabs[0], crate::folder::read(dir));
        a
    }

    #[test]
    fn renaming_many_takes_tabs_inside_along_and_ctrl_z_and_ctrl_y_work() {
        use crate::ui::Command;
        let s = Scratch::new("many");
        let dir = s.0.clone();
        for d in ["2023", "2024"] {
            std::fs::create_dir_all(join(&dir, d)).unwrap();
            std::fs::write(join(&join(&dir, d), "in.txt"), d).unwrap();
        }
        for f in ["b.JPG", "a.jpg", "notes.txt"] {
            std::fs::write(join(&dir, f), f).unwrap();
        }
        let mut a = app_in(&dir);
        // Another tab is inside one of the folders, with the other in its Back list.
        let mut inside = a.make_tab(join(&dir, "2023"));
        inside.back.push(join(&dir, "2024"));
        a.tabs.push(inside);
        let t = a.tab_mut();
        t.selected = t.folder.entries.iter().map(|e| e.name != "notes.txt").collect();
        a.ask_rename_many();
        assert!(a.form_answer(Some(typed("Trip #", "", ""))));
        finish_job(&mut a);
        let names = || {
            let mut n: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
            n.sort();
            n
        };
        // Numbered as shown: folders first, then the files by name.
        assert_eq!(names(), ["Trip 1", "Trip 2", "Trip 3.jpg", "Trip 4.JPG", "notes.txt"]);
        assert_eq!(std::fs::read_to_string(join(&dir, "Trip 1\\in.txt")).unwrap(), "2023");
        assert_eq!((a.tabs[1].path.clone(), a.tabs[1].back.clone()), (join(&dir, "Trip 1"), vec![join(&dir, "Trip 2")]), "the tab inside went along");
        assert_eq!(a.history.next_undo().map(|s| s.describe()).as_deref(), Some("renaming 4 items"));
        a.run(Command::Undo);
        finish_job(&mut a);
        assert_eq!(a.note(), Some("Undid renaming 4 items \u{b7} Ctrl+Y to redo."));
        assert_eq!(names(), ["2023", "2024", "a.jpg", "b.JPG", "notes.txt"]);
        assert_eq!((a.tabs[1].path.clone(), a.tabs[1].back.clone()), (join(&dir, "2023"), vec![join(&dir, "2024")]));
        a.run(Command::Redo);
        finish_job(&mut a);
        assert_eq!(names(), ["Trip 1", "Trip 2", "Trip 3.jpg", "Trip 4.JPG", "notes.txt"]);
        assert_eq!(a.tabs[1].path, join(&dir, "Trip 1"));
    }

    #[test]
    fn folders_that_swap_names_take_each_tab_to_its_own_folder() {
        let s = Scratch::new("swap");
        for d in ["Day 1", "Day 2"] {
            std::fs::create_dir_all(join(&s.0, d)).unwrap();
            std::fs::write(join(&join(&s.0, d), "in.txt"), d).unwrap();
        }
        let mut a = app_in(&s.0);
        for d in ["Day 1", "Day 2"] {
            let t = a.make_tab(join(&s.0, d));
            a.tabs.push(t);
        }
        // Largest first: "Day 2" is shown first, so it becomes Day 1, and Day 1 becomes Day 2.
        let t = a.tab_mut();
        t.descending = true;
        t.rebuild_view();
        t.selected = vec![true; t.folder.entries.len()];
        a.ask_rename_many();
        assert!(a.form_answer(Some(typed("Day #", "", ""))));
        finish_job(&mut a);
        assert_eq!(std::fs::read_to_string(join(&s.0, "Day 2\\in.txt")).unwrap(), "Day 1");
        assert_eq!(a.tabs[1].path, join(&s.0, "Day 2"), "the tab in Day 1 stays in that folder, now called Day 2");
        assert_eq!(a.tabs[2].path, join(&s.0, "Day 1"));
    }

    /// Waits for the file job just started, and takes in what it did as the window does - but
    /// without the folders to read again: that reads every drive again too, network drives as well.
    fn finish_job(a: &mut AppState) {
        use super::super::Loaded;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match a.rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
                Ok(Loaded::FileOp { after, outcome }) => return a.after_file_op(after, crate::fileops::Outcome { changed: Vec::new(), ..outcome }),
                Ok(_) => {}
                Err(_) => panic!("the file job never finished"),
            }
        }
    }

    #[test]
    fn compress_asks_type_password_and_parts_in_one_box() {
        let mut a = app_with(&["a.txt"], &["a.txt"]);
        a.ask_pack();
        let form = a.form.as_ref().expect("the form opened");
        assert_eq!(form.title, "Compress");
        assert_eq!(form.slider.as_ref().map(|s| s.label), Some("Split into"));
        assert_eq!(form.fields.len(), 1, "one password box");
        assert!(form.fields[0].secret, "dots at first");
        let s = form.slider.as_ref().expect("a Parts slider");
        assert_eq!((s.min, s.max, s.start), (1, 10, 1));
        // Values: password, type ("0" zip, "1" 7z), parts. Empty password is fine (none).
        let v = |pw: &str, side: &str, parts: &str| vec![pw.to_string(), side.to_string(), parts.to_string()];
        for (pw, side) in [("", "0"), ("", "1"), ("cat123", "0"), ("cat123", "1")] {
            let p = a.form_preview(&v(pw, side, "1"));
            assert!(p.ready && p.problem.is_none(), "{pw} {side}");
        }
        // By size: a chip, or your own MB.
        let z = |chip: &str, own: &str| vec!["".to_string(), "0".to_string(), "1".to_string(), "1".to_string(), chip.to_string(), own.to_string()];
        assert_eq!(a.form_preview(&z("3", "")).lines[0], "a.zip.001, .002\u{2026} each up to 4 GB");
        assert!(a.form_preview(&z("3", "")).ready);
        let empty = a.form_preview(&z("4", ""));
        assert!(!empty.ready && empty.problem.is_none(), "an empty own box says nothing yet");
        assert_eq!(a.form_preview(&z("4", "50")).lines[0], "a.zip.001, .002\u{2026} each up to 50 MB");
        assert!(a.form_preview(&z("4", "abc")).problem.is_some());
        assert!(a.form_preview(&z("4", "0")).problem.is_some());
        assert_eq!(pack_split("5", "1", "0", ""), Ok(crate::fileops::Split::Size(25 * MB)));
        assert_eq!(pack_split("5", "0", "0", ""), Ok(crate::fileops::Split::Files(5)));
        assert_eq!(a.form_preview(&v("", "1", "1")).lines[0], "a.7z");
        assert_eq!(a.form_preview(&v("", "0", "1")).lines[0], "a.zip");
        assert_eq!(a.form_preview(&v("", "1", "3")).lines[0], "a.7z.001 to a.7z.003");
        assert_eq!(pack_parts("0"), 1);
        assert_eq!(pack_parts("99"), 10);
        assert_eq!(pack_parts(""), 1);
    }
}
