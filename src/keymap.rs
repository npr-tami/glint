//! The keyboard shortcuts: which keys run what, the ones a person changed in Settings (kept in
//! settings.json as `"Shortcuts": {"NewTab": "Ctrl+K"}`, only those that differ from the default),
//! the keys that can't be used, and how a shortcut is written ("Ctrl+Shift+T").
//!
//! The main window (`window.rs`) and the file list (`list.rs`) ask [`KeyMap::find`] what a key press
//! does; the Settings window lists [`ALL`] on its Keyboard shortcuts page and changes them; menus
//! and the command box show the shortcut as it is now ([`swap_label`], [`swap_keys`]).
//!
//! A few things stay fixed and are handled where they always were: Ctrl+1 to 9 for the tabs,
//! Ctrl+Shift+1 to 6 for the views, Backspace (it has its own setting), Enter, Esc, the arrows and
//! the right-click menu keys. A shortcut can also have no keys: right-click on its button in
//! Settings removes them (saved as "").

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;

use crate::ui::Mods;

/// A key with Ctrl, Shift and Alt held or not. `vk` is Windows' virtual-key code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Chord {
    pub vk: u16,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Which held keys a default shortcut doesn't mind (today's rules: F5 refreshes with anything
/// held, Ctrl+Shift+W closes a tab too, AltGr+Left still goes back).
const LOOSE_SHIFT: u8 = 1;
const LOOSE_CTRL: u8 = 2;
const LOOSE_ALT: u8 = 4;

// The virtual-key codes used by name here.
const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_DOWN: u16 = 0x28;
const VK_INSERT: u16 = 0x2D;
const VK_DELETE: u16 = 0x2E;
const VK_LWIN: u16 = 0x5B;
const VK_RWIN: u16 = 0x5C;
const VK_F1: u16 = 0x70;
const VK_F4: u16 = 0x73;
const VK_F10: u16 = 0x79;
const VK_F24: u16 = 0x87;
const VK_OEM_5: u16 = 0xDC;

/// Keys with a name of their own, as settings.json writes them (letters, digits, F1 to F24 and the
/// number pad's digits are worked out). The punctuation keys are written as an American keyboard
/// shows them; on screen they show as the person's keyboard does (see [`Chord::text`]).
const NAMED: &[(u16, &str)] = &[
    (VK_BACK, "Backspace"),
    (VK_TAB, "Tab"),
    (VK_RETURN, "Enter"),
    (0x13, "Pause"),
    (VK_ESCAPE, "Esc"),
    (VK_SPACE, "Space"),
    (VK_PRIOR, "Page Up"),
    (0x22, "Page Down"),
    (0x23, "End"),
    (0x24, "Home"),
    (0x25, "Left"),
    (0x26, "Up"),
    (0x27, "Right"),
    (VK_DOWN, "Down"),
    (VK_INSERT, "Insert"),
    (VK_DELETE, "Delete"),
    (0x6A, "Num*"),
    (0x6B, "Num+"),
    (0x6D, "Num-"),
    (0x6E, "Num."),
    (0x6F, "Num/"),
    (0xBA, ";"),
    (0xBB, "="),
    (0xBC, ","),
    (0xBD, "-"),
    (0xBE, "."),
    (0xBF, "/"),
    (0xC0, "`"),
    (0xDB, "["),
    (VK_OEM_5, "\\"),
    (0xDD, "]"),
    (0xDE, "'"),
    (0xDF, "Oem8"),
    (0xE2, "Oem102"),
];

/// Other ways of writing a key that are read too (menus say "Del").
const ALIASES: &[(&str, u16)] = &[("del", VK_DELETE), ("ins", VK_INSERT), ("escape", VK_ESCAPE), ("return", VK_RETURN), ("pgup", VK_PRIOR), ("pgdn", 0x22)];

/// A key's name as settings.json writes it; None for a key without one (media keys, Print Screen,
/// the modifier keys themselves), which can't be a shortcut.
fn key_name(vk: u16) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(vk as u8).to_string()),
        VK_F1..=VK_F24 => Some(format!("F{}", vk - VK_F1 + 1)),
        0x60..=0x69 => Some(format!("Num{}", vk - 0x60)),
        _ => NAMED.iter().find(|(k, _)| *k == vk).map(|(_, n)| n.to_string()),
    }
}

/// The key a name stands for (any case, spaces ignored).
fn key_of(name: &str) -> Option<u16> {
    let squeezed: String = name.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    if squeezed.is_empty() {
        return None;
    }
    if let Some((_, vk)) = ALIASES.iter().find(|(a, _)| *a == squeezed) {
        return Some(*vk);
    }
    let b = squeezed.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
        return Some(u16::from(b[0].to_ascii_uppercase()));
    }
    if let Some(n) = squeezed.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(VK_F1 + n - 1);
    }
    if let Some(n) = squeezed.strip_prefix("num").and_then(|n| n.parse::<u16>().ok()) {
        return (n <= 9).then_some(0x60 + n);
    }
    NAMED.iter().find(|(_, n)| n.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase() == squeezed).map(|(k, _)| *k)
}

impl Chord {
    pub const fn new(vk: u16, ctrl: bool, shift: bool, alt: bool) -> Chord {
        Chord { vk, ctrl, shift, alt }
    }

    /// The key pressed with what's held now.
    pub fn of(vk: u16, m: Mods) -> Chord {
        Chord { vk, ctrl: m.ctrl, shift: m.shift, alt: m.alt }
    }

    /// Reads "Ctrl+Shift+T" (any case; "Del" for Delete). None for anything else.
    pub fn parse(text: &str) -> Option<Chord> {
        let mut rest = text.trim();
        let mut c = Chord::default();
        // The modifiers come first, each once, ending in "+"; what's left is the key (which may be
        // "Num+" itself).
        loop {
            let lower = rest.to_ascii_lowercase();
            let held = if lower.starts_with("ctrl+") {
                &mut c.ctrl
            } else if lower.starts_with("shift+") {
                &mut c.shift
            } else if lower.starts_with("alt+") {
                &mut c.alt
            } else {
                break;
            };
            if *held {
                return None;
            }
            *held = true;
            rest = &rest[lower.find('+').map_or(0, |i| i + 1)..];
        }
        c.vk = key_of(rest)?;
        Some(c)
    }

    /// How settings.json writes it: "Ctrl+Shift+T", the same on every keyboard. None for a key
    /// without a name.
    pub fn saved(&self) -> Option<String> {
        Some(self.with_key(&key_name(self.vk)?))
    }

    /// How it shows on screen: as [`Chord::saved`], except that a punctuation key shows as the
    /// person's keyboard has it (Ctrl+Ø on a Norwegian one, where an American one has Ctrl+`).
    pub fn text(&self) -> String {
        self.text_with(layout_char)
    }

    /// [`Chord::text`] with `typed` telling what a key types on this keyboard.
    fn text_with(&self, typed: impl Fn(u16) -> Option<char>) -> String {
        let punctuation = (0xBA..=0xE2).contains(&self.vk);
        let key = match (punctuation, typed(self.vk)) {
            (true, Some(ch)) if !ch.is_control() && !ch.is_whitespace() => ch.to_uppercase().collect(),
            _ => key_name(self.vk).unwrap_or_else(|| format!("Key {}", self.vk)),
        };
        self.with_key(&key)
    }

    fn with_key(&self, key: &str) -> String {
        let mut s = String::new();
        if self.ctrl {
            s.push_str("Ctrl+");
        }
        if self.alt {
            s.push_str("Alt+");
        }
        if self.shift {
            s.push_str("Shift+");
        }
        s.push_str(key);
        s
    }

    /// Whether `pressed` is this, allowing the held keys in `loose` to be down too.
    fn takes(&self, pressed: Chord, loose: u8) -> bool {
        let fits = |want: bool, got: bool, bit: u8| want == got || (!want && loose & bit != 0);
        self.vk == pressed.vk && fits(self.ctrl, pressed.ctrl, LOOSE_CTRL) && fits(self.shift, pressed.shift, LOOSE_SHIFT) && fits(self.alt, pressed.alt, LOOSE_ALT)
    }
}

/// What a key types on the keyboard in use (Windows' MapVirtualKeyW), for the punctuation keys'
/// names. None where Windows can't tell.
fn layout_char(vk: u16) -> Option<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_CHAR};
    // SAFETY: a plain keyboard-layout query.
    let got = unsafe { MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_CHAR) };
    // (The top bit marks a dead key - an accent waiting for its letter; the character is still it.)
    char::from_u32(got & 0xFFFF).filter(|c| *c != '\0')
}

/// Everything a shortcut can do. Their names in settings.json, what Settings calls them and their
/// defaults are in [`ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    NewTab,
    CloseTab,
    ReopenClosedTab,
    NextTab,
    PreviousTab,
    NewWindow,
    SplitView,
    SwitchPane,
    TypePath,
    SearchBar,
    SearchSubfolders,
    Back,
    Forward,
    Up,
    Refresh,
    PinToSidebar,
    OpenInTerminal,
    DiskSpace,
    OpenInNewTab,
    Rename,
    Copy,
    Cut,
    Paste,
    CopyPath,
    Delete,
    DeletePermanently,
    NewFolder,
    Undo,
    Redo,
    SelectAll,
    Properties,
    Preview,
    Sidebar,
    DetailsPanel,
    HiddenFiles,
    LightDarkMode,
    CommandBox,
    Settings,
    ExitGlint,
}

impl Action {
    /// The file list's own (list.rs); the rest are the window's.
    pub fn in_list(self) -> bool {
        use Action as A;
        matches!(self, A::OpenInNewTab | A::Rename | A::Copy | A::Cut | A::Paste | A::CopyPath | A::Delete | A::DeletePermanently | A::NewFolder | A::Undo | A::Redo | A::SelectAll)
    }

    pub fn spec(self) -> &'static Spec {
        ALL.iter().find(|s| s.action == self).unwrap_or(&ALL[0])
    }
}

/// What a shortcut is about, for the order the Keyboard shortcuts page lists them in (one box,
/// Glint's own first).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Tabs,
    Moving,
    Files,
    View,
    Glint,
}

impl Group {
    pub const ALL: [Group; 5] = [Group::Glint, Group::Tabs, Group::Moving, Group::Files, Group::View];
}

/// One shortcut that can be changed.
#[derive(Debug)]
pub struct Spec {
    pub action: Action,
    /// Its name in settings.json ("Shortcuts": {"NewTab": "Ctrl+K"}).
    pub name: &'static str,
    /// What the Keyboard shortcuts page calls it.
    pub label: &'static str,
    pub group: Group,
    /// The shortcut it has until it's changed, as it has always been written.
    pub default: &'static str,
    /// The held keys the default doesn't mind (see LOOSE_SHIFT).
    loose: u8,
    /// Its row's key for the Settings window (`settingswin::pages::LIVE`).
    pub live: &'static str,
}

const fn spec(action: Action, name: &'static str, label: &'static str, group: Group, default: &'static str, loose: u8) -> Spec {
    Spec { action, name, label, group, default, loose, live: "@Keys" }
}

const fn spec_live(action: Action, name: &'static str, label: &'static str, group: Group, default: &'static str, live: &'static str) -> Spec {
    Spec { action, name, label, group, default, loose: 0, live }
}

/// Every shortcut that can be changed, in the page's order. The defaults are what Glint has always
/// done.
pub const ALL: &[Spec] = {
    use Action as A;
    use Group as G;
    &[
        spec(A::NewTab, "NewTab", "New tab", G::Tabs, "Ctrl+T", LOOSE_SHIFT),
        spec(A::CloseTab, "CloseTab", "Close tab", G::Tabs, "Ctrl+W", LOOSE_SHIFT),
        spec(A::ReopenClosedTab, "ReopenClosedTab", "Reopen closed tab", G::Tabs, "Ctrl+Shift+T", 0),
        spec(A::NextTab, "NextTab", "Next tab", G::Tabs, "Ctrl+Tab", 0),
        spec(A::PreviousTab, "PreviousTab", "Previous tab", G::Tabs, "Ctrl+Shift+Tab", 0),
        spec(A::NewWindow, "NewWindow", "New window", G::Tabs, "Ctrl+N", 0),
        spec(A::SplitView, "SplitView", "Split view", G::Tabs, "Ctrl+\\", 0),
        spec(A::SwitchPane, "SwitchPane", "Switch pane", G::Tabs, "F6", LOOSE_SHIFT),
        spec(A::TypePath, "TypePath", "Type a path", G::Moving, "Ctrl+L", LOOSE_SHIFT),
        spec(A::SearchBar, "SearchBar", "Search bar", G::Moving, "Ctrl+F", LOOSE_SHIFT),
        spec_live(A::SearchSubfolders, "SearchSubfolders", "Search subfolders", G::Moving, "Ctrl+Shift+F", "@Search"),
        spec(A::Back, "Back", "Back", G::Moving, "Alt+Left", LOOSE_SHIFT | LOOSE_CTRL),
        spec(A::Forward, "Forward", "Forward", G::Moving, "Alt+Right", LOOSE_SHIFT | LOOSE_CTRL),
        spec(A::Up, "Up", "Go up", G::Moving, "Alt+Up", LOOSE_SHIFT | LOOSE_CTRL),
        spec(A::Refresh, "Refresh", "Refresh", G::Moving, "F5", LOOSE_SHIFT | LOOSE_CTRL | LOOSE_ALT),
        spec_live(A::PinToSidebar, "PinToSidebar", "Pin to sidebar", G::Moving, "Ctrl+D", "@Pins"),
        spec(A::OpenInTerminal, "OpenInTerminal", "Open in terminal", G::Moving, "Ctrl+`", LOOSE_SHIFT),
        spec_live(A::DiskSpace, "DiskSpace", "Disk space", G::Moving, "Ctrl+Shift+U", "@DiskUsage"),
        spec(A::OpenInNewTab, "OpenInNewTab", "Open in new tab", G::Files, "Ctrl+Enter", LOOSE_SHIFT),
        spec(A::Rename, "Rename", "Rename", G::Files, "F2", 0),
        spec(A::Copy, "Copy", "Copy", G::Files, "Ctrl+C", 0),
        spec(A::Cut, "Cut", "Cut", G::Files, "Ctrl+X", 0),
        spec(A::Paste, "Paste", "Paste", G::Files, "Ctrl+V", 0),
        spec(A::CopyPath, "CopyPath", "Copy path", G::Files, "Ctrl+Shift+C", 0),
        spec(A::Delete, "Delete", "Delete", G::Files, "Delete", 0),
        spec(A::DeletePermanently, "DeletePermanently", "Delete permanently", G::Files, "Shift+Delete", 0),
        spec(A::NewFolder, "NewFolder", "New folder", G::Files, "Ctrl+Shift+N", 0),
        spec(A::Undo, "Undo", "Undo", G::Files, "Ctrl+Z", 0),
        spec(A::Redo, "Redo", "Redo", G::Files, "Ctrl+Y", 0),
        spec(A::SelectAll, "SelectAll", "Select all", G::Files, "Ctrl+A", LOOSE_SHIFT),
        spec(A::Properties, "Properties", "Properties", G::Files, "Alt+Enter", LOOSE_SHIFT | LOOSE_CTRL),
        spec_live(A::Preview, "Preview", "Preview", G::Files, "Space", "@Preview"),
        spec(A::Sidebar, "Sidebar", "Show or hide sidebar", G::View, "Ctrl+B", 0),
        spec_live(A::DetailsPanel, "DetailsPanel", "Show or hide details panel", G::View, "Alt+P", "ShowDetails"),
        spec(A::HiddenFiles, "HiddenFiles", "Show or hide hidden files", G::View, "Ctrl+H", 0),
        spec(A::LightDarkMode, "LightDarkMode", "Light/dark mode", G::View, "Ctrl+Shift+L", 0),
        spec(A::CommandBox, "CommandBox", "Command box", G::Glint, "Ctrl+U", 0),
        spec(A::Settings, "Settings", "Settings", G::Glint, "Ctrl+,", 0),
        spec(A::ExitGlint, "ExitGlint", "Exit Glint", G::Glint, "Ctrl+Q", 0),
    ]
};

/// Second ways to do something, built in: they keep working whatever the main shortcut is, unless
/// the person gives that key to something else. (With the held keys each doesn't mind.) None of
/// them is a shortcut's own default: a shortcut that is changed leaves its old key free.
const EXTRAS: &[(&str, Action, u8)] = &[
    ("F4", Action::TypePath, LOOSE_SHIFT | LOOSE_CTRL),
    ("Alt+D", Action::TypePath, LOOSE_SHIFT),
    ("F3", Action::SearchBar, LOOSE_SHIFT),
    ("Ctrl+R", Action::Refresh, LOOSE_SHIFT),
    // The old copy and paste keys, which Explorer still knows.
    ("Ctrl+Insert", Action::Copy, 0),
    ("Shift+Insert", Action::Paste, 0),
    ("Ctrl+Shift+Z", Action::Redo, 0),
];

/// The shortcuts as they are now: one key each at most, never the same key for two.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyMap {
    keys: Vec<Option<Chord>>,
}

impl KeyMap {
    /// Every shortcut at its default.
    pub fn defaults() -> KeyMap {
        KeyMap::of(&BTreeMap::new())
    }

    /// From the changed shortcuts as settings.json keeps them. A name Glint doesn't know, or a key
    /// it can't read or use, is left out (that shortcut keeps its default). If two end up on the
    /// same key (the file was edited by hand), a changed one wins over a default one, and between
    /// two changed ones the first on the Keyboard shortcuts page wins; the other gets its default
    /// back, or no shortcut if its default is taken too. One saved as "" was removed (right-click
    /// on its button): it has none.
    pub fn of(saved: &BTreeMap<String, String>) -> KeyMap {
        let mut keys: Vec<Option<Chord>> = vec![None; ALL.len()];
        let taken = |keys: &[Option<Chord>], c: Chord| keys.iter().any(|k| *k == Some(c));
        for (i, s) in ALL.iter().enumerate() {
            let Some(c) = saved.get(s.name).and_then(|t| Chord::parse(t)) else { continue };
            if problem(c).is_none() && !taken(&keys, c) {
                keys[i] = Some(c);
            }
        }
        for (i, s) in ALL.iter().enumerate() {
            if keys[i].is_none() && !saved.get(s.name).is_some_and(String::is_empty) {
                keys[i] = default_of(s).filter(|c| !taken(&keys, *c));
            }
        }
        KeyMap { keys }
    }

    /// The shortcuts as they are in these settings.
    pub fn of_settings(s: &crate::settings::Settings) -> KeyMap {
        KeyMap::of(&s.shortcuts)
    }

    /// What it is now; None when it has none (its key went to another, in a hand-edited file).
    pub fn chord(&self, a: Action) -> Option<Chord> {
        ALL.iter().position(|s| s.action == a).and_then(|i| self.keys[i])
    }

    /// Whether it isn't on its default key.
    pub fn changed(&self, a: Action) -> bool {
        self.chord(a) != default_of(a.spec())
    }

    /// How the shortcut shows on screen: its default as it has always been written, a changed one
    /// as the person's keyboard names it, "" for none.
    pub fn text(&self, a: Action) -> String {
        match self.chord(a) {
            None => String::new(),
            Some(_) if !self.changed(a) => a.spec().default.to_string(),
            Some(c) => c.text(),
        }
    }

    /// The action with this key now, if any.
    pub fn owner(&self, c: Chord) -> Option<Action> {
        self.keys.iter().position(|k| *k == Some(c)).map(|i| ALL[i].action)
    }

    /// What a key press does. Its exact key first; then the built-in second ways whose key nobody
    /// else has; then the defaults that don't mind an extra key held (F5 with Ctrl).
    pub fn find(&self, pressed: Chord) -> Option<Action> {
        if let Some(a) = self.owner(pressed) {
            return Some(a);
        }
        let extras = || EXTRAS.iter().filter_map(|(t, a, loose)| Chord::parse(t).filter(|c| self.owner(*c).is_none()).map(|c| (c, *a, *loose)));
        if let Some((_, a, _)) = extras().find(|(c, _, _)| *c == pressed) {
            return Some(a);
        }
        for (i, s) in ALL.iter().enumerate() {
            if let Some(c) = self.keys[i].filter(|c| s.loose != 0 && Some(*c) == default_of(s)) {
                if c.takes(pressed, s.loose) {
                    return Some(s.action);
                }
            }
        }
        extras().find(|(c, _, loose)| c.takes(pressed, *loose)).map(|(_, a, _)| a)
    }

    /// What a key press for the Settings window to wait for means for `a`: Ok(the new saved
    /// shortcuts) or Err(the one line saying why not). Pressing its own key again changes nothing.
    pub fn change(&self, saved: &BTreeMap<String, String>, a: Action, c: Chord) -> Result<BTreeMap<String, String>, String> {
        // Space on its own is only ever the preview's (it types a space into a name otherwise).
        let space = c.vk == VK_SPACE && !c.ctrl && !c.alt && !c.shift;
        if let Some(p) = problem(c).filter(|_| !(space && a == Action::Preview)) {
            return Err(match self.owner(c) {
                Some(other) if space => format!("{} is already used by {}.", c.text(), other.spec().label),
                _ => p.say(c),
            });
        }
        match self.owner(c) {
            Some(other) if other != a => return Err(format!("{} is already used by {}.", c.text(), other.spec().label)),
            _ => {}
        }
        let mut out = saved.clone();
        let spec = a.spec();
        if Some(c) == default_of(spec) {
            out.remove(spec.name);
        } else if let Some(text) = c.saved() {
            out.insert(spec.name.to_string(), text);
        }
        Ok(tidy(out))
    }

    /// `a` without keys (right-click on its button): the new saved shortcuts. RESET gives its
    /// default back.
    pub fn remove(&self, saved: &BTreeMap<String, String>, a: Action) -> BTreeMap<String, String> {
        let mut out = saved.clone();
        out.insert(a.spec().name.to_string(), String::new());
        tidy(out)
    }
}

/// Only what's in use stays (a hand-edited entry that lost is dropped once one is changed).
fn tidy(mut out: BTreeMap<String, String>) -> BTreeMap<String, String> {
    let now = KeyMap::of(&out);
    out.retain(|name, _| ALL.iter().find(|s| s.name == name).is_some_and(|s| now.changed(s.action)));
    out
}

/// What a key pressed while a shortcut waits for its new keys means.
#[derive(Clone, Debug, PartialEq)]
pub enum Pressed {
    /// Ctrl, Shift or Alt on its own (or a lock key): it goes on waiting for the key itself.
    Wait,
    /// Esc on its own: the waiting stops and nothing changes.
    Stop,
    /// These keys are the shortcut now: the changed shortcuts to save.
    Set(BTreeMap<String, String>),
    /// They can't be: the one line saying why (it goes on waiting).
    Refused(String),
}

/// Decides what the key `c`, pressed while `a` waits for its new keys, means (`saved`: the changed
/// shortcuts as they are now).
pub fn pressed_while_waiting(saved: &BTreeMap<String, String>, a: Action, c: Chord) -> Pressed {
    // Shift, Ctrl, Alt (either side) and the three lock keys.
    if matches!(c.vk, 0x10..=0x12 | 0x14 | 0x90 | 0x91 | 0xA0..=0xA5) {
        return Pressed::Wait;
    }
    if c.vk == VK_ESCAPE && !c.ctrl && !c.shift && !c.alt {
        return Pressed::Stop;
    }
    match KeyMap::of(saved).change(saved, a, c) {
        Ok(new) => Pressed::Set(new),
        Err(why) => Pressed::Refused(why),
    }
}

/// A default's key.
fn default_of(s: &Spec) -> Option<Chord> {
    Chord::parse(s.default)
}

/// Why a key can't be a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Windows keeps it (Alt+F4, Alt+Tab, the Windows key...).
    Windows,
    /// Ctrl+Alt: AltGr on many keyboards.
    AltGr,
    /// Without Ctrl or Alt it's for typing or moving (letters, digits, Enter, Tab, the arrows).
    Typing,
    /// A key with no name (a media key, Print Screen).
    NoName,
    /// Glint uses it for something that can't be changed.
    Fixed(&'static str),
}

impl Problem {
    /// The one line Settings shows.
    pub fn say(self, c: Chord) -> String {
        let keys = c.text();
        match self {
            Problem::Windows if c.vk == VK_LWIN || c.vk == VK_RWIN => "The Windows key belongs to Windows, so Glint can't use it.".to_string(),
            Problem::Windows => format!("{keys} belongs to Windows, so Glint can't use it."),
            Problem::AltGr => format!("{keys} can't be used: many keyboards type characters with Ctrl+Alt."),
            Problem::Typing => format!("{keys} is needed for typing and moving around, so add Ctrl or Alt."),
            Problem::NoName => "That key can't be used for a shortcut.".to_string(),
            Problem::Fixed(what) => format!("{keys} is already used by {what}."),
        }
    }
}

/// Why `c` can't be a shortcut; None if it can.
pub fn problem(c: Chord) -> Option<Problem> {
    let vk = c.vk;
    let windows = vk == VK_LWIN
        || vk == VK_RWIN
        || (c.alt && !c.ctrl && matches!(vk, VK_F4 | VK_SPACE | VK_TAB | VK_ESCAPE))
        || (c.ctrl && !c.alt && vk == VK_ESCAPE)
        || (c.ctrl && c.alt && vk == VK_DELETE);
    if windows {
        return Some(Problem::Windows);
    }
    if key_name(vk).is_none() {
        return Some(Problem::NoName);
    }
    if c.ctrl && c.alt {
        return Some(Problem::AltGr);
    }
    // What the list and the window keep for themselves. (The arrows, Home, End and the Page keys
    // move around the list with Ctrl and Shift too: said before the line about typing, whose advice
    // to add Ctrl wouldn't help with them.)
    if (VK_PRIOR..=VK_DOWN).contains(&vk) && !c.alt {
        return Some(Problem::Fixed("Moving around the list"));
    }
    let function = (VK_F1..=VK_F24).contains(&vk);
    if !c.ctrl && !c.alt && !function && vk != VK_DELETE && vk != VK_INSERT {
        return Some(Problem::Typing);
    }
    let digit = (0x31..=0x39).contains(&vk);
    if c.ctrl && !c.alt && digit {
        return Some(Problem::Fixed(fixed_tab_or_view(c)));
    }
    if c.ctrl && !c.alt && !c.shift && vk == VK_SPACE {
        return Some(Problem::Fixed("Select or unselect an item"));
    }
    if c.shift && !c.ctrl && !c.alt && vk == VK_F10 {
        return Some(Problem::Fixed("Full right-click menu"));
    }
    None
}

/// What Ctrl+digit does: the views with Shift (1 to 6), otherwise the tabs.
fn fixed_tab_or_view(c: Chord) -> &'static str {
    const TABS: [&str; 9] = ["Go to tab 1", "Go to tab 2", "Go to tab 3", "Go to tab 4", "Go to tab 5", "Go to tab 6", "Go to tab 7", "Go to tab 8", "Go to tab 9"];
    let n = usize::from(c.vk - 0x31);
    if c.shift && n < 6 {
        "Change view"
    } else {
        TABS[n.min(8)]
    }
}

/// Reads settings.json's "Shortcuts", keeping each entry that is text (anything else in it, or a
/// "Shortcuts" that isn't a list of names, is left out: those shortcuts keep their defaults).
pub fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Object(m) => m.into_iter().filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string()))).collect(),
        _ => BTreeMap::new(),
    })
}

/// A menu item's words with the shortcut after its tab as it is now: "Close tab\tCtrl+W" becomes
/// "Close tab\tCtrl+K" once Close tab is on Ctrl+K. None when it stays as it is (no shortcut, one
/// that isn't changed, or one that isn't a changeable shortcut's default).
pub fn swap_label(label: &str, map: &KeyMap) -> Option<String> {
    let (words, keys) = label.split_once('\t')?;
    let now = current_for(keys, map)?;
    Some(if now.is_empty() { words.to_string() } else { format!("{words}\t{now}") })
}

/// The command box's shortcut text as it is now (see [`swap_label`]).
pub fn swap_keys(keys: &'static str, map: &KeyMap) -> &'static str {
    current_for(keys, map).map_or(keys, |now| interned(&now))
}

/// The shortcut that has taken the place of `keys`, a default shortcut, if it changed.
fn current_for(keys: &str, map: &KeyMap) -> Option<String> {
    let c = Chord::parse(keys)?;
    let spec = ALL.iter().find(|s| default_of(s) == Some(c))?;
    map.changed(spec.action).then(|| map.text(spec.action))
}

/// Swaps the shortcuts in a menu's items (and their submenus') for the ones in use now.
pub fn swap_menu(items: &mut [crate::ui::MenuItem], map: &KeyMap) {
    for item in items {
        if let Some(label) = swap_label(&item.label, map) {
            item.label = label;
        }
        swap_menu(&mut item.submenu, map);
    }
}

/// The shortcuts this Glint has now, for the messages that name one but have no settings at hand
/// (a folder's "Only hidden items are here" line, what a file job says). None until the window has
/// set them: the defaults.
static NOW: Mutex<Option<KeyMap>> = Mutex::new(None);

/// Keeps `saved` (the changed shortcuts) as the ones in use: at start-up and when Settings changes
/// them. (Not while testing: tests run side by side and would see each other's.)
pub fn remember(saved: &BTreeMap<String, String>) {
    if cfg!(test) {
        return;
    }
    *NOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(KeyMap::of(saved));
}

/// How `a`'s shortcut shows now ("" for none), from the shortcuts [`remember`] kept.
pub fn keys_now(a: Action) -> String {
    let now = NOW.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    now.as_ref().map_or_else(|| KeyMap::defaults().text(a), |m| m.text(a))
}

/// A message naming a shortcut: `with` with "{keys}" swapped for its keys now, or `without` when
/// it has none.
pub fn say_with(keys: &str, with: &str, without: &str) -> String {
    if keys.is_empty() {
        without.to_string()
    } else {
        with.replace("{keys}", keys)
    }
}

/// A shortcut's text as the `&'static str` the command box's rows hold. Kept once each for the
/// rest of the run: a handful of short strings.
fn interned(text: &str) -> &'static str {
    static KEPT: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(s) = kept.iter().find(|s| **s == text) {
        return s;
    }
    let s: &'static str = Box::leak(text.to_string().into_boxed_str());
    kept.push(s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(text: &str) -> Chord {
        Chord::parse(text).unwrap_or_else(|| panic!("{text} doesn't read"))
    }

    fn saved(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn m(ctrl: bool, shift: bool, alt: bool) -> Mods {
        Mods { ctrl, shift, alt }
    }

    /// Messages name the keys in use, or say it another way when there are none.
    #[test]
    fn messages_name_the_keys_in_use() {
        assert_eq!(say_with("F9", "Rename it ({keys}) first.", "Rename it first."), "Rename it (F9) first.");
        assert_eq!(say_with("", "Rename it ({keys}) first.", "Rename it first."), "Rename it first.");
        // While testing nothing is kept (tests run side by side): always the defaults.
        remember(&saved(&[("Rename", "F9")]));
        assert_eq!(keys_now(Action::Rename), "F2");
        assert_eq!(keys_now(Action::HiddenFiles), "Ctrl+H");
    }

    /// The defaults do what the keys always did, key by key (the old if-chains' rules).
    #[test]
    fn defaults_are_the_keys_glint_always_had() {
        let k = KeyMap::defaults();
        let cases: &[(&str, Action)] = &[
            ("Ctrl+T", Action::NewTab),
            ("Ctrl+W", Action::CloseTab),
            ("Ctrl+Shift+W", Action::CloseTab),
            ("Ctrl+Shift+T", Action::ReopenClosedTab),
            ("Ctrl+Tab", Action::NextTab),
            ("Ctrl+Shift+Tab", Action::PreviousTab),
            ("Ctrl+N", Action::NewWindow),
            ("Ctrl+\\", Action::SplitView),
            ("F6", Action::SwitchPane),
            ("Shift+F6", Action::SwitchPane),
            ("Ctrl+L", Action::TypePath),
            ("F4", Action::TypePath),
            ("Ctrl+F4", Action::TypePath),
            ("Alt+D", Action::TypePath),
            ("Ctrl+F", Action::SearchBar),
            ("F3", Action::SearchBar),
            ("Ctrl+Shift+F", Action::SearchSubfolders),
            ("Alt+Left", Action::Back),
            ("Alt+Right", Action::Forward),
            ("Alt+Up", Action::Up),
            ("Ctrl+Alt+Left", Action::Back),
            ("F5", Action::Refresh),
            ("Ctrl+F5", Action::Refresh),
            ("Alt+Shift+F5", Action::Refresh),
            ("Ctrl+R", Action::Refresh),
            ("Ctrl+Shift+R", Action::Refresh),
            ("Ctrl+D", Action::PinToSidebar),
            ("Ctrl+`", Action::OpenInTerminal),
            ("Ctrl+Shift+`", Action::OpenInTerminal),
            ("Ctrl+Shift+U", Action::DiskSpace),
            ("Ctrl+Enter", Action::OpenInNewTab),
            ("F2", Action::Rename),
            ("Ctrl+C", Action::Copy),
            ("Ctrl+Insert", Action::Copy),
            ("Ctrl+X", Action::Cut),
            ("Ctrl+V", Action::Paste),
            ("Shift+Insert", Action::Paste),
            ("Ctrl+Shift+C", Action::CopyPath),
            ("Del", Action::Delete),
            ("Shift+Del", Action::DeletePermanently),
            ("Ctrl+Shift+N", Action::NewFolder),
            ("Ctrl+Z", Action::Undo),
            ("Ctrl+Y", Action::Redo),
            ("Ctrl+Shift+Z", Action::Redo),
            ("Ctrl+A", Action::SelectAll),
            ("Alt+Enter", Action::Properties),
            ("Ctrl+Alt+Enter", Action::Properties),
            ("Ctrl+B", Action::Sidebar),
            ("Alt+P", Action::DetailsPanel),
            ("Ctrl+H", Action::HiddenFiles),
            ("Ctrl+Shift+L", Action::LightDarkMode),
            ("Ctrl+U", Action::CommandBox),
            ("Ctrl+,", Action::Settings),
            ("Ctrl+Q", Action::ExitGlint),
        ];
        for (keys, want) in cases {
            assert_eq!(k.find(c(keys)), Some(*want), "{keys}");
        }
        // And what they never did.
        for keys in ["Ctrl+Shift+H", "Ctrl+Shift+D", "Ctrl+Shift+Q", "Ctrl+Shift+B", "Ctrl+Shift+,", "Ctrl+Alt+Q", "Ctrl+Alt+T", "Ctrl+Alt+D", "Alt+Ctrl+P", "Shift+F2", "Ctrl+F2", "Ctrl+Delete", "Ctrl+Shift+V", "Ctrl+Shift+X", "Ctrl+Shift+Y", "Ctrl+Alt+F3", "Ctrl+Alt+F4", "Alt+F4", "Ctrl+Shift+Insert", "Q", "T"] {
            assert_eq!(k.find(c(keys)), None, "{keys}");
        }
        // Every default reads, can be used, and is nobody else's.
        for s in ALL {
            let d = default_of(s).unwrap_or_else(|| panic!("{}", s.default));
            // (Space alone is refused for any other shortcut: it types a space into a name.)
            assert_eq!(problem(d).filter(|_| s.action != Action::Preview), None, "{}", s.default);
            assert_eq!(k.chord(s.action), Some(d));
            assert!(!k.changed(s.action));
            assert_eq!(k.text(s.action), s.default);
            assert_eq!(ALL.iter().filter(|o| o.name == s.name || o.label == s.label).count(), 1, "{}", s.name);
            assert_eq!(s.action.spec().name, s.name);
        }
    }

    /// AltGr comes as Ctrl+Alt: AltGr+Q (@ on a German keyboard) isn't Ctrl+Q, nor is any Ctrl
    /// shortcut; and a Ctrl+Alt shortcut can't be set.
    #[test]
    fn altgr_is_not_ctrl() {
        let k = KeyMap::defaults();
        for s in ALL.iter().filter(|s| s.default.starts_with("Ctrl+") && !s.default.contains("Alt")) {
            let mut alt_gr = default_of(s).unwrap();
            alt_gr.alt = true;
            assert_ne!(k.find(alt_gr), Some(s.action), "{}", s.default);
        }
        assert_eq!(Chord::of(u16::from(b'Q'), m(true, false, true)), c("Ctrl+Alt+Q"));
        assert_eq!(problem(c("Ctrl+Alt+Q")), Some(Problem::AltGr));
        assert!(k.change(&BTreeMap::new(), Action::NewTab, c("Ctrl+Alt+K")).is_err());
    }

    /// Every key name reads back as it's written, and the ways of writing it all read.
    #[test]
    fn names_round_trip() {
        let mut keys: Vec<u16> = (0x30..=0x39).chain(0x41..=0x5A).chain(VK_F1..=VK_F24).chain(0x60..=0x69).collect();
        keys.extend(NAMED.iter().map(|(k, _)| *k));
        for vk in keys {
            for (ctrl, shift, alt) in [(false, false, false), (true, false, false), (true, true, false), (false, true, true), (false, false, true)] {
                let chord = Chord::new(vk, ctrl, shift, alt);
                let text = chord.saved().unwrap();
                assert_eq!(Chord::parse(&text), Some(chord), "{text}");
                assert_eq!(Chord::parse(&text.to_lowercase()), Some(chord), "{text}");
                assert_eq!(Chord::parse(&text.to_uppercase()), Some(chord), "{text}");
            }
        }
        assert_eq!(c("Ctrl+Num+"), Chord::new(0x6B, true, false, false));
        assert_eq!(c("Ctrl+-"), Chord::new(0xBD, true, false, false));
        assert_eq!(c(" shift+del "), Chord::new(VK_DELETE, false, true, false));
        assert_eq!(c("Ctrl+PgDn"), c("Ctrl+Page Down"));
        assert_eq!(c("Ctrl+pageup").saved().unwrap(), "Ctrl+Page Up");
        assert_eq!(c("Shift+Alt+Ctrl+K").saved().unwrap(), "Ctrl+Alt+Shift+K");
        // Not keys: nothing, twice the same, made-up names, odd letters, far too long.
        for bad in ["", "Ctrl+", "Ctrl+Ctrl+T", "Ctrl+Shift", "Ctrl+Banana", "F25", "F0", "Num10", "Ctrl+İ", "Ctrl+ñ", "Ctrl+😀", "Ctrl+#", "Hyper+T", "Ctrl T", &"Ctrl+".repeat(300)] {
            assert_eq!(Chord::parse(bad), None, "{bad}");
        }
        // A key with no name of its own can't be written (Print Screen, the volume keys).
        assert_eq!(Chord::new(0x2C, true, false, false).saved(), None);
        assert_eq!(Chord::new(0xAF, true, false, false).saved(), None);
    }

    /// A punctuation key shows as the keyboard in use has it; the rest by their names.
    #[test]
    fn names_follow_the_keyboard() {
        let norwegian = |vk: u16| match vk {
            0xC0 => Some('ø'),
            0xDC => Some('|'),
            _ => None,
        };
        assert_eq!(c("Ctrl+`").text_with(norwegian), "Ctrl+Ø");
        assert_eq!(c("Ctrl+\\").text_with(norwegian), "Ctrl+|");
        assert_eq!(c("Ctrl+;").text_with(norwegian), "Ctrl+;");
        assert_eq!(c("Ctrl+K").text_with(|_| Some('x')), "Ctrl+K");
        assert_eq!(c("Alt+F7").text_with(|_| Some('x')), "Alt+F7");
        assert_eq!(c("Ctrl+=").text_with(|_| Some('\0')), "Ctrl+=");
        // A default shows as it always did, whatever the keyboard.
        assert_eq!(KeyMap::defaults().text(Action::OpenInTerminal), "Ctrl+`");
        assert_eq!(KeyMap::defaults().text(Action::Delete), "Delete");
    }

    /// Preview is a shortcut like the others: Space at first, other keys after. Space alone stays the
    /// preview's (for anything else it types a space into a name being typed).
    #[test]
    fn preview_can_be_changed() {
        let k = KeyMap::defaults();
        let none = BTreeMap::new();
        assert_eq!((k.find(c("Space")), k.text(Action::Preview).as_str()), (Some(Action::Preview), "Space"));
        let moved = k.change(&none, Action::Preview, c("Ctrl+P")).unwrap();
        assert_eq!(moved, saved(&[("Preview", "Ctrl+P")]));
        let k2 = KeyMap::of(&moved);
        assert_eq!((k2.find(c("Ctrl+P")), k2.find(c("Space"))), (Some(Action::Preview), None));
        // Space alone is still refused for anything else; for Preview it is its default again.
        assert_eq!(k2.change(&moved, Action::Rename, c("Space")).unwrap_err(), "Space is needed for typing and moving around, so add Ctrl or Alt.");
        assert_eq!(k2.change(&moved, Action::Preview, c("Space")).unwrap(), BTreeMap::new());
        // Shift+Space or Ctrl+Shift+Space for the preview follow the usual rules.
        assert!(k.change(&none, Action::Preview, c("Shift+Space")).is_err());
        assert_eq!(k.change(&none, Action::Preview, c("Ctrl+Shift+Space")).unwrap(), saved(&[("Preview", "Ctrl+Shift+Space")]));
        // A hand-edited file can't give Space alone to another shortcut.
        assert_eq!(KeyMap::of(&saved(&[("Rename", "Space")])).text(Action::Rename), "F2");
    }

    /// Right-click on a shortcut's button: it has no keys ("" in settings.json), its key is free for
    /// another, and new keys or RESET (its default again) bring it back.
    #[test]
    fn a_shortcut_can_be_removed() {
        let k = KeyMap::defaults();
        let none = BTreeMap::new();
        let gone = k.remove(&none, Action::CloseTab);
        assert_eq!(gone, saved(&[("CloseTab", "")]));
        let k2 = KeyMap::of(&gone);
        assert_eq!((k2.chord(Action::CloseTab), k2.text(Action::CloseTab).as_str(), k2.changed(Action::CloseTab)), (None, "", true));
        assert_eq!(k2.find(c("Ctrl+W")), None);
        // The others keep theirs; removing it again changes nothing.
        assert_eq!(k2.text(Action::NewTab), "Ctrl+T");
        assert_eq!(k2.remove(&gone, Action::CloseTab), gone);
        // Its old key can go to another shortcut.
        let given = k2.change(&gone, Action::NewTab, c("Ctrl+W")).unwrap();
        assert_eq!(given, saved(&[("CloseTab", ""), ("NewTab", "Ctrl+W")]));
        assert_eq!(KeyMap::of(&given).find(c("Ctrl+W")), Some(Action::NewTab));
        // Its default again (RESET): nothing left to save.
        assert_eq!(k2.change(&gone, Action::CloseTab, c("Ctrl+W")).unwrap(), BTreeMap::new());
        // Other keys instead.
        assert_eq!(k2.change(&gone, Action::CloseTab, c("Ctrl+Shift+W")).unwrap(), saved(&[("CloseTab", "Ctrl+Shift+W")]));
        // Preview too: then Space does nothing.
        let no_preview = k.remove(&none, Action::Preview);
        assert_eq!(KeyMap::of(&no_preview).find(c("Space")), None);
        // Two removed, odd names in the file kept out as before.
        let two = KeyMap::of(&gone).remove(&gone, Action::Rename);
        assert_eq!(two, saved(&[("CloseTab", ""), ("Rename", "")]));
        assert_eq!(KeyMap::of(&saved(&[("NoSuchThing \u{1F600}", "")])), KeyMap::defaults());
    }

    /// Keys Windows keeps, bare typing keys and the list's own keys are refused, each with its line;
    /// function keys alone, Delete and Insert are fine.
    #[test]
    fn reserved_keys_are_refused() {
        let k = KeyMap::defaults();
        let none = BTreeMap::new();
        let refused = |keys: &str| k.change(&none, Action::NewTab, c(keys)).unwrap_err();
        assert_eq!(refused("Alt+F4"), "Alt+F4 belongs to Windows, so Glint can't use it.");
        assert_eq!(refused("Alt+Space"), "Alt+Space belongs to Windows, so Glint can't use it.");
        assert_eq!(refused("Alt+Tab"), "Alt+Tab belongs to Windows, so Glint can't use it.");
        assert_eq!(refused("Ctrl+Esc"), "Ctrl+Esc belongs to Windows, so Glint can't use it.");
        assert_eq!(refused("Ctrl+Shift+Esc"), "Ctrl+Shift+Esc belongs to Windows, so Glint can't use it.");
        assert_eq!(refused("Ctrl+Alt+Delete"), "Ctrl+Alt+Delete belongs to Windows, so Glint can't use it.");
        assert_eq!(k.change(&none, Action::NewTab, Chord::new(VK_LWIN, false, false, false)).unwrap_err(), "The Windows key belongs to Windows, so Glint can't use it.");
        assert_eq!(k.change(&none, Action::NewTab, Chord::new(VK_RWIN, true, false, false)).unwrap_err(), "The Windows key belongs to Windows, so Glint can't use it.");
        for keys in ["K", "7", "Enter", "Tab", "Backspace", "Shift+K", "Shift+Enter", "Shift+Space"] {
            assert_eq!(refused(keys), format!("{keys} is needed for typing and moving around, so add Ctrl or Alt."));
        }
        // The arrows and their neighbours, and Space: what has them (adding Ctrl wouldn't free them).
        for keys in ["Left", "Down", "Home", "End", "Page Up", "Page Down", "Shift+Up"] {
            assert_eq!(refused(keys), format!("{keys} is already used by Moving around the list."));
        }
        assert_eq!(refused("Space"), "Space is already used by Preview.");
        assert!(k.change(&none, Action::NewTab, c("Ctrl+Shift+Space")).is_ok());
        assert_eq!(refused("Ctrl+Alt+K"), "Ctrl+Alt+K can't be used: many keyboards type characters with Ctrl+Alt.");
        assert_eq!(k.change(&none, Action::NewTab, Chord::new(0x2C, true, false, false)).unwrap_err(), "That key can't be used for a shortcut.");
        assert_eq!(refused("Ctrl+3"), "Ctrl+3 is already used by Go to tab 3.");
        assert_eq!(refused("Ctrl+Shift+2"), "Ctrl+Shift+2 is already used by Change view.");
        assert_eq!(refused("Ctrl+Shift+8"), "Ctrl+Shift+8 is already used by Go to tab 8.");
        assert_eq!(refused("Ctrl+Home"), "Ctrl+Home is already used by Moving around the list.");
        assert_eq!(refused("Ctrl+Shift+Down"), "Ctrl+Shift+Down is already used by Moving around the list.");
        assert_eq!(refused("Ctrl+Space"), "Ctrl+Space is already used by Select or unselect an item.");
        assert_eq!(refused("Shift+F10"), "Shift+F10 is already used by Full right-click menu.");
        for keys in ["F7", "F12", "Shift+F7", "F24", "Insert", "Ctrl+0", "Alt+K", "Alt+Shift+K", "Ctrl+Backspace", "Alt+Down", "Ctrl+Num5", "Ctrl+F10"] {
            assert!(k.change(&none, Action::NewTab, c(keys)).is_ok(), "{keys}");
        }
    }

    /// A key another shortcut has is refused, saying which; its own key changes nothing.
    #[test]
    fn a_key_in_use_is_refused() {
        let k = KeyMap::defaults();
        let none = BTreeMap::new();
        assert_eq!(k.change(&none, Action::CloseTab, c("Ctrl+T")).unwrap_err(), "Ctrl+T is already used by New tab.");
        assert_eq!(k.change(&none, Action::NewTab, c("F2")).unwrap_err(), "F2 is already used by Rename.");
        assert_eq!(k.change(&none, Action::NewTab, c("Ctrl+T")), Ok(BTreeMap::new()));
        // A built-in second way isn't in the way: it gives way.
        let moved = k.change(&none, Action::Rename, c("F3")).unwrap();
        let k2 = KeyMap::of(&moved);
        assert_eq!(k2.find(c("F3")), Some(Action::Rename));
        assert_eq!(k2.find(c("Ctrl+F")), Some(Action::SearchBar));
        // And a changed one is in the way like a default one.
        assert_eq!(k2.change(&moved, Action::NewTab, c("F3")).unwrap_err(), "F3 is already used by Rename.");
    }

    /// Changing one, putting it back, putting them all back.
    #[test]
    fn change_and_reset() {
        let k = KeyMap::defaults();
        let one = k.change(&BTreeMap::new(), Action::NewTab, c("Ctrl+K")).unwrap();
        assert_eq!(one, saved(&[("NewTab", "Ctrl+K")]));
        let k1 = KeyMap::of(&one);
        assert!(k1.changed(Action::NewTab));
        assert_eq!(k1.find(c("Ctrl+K")), Some(Action::NewTab));
        assert_eq!(k1.find(c("Ctrl+T")), None, "the old key is free");
        assert_eq!(k1.text(Action::NewTab), "Ctrl+K");
        // Changed again: the newest.
        let two = k1.change(&one, Action::NewTab, c("Alt+N")).unwrap();
        assert_eq!(two, saved(&[("NewTab", "Alt+N")]));
        // The freed key can go to another.
        let k2 = KeyMap::of(&two);
        let three = k2.change(&two, Action::CloseTab, c("Ctrl+T")).unwrap();
        assert_eq!(three, saved(&[("CloseTab", "Ctrl+T"), ("NewTab", "Alt+N")]));
        // Put back on its own: chosen again as its default, the entry goes.
        let k3 = KeyMap::of(&three);
        let back = k3.change(&three, Action::NewTab, c("Ctrl+T")).unwrap_err();
        assert_eq!(back, "Ctrl+T is already used by Close tab.");
        let back = k3.change(&three, Action::CloseTab, c("Ctrl+W")).unwrap();
        assert_eq!(back, saved(&[("NewTab", "Alt+N")]));
        // All back: nothing saved is every default.
        assert_eq!(KeyMap::of(&BTreeMap::new()), KeyMap::defaults());
    }

    /// Hand-edited files: two on one key, names and keys Glint doesn't know, keys it can't use.
    #[test]
    fn hand_edited_files() {
        // Changed beats default: Close tab takes Ctrl+T, and New tab, whose default that was, has none.
        let k = KeyMap::of(&saved(&[("CloseTab", "Ctrl+T")]));
        assert_eq!(k.find(c("Ctrl+T")), Some(Action::CloseTab));
        assert_eq!(k.chord(Action::NewTab), None);
        assert_eq!(k.text(Action::NewTab), "");
        assert_eq!(k.find(c("Ctrl+W")), None);
        // Two changed on one key: the first on the page wins, the other keeps its default.
        let k = KeyMap::of(&saved(&[("Rename", "Ctrl+K"), ("NewTab", "Ctrl+K")]));
        assert_eq!(k.find(c("Ctrl+K")), Some(Action::NewTab));
        assert_eq!(k.chord(Action::Rename), Some(c("F2")));
        // Unknown names, unreadable keys and refused keys: defaults.
        let k = KeyMap::of(&saved(&[("Teleport", "Ctrl+K"), ("NewTab", "Ctrl+Banana"), ("CloseTab", "Alt+F4"), ("Rename", "K"), ("Redo", "Ctrl+Alt+Y"), ("Copy", &"x".repeat(300)), ("Paste", "Ctrl+😀")]));
        assert_eq!(k, KeyMap::defaults());
        // Empty: removed (right-click in Settings), so none.
        let k = KeyMap::of(&saved(&[("Undo", "")]));
        assert_eq!((k.chord(Action::Undo), k.find(c("Ctrl+Z"))), (None, None));
        // Never two on one key, whatever the file says.
        let all_on_k: BTreeMap<String, String> = ALL.iter().map(|s| (s.name.to_string(), "Ctrl+K".to_string())).collect();
        let k = KeyMap::of(&all_on_k);
        let mut seen = std::collections::HashSet::new();
        for s in ALL {
            if let Some(chord) = k.chord(s.action) {
                assert!(seen.insert(chord), "{} twice", chord.text());
            }
        }
        assert_eq!(k.find(c("Ctrl+K")), Some(Action::NewTab));
        // A change made after that keeps only what's in use.
        let tidy = k.change(&all_on_k, Action::Rename, c("F8")).unwrap();
        assert_eq!(tidy, saved(&[("NewTab", "Ctrl+K"), ("Rename", "F8")]));
    }

    /// settings.json's "Shortcuts": text entries kept; others, or a wrong kind of value, dropped.
    #[test]
    fn reading_the_file_is_lenient() {
        #[derive(serde::Deserialize)]
        struct S {
            #[serde(deserialize_with = "lenient", default)]
            shortcuts: BTreeMap<String, String>,
        }
        let s: S = serde_json::from_str(r#"{"shortcuts": {"NewTab": "Ctrl+K", "CloseTab": 5, "Rename": null, "İñ#😀": "F8"}}"#).unwrap();
        assert_eq!(s.shortcuts, saved(&[("NewTab", "Ctrl+K"), ("İñ#😀", "F8")]));
        for odd in [r#"{"shortcuts": 7}"#, r#"{"shortcuts": "Ctrl+K"}"#, r#"{"shortcuts": [1, 2]}"#, r#"{"shortcuts": null}"#, "{}"] {
            let s: S = serde_json::from_str(odd).unwrap();
            assert!(s.shortcuts.is_empty(), "{odd}");
        }
    }

    /// Menus and the command box show the shortcut in use: changed, the new one; none, nothing.
    #[test]
    fn menus_and_the_command_box_follow_a_change() {
        let k = KeyMap::of(&saved(&[("CloseTab", "Ctrl+Shift+K"), ("Delete", "Ctrl+Delete"), ("NewTab", "Ctrl+W")]));
        assert_eq!(swap_label("Close tab\tCtrl+W", &k).as_deref(), Some("Close tab\tCtrl+Shift+K"));
        assert_eq!(swap_label("Delete\tDel", &k).as_deref(), Some("Delete\tCtrl+Delete"));
        assert_eq!(swap_label("Copy\tCtrl+C", &k), None);
        assert_eq!(swap_label("Open\tEnter", &k), None);
        assert_eq!(swap_label("Large icons\tCtrl+Shift+2", &k), None);
        assert_eq!(swap_label("Restore", &k), None);
        assert_eq!(swap_keys("Ctrl+W", &k), "Ctrl+Shift+K");
        assert_eq!(swap_keys("Ctrl+T", &k), "Ctrl+W");
        assert_eq!(swap_keys("Keyboard shortcuts", &k), "Keyboard shortcuts");
        assert_eq!(swap_keys("", &k), "");
        // A shortcut that has no key now shows none.
        let k = KeyMap::of(&saved(&[("CloseTab", "Ctrl+T")]));
        assert_eq!(swap_label("New tab\tCtrl+T", &k).as_deref(), Some("New tab"));
        assert_eq!(swap_keys("Ctrl+T", &k), "");
        // Submenus too.
        let mut items = vec![crate::ui::MenuItem { label: "View".into(), submenu: vec![crate::ui::MenuItem { label: "Sidebar\tCtrl+B".into(), ..Default::default() }], ..Default::default() }];
        swap_menu(&mut items, &KeyMap::of(&saved(&[("Sidebar", "Alt+B")])));
        assert_eq!(items[0].submenu[0].label, "Sidebar\tAlt+B");
        assert_eq!(items[0].label, "View");
    }

    /// The command box and a real right-click menu, with two shortcuts changed: the new keys show.
    #[test]
    fn the_command_box_and_menus_show_a_changed_shortcut() {
        let mut settings = crate::settings::Settings::default();
        settings.shortcuts = saved(&[("NewTab", "Ctrl+K"), ("Refresh", "Ctrl+F9")]);
        let app = crate::app::AppState::for_tests(settings);
        let items = crate::palette::actions(&app);
        let keys_of = |name: &str| items.iter().find(|i| i.name == name).map(|i| i.keys);
        assert_eq!(keys_of("New tab"), Some("Ctrl+K"));
        assert_eq!(keys_of("Close tab"), Some("Ctrl+W"), "unchanged");
        let menu = crate::menu::for_background(&app, crate::ui::Point::new(0.0, 0.0), Default::default());
        let mut items = match menu {
            crate::ui::Effect::Menu(m) => m.items,
            crate::ui::Effect::FilesMenu(m) => m.menu.items,
            _ => panic!("no menu"),
        };
        assert!(items.iter().any(|i| i.label == "Refresh\tF5"));
        swap_menu(&mut items, &KeyMap::of_settings(&app.settings));
        assert!(items.iter().any(|i| i.label == "Refresh\tCtrl+F9"));
        assert!(!items.iter().any(|i| i.label.ends_with("\tF5")));
    }

    /// The built-in second ways: they work while nobody has their key, and give way when someone does.
    #[test]
    fn second_ways_give_way() {
        let k = KeyMap::of(&saved(&[("TypePath", "Ctrl+Shift+P")]));
        assert_eq!(k.find(c("F4")), Some(Action::TypePath));
        assert_eq!(k.find(c("Alt+D")), Some(Action::TypePath));
        assert_eq!(k.find(c("Ctrl+L")), None, "the main key moved");
        // Changed shortcuts are exact: Shift doesn't count any more.
        assert_eq!(k.find(c("Ctrl+Shift+P")), Some(Action::TypePath));
        let k = KeyMap::of(&saved(&[("CloseTab", "Ctrl+K")]));
        assert_eq!(k.find(c("Ctrl+Shift+K")), None);
        let k = KeyMap::of(&saved(&[("NewWindow", "Alt+D"), ("Refresh", "Ctrl+R")]));
        assert_eq!(k.find(c("Alt+D")), Some(Action::NewWindow));
        assert_eq!(k.find(c("Ctrl+R")), Some(Action::Refresh));
        assert_eq!(k.find(c("F5")), None);
        assert_eq!(k.find(c("Alt+Up")), Some(Action::Up));
        // A shortcut that moved leaves its old key free: no second way keeps it going.
        let k = KeyMap::of(&saved(&[("Back", "Ctrl+J"), ("Refresh", "Ctrl+F9")]));
        assert_eq!((k.find(c("Ctrl+J")), k.find(c("Alt+Left")), k.find(c("Ctrl+Alt+Left"))), (Some(Action::Back), None, None));
        assert_eq!((k.find(c("Ctrl+F9")), k.find(c("F5")), k.find(c("Ctrl+R"))), (Some(Action::Refresh), None, Some(Action::Refresh)));
        assert_eq!(k.find(c("Alt+Right")), Some(Action::Forward), "the others are where they were");
        for (keys, a, _) in EXTRAS {
            assert!(ALL.iter().all(|s| s.default != *keys), "{keys} is {a:?}'s own key");
        }
    }

    /// A key pressed while a shortcut waits: held keys alone go on waiting, Esc stops, anything
    /// else is taken or refused - Enter, Esc with a held key and the Windows keys among them.
    #[test]
    fn keys_pressed_while_a_shortcut_waits() {
        let none = BTreeMap::new();
        let press = |vk: u16, ctrl: bool, shift: bool, alt: bool| pressed_while_waiting(&none, Action::NewTab, Chord::new(vk, ctrl, shift, alt));
        // Shift, Ctrl, Alt (either side), Caps Lock, Num Lock, Scroll Lock: still waiting.
        for vk in [0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x14, 0x90, 0x91] {
            assert_eq!(press(vk, false, false, false), Pressed::Wait, "{vk:#x}");
            assert_eq!(press(vk, true, true, false), Pressed::Wait, "{vk:#x} with others held");
        }
        // AltGr comes as Ctrl and Alt held: its own press waits, a key with it is refused.
        assert_eq!(press(0xA5, true, false, true), Pressed::Wait);
        assert!(matches!(press(u16::from(b'Q'), true, false, true), Pressed::Refused(why) if why.starts_with("Ctrl+Alt+Q can't be used")));
        // Esc alone stops; with a held key it is a key like any other (and Windows keeps those).
        assert_eq!(press(VK_ESCAPE, false, false, false), Pressed::Stop);
        assert!(matches!(press(VK_ESCAPE, false, true, false), Pressed::Refused(_)));
        assert!(matches!(press(VK_ESCAPE, true, false, false), Pressed::Refused(why) if why.contains("belongs to Windows")));
        // Enter, Tab, a letter: refused with the line about typing, never saved.
        for vk in [VK_RETURN, VK_TAB, u16::from(b'K')] {
            assert!(matches!(press(vk, false, false, false), Pressed::Refused(why) if why.ends_with("so add Ctrl or Alt.")), "{vk:#x}");
        }
        // A free key is taken; a key another shortcut has is refused, saying which.
        assert_eq!(press(u16::from(b'K'), true, false, false), Pressed::Set(saved(&[("NewTab", "Ctrl+K")])));
        assert_eq!(press(u16::from(b'W'), true, false, false), Pressed::Refused("Ctrl+W is already used by Close tab.".into()));
        // Its own key again: nothing to save, and the waiting is over.
        assert_eq!(press(u16::from(b'T'), true, false, false), Pressed::Set(BTreeMap::new()));
    }
}
