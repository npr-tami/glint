//! Searching subfolders: the "Subfolders" switch next to the filter (Ctrl+Shift+F). With it on,
//! what is typed in the filter is looked for in the folder on screen and everything below it:
//! - the same rules as the filter (several words, or a pattern like `*.pdf`; folder.rs `NameFilter`);
//! - hidden items only when hidden files are shown, and nothing inside hidden folders then either;
//! - junctions and symbolic links are never followed (they can loop, or lead to the same files
//!   twice);
//! - at most [`LIMIT`] results, so a search of a whole drive can't eat the memory (and the list
//!   of folders still to go through is kept small too);
//! - folders that can't be read (no permission) are skipped without a word.
//!
//! The search runs on a thread of its own. Results go to the window in batches a few times a second
//! (app.rs takes them in, the list shows them with a Folder column), so the window never waits and
//! isn't flooded either. Each search has a cancel flag: typing more, switching Subfolders off,
//! leaving the folder or closing the tab stops it at once (dropping the [`Search`] does it).
//!
//! The results are ordinary entries whose `target` is their full path (folder.rs), so opening,
//! copying, deleting and the right-click menus act on the very file found, wherever it is.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW, FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
};

use crate::folder::{self, Entry, Folder, NameFilter};
use crate::shell::{self, ComGuard};

/// The most results one search shows.
pub const LIMIT: usize = 20_000;
/// Folders waiting to be searched, at most (about 150 bytes each), before the search goes deep
/// first instead of near first.
const MAX_WAITING_FOLDERS: usize = 50_000;
/// Results go to the window at most this often while a search runs.
const BATCH_EVERY: Duration = Duration::from_millis(100);
/// A search started by typing waits this long first, so each key doesn't start a new walk through
/// the disk.
pub const TYPING_PAUSE: Duration = Duration::from_millis(250);

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;

/// A tab's search (app.rs keeps it in `Tab::search`). Dropping it stops its thread.
pub struct Search {
    /// The folder searched (the tab's folder).
    pub root: String,
    /// What it looks for (the filter text).
    pub query: String,
    /// Tells this search's results from an older one's.
    pub generation: u64,
    cancel: Arc<AtomicBool>,
    /// Results of this search have come in (until then the list keeps showing the previous
    /// search's, narrowed down by the filter, so typing doesn't make it flash empty).
    pub fresh: bool,
    /// The whole folder tree has been searched (or LIMIT reached).
    pub done: bool,
    /// Stopped at LIMIT results.
    pub capped: bool,
    /// When it was asked for (for the log).
    pub started: Instant,
    /// The folder's own listing, put aside while the results are shown (it comes back when the
    /// search ends).
    pub listing: Folder,
}

impl Search {
    /// Stops the thread (it notices before its next item).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// A search with no thread behind it, for tests elsewhere.
#[cfg(test)]
pub fn test_search(root: &str) -> Search {
    Search {
        root: root.to_string(),
        query: String::new(),
        generation: 1,
        cancel: Arc::new(AtomicBool::new(false)),
        fresh: false,
        done: false,
        capped: false,
        started: Instant::now(),
        listing: Folder::default(),
    }
}

/// Results the search thread found since the last batch.
pub struct Batch {
    pub entries: Vec<Entry>,
    /// The search is over (everything searched, or LIMIT reached).
    pub done: bool,
    pub capped: bool,
}

/// What to search for, and how.
pub struct Ask {
    /// The folder to search (and everything below it).
    pub root: String,
    /// The filter text.
    pub query: String,
    pub generation: u64,
    /// Hidden items too (and protected operating-system files), as the list shows them.
    pub show_hidden: bool,
    pub show_system: bool,
    /// Wait TYPING_PAUSE first (the filter is being typed).
    pub pause: bool,
}

/// Starts a search on a new thread. `send` gets the batches on that thread; when it returns false
/// (the window is gone) the search stops. `listing` (the folder's own) is kept in the search.
/// Returns the search to keep - or, if no thread could be had, the listing back.
#[allow(clippy::result_large_err)] // (the listing back, rarely: no thread could be had)
pub fn start(ask: Ask, listing: Folder, mut send: impl FnMut(Batch) -> bool + Send + 'static) -> Result<Search, Folder> {
    let Ask { root, query, generation, show_hidden, show_system, pause } = ask;
    let cancel = Arc::new(AtomicBool::new(false));
    let (flag, dir, text) = (cancel.clone(), root.clone(), query.clone());
    let started = shell::background("search subfolders", move || {
        // Type names ("Text Document") come from the shell, which needs COM.
        let _com = ComGuard::init();
        if pause && !wait(&flag, TYPING_PAUSE) {
            return;
        }
        let filter = NameFilter::new(&text);
        let mut found = Vec::new();
        let mut last_sent = Instant::now();
        let outcome = walk(&dir, &filter, show_hidden, show_system, &flag, &mut |e| {
            found.push(e);
            if last_sent.elapsed() >= BATCH_EVERY {
                last_sent = Instant::now();
                return send(Batch { entries: std::mem::take(&mut found), done: false, capped: false });
            }
            true
        });
        if let Some(capped) = outcome {
            send(Batch { entries: found, done: true, capped });
        }
    });
    if !started {
        return Err(listing);
    }
    Ok(Search { root, query, generation, cancel, fresh: false, done: false, capped: false, started: Instant::now(), listing })
}

/// Sleeps `time` unless cancelled first. True if not cancelled.
fn wait(cancel: &AtomicBool, time: Duration) -> bool {
    let end = Instant::now() + time;
    while Instant::now() < end {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    !cancel.load(Ordering::SeqCst)
}

/// Goes through `root` and its subfolders, nearest first, handing each match to `found` (which
/// returns false to stop). Returns None when stopped (cancelled, or `found` said so), otherwise
/// whether LIMIT was reached.
fn walk(
    root: &str,
    filter: &NameFilter,
    show_hidden: bool,
    show_system: bool,
    cancel: &AtomicBool,
    found: &mut dyn FnMut(Entry) -> bool,
) -> Option<bool> {
    let mut folders: VecDeque<String> = VecDeque::from([root.to_string()]);
    let mut types: HashMap<String, Arc<str>> = HashMap::new();
    let mut count = 0usize;
    while let Some(dir) = folders.pop_front() {
        let mut stop = false;
        let listed = list(&dir, cancel, &mut |data: &WIN32_FIND_DATAW| {
            let attrs = data.dwFileAttributes;
            let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
            let name = String::from_utf16_lossy(&data.cFileName[..len]);
            if name == "." || name == ".." || !folder::visible(attrs, show_hidden, show_system) {
                return true;
            }
            let is_dir = attrs & ATTR_DIRECTORY != 0;
            if is_dir && attrs & ATTR_REPARSE_POINT == 0 {
                // Nearest folders first - but in a tree so wide that the folders waiting would take
                // a lot of memory, the new ones are gone through first (that list then stops growing).
                let sub = folder::join(&dir, &name);
                if folders.len() < MAX_WAITING_FOLDERS {
                    folders.push_back(sub);
                } else {
                    folders.push_front(sub);
                }
            }
            if !filter.matches(&name.to_lowercase()) {
                return true;
            }
            let Some(mut e) = folder::entry_from(data, &mut types) else { return true };
            e.target = Some(folder::join(&dir, &name));
            count += 1;
            if !found(e) {
                stop = true;
                return false;
            }
            if count >= LIMIT {
                stop = true;
                return false;
            }
            true
        });
        if stop || !listed {
            return if count >= LIMIT && !cancel.load(Ordering::SeqCst) { Some(true) } else { None };
        }
    }
    Some(false)
}

/// Lists one folder, calling `each` for every item until it returns false. Returns false when
/// cancelled. A folder that can't be read is simply skipped.
fn list(dir: &str, cancel: &AtomicBool, each: &mut dyn FnMut(&WIN32_FIND_DATAW) -> bool) -> bool {
    let pattern = folder::wide_path(&folder::join(dir, "*"));
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `pattern` is NUL-terminated and outlives the call; `data` is a valid WIN32_FIND_DATAW.
    let handle = unsafe {
        FindFirstFileExW(PCWSTR(pattern.as_ptr()), FindExInfoBasic, &mut data as *mut _ as *mut _, FindExSearchNameMatch, None, FIND_FIRST_EX_LARGE_FETCH)
    };
    let Ok(handle) = handle else { return !cancel.load(Ordering::SeqCst) };
    let mut go_on = true;
    loop {
        if cancel.load(Ordering::SeqCst) {
            go_on = false;
            break;
        }
        if !each(&data) {
            go_on = false;
            break;
        }
        // SAFETY: `handle` is the live search handle from above; `data` is valid.
        if unsafe { FindNextFileW(handle, &mut data) }.is_err() {
            break;
        }
    }
    // SAFETY: closes the handle we opened; it is not used afterwards.
    unsafe {
        let _ = FindClose(handle);
    }
    go_on
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// A small tree under the temporary folder, removed afterwards.
    struct Tree(std::path::PathBuf);

    impl Tree {
        fn new(name: &str, files: &[&str]) -> Tree {
            let root = std::env::temp_dir().join(format!("glint-search-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for f in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                if !f.ends_with('/') {
                    std::fs::write(&p, b"x").unwrap();
                }
            }
            Tree(root)
        }
        fn path(&self) -> String {
            self.0.to_string_lossy().replace('/', "\\")
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn ask(root: &str, query: &str, pause: bool) -> Ask {
        Ask { root: root.to_string(), query: query.to_string(), generation: 1, show_hidden: false, show_system: false, pause }
    }

    fn search(root: &str, query: &str) -> (Vec<String>, bool) {
        let (tx, rx) = channel();
        let s = start(ask(root, query, false), Folder::default(), move |b| tx.send(b).is_ok());
        assert!(s.is_ok());
        let mut names = Vec::new();
        loop {
            let b = rx.recv_timeout(Duration::from_secs(20)).expect("the search ends");
            names.extend(b.entries.iter().map(|e| e.target.clone().unwrap_or_default()));
            if b.done {
                names.sort();
                return (names, b.capped);
            }
        }
    }

    #[test]
    fn finds_matches_in_subfolders_with_their_full_paths() {
        let t = Tree::new("find", &["a/report-2024.pdf", "a/b/report-2023.pdf", "a/b/notes.txt", "top.pdf", "c/"]);
        let root = t.path();
        let (found, capped) = search(&root, "*.pdf");
        assert!(!capped);
        assert_eq!(found, vec![format!("{root}\\a\\b\\report-2023.pdf"), format!("{root}\\a\\report-2024.pdf"), format!("{root}\\top.pdf")]);
        // Several words, in any order; folders are found too.
        let (found, _) = search(&root, "2023 REPORT");
        assert_eq!(found, vec![format!("{root}\\a\\b\\report-2023.pdf")]);
        let (found, _) = search(&root, "b");
        assert_eq!(found, vec![format!("{root}\\a\\b")]);
    }

    #[test]
    fn a_cancelled_search_stops() {
        let t = Tree::new("cancel", &["x/1.txt", "x/2.txt"]);
        let (tx, rx) = channel();
        let s = start(ask(&t.path(), "txt", true), Folder::default(), move |b| tx.send(b).is_ok()).ok().unwrap();
        drop(s); // cancelled while it waits for typing to pause
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_err(), "nothing comes after cancelling");
    }
}
