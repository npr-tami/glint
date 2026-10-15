//! Searching subfolders: the "Subfolders" switch next to the filter (Ctrl+Shift+F). With it on,
//! what is typed in the filter is looked for in the folder on screen and everything below it:
//! - the same rules as the filter (several words, or a pattern like `*.pdf`; folder.rs `NameFilter`);
//! - hidden items only when hidden files are shown, and nothing inside hidden folders then either;
//! - junctions and symbolic links are never followed (they can loop, or lead to the same files
//!   twice);
//! - in This PC (a root of ""), every ready local and removable drive is searched;
//! - at most [`LIMIT`] results, so a search of a whole drive can't eat the memory (and the list
//!   of folders still to go through is kept small too);
//! - folders that can't be read (no permission) are skipped without a word.
//!
//! The search runs on threads of its own: a few list folders at the same time (a disk answers
//! several at once faster than one by one, so a whole drive goes quicker), and one gathers what
//! they find. Results go to the window in batches a few times a second (app.rs takes them in, the
//! list shows them with a Folder column), so the window never waits and isn't flooded either. Each search has a cancel flag: typing more, switching Subfolders off,
//! leaving the folder or closing the tab stops it at once (dropping the [`Search`] does it).
//!
//! The results are ordinary entries whose `target` is their full path (folder.rs), so opening,
//! copying, deleting and the right-click menus act on the very file found, wherever it is.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
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
/// Threads one search lists folders with at most (more mostly just queue up at the disk).
const MAX_THREADS: usize = 4;
/// Results go to the window at most this often while a search runs.
const BATCH_EVERY: Duration = Duration::from_millis(100);
/// A search started by typing waits this long first, so each key doesn't start a new walk through
/// the disk.
pub const TYPING_PAUSE: Duration = Duration::from_millis(250);

const ATTR_DIRECTORY: u32 = 0x10;
const ATTR_REPARSE_POINT: u32 = 0x400;
/// The bit of a reparse point's tag that says it only stands for another place (a junction,
/// symbolic link or mount point).
const TAG_NAME_SURROGATE: u32 = 0x2000_0000;

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
    /// A search for duplicate files (dupes.rs), not for names: what it found, once it's done.
    pub dupes: Option<crate::dupes::Summary>,
}

impl Search {
    /// It looks for duplicate files ("Find duplicates"), not for names.
    pub fn is_dupes(&self) -> bool {
        self.dupes.is_some()
    }
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
        dupes: None,
    }
}

/// Whether files with names beyond plain ASCII (accents, emoji) can be made in `dir`, for tests
/// elsewhere: always on Windows, but not under Wine run without a UTF-8 locale.
#[cfg(test)]
pub fn odd_names_work(dir: &std::path::Path) -> bool {
    let probe = dir.join("caf\u{e9} \u{1f600}.probe");
    let made = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    made
}

/// Results the search thread found since the last batch.
pub struct Batch {
    pub entries: Vec<Entry>,
    /// The search is over (everything searched, or LIMIT reached).
    pub done: bool,
    pub capped: bool,
    /// The duplicate finder's last batch: what it found in all (None when it stopped at LIMIT).
    pub summary: Option<crate::dupes::Summary>,
}

/// What the duplicate finder's thread hands the thread that sends its batches.
enum Found {
    /// A set of files with the same content.
    Set(Vec<Entry>),
    /// All looked through: what it found in all.
    End(crate::dupes::Summary),
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
    /// Look for duplicate files instead of names (dupes.rs; `query` isn't used).
    pub dupes: bool,
}

/// Starts a search on a new thread. `send` gets the batches on that thread; when it returns false
/// (the window is gone) the search stops. `listing` (the folder's own) is kept in the search.
/// A `root` of "" (This PC) searches every ready local and removable drive.
/// Returns the search to keep - or, if no thread could be had, the listing back.
#[allow(clippy::result_large_err)] // (the listing back, rarely: no thread could be had)
pub fn start(ask: Ask, listing: Folder, mut send: impl FnMut(Batch) -> bool + Send + 'static) -> Result<Search, Folder> {
    let Ask { root, query, generation, show_hidden, show_system, pause, dupes } = ask;
    let cancel = Arc::new(AtomicBool::new(false));
    let (flag, dir, text) = (cancel.clone(), root.clone(), query.clone());
    if dupes {
        // One thread looks for the sets, and another sends them to the window at most every
        // BATCH_EVERY, like a name search's results: each batch has the window rebuild its whole
        // list, so thousands of sets sent one by one would freeze it.
        let (tx, rx) = channel::<Found>();
        let stop = cancel.clone();
        let gathering = shell::background("find duplicates", move || {
            // Each set's files carry the set's number in `free` (folder.rs), which the list draws
            // them by. At most LIMIT files, as in a search (the set that gets there is shown whole).
            let (mut set, mut found, mut files, mut last_sent) = (0u64, Vec::new(), 0, Instant::now());
            loop {
                let next = rx.recv_timeout(BATCH_EVERY);
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let summary = match next {
                    Ok(Found::Set(mut group)) => {
                        set += 1;
                        files += group.len();
                        group.iter_mut().for_each(|e| e.free = set);
                        found.append(&mut group);
                        None
                    }
                    Ok(Found::End(summary)) => Some(summary),
                    Err(RecvTimeoutError::Timeout) => None,
                    // Stopped (or the finder couldn't start): nothing more comes.
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                let capped = files > LIMIT; // exactly LIMIT is everything, unless another set follows
                if summary.is_some() || capped {
                    if capped {
                        stop.store(true, Ordering::SeqCst); // the rest isn't looked for
                    }
                    send(Batch { entries: found, done: true, capped, summary });
                    return;
                }
                if !found.is_empty() && last_sent.elapsed() >= BATCH_EVERY {
                    last_sent = Instant::now();
                    if !send(Batch { entries: std::mem::take(&mut found), done: false, capped: false, summary: None }) {
                        stop.store(true, Ordering::SeqCst);
                        return;
                    }
                }
            }
        });
        let started = gathering
            && shell::background("find duplicates", move || {
                let _com = ComGuard::init();
                if let Some(summary) = crate::dupes::find(&dir, show_hidden, show_system, &flag, &mut |group| tx.send(Found::Set(group)).is_ok()) {
                    let _ = tx.send(Found::End(summary));
                }
            });
        if !started {
            return Err(listing);
        }
        let found = Some(crate::dupes::Summary::default());
        return Ok(Search { root, query, generation, cancel, fresh: false, done: false, capped: false, started: Instant::now(), listing, dupes: found });
    }
    let started = shell::background("search subfolders", move || {
        if pause && !wait(&flag, TYPING_PAUSE) {
            return;
        }
        let roots = if dir.is_empty() { drive_roots() } else { vec![dir] };
        let filter = NameFilter::new(&text);
        let mut found = Vec::new();
        let mut last_sent = Instant::now();
        let outcome = walk(roots, &filter, show_hidden, show_system, &flag, &mut |e| {
            if let Some(e) = e {
                found.push(e);
            }
            if !found.is_empty() && last_sent.elapsed() >= BATCH_EVERY {
                last_sent = Instant::now();
                return send(Batch { entries: std::mem::take(&mut found), done: false, capped: false, summary: None });
            }
            true
        });
        if let Some(capped) = outcome {
            send(Batch { entries: found, done: true, capped, summary: None });
        }
    });
    if !started {
        return Err(listing);
    }
    Ok(Search { root, query, generation, cancel, fresh: false, done: false, capped: false, started: Instant::now(), listing, dupes: None })
}

/// The drives a search of This PC goes through: the local and removable ones that are ready (not
/// network drives, which would be slow, nor an empty DVD drive).
fn drive_roots() -> Vec<String> {
    folder::read_drives()
        .into_iter()
        .filter(|d| d.ready && matches!(d.kind_name, "Local Disk" | "Removable Disk" | "RAM Disk"))
        .map(|d| d.root)
        .collect()
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

/// The folders still to be searched, shared by the threads of one search.
struct Work {
    waiting: Mutex<Waiting>,
    /// Rung when folders are added, or when the last busy thread runs out of work.
    ring: Condvar,
    /// Results handed out so far (never more than LIMIT).
    count: AtomicUsize,
    /// Set to stop every thread: LIMIT reached, or the results aren't wanted any more.
    halt: AtomicBool,
}

struct Waiting {
    folders: VecDeque<String>,
    /// Threads going through a folder (or folders of their own); while any is, more may come.
    busy: usize,
}

impl Work {
    /// The next folder to go through, waiting for one if other threads may still find some. None
    /// when the whole tree is done, or the search was stopped.
    fn take(&self, cancel: &AtomicBool) -> Option<String> {
        let mut w = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if cancel.load(Ordering::SeqCst) || self.halt.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(dir) = w.folders.pop_front() {
                w.busy += 1;
                return Some(dir);
            }
            if w.busy == 0 {
                return None;
            }
            // Woken when there is work or all is done; the time limit is only so cancelling is
            // noticed by a thread with nothing to do.
            w = self.ring.wait_timeout(w, Duration::from_millis(30)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Hands folders found to the other threads, as long as the shared list has room; the rest
    /// stay in `own` for this thread to go through itself (deep first), which keeps the memory
    /// small in a very wide tree.
    fn share(&self, own: &mut Vec<String>) {
        if own.is_empty() {
            return;
        }
        let mut w = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        let room = MAX_WAITING_FOLDERS.saturating_sub(w.folders.len());
        let moved = own.len().min(room);
        let from = own.len() - moved;
        w.folders.extend(own.drain(from..));
        if moved > 0 {
            self.ring.notify_all();
        }
    }

    /// This thread has nothing of its own left to do.
    fn idle(&self) {
        let mut w = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        w.busy = w.busy.saturating_sub(1);
        if w.busy == 0 && w.folders.is_empty() {
            self.ring.notify_all();
        }
    }
}

/// Goes through `roots` and their subfolders with a few threads at once (listing folders is mostly
/// waiting for the disk, and a disk answers several questions at a time faster than one by one),
/// nearest first more or less. Each match goes to `found` on this thread; `found` is also called
/// with None every BATCH_EVERY while nothing new turns up, and returns false to stop. Returns
/// None when stopped (cancelled, or `found` said so), otherwise whether LIMIT was reached.
fn walk(
    roots: Vec<String>,
    filter: &NameFilter,
    show_hidden: bool,
    show_system: bool,
    cancel: &Arc<AtomicBool>,
    found: &mut dyn FnMut(Option<Entry>) -> bool,
) -> Option<bool> {
    let work = Arc::new(Work {
        waiting: Mutex::new(Waiting { folders: roots.into(), busy: 0 }),
        ring: Condvar::new(),
        count: AtomicUsize::new(0),
        halt: AtomicBool::new(false),
    });
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, MAX_THREADS);
    let (tx, rx) = channel::<Entry>();
    let mut running = 0;
    for _ in 0..threads {
        let (work, cancel, tx, filter) = (work.clone(), cancel.clone(), tx.clone(), filter.clone());
        if shell::background("search subfolders", move || search_folders(&work, &filter, show_hidden, show_system, &cancel, &tx)) {
            running += 1;
        }
    }
    if running == 0 {
        // No thread could be had: this one goes through the folders itself.
        search_folders(&work, filter, show_hidden, show_system, cancel, &tx);
    }
    drop(tx);
    let stop = |work: &Work| {
        work.halt.store(true, Ordering::SeqCst);
        None
    };
    loop {
        let next = match rx.recv_timeout(BATCH_EVERY) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if cancel.load(Ordering::SeqCst) || !found(next) {
            return stop(&work);
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return None;
    }
    // The count only goes past LIMIT when one more match turned up: exactly LIMIT isn't capped.
    Some(work.count.load(Ordering::SeqCst) > LIMIT)
}

/// One of a search's threads: takes folders from `work` until there are none left, sending each
/// match to `tx`.
fn search_folders(work: &Work, filter: &NameFilter, show_hidden: bool, show_system: bool, cancel: &AtomicBool, tx: &Sender<Entry>) {
    // Type names ("Text Document") come from the shell, which needs COM.
    let _com = ComGuard::init();
    let mut types: HashMap<String, Arc<str>> = HashMap::new();
    let mut own: Vec<String> = Vec::new();
    while let Some(first) = work.take(cancel) {
        let mut next = Some(first);
        while let Some(dir) = next {
            let mut subs = Vec::new();
            let go_on = list(&dir, cancel, &mut |data: &WIN32_FIND_DATAW| {
                if work.halt.load(Ordering::Relaxed) {
                    return false;
                }
                let attrs = data.dwFileAttributes;
                let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
                let name = String::from_utf16_lossy(&data.cFileName[..len]);
                if name == "." || name == ".." || !folder::visible(attrs, show_hidden, show_system) {
                    return true;
                }
                if attrs & ATTR_DIRECTORY != 0 && !is_link(data) {
                    subs.push(folder::join(&dir, &name));
                }
                if !filter.keeps(&name.to_lowercase(), || folder::join(&dir, &name)) {
                    return true;
                }
                let Some(mut e) = folder::entry_from(data, &mut types) else { return true };
                e.target = Some(folder::join(&dir, &name));
                // Several threads find things at once; the count makes sure no more than LIMIT go out.
                if work.count.fetch_add(1, Ordering::SeqCst) >= LIMIT {
                    work.halt.store(true, Ordering::SeqCst);
                    return false;
                }
                if tx.send(e).is_err() {
                    work.halt.store(true, Ordering::SeqCst);
                    return false;
                }
                true
            });
            if !go_on || work.halt.load(Ordering::SeqCst) {
                work.halt.store(true, Ordering::SeqCst);
                work.idle();
                return;
            }
            // Nearer folders first: the new ones go to the back of the shared list, and only what
            // doesn't fit there is gone through by this thread before it takes the next.
            own.append(&mut subs);
            work.share(&mut own);
            next = own.pop();
        }
        work.idle();
    }
}

/// Lists one folder, calling `each` for every item until it returns false. Returns false when
/// cancelled. A folder that can't be read is simply skipped.
pub(crate) fn list(dir: &str, cancel: &AtomicBool, each: &mut dyn FnMut(&WIN32_FIND_DATAW) -> bool) -> bool {
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

/// Whether an item [`list`] found is a junction, symbolic link or mount point, which a walk through
/// subfolders doesn't go into (they can loop, or lead to the same files twice). Other reparse
/// points, like OneDrive and Dropbox folders whose files are partly in the cloud, hold files of
/// their own and are gone into.
pub(crate) fn is_link(data: &WIN32_FIND_DATAW) -> bool {
    // The tag (in dwReserved0) only means something on a reparse point.
    data.dwFileAttributes & ATTR_REPARSE_POINT != 0 && data.dwReserved0 & TAG_NAME_SURROGATE != 0
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
        Ask { root: root.to_string(), query: query.to_string(), generation: 1, show_hidden: false, show_system: false, pause, dupes: false }
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

    #[test]
    fn finds_everything_in_many_folders_once() {
        let mut files = Vec::new();
        for a in 0..12 {
            for b in 0..6 {
                for c in 0..5 {
                    files.push(format!("d{a}/e{b}/hit-{c}.log"));
                }
                files.push(format!("d{a}/e{b}/miss.txt"));
            }
        }
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let t = Tree::new("many", &refs);
        let root = t.path();
        let (found, capped) = search(&root, "*.log");
        assert!(!capped);
        let mut want: Vec<String> = files.iter().filter(|f| f.ends_with(".log")).map(|f| format!("{root}\\{}", f.replace('/', "\\"))).collect();
        want.sort();
        assert_eq!(found, want);
    }

    /// How long a search of a big tree takes (not run by default): GLINT_SEARCH_BENCH=folder.
    #[test]
    #[ignore]
    fn timing_of_a_big_tree() {
        let Ok(root) = std::env::var("GLINT_SEARCH_BENCH") else { return };
        for query in ["*.txt", "zzz-nothing"] {
            let t = Instant::now();
            let (found, capped) = search(&root, query);
            eprintln!("search {query:?}: {} found (capped {capped}) in {:?}", found.len(), t.elapsed());
        }
    }

    #[test]
    fn capitals_accents_and_wildcards_find_the_same_names() {
        if !odd_names_work(&std::env::temp_dir()) {
            return;
        }
        let t = Tree::new("odd", &["\u{c9}T\u{c9} Photo.JPG", "sub/\u{e9}t\u{e9}2.jpg", "sub/[1] draft.txt", "sub/a+b.txt", "sub/emoji \u{1f600}.txt", "sub/x.TXT"]);
        let root = t.path();
        let names = |q: &str| search(&root, q).0.into_iter().map(|p| p[root.len() + 1..].to_string()).collect::<Vec<_>>();
        // Capitals and accents: the same letter in either case, but an accent is not left out.
        assert_eq!(names("\u{e9}t\u{e9}").len(), 2);
        assert_eq!(names("\u{c9}T\u{c9}*.jpg").len(), 2);
        assert!(names("ete").is_empty());
        // `?` is one character, an emoji too; [ ] and + are plain letters.
        assert_eq!(names("emoji ?.txt"), ["sub\\emoji \u{1f600}.txt"]);
        assert_eq!(names("[1]*"), ["sub\\[1] draft.txt"]);
        assert_eq!(names("[1"), ["sub\\[1] draft.txt"]);
        assert_eq!(names("a+b"), ["sub\\a+b.txt"]);
        assert_eq!(names("*.TXT").len(), 4);
        // Everything, and nothing.
        assert_eq!(names("*").len(), 7, "files and the folder");
        assert!(names("*.nothing").is_empty());
        assert!(names("?????????????????????????????????????????").is_empty());
    }

    #[test]
    fn a_missing_root_ends_the_search_with_nothing() {
        let t = Tree::new("noroot", &["a.txt"]);
        let (found, capped) = search(&format!("{}\\nope", t.path()), "*");
        assert!(found.is_empty() && !capped);
        // A file as the root too.
        let (found, _) = search(&format!("{}\\a.txt", t.path()), "*");
        assert!(found.is_empty());
        // The duplicate finder on a missing folder ends with an empty summary.
        let (tx, rx) = channel();
        let ask = Ask { dupes: true, ..ask(&format!("{}\\nope", t.path()), "", false) };
        let _s = start(ask, Folder::default(), move |b| tx.send(b).is_ok()).ok().unwrap();
        let b = rx.recv_timeout(Duration::from_secs(20)).expect("it ends");
        assert!(b.done && b.entries.is_empty() && !b.capped);
        assert_eq!(b.summary.map(|s| s.groups), Some(0));
    }

    #[test]
    fn a_search_cancelled_in_the_middle_sends_no_more() {
        let files: Vec<String> = (0..60).map(|i| format!("d{}/hit{i}.log", i % 20)).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let t = Tree::new("cancelmid", &refs);
        let (tx, rx) = channel();
        let s = start(ask(&t.path(), "*.log", false), Folder::default(), move |b| tx.send(b).is_ok()).ok().unwrap();
        s.cancel();
        // Whatever was in flight arrives; then it ends without ever saying "done" twice.
        let mut dones = 0;
        while let Ok(b) = rx.recv_timeout(Duration::from_millis(300)) {
            dones += usize::from(b.done);
        }
        assert!(dones <= 1);
        // Cancelling twice, and dropping, is fine.
        s.cancel();
        drop(s);
    }

    #[test]
    fn files_deleted_while_it_searches_are_not_a_problem() {
        let files: Vec<String> = (0..40).map(|i| format!("d{}/e{}/hit{i}.log", i % 8, i % 5)).collect();
        let refs: Vec<&str> = files.iter().map(String::as_str).collect();
        let t = Tree::new("deleted", &refs);
        let root = t.0.clone();
        let (tx, rx) = channel();
        let mut first = true;
        let s = start(ask(&t.path(), "*.log", false), Folder::default(), move |b| {
            // The first news arrives: half the tree goes away under the search.
            if std::mem::take(&mut first) {
                for i in 0..4 {
                    let _ = std::fs::remove_dir_all(root.join(format!("d{i}")));
                }
            }
            tx.send(b).is_ok()
        });
        assert!(s.is_ok());
        let mut done = false;
        while !done {
            let b = rx.recv_timeout(Duration::from_secs(20)).expect("the search ends");
            done = b.done;
        }
    }

    #[test]
    fn the_duplicate_search_sends_numbered_sets_and_a_summary() {
        let t = Tree::new("dupset", &["a/1.txt", "b/2.txt", "c/3.txt"]);
        let (tx, rx) = channel();
        let ask = Ask { dupes: true, ..ask(&t.path(), "", false) };
        let s = start(ask, Folder::default(), move |b| tx.send(b).is_ok()).ok().unwrap();
        assert!(s.is_dupes());
        let mut entries = Vec::new();
        let summary;
        loop {
            let b = rx.recv_timeout(Duration::from_secs(20)).expect("it ends");
            entries.extend(b.entries);
            if b.done {
                assert!(!b.capped, "three files are not a cap");
                summary = b.summary;
                break;
            }
        }
        // (Every test file holds the same "x".)
        let summary = summary.expect("a summary at the end");
        assert_eq!((summary.groups, summary.files, summary.smallest_compared), (1, 3, 0));
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|e| e.free == 1), "numbered by set");
    }
}
