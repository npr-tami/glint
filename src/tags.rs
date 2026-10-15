//! Colour tags: a coloured dot on the files and folders the person marks (right-click > Tag: Red,
//! Orange, Yellow, Green, Blue or Purple, and Remove tag; menu.rs). The dot shows by the name in
//! Details, Tree and List and on the tile in the icon views (list.rs calls [`after_name`] and
//! [`on_tile`]), and the filter box finds them (folder.rs `NameFilter`): `tag:red` keeps the red
//! items, `tag:` alone every tagged one, and `tag:red invoice` the red ones with "invoice" in the
//! name. An item has one tag at most.
//!
//! **Where they're kept.** In Glint's own small file, `tags.txt` next to settings.json (in the
//! portable folder too: settings.rs `config_now`), one item a line: its colour, a tab, its path.
//! The files themselves are never touched. Paths are compared without minding capitals, as
//! Windows does.
//!
//! **Several windows.** Each Glint window is a process of its own (life.rs). Each reads the file
//! at start and again when it comes to the front ([`Tags::refresh`]); a change is written at once,
//! under a lock all of them share, on top of what the file says by then - so two windows never
//! wipe out each other's tags.
//!
//! **Moving.** Tags go along with what Glint moves or renames - F2, Rename all, cut and paste,
//! dragging, Move to other side, the copy queue, and Ctrl+Z / Ctrl+Y of all of them - by what the
//! job says it did (app/files.rs `after_file_op`); a moved folder takes the tags inside it along.
//! Copies get none. An item sent to the Recycle Bin keeps its line, so it has its tag again when
//! it comes back; one deleted for good loses it.
//!
//! **Tidying.** Lines of items deleted elsewhere would pile up: a while after the main window
//! starts, a background thread drops those whose item is gone from this computer's own disks (but
//! not what this Glint sent to the Recycle Bin meanwhile). Network drives, USB sticks and discs are
//! never looked at (they may just be away).
//!
//! **Cost.** The file is read once and kept in a hash map by path; drawing a row is one look-up
//! ([`Lookup`]), and nothing at all while nothing is tagged.

use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetFileAttributesW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, INVALID_FILE_ATTRIBUTES, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentThread, ReleaseMutex, SetThreadPriority, WaitForSingleObject, THREAD_MODE_BACKGROUND_BEGIN, THREAD_MODE_BACKGROUND_END,
};

use crate::folder::{join, wide, wide_path, Entry};
use crate::log;
use crate::render::{Renderer, TextStyle};
use crate::theme::{Color, Theme};
use crate::ui::{Point, Rect};

/// The file's name, next to settings.json.
pub const FILE_NAME: &str = "tags.txt";
/// How a tag is asked for in the filter box: `tag:red`, or `tag:` for any.
pub const FILTER_WORD: &str = "tag:";
/// The lock every Glint window takes to write the file.
const LOCK_NAME: PCWSTR = w!("Local\\Glint.Tags");
/// How long a window waits for another one's write (which takes a few milliseconds).
const LOCK_WAIT_MS: u32 = 2000;
/// How long after the start the clean-up waits: the first folders are on screen by then.
const TIDY_AFTER: Duration = Duration::from_secs(30);
/// The file's first lines.
const HEADER: &str = "# Glint's colour tags: one item a line, its colour, a tab, then its path.\n# Glint keeps this file up to date; the files themselves are never changed.\n";

// ---------------------------------------------------------------- the tags

/// A colour tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl Tag {
    /// All of them, in the menu's order.
    pub const ALL: [Tag; 6] = [Tag::Red, Tag::Orange, Tag::Yellow, Tag::Green, Tag::Blue, Tag::Purple];

    /// Its name in the menu.
    pub fn name(self) -> &'static str {
        match self {
            Tag::Red => "Red",
            Tag::Orange => "Orange",
            Tag::Yellow => "Yellow",
            Tag::Green => "Green",
            Tag::Blue => "Blue",
            Tag::Purple => "Purple",
        }
    }

    /// Its word in the file, the filter box and the status bar.
    pub fn word(self) -> &'static str {
        match self {
            Tag::Red => "red",
            Tag::Orange => "orange",
            Tag::Yellow => "yellow",
            Tag::Green => "green",
            Tag::Blue => "blue",
            Tag::Purple => "purple",
        }
    }

    /// The tag a word in the file stands for (capitals don't matter).
    pub fn from_word(word: &str) -> Option<Tag> {
        Tag::ALL.into_iter().find(|t| t.word().eq_ignore_ascii_case(word))
    }

    /// Its dot's colour: bright enough for a dark skin, deep enough for a light one (where it would
    /// still blend in, the dot gets a thin edge: see `look`).
    pub fn color(self) -> Color {
        Color::hex(match self {
            Tag::Red => 0xef4444,
            Tag::Orange => 0xf97316,
            Tag::Yellow => 0xeab308,
            Tag::Green => 0x16a34a,
            Tag::Blue => 0x3b82f6,
            Tag::Purple => 0xa855f7,
        })
    }

    fn bit(self) -> u8 {
        1 << self as u8
    }

    /// The tags whose word starts with `start` (in lower case), one bit each: the six have six first
    /// letters, so `tag:g` is already green. "" is any tag; a word no colour has, none.
    fn starting(start: &str) -> u8 {
        Tag::ALL.into_iter().filter(|t| t.word().starts_with(start)).fold(0, |bits, t| bits | t.bit())
    }
}

/// One tagged item: its tag, and its path as it was given (the key it's kept under is that path in
/// lower case).
#[derive(Clone, Debug, PartialEq)]
pub struct Tagged {
    tag: Tag,
    path: String,
}

/// Every tagged item, by its path in lower case.
pub type Map = HashMap<String, Tagged>;

/// The key a path is kept under: Windows doesn't mind capitals in names, so neither do the tags.
fn key(path: &str) -> String {
    path.to_lowercase()
}

/// A key, then the key of each folder it's in, nearest first.
fn up(key: &str) -> impl Iterator<Item = &str> {
    std::iter::once(key).chain(key.rmatch_indices('\\').map(move |(i, _)| &key[..i]))
}

// ---------------------------------------------------------------- the filter

/// The `tag:` part of a filter (folder.rs `NameFilter`): the tags an item must have one of, with
/// the tags as they were when the filter was made.
#[derive(Clone, Debug)]
pub struct TagFilter {
    /// One bit for each tag that is kept (none: nothing is).
    wanted: u8,
    tags: Arc<Map>,
}

impl TagFilter {
    /// Takes the `tag:` words out of a filter's text (in lower case): the filter they make, and
    /// the other words. None when there are none. `tags` gives the tags (asked only then).
    pub fn take(text: &str, tags: impl FnOnce() -> Arc<Map>) -> Option<(TagFilter, String)> {
        if !text.split_whitespace().any(|w| w.starts_with(FILTER_WORD)) {
            return None;
        }
        let mut wanted = 0;
        let mut rest: Vec<&str> = Vec::new();
        for word in text.split_whitespace() {
            match word.strip_prefix(FILTER_WORD) {
                // Several of them: any of those tags.
                Some(colour) => wanted |= Tag::starting(colour),
                None => rest.push(word),
            }
        }
        Some((TagFilter { wanted, tags: tags() }, rest.join(" ")))
    }

    /// Whether the item `path` gives has one of the tags wanted (`path` is asked only when
    /// something is tagged at all).
    pub fn keeps(&self, path: impl FnOnce() -> String) -> bool {
        self.wanted != 0 && !self.tags.is_empty() && self.tags.get(&key(&path())).is_some_and(|t| self.wanted & t.tag.bit() != 0)
    }
}

/// Whether a filter's text asks for tags: its list changes when tags do.
pub fn in_filter(text: &str) -> bool {
    text.split_whitespace().any(|w| w.get(..FILTER_WORD.len()).is_some_and(|start| start.eq_ignore_ascii_case(FILTER_WORD)))
}

// The tags the filter sees (`shown`). Tests keep theirs on each test's own thread, so tests
// running side by side don't see each other's.
#[cfg(not(test))]
static SHOWN: Mutex<Option<Arc<Map>>> = Mutex::new(None);
#[cfg(test)]
thread_local! {
    static SHOWN: std::cell::RefCell<Option<Arc<Map>>> = const { std::cell::RefCell::new(None) };
}

/// The tags as the filter box sees them: the window's own, as they were last changed. (A filter is
/// made in many places, search threads too, so they're put here rather than handed round.)
pub fn shown() -> Arc<Map> {
    #[cfg(not(test))]
    let map = SHOWN.lock().unwrap_or_else(PoisonError::into_inner).clone();
    #[cfg(test)]
    let map = SHOWN.with(|s| s.borrow().clone());
    map.unwrap_or_default()
}

fn publish(map: &Arc<Map>) {
    #[cfg(not(test))]
    {
        *SHOWN.lock().unwrap_or_else(PoisonError::into_inner) = Some(map.clone());
    }
    #[cfg(test)]
    SHOWN.with(|s| *s.borrow_mut() = Some(map.clone()));
}

// ---------------------------------------------------------------- the window's tags

/// The tags, as this window knows them, and the file they're kept in.
#[derive(Default)]
pub struct Tags {
    map: Arc<Map>,
    /// None: kept in memory only (tests).
    file: Option<PathBuf>,
    /// The file as it was when last read or written here. None: not read yet (or the last read
    /// failed), so it's read again before anything is written over it.
    stamp: Option<Stamp>,
    /// Changes made here that aren't in the file yet. Normally none: a change is written at once,
    /// and one that couldn't be is tried again with the next one.
    pending: Vec<Change>,
    /// Why the last write didn't work, put in the error log once (until a write works again).
    problem: Option<String>,
}

impl Tags {
    /// The person's tags, read now. In the main window this also starts the clean-up, a while
    /// later (see the top), when there's anything to tidy.
    pub fn open() -> Tags {
        let file = crate::settings::config_now().join(FILE_NAME);
        let tags = Tags::at(file.clone());
        if !tags.map.is_empty() && !crate::settings::is_extra_window() {
            tidy_later(file);
        }
        tags
    }

    /// The tags kept in `file`, read now.
    pub fn at(file: PathBuf) -> Tags {
        let mut tags = Tags { file: Some(file), ..Default::default() };
        tags.refresh();
        publish(&tags.map);
        tags
    }

    /// The tag of the item at `path`.
    pub fn of(&self, path: &str) -> Option<Tag> {
        if self.map.is_empty() {
            return None;
        }
        self.map.get(&key(path)).map(|t| t.tag)
    }

    /// For going through the items of the listing of `dir` (drawing its rows): see [`Lookup`].
    pub fn lookup(&self, dir: &str) -> Lookup<'_> {
        let mut folder = String::new();
        if !self.map.is_empty() && !dir.is_empty() {
            folder = key(dir);
            if !folder.ends_with('\\') {
                folder.push('\\');
            }
        }
        Lookup { map: &self.map, folder, buf: String::new() }
    }

    /// Puts `tag` on these items, or takes their tags off (None), and writes it. Whether anything
    /// changed.
    pub fn set(&mut self, paths: &[String], tag: Option<Tag>) -> bool {
        self.change(Change::Set(paths.to_vec(), tag))
    }

    /// Items Glint moved or renamed, as (where it was, where it is now): their tags go along, and
    /// so do those of the items inside a moved folder. Whether any did.
    pub fn moved(&mut self, pairs: &[(String, String)]) -> bool {
        if pairs.is_empty() {
            return false;
        }
        self.refresh();
        if self.map.is_empty() {
            return false;
        }
        self.change(Change::Moved(pairs.to_vec()))
    }

    /// Items Glint deleted for good: their tags go, and those of what was inside them. Whether any
    /// did.
    pub fn deleted(&mut self, paths: &[String]) -> bool {
        if paths.is_empty() {
            return false;
        }
        self.refresh();
        if self.map.is_empty() {
            return false;
        }
        self.change(Change::Deleted(paths.iter().map(|p| key(p.trim_end_matches('\\'))).collect()))
    }

    /// Items Glint sent to the Recycle Bin: their lines stay (the clean-up leaves them alone while
    /// this Glint runs), so they have their tags again when Ctrl+Z or Restore brings them back.
    pub fn recycled(&self, paths: &[String]) {
        if self.map.is_empty() {
            return;
        }
        let mut binned = BINNED.lock().unwrap_or_else(PoisonError::into_inner);
        binned.extend(paths.iter().map(|p| key(p.trim_end_matches('\\'))));
    }

    /// Reads the file again when another window wrote it since (this one came to the front).
    /// Whether the tags changed.
    pub fn refresh(&mut self) -> bool {
        let Some(file) = self.file.clone() else { return false };
        let before = self.map.clone();
        let now = stamp(&file);
        if self.stamp != Some(now) {
            let mut read = match read_file(&file) {
                Ok(read) => read,
                // (What's known stays, and the file is read again next time.)
                Err(e) => {
                    log::line(&format!("tags: couldn't read {}: {e}", file.display()));
                    return false;
                }
            };
            for c in &self.pending {
                c.apply(&mut read);
            }
            self.map = Arc::new(read);
            self.stamp = Some(now);
        }
        // A change that couldn't be written before is tried again.
        if !self.pending.is_empty() {
            self.save();
        }
        let changed = !Arc::ptr_eq(&before, &self.map) && *before != *self.map;
        if changed {
            publish(&self.map);
        }
        changed
    }

    /// Makes a change here and writes it. Whether it changed anything.
    fn change(&mut self, change: Change) -> bool {
        // What another window wrote meanwhile first (it's normally read already: this window was
        // brought to the front to do this).
        self.refresh();
        if !change.apply(Arc::make_mut(&mut self.map)) {
            return false;
        }
        self.pending.push(change);
        self.save();
        publish(&self.map);
        true
    }

    /// Writes the changes made here into the file, on top of what it holds by now: another window
    /// may have written it since this one last looked, and what it wrote stays.
    fn save(&mut self) {
        let Some(file) = self.file.clone() else {
            self.pending.clear();
            return;
        };
        let _lock = Lock::take();
        let now = stamp(&file);
        if self.stamp != Some(now) {
            // Another window's tags, with this one's changes on top. (One that can't be read right
            // now is never written over: the changes wait for the next try.)
            let mut read = match read_file(&file) {
                Ok(read) => read,
                Err(e) => return self.trouble(format!("Couldn't save the colour tags: {} couldn't be read ({e}).", file.display())),
            };
            for c in &self.pending {
                c.apply(&mut read);
            }
            self.map = Arc::new(read);
        }
        match write_atomically(&file, &text(&self.map)) {
            Ok(()) => {
                self.stamp = Some(stamp(&file));
                self.pending.clear();
                if self.problem.take().is_some() {
                    log::error(&format!("The colour tags could be saved again ({}).", file.display()));
                }
            }
            Err(e) => self.trouble(format!("Couldn't save the colour tags to {}: {e}", file.display())),
        }
    }

    /// Puts a problem with saving in the error log, unless it's the one already there.
    fn trouble(&mut self, problem: String) {
        if self.problem.as_deref() != Some(problem.as_str()) {
            log::error(&problem);
            self.problem = Some(problem);
        }
    }
}

/// Finds the tags of one listing's items one after the other, as a list draws its rows: one
/// look-up each, without making a path for each (the folder's part is made once).
pub struct Lookup<'a> {
    map: &'a Map,
    /// The listing's folder in lower case, with a backslash at the end ("" while nothing is tagged).
    folder: String,
    /// Where each item's key is put together.
    buf: String,
}

impl Lookup<'_> {
    /// The tag of one of the listing's items.
    pub fn of(&mut self, e: &Entry) -> Option<Tag> {
        if self.map.is_empty() {
            return None;
        }
        self.buf.clear();
        match &e.target {
            // Search results, items of folders open in the tree, Home's: by their own path.
            Some(t) => self.buf.push_str(&key(t)),
            None => {
                self.buf.push_str(&self.folder);
                self.buf.push_str(&e.lower);
            }
        }
        self.map.get(self.buf.as_str()).map(|t| t.tag)
    }
}

/// A change made in one window, kept until it's in the file.
#[derive(Clone, Debug)]
enum Change {
    /// These items got this tag, or lost theirs (None).
    Set(Vec<String>, Option<Tag>),
    /// Items moved or renamed, as (from, to).
    Moved(Vec<(String, String)>),
    /// Items deleted for good, by key.
    Deleted(Vec<String>),
}

impl Change {
    /// Makes the change in `map`. Whether it changed anything.
    fn apply(&self, map: &mut Map) -> bool {
        match self {
            Change::Set(paths, tag) => {
                let mut changed = false;
                for path in paths {
                    let k = key(path);
                    match tag {
                        Some(tag) => {
                            let new = Tagged { tag: *tag, path: path.clone() };
                            if map.get(&k) != Some(&new) {
                                map.insert(k, new);
                                changed = true;
                            }
                        }
                        None => changed |= map.remove(&k).is_some(),
                    }
                }
                changed
            }
            Change::Moved(pairs) => move_entries(map, pairs),
            Change::Deleted(keys) => {
                // Each one, and what was inside the folders among them.
                let gone: HashSet<&str> = keys.iter().map(String::as_str).collect();
                let before = map.len();
                map.retain(|k, _| !up(k).any(|p| gone.contains(p)));
                map.len() < before
            }
        }
    }
}

/// Moves the tags of items that moved, as (from, to), and of what's inside the folders among them.
/// All are taken out before any goes in, so items that swapped names (Rename all) swap tags too.
/// Whether any moved.
fn move_entries(map: &mut Map, pairs: &[(String, String)]) -> bool {
    if map.is_empty() || pairs.is_empty() {
        return false;
    }
    let from: HashMap<String, usize> = pairs.iter().enumerate().map(|(i, (f, _))| (key(f.trim_end_matches('\\')), i)).collect();
    // Each tagged item, then each folder it's in: the first one that moved takes it along.
    let moving: Vec<(String, usize)> = map.keys().filter_map(|k| up(k).find_map(|p| from.get(p)).map(|&i| (k.clone(), i))).collect();
    if moving.is_empty() {
        return false;
    }
    let mut moved = Vec::with_capacity(moving.len());
    for (k, i) in moving {
        if let Some(t) = map.remove(&k) {
            let (from, to) = &pairs[i];
            moved.push(Tagged { path: moved_to(&t.path, from, to), tag: t.tag });
        }
    }
    for t in moved {
        map.insert(key(&t.path), t);
    }
    true
}

/// Where `path` (`from` itself, or something inside it) is after `from` moved to `to`.
fn moved_to(path: &str, from: &str, to: &str) -> String {
    // Counted in names, not letters: in lower case a name can be longer or shorter.
    let depth = from.trim_end_matches('\\').split('\\').count();
    match path.splitn(depth + 1, '\\').nth(depth) {
        Some(rest) => join(to, rest),
        None => to.to_string(),
    }
}

// ---------------------------------------------------------------- the file

/// What the file is like: its length and when it last changed (None: there is none). A write by
/// another window changes it.
type Stamp = Option<(u64, SystemTime)>;

fn stamp(file: &Path) -> Stamp {
    let m = std::fs::metadata(file).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Reads the file: no tags when there is none; an error when it can't be read right now (in use,
/// no permission).
fn read_file(file: &Path) -> std::io::Result<Map> {
    match std::fs::read(file) {
        Ok(bytes) => Ok(parse(&String::from_utf8_lossy(&bytes))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e),
    }
}

/// The tags in the file's text. Lines that aren't a colour, a tab and a path are passed over (the
/// two at the top; a line someone typed wrong).
fn parse(text: &str) -> Map {
    let mut map = Map::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let Some((word, path)) = line.split_once('\t') else { continue };
        let Some(tag) = Tag::from_word(word.trim()) else { continue };
        let path = path.trim_end_matches(['\r', '\n']);
        if !path.is_empty() {
            map.insert(key(path), Tagged { tag, path: path.to_string() });
        }
    }
    map
}

/// The file's text for these tags (in the order of their paths, so it reads well).
fn text(map: &Map) -> String {
    let mut items: Vec<(&String, &Tagged)> = map.iter().collect();
    items.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut out = String::with_capacity(HEADER.len() + items.iter().map(|(_, t)| t.path.len() + 8).sum::<usize>());
    out.push_str(HEADER);
    for (_, t) in items {
        out.push_str(t.tag.word());
        out.push('\t');
        out.push_str(&t.path);
        out.push('\n');
    }
    out
}

/// Writes `text` to `file` whole: into a file of its own next to it, which then replaces it, so
/// another window never reads half of it.
fn write_atomically(file: &Path, text: &str) -> std::io::Result<()> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // (Its own name for each write: the clean-up writes from another thread.)
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = file.with_extension(format!("txt.{}.{n}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written {
        // A full disk, say: no half-written file of ours left behind.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // A virus scanner or a sync tool may hold the old file for a moment: a few tries.
    let mut tries = 0;
    loop {
        match std::fs::rename(&tmp, file) {
            Ok(()) => return Ok(()),
            Err(_) if tries < 4 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
}

/// The lock every Glint window holds while it writes the file (a named mutex), until dropped. A
/// window that doesn't get it in time writes all the same: better than losing the change.
struct Lock(Option<HANDLE>);

impl Lock {
    fn take() -> Lock {
        // SAFETY: a named mutex this thread opens (or makes) here and lets go of and closes in `drop`.
        unsafe {
            let Ok(h) = CreateMutexW(None, false, LOCK_NAME) else { return Lock(None) };
            // (Abandoned: a window ended while it held the lock. The file is whole all the same: it is
            // only ever replaced.)
            let got = WaitForSingleObject(h, LOCK_WAIT_MS);
            if got == WAIT_OBJECT_0 || got == WAIT_ABANDONED {
                return Lock(Some(h));
            }
            let _ = CloseHandle(h);
            Lock(None)
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            // SAFETY: the mutex was taken in `take` by this thread; it's let go of and closed once.
            unsafe {
                let _ = ReleaseMutex(h);
                let _ = CloseHandle(h);
            }
        }
    }
}

// ---------------------------------------------------------------- the clean-up

/// Items this Glint sent to the Recycle Bin, by key (`Tags::recycled`): the clean-up leaves their
/// lines alone.
static BINNED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A while after the main window starts, on a thread of its own at low priority: drops the lines of
/// items that are gone from this computer's own disks (see the top).
fn tidy_later(file: PathBuf) {
    crate::shell::background("tidy tags", move || {
        std::thread::sleep(TIDY_AFTER);
        // SAFETY: lowers this thread's own priority, for its disk use too.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
        let Ok(map) = read_file(&file) else { return };
        let binned: HashSet<String> = BINNED.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect();
        let gone = gone_items(&map, &binned);
        if gone.is_empty() {
            return;
        }
        // At the usual speed again while it holds the lock: a window may be waiting for it.
        // SAFETY: as above.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_END) };
        let _lock = Lock::take();
        let Ok(mut now) = read_file(&file) else { return };
        let before = now.len();
        // (Looked at again: one may have come back meanwhile, out of the Recycle Bin say.)
        for k in gone {
            if now.get(&k).is_some_and(|t| is_gone(&t.path)) {
                now.remove(&k);
            }
        }
        if now.len() < before {
            match write_atomically(&file, &text(&now)) {
                Ok(()) => log::line(&format!("tags: items gone, their tags dropped: {}", before - now.len())),
                Err(e) => log::line(&format!("tags: couldn't tidy {}: {e}", file.display())),
            }
        }
    });
}

/// The keys of the items in `map` that are gone from this computer's own disks - but not those
/// `binned` (sent to the Recycle Bin, or inside a folder that was).
fn gone_items(map: &Map, binned: &HashSet<String>) -> Vec<String> {
    let mut own: HashMap<u8, bool> = HashMap::new();
    map.iter()
        .filter(|(_, t)| match t.path.as_bytes() {
            [letter, b':', b'\\', ..] if letter.is_ascii_alphabetic() => *own.entry(letter.to_ascii_uppercase()).or_insert_with(|| own_disk(*letter)),
            _ => false, // \\server\share
        })
        .filter(|(k, _)| !up(k).any(|p| binned.contains(p)))
        .filter(|(_, t)| is_gone(&t.path))
        .map(|(k, _)| k.clone())
        .collect()
}

/// Whether a drive is one of this computer's own disks: not a USB stick or disk, a memory card, a
/// disc or a network drive, which may just be away (or slow to answer), with another drive taking
/// its letter meanwhile.
fn own_disk(letter: u8) -> bool {
    let root = wide(&format!("{}:\\", letter as char));
    // SAFETY: a NUL-terminated path that outlives the call.
    matches!(unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) }, 3 | 6) /* DRIVE_FIXED, DRIVE_RAMDISK */ && !plugged_in(letter)
}

/// Whether a drive is plugged in from outside: on USB, FireWire or a memory card slot, or with a
/// disk that comes out. An external USB disk calls itself a fixed disk, so its bus is asked.
/// False when Windows won't say.
fn plugged_in(letter: u8) -> bool {
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
    let volume = wide(&format!("\\\\.\\{}:", letter as char));
    // SAFETY: a NUL-terminated path that outlives the call. No access asked for: the question
    // needs none, and nothing is read from or written to the disk.
    let Ok(handle) = (unsafe { CreateFileW(PCWSTR(volume.as_ptr()), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING, FILE_FLAGS_AND_ATTRIBUTES(0), None) }) else {
        return false;
    };
    // STORAGE_PROPERTY_QUERY: StorageDeviceProperty, PropertyStandardQuery.
    let query = [0u32; 3];
    // STORAGE_DEVICE_DESCRIPTOR's fixed part: RemovableMedia at byte 10, BusType at 28.
    let mut out = [0u8; 40];
    let mut got = 0u32;
    // SAFETY: both buffers outlive the call, with their true sizes; the handle is ours, closed once.
    let asked = unsafe {
        let asked = DeviceIoControl(handle, IOCTL_STORAGE_QUERY_PROPERTY, Some(query.as_ptr().cast()), 12, Some(out.as_mut_ptr().cast()), out.len() as u32, Some(&mut got), None);
        let _ = CloseHandle(handle);
        asked
    };
    if asked.is_err() || got < 32 {
        return false;
    }
    let bus = u32::from_le_bytes([out[28], out[29], out[30], out[31]]);
    out[10] != 0 || matches!(bus, 4 | 7 | 0xC | 0xD) // 1394, USB, SD, MMC
}

/// Whether nothing is at `path` any more (not just out of reach: no permission, say).
fn is_gone(path: &str) -> bool {
    let w = wide_path(path);
    // SAFETY: a NUL-terminated path that outlives the call; its error is read right after it.
    unsafe {
        if GetFileAttributesW(PCWSTR(w.as_ptr())) != INVALID_FILE_ATTRIBUTES {
            return false;
        }
        let e = GetLastError();
        e == ERROR_FILE_NOT_FOUND || e == ERROR_PATH_NOT_FOUND
    }
}

// ---------------------------------------------------------------- the dot

/// The dot's size, and the gap between a name and its dot, in DIPs.
const DOT: f32 = 8.0;
const GAP: f32 = 6.0;
/// A dot that stands out from what's behind it less than this (1 is not at all, 21 black on
/// white) gets a thin edge in the text's colour.
const EDGE_BELOW: f32 = 2.0;

/// How much of a name's room its dot takes (the gap and the dot), when it has one (Details, Tree,
/// List: the name is cut that much shorter).
pub fn room(tag: Option<Tag>) -> f32 {
    if tag.is_some() {
        GAP + DOT
    } else {
        0.0
    }
}

/// Draws an item's dot just after its name, which was drawn with `style` in `name` (its box: the
/// left edge and the row). Nothing without a tag. `selected`: the row is drawn selected.
pub fn after_name(r: &mut Renderer, theme: &Theme, tag: Option<Tag>, text: &str, style: &TextStyle, name: Rect, selected: bool) {
    let Some(tag) = tag else { return };
    let w = r.measure(text, style).min(name.w.max(0.0));
    dot(r, theme, tag, Point::new(name.x + w + GAP + DOT / 2.0, name.y + name.h / 2.0), selected);
}

/// Draws an item's dot on its tile: by the top right corner of its picture (`picture`: where it was
/// drawn), outside it and inside the tile.
pub fn on_tile(r: &mut Renderer, theme: &Theme, tag: Option<Tag>, picture: Rect, tile: Rect, selected: bool) {
    let Some(tag) = tag else { return };
    let x = (picture.right() + 2.0 + DOT / 2.0).min(tile.right() - 1.0 - DOT / 2.0);
    dot(r, theme, tag, Point::new(x, picture.y + DOT / 2.0), selected);
}

/// One dot, round `c`. Whole pixels across and centred so its edges fall where pixels do: as sharp
/// at 150 % as at 100 %.
fn dot(r: &mut Renderer, theme: &Theme, tag: Tag, c: Point, selected: bool) {
    let s = r.scale();
    let px = (DOT * s).round().max(4.0);
    let snap = |v: f32| ((v * s - px / 2.0).round() + px / 2.0) / s;
    let c = Point::new(snap(c.x), snap(c.y));
    let radius = px / s / 2.0;
    let (color, edge) = look(theme, tag, selected);
    match edge {
        Some(edge) => {
            // The edge one pixel wide, all round.
            r.fill_ellipse(c, radius, radius, edge);
            let inner = radius - 1.0 / s;
            r.fill_ellipse(c, inner, inner, color);
        }
        None => r.fill_ellipse(c, radius, radius, color),
    }
}

/// A dot's colour, and the edge it gets where it would blend into what's behind it: the yellow on a
/// white list, the blue on a blue selection. The edge is in the colour the skin writes text in
/// there, which every skin makes readable.
fn look(theme: &Theme, tag: Tag, selected: bool) -> (Color, Option<Color>) {
    // (98 and 2000 mark only a selected name, not what's beside it.)
    let selected = selected && !theme.era.classic();
    let back = behind(theme, selected);
    let color = tag.color();
    let edge = if selected { theme.selection_text } else { theme.text };
    (color, (contrast(color, back) < EDGE_BELOW).then_some(Color { a: 1.0, ..edge }))
}

/// What a dot is drawn on: the list, or a selected row on it.
fn behind(theme: &Theme, selected: bool) -> Color {
    let panel = Color { a: 1.0, ..theme.panel };
    if !selected {
        return panel;
    }
    let tint = theme.accent_tint;
    panel.mix(Color { a: 1.0, ..tint }, tint.a)
}

/// How bright a colour looks, 0 (black) to 1 (white): the W3C's relative luminance.
fn luminance(c: Color) -> f32 {
    let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// How much two colours stand apart, 1 (not at all) to 21 (black and white), as the W3C measures it.
fn contrast(a: Color, b: Color) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Tags on these paths, for tests elsewhere (the filter's).
#[cfg(test)]
pub fn map_of(items: &[(&str, Tag)]) -> Arc<Map> {
    Arc::new(items.iter().map(|&(p, tag)| (key(p), Tagged { tag, path: p.to_string() })).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file of its own in the temp folder for one test (gone again at the end), and its path.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            let p = std::env::temp_dir().join(format!("glint-tags-test-{name}-{}.txt", std::process::id()));
            let _ = std::fs::remove_file(&p);
            Scratch(p)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn tags_are_saved_and_come_back_after_a_restart() {
        let f = Scratch::new("save");
        let mut t = Tags::at(f.0.clone());
        assert!(t.set(&paths(&["C:\\Docs\\Invoice.pdf", "C:\\Docs\\Photos"]), Some(Tag::Red)));
        assert!(t.set(&paths(&["D:\\Music"]), Some(Tag::Blue)));
        assert!(!t.set(&paths(&["C:\\Docs\\Invoice.pdf"]), Some(Tag::Red)), "already red: nothing to write");
        // Glint starts again.
        let again = Tags::at(f.0.clone());
        assert_eq!(again.of("C:\\Docs\\Invoice.pdf"), Some(Tag::Red));
        assert_eq!(again.of("c:\\docs\\PHOTOS"), Some(Tag::Red), "capitals don't matter");
        assert_eq!(again.of("D:\\Music"), Some(Tag::Blue));
        assert_eq!(again.of("C:\\Docs\\other.txt"), None);
        // The file is plain lines, with the paths as they were given.
        let text = std::fs::read_to_string(&f.0).unwrap();
        assert!(text.starts_with('#') && text.contains("red\tC:\\Docs\\Invoice.pdf\n") && text.contains("blue\tD:\\Music\n"), "{text}");
        // No file at all (a first start): no tags, and nothing goes wrong.
        let none = Tags::at(std::env::temp_dir().join(format!("glint-tags-test-missing-{}.txt", std::process::id())));
        assert_eq!(none.of("C:\\Docs\\Invoice.pdf"), None);
    }

    #[test]
    fn a_tag_comes_off_again() {
        let f = Scratch::new("remove");
        let mut t = Tags::at(f.0.clone());
        t.set(&paths(&["C:\\a.txt", "C:\\b.txt"]), Some(Tag::Green));
        assert!(t.set(&paths(&["C:\\A.TXT"]), None));
        assert_eq!(t.of("C:\\a.txt"), None);
        assert_eq!(t.of("C:\\b.txt"), Some(Tag::Green));
        assert!(!t.set(&paths(&["C:\\a.txt"]), None), "it had none any more");
        assert_eq!(Tags::at(f.0.clone()).of("C:\\a.txt"), None, "gone from the file too");
        // Another colour replaces the one it had.
        t.set(&paths(&["C:\\b.txt"]), Some(Tag::Purple));
        assert_eq!(Tags::at(f.0.clone()).of("C:\\b.txt"), Some(Tag::Purple));
        // Deleted for good: its tag goes, and those of what was inside - not of a folder whose name
        // only starts the same.
        t.set(&paths(&["C:\\Old", "C:\\Old\\x.txt", "C:\\Older\\y.txt"]), Some(Tag::Red));
        assert!(t.deleted(&paths(&["c:\\OLD\\"])));
        assert_eq!((t.of("C:\\Old"), t.of("C:\\Old\\x.txt"), t.of("C:\\Older\\y.txt")), (None, None, Some(Tag::Red)));
        assert!(!t.deleted(&paths(&["C:\\untagged.txt"])));
        assert_eq!(Tags::at(f.0.clone()).of("C:\\Old\\x.txt"), None);
    }

    #[test]
    fn two_windows_keep_each_others_tags() {
        let f = Scratch::new("windows");
        // Both started before either tagged anything.
        let mut one = Tags::at(f.0.clone());
        let mut two = Tags::at(f.0.clone());
        one.set(&paths(&["C:\\x\\a.txt"]), Some(Tag::Red));
        // The second window hasn't seen that, and writes its own: the first one's stays.
        two.set(&paths(&["C:\\x\\b.txt"]), Some(Tag::Blue));
        let file = Tags::at(f.0.clone());
        assert_eq!((file.of("C:\\x\\a.txt"), file.of("C:\\x\\b.txt")), (Some(Tag::Red), Some(Tag::Blue)));
        // The first window comes to the front: it shows the second one's tag.
        assert!(one.refresh());
        assert_eq!(one.of("C:\\x\\b.txt"), Some(Tag::Blue));
        assert!(!one.refresh(), "nothing new since");
        // A tag taken off in one window is gone in the other one too.
        two.set(&paths(&["C:\\x\\a.txt"]), None);
        assert!(one.refresh());
        assert_eq!(one.of("C:\\x\\a.txt"), None);
        // A move in one window takes along a tag the other one just put on.
        two.set(&paths(&["C:\\x\\c.txt"]), Some(Tag::Yellow));
        assert!(one.moved(&[("C:\\x\\c.txt".into(), "C:\\y\\c.txt".into())]));
        let file = Tags::at(f.0.clone());
        assert_eq!(file.of("C:\\y\\c.txt"), Some(Tag::Yellow));
        assert_eq!(file.of("C:\\x\\c.txt"), None);
        assert_eq!(file.of("C:\\x\\b.txt"), Some(Tag::Blue));
    }

    #[test]
    fn tags_follow_moves_and_renames() {
        let mut t = Tags::default();
        t.set(&paths(&["C:\\Work\\Plan.docx", "C:\\Work\\Old\\notes.txt", "C:\\Work\\Old", "C:\\Workshop\\saw.txt", "C:\\a.txt"]), Some(Tag::Orange));
        t.set(&paths(&["C:\\b.txt"]), Some(Tag::Blue));
        // A rename (F2), with other capitals than the tag was put on with.
        assert!(t.moved(&[("c:\\work\\plan.docx".into(), "C:\\Work\\Plan v2.docx".into())]));
        assert_eq!(t.of("C:\\Work\\Plan v2.docx"), Some(Tag::Orange));
        assert_eq!(t.of("C:\\Work\\Plan.docx"), None);
        // A folder moved: it and everything inside go along - not a folder whose name only starts the same.
        assert!(t.moved(&[("C:\\Work".into(), "D:\\Archive\\Work".into())]));
        assert_eq!(t.of("D:\\Archive\\Work\\Old"), Some(Tag::Orange));
        assert_eq!(t.of("D:\\Archive\\Work\\Old\\notes.txt"), Some(Tag::Orange));
        assert_eq!(t.of("D:\\Archive\\Work\\Plan v2.docx"), Some(Tag::Orange));
        assert_eq!(t.of("C:\\Workshop\\saw.txt"), Some(Tag::Orange));
        // Two names swapped at once (Rename all): the tags swap too.
        assert!(t.moved(&[("C:\\a.txt".into(), "C:\\b.txt".into()), ("C:\\b.txt".into(), "C:\\a.txt".into())]));
        assert_eq!((t.of("C:\\a.txt"), t.of("C:\\b.txt")), (Some(Tag::Blue), Some(Tag::Orange)));
        // Something untagged moved: nothing to do.
        assert!(!t.moved(&[("C:\\c.txt".into(), "C:\\d.txt".into())]));
        // Ctrl+Z: it moves back, and so does its tag.
        assert!(t.moved(&[("D:\\Archive\\Work".into(), "C:\\Work".into())]));
        assert_eq!(t.of("C:\\Work\\Old\\notes.txt"), Some(Tag::Orange));
        // The paths keep their own capitals for the file.
        assert_eq!(t.map.get(&key("C:\\Work\\Old\\notes.txt")).map(|x| x.path.as_str()), Some("C:\\Work\\Old\\notes.txt"));
    }

    #[test]
    fn the_file_is_plain_lines_and_odd_ones_are_passed_over() {
        let map = parse("\u{feff}# a comment\r\nred\tC:\\a.txt\r\nBLUE\tC:\\B.txt\npink\tC:\\c.txt\nno tab here\ngreen\t\n\ngreen\tC:\\d e.txt\n");
        let got: HashMap<&str, Tag> = map.values().map(|t| (t.path.as_str(), t.tag)).collect();
        assert_eq!(got, HashMap::from([("C:\\a.txt", Tag::Red), ("C:\\B.txt", Tag::Blue), ("C:\\d e.txt", Tag::Green)]));
        // And back: the same tags.
        assert_eq!(parse(&text(&map)), map);
    }

    #[test]
    fn a_listing_finds_its_items_tags() {
        let mut t = Tags::default();
        t.set(&paths(&["C:\\Docs\\Report.pdf", "C:\\Docs\\Sub\\deep.txt", "C:\\top.txt"]), Some(Tag::Green));
        let entry = |name: &str, target: Option<&str>| Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: crate::folder::EntryKind::File,
            size: 0,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: String::new(),
            type_name: Arc::from(""),
            target: target.map(str::to_string),
            free: 0,
        };
        let mut docs = t.lookup("C:\\DOCS");
        assert_eq!(docs.of(&entry("Report.pdf", None)), Some(Tag::Green));
        assert_eq!(docs.of(&entry("other.pdf", None)), None);
        // A search result (or an item of a folder open in the tree): by its own path.
        assert_eq!(docs.of(&entry("deep.txt", Some("C:\\Docs\\Sub\\deep.txt"))), Some(Tag::Green));
        // A drive's root ends in a backslash already.
        assert_eq!(t.lookup("C:\\").of(&entry("top.txt", None)), Some(Tag::Green));
        // Nothing tagged: nothing to look for.
        assert_eq!(Tags::default().lookup("C:\\Docs").of(&entry("Report.pdf", None)), None);
    }

    #[test]
    fn the_filter_words_pick_tags() {
        let tags = map_of(&[("C:\\a.txt", Tag::Red), ("C:\\b.txt", Tag::Green), ("C:\\c.txt", Tag::Blue)]);
        let (f, rest) = TagFilter::take("tag:red invoice", || tags.clone()).unwrap();
        assert_eq!(rest, "invoice");
        assert!(f.keeps(|| "C:\\A.txt".into()) && !f.keeps(|| "C:\\b.txt".into()) && !f.keeps(|| "C:\\none.txt".into()));
        // Any tag; the start of a colour's name; two colours.
        let any = TagFilter::take("tag:", || tags.clone()).unwrap().0;
        assert!(any.keeps(|| "C:\\b.txt".into()) && !any.keeps(|| "C:\\none.txt".into()));
        assert!(TagFilter::take("tag:gr", || tags.clone()).unwrap().0.keeps(|| "C:\\b.txt".into()));
        let two = TagFilter::take("tag:red tag:blue", || tags.clone()).unwrap().0;
        assert!(two.keeps(|| "C:\\a.txt".into()) && two.keeps(|| "C:\\c.txt".into()) && !two.keeps(|| "C:\\b.txt".into()));
        // A colour there isn't: nothing.
        assert!(!TagFilter::take("tag:pink", || tags.clone()).unwrap().0.keeps(|| "C:\\a.txt".into()));
        // No tag word: no tag filter (and the tags aren't even asked for).
        assert!(TagFilter::take("tags invoice", || unreachable!()).is_none());
        assert!(in_filter("TAG:Red invoice") && in_filter("x tag:") && !in_filter("tags") && !in_filter(""));
    }

    #[test]
    fn only_items_gone_from_this_computers_own_disks_are_tidied() {
        let here = Scratch::new("tidy-here");
        std::fs::write(&here.0, b"x").unwrap();
        let gone = std::env::temp_dir().join(format!("glint-tags-test-tidy-gone-{}.txt", std::process::id()));
        let path = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let inside = format!("{}\\sub\\x.txt", path(&gone));
        let map = map_of(&[(&path(&here.0), Tag::Red), (&path(&gone), Tag::Blue), (&inside, Tag::Yellow), ("\\\\server\\share\\gone.txt", Tag::Green)]);
        let mut found = gone_items(&map, &HashSet::new());
        found.sort();
        assert_eq!(found, vec![key(&path(&gone)), key(&inside)]);
        // Sent to the Recycle Bin by Glint (what was inside it too): kept, for when it comes back.
        let binned = HashSet::from([key(&path(&gone))]);
        assert!(gone_items(&map, &binned).is_empty());
    }

    #[test]
    fn every_dot_stands_out_from_what_its_drawn_on() {
        // The built-in skins, light and dark, and a few lists no skin may have yet.
        let mut themes: Vec<Theme> = ["amd-radeon", "amd-radeon-light", "nvidia", "nvidia-light", "chrome", "gunmetal"].iter().map(|id| crate::skin::built_in_theme(id)).collect();
        for back in [0xffffff, 0x000000, 0x808080, 0xfff59d, 0x0b2a6f, 0x1e88e5] {
            let mut t = Theme::dark();
            t.panel = Color::hex(back);
            t.text = if luminance(t.panel) > 0.18 { Color::hex(0x000000) } else { Color::hex(0xffffff) };
            // (Selected the same: these lists have no selection colours of their own.)
            t.accent_tint = Color::rgba(0, 0, 0, 0.0);
            t.selection_text = t.text;
            themes.push(t);
        }
        for theme in &themes {
            for selected in [false, true] {
                for tag in Tag::ALL {
                    let back = behind(theme, selected && !theme.era.classic());
                    let (color, edge) = look(theme, tag, selected);
                    let seen = edge.map_or(contrast(color, back), |e| contrast(e, back));
                    assert!(seen >= EDGE_BELOW, "{tag:?} on {back:?} (selected: {selected})");
                }
            }
        }
        // On a white list only yellow needs its edge.
        let mut light = Theme::dark();
        light.panel = Color::hex(0xffffff);
        let edged: Vec<Tag> = Tag::ALL.into_iter().filter(|&t| look(&light, t, false).1.is_some()).collect();
        assert_eq!(edged, vec![Tag::Yellow]);
    }

    fn lines_in(file: &Path) -> usize {
        std::fs::read_to_string(file).unwrap().lines().filter(|l| !l.starts_with('#') && !l.is_empty()).count()
    }

    #[test]
    fn the_same_file_tagged_twice_keeps_one_tag() {
        let f = Scratch::new("twice");
        let mut t = Tags::at(f.0.clone());
        // Twice in one go, with other capitals the second time.
        assert!(t.set(&paths(&["C:\\Docs\\a.txt", "c:\\docs\\A.TXT"]), Some(Tag::Red)));
        assert_eq!(t.map.len(), 1);
        assert_eq!(lines_in(&f.0), 1);
        // Another colour on the same file replaces the first, whatever capitals it is asked with.
        assert!(t.set(&paths(&["C:\\DOCS\\a.txt"]), Some(Tag::Blue)));
        assert_eq!((t.map.len(), t.of("c:\\docs\\a.txt")), (1, Some(Tag::Blue)));
        // Two colours for the same file in the same go: the last one stays.
        t.set(&paths(&["C:\\Docs\\a.txt"]), Some(Tag::Green));
        assert_eq!(Tags::at(f.0.clone()).of("C:\\Docs\\a.txt"), Some(Tag::Green));
        assert_eq!(lines_in(&f.0), 1);
        // Taking it off twice in one go is fine, and an empty list does nothing.
        assert!(t.set(&paths(&["C:\\Docs\\a.txt", "C:\\docs\\A.txt"]), None));
        assert!(!t.set(&[], Some(Tag::Red)));
        assert_eq!(lines_in(&f.0), 0);
    }

    #[test]
    fn a_tagged_folder_does_not_tag_what_is_inside_it() {
        let mut t = Tags::default();
        t.set(&paths(&["C:\\Photos"]), Some(Tag::Purple));
        assert_eq!(t.of("C:\\photos"), Some(Tag::Purple));
        assert_eq!(t.of("C:\\Photos\\a.jpg"), None);
        assert_eq!(t.of("C:\\Photos2"), None);
        // Renamed: the folder's tag goes along; a trailing backslash on either name is no matter.
        assert!(t.moved(&[("C:\\Photos\\".into(), "C:\\Pictures".into())]));
        assert_eq!((t.of("C:\\Photos"), t.of("C:\\Pictures")), (None, Some(Tag::Purple)));
        // Something inside it deleted for good doesn't take the folder's tag.
        assert!(!t.deleted(&paths(&["C:\\Pictures\\a.jpg"])));
        assert_eq!(t.of("C:\\Pictures"), Some(Tag::Purple));
        // The folder itself deleted does.
        assert!(t.deleted(&paths(&["C:\\Pictures"])));
        assert!(t.map.is_empty());
    }

    #[test]
    fn a_folder_and_a_namesake_with_a_longer_name_are_told_apart() {
        let mut t = Tags::default();
        t.set(&paths(&["D:\\x\\f.txt", "D:\\xy\\f.txt", "D:\\x y\\f.txt", "D:\\x"]), Some(Tag::Red));
        assert!(t.moved(&[("D:\\x".into(), "D:\\z".into())]));
        assert_eq!((t.of("D:\\z"), t.of("D:\\z\\f.txt")), (Some(Tag::Red), Some(Tag::Red)));
        assert_eq!((t.of("D:\\xy\\f.txt"), t.of("D:\\x y\\f.txt")), (Some(Tag::Red), Some(Tag::Red)));
        assert!(t.deleted(&paths(&["D:\\z"])));
        assert_eq!(t.map.len(), 2, "D:\\xy and D:\\x y stay");
    }

    #[test]
    fn a_damaged_or_empty_tag_file_gives_no_tags_and_no_trouble() {
        for (name, bytes) in [
            ("empty", Vec::new()),
            ("header-only", HEADER.as_bytes().to_vec()),
            ("garbage", vec![0xff, 0xfe, 0x00, 0x80, 0x81, 0x00, 0x00, 0x0a, 0xc3, 0x28]),
            ("utf16", "red\tC:\\a.txt\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect()),
            ("only-tabs", b"\t\t\t\n\t\n".to_vec()),
            ("one-long-line", vec![b'x'; 200_000]),
        ] {
            let f = Scratch::new(&format!("damaged-{name}"));
            std::fs::write(&f.0, &bytes).unwrap();
            let mut t = Tags::at(f.0.clone());
            assert!(t.map.is_empty(), "{name}");
            assert_eq!(t.of("C:\\a.txt"), None, "{name}");
            // It still works: a tag goes on, is written whole, and reads back.
            assert!(t.set(&paths(&["C:\\a.txt"]), Some(Tag::Green)), "{name}");
            assert_eq!(Tags::at(f.0.clone()).of("C:\\a.txt"), Some(Tag::Green), "{name}");
            assert_eq!(lines_in(&f.0), 1, "{name}");
        }
        // Half damaged: the good lines are kept.
        let f = Scratch::new("half");
        std::fs::write(&f.0, b"red\tC:\\a.txt\n\xff\xfe\x00junk\nblue\tC:\\b.txt\nred C:\\no tab.txt\ngreen\tC:\\caf\xc3\xa9.txt\n").unwrap();
        let t = Tags::at(f.0.clone());
        assert_eq!((t.of("C:\\a.txt"), t.of("C:\\b.txt"), t.of("C:\\caf\u{e9}.txt")), (Some(Tag::Red), Some(Tag::Blue), Some(Tag::Green)));
        assert_eq!(t.of("C:\\no tab.txt"), None);
        // A file that is taken away is the same as no tags; one put back is read again.
        let mut one = Tags::at(f.0.clone());
        std::fs::remove_file(&f.0).unwrap();
        assert!(one.refresh() && one.map.is_empty());
        std::fs::write(&f.0, b"yellow\tC:\\y.txt\n").unwrap();
        assert!(one.refresh());
        assert_eq!(one.of("C:\\y.txt"), Some(Tag::Yellow));
        // Another window wiped the file empty: the tags go here too.
        std::fs::write(&f.0, b"").unwrap();
        assert!(one.refresh() && one.map.is_empty());
    }

    #[test]
    fn a_file_renamed_to_a_tagged_files_name_takes_that_name_with_its_own_tag() {
        let f = Scratch::new("onto");
        let mut t = Tags::at(f.0.clone());
        t.set(&paths(&["C:\\a.txt"]), Some(Tag::Red));
        t.set(&paths(&["C:\\b.txt"]), Some(Tag::Blue));
        // a.txt replaced b.txt: one file, one tag - not a tag left over for a file that is gone.
        assert!(t.moved(&[("C:\\a.txt".into(), "C:\\b.txt".into())]));
        assert_eq!((t.of("C:\\a.txt"), t.of("C:\\b.txt")), (None, Some(Tag::Red)));
        assert_eq!(t.map.len(), 1);
        assert_eq!(lines_in(&f.0), 1);
        // An untagged file renamed onto a tagged name changes nothing (the tag is on the name).
        assert!(!t.moved(&[("C:\\c.txt".into(), "C:\\d.txt".into())]));
        // Three in a ring (a to b, b to c, c to a): each takes the next one's tag along.
        let mut ring = Tags::default();
        ring.set(&paths(&["C:\\a"]), Some(Tag::Red));
        ring.set(&paths(&["C:\\b"]), Some(Tag::Green));
        ring.set(&paths(&["C:\\c"]), Some(Tag::Blue));
        ring.moved(&[("C:\\a".into(), "C:\\b".into()), ("C:\\b".into(), "C:\\c".into()), ("C:\\c".into(), "C:\\a".into())]);
        assert_eq!((ring.of("C:\\b"), ring.of("C:\\c"), ring.of("C:\\a")), (Some(Tag::Red), Some(Tag::Green), Some(Tag::Blue)));
        // Renamed to the same name, or to the same name in other capitals: the tag stays.
        assert!(ring.moved(&[("C:\\a".into(), "C:\\a".into())]));
        assert!(ring.moved(&[("C:\\a".into(), "C:\\A".into())]));
        assert_eq!((ring.of("C:\\a"), ring.map.len()), (Some(Tag::Blue), 3));
        assert_eq!(ring.map.get(&key("C:\\a")).map(|x| x.path.as_str()), Some("C:\\A"), "the file has its new capitals");
    }

    #[test]
    fn moving_back_and_forth_and_undoing_leaves_the_tags_where_they_were() {
        let f = Scratch::new("undo");
        let mut t = Tags::at(f.0.clone());
        t.set(&paths(&["C:\\Work\\a.txt", "C:\\Work\\sub\\b.txt", "C:\\Work\\sub"]), Some(Tag::Yellow));
        let before = t.map.clone();
        for _ in 0..3 {
            t.moved(&[("C:\\Work".into(), "D:\\Moved\\Work".into())]);
            assert_eq!(t.of("D:\\Moved\\Work\\sub\\b.txt"), Some(Tag::Yellow));
            t.moved(&[("D:\\Moved\\Work".into(), "C:\\Work".into())]);
        }
        assert_eq!(*t.map, *before);
        assert_eq!(*Tags::at(f.0.clone()).map, *before, "and in the file");
        // Redo after undo, then the same name from somewhere else is not tagged.
        t.moved(&[("C:\\Work".into(), "C:\\Done".into())]);
        assert_eq!((t.of("C:\\Done\\a.txt"), t.of("C:\\Work\\a.txt")), (Some(Tag::Yellow), None));
        // A drive's root as the place something moves out of or into.
        t.moved(&[("C:\\Done\\a.txt".into(), "E:\\a.txt".into())]);
        assert_eq!(t.of("E:\\a.txt"), Some(Tag::Yellow));
        t.moved(&[("E:\\a.txt".into(), "C:\\".to_string() + "a.txt")]);
        assert_eq!(t.of("C:\\a.txt"), Some(Tag::Yellow));
    }

    #[test]
    fn long_odd_and_emoji_names_survive_a_save_and_a_restart() {
        let f = Scratch::new("odd");
        let long = format!("C:\\{}\\{}.txt", "folder".repeat(30), "n".repeat(250));
        assert!(long.len() > 400);
        let odd = [
            long.as_str(),
            "C:\\caf\u{e9}\\\u{1f600} smile.png",
            "C:\\#hash\\100%.txt",
            "C:\\a. b\\trailing dot.",
            "C:\\  leading spaces.txt",
            "C:\\trailing space .txt",
            "C:\\semi;colon,comma\\[x].txt",
            "\\\\server\\share\\f\u{fc}r.txt",
            "C:\\\u{130}stanbul\\I.txt",
        ];
        let mut t = Tags::at(f.0.clone());
        assert!(t.set(&odd.iter().map(|s| s.to_string()).collect::<Vec<_>>(), Some(Tag::Orange)));
        let again = Tags::at(f.0.clone());
        for p in odd {
            assert_eq!(again.of(p), Some(Tag::Orange), "{p}");
        }
        assert_eq!(lines_in(&f.0), odd.len());
        // Windows lines: the same file, as Notepad would save it (CRLF).
        let crlf = std::fs::read_to_string(&f.0).unwrap().replace('\n', "\r\n");
        std::fs::write(&f.0, crlf).unwrap();
        let again = Tags::at(f.0.clone());
        for p in odd {
            assert_eq!(again.of(p), Some(Tag::Orange), "{p} after CRLF");
        }
    }

    #[test]
    fn thousands_of_tags_are_saved_found_and_moved_quickly() {
        let f = Scratch::new("many");
        let mut t = Tags::at(f.0.clone());
        let start = std::time::Instant::now();
        let all: Vec<String> = (0..20_000).map(|i| format!("C:\\Big\\dir{}\\file {i}.txt", i % 50)).collect();
        assert!(t.set(&all, Some(Tag::Blue)));
        assert_eq!(t.map.len(), 20_000);
        assert_eq!(lines_in(&f.0), 20_000);
        assert_eq!(Tags::at(f.0.clone()).map.len(), 20_000);
        // A folder with 400 tagged files moved, then all of them deleted for good.
        assert!(t.moved(&[("C:\\Big\\dir7".into(), "C:\\Big\\moved".into())]));
        assert_eq!(t.of("C:\\Big\\moved\\file 57.txt"), Some(Tag::Blue));
        assert_eq!(t.of("C:\\Big\\dir7\\file 57.txt"), None);
        assert!(t.deleted(&paths(&["C:\\Big"])));
        assert!(t.map.is_empty() && lines_in(&f.0) == 0);
        assert!(start.elapsed() < std::time::Duration::from_secs(20), "{:?}", start.elapsed());
        // Listing a folder with many tagged items: one look-up each.
        let mut t = Tags::default();
        t.set(&(0..5_000).map(|i| format!("C:\\Many\\{i}.txt")).collect::<Vec<_>>(), Some(Tag::Green));
        let mut look = t.lookup("C:\\Many");
        let entry = |name: String| Entry {
            lower: name.to_lowercase(),
            name,
            kind: crate::folder::EntryKind::File,
            size: 0,
            modified: 0,
            created: 0,
            attributes: 0,
            ext: String::new(),
            type_name: Arc::from(""),
            target: None,
            free: 0,
        };
        assert_eq!((0..5_000).filter(|i| look.of(&entry(format!("{i}.TXT"))).is_some()).count(), 5_000);
        assert_eq!(look.of(&entry("5000.txt".into())), None);
    }

    #[test]
    fn a_tag_filter_with_a_folder_and_an_empty_map() {
        // Nothing tagged: nothing is kept, and the path isn't even built.
        let none = TagFilter::take("tag:", || Arc::new(Map::new())).unwrap().0;
        assert!(!none.keeps(|| unreachable!()));
        // A tagged folder is found like a file.
        let tags = map_of(&[("C:\\Photos", Tag::Blue)]);
        let f = TagFilter::take("tag:blue", || tags.clone()).unwrap().0;
        assert!(f.keeps(|| "c:\\PHOTOS".into()) && !f.keeps(|| "C:\\Photos\\a.jpg".into()));
        // A colour whose start nobody has, and "tag:" with a colon in the colour.
        assert!(!TagFilter::take("tag:b:l", || tags.clone()).unwrap().0.keeps(|| "C:\\Photos".into()));
        assert!(!TagFilter::take("tag:blue2", || tags.clone()).unwrap().0.keeps(|| "C:\\Photos".into()));
        // The word alone, or with accents or emoji after it, never panics.
        for text in ["tag:\u{e9}", "tag:\u{1f600}", "tag:tag:red", " tag:red  tag:", "x tag:r y"] {
            let _ = TagFilter::take(text, || tags.clone());
            let _ = in_filter(text);
        }
        assert!(!in_filter("ta\u{e9}g:"));
    }
}
