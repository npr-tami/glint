//! Folder sizes, worked out in the background: the Size column of a folder shows the total of
//! everything inside it, like the Properties window does, instead of nothing.
//! - Every file counts, hidden or not (it takes up the disk all the same).
//! - Junctions and symbolic links are never followed (they can loop, or count the same files
//!   twice).
//! - Folders that can't be read (no permission) count as empty.
//!
//! A [`Sizer`] works through the folders on screen on one thread of its own and sends each total
//! as soon as it has it, so the biggest folder doesn't hold up the others. Totals are remembered
//! for a minute, so going back to a folder shows its sizes at once; a file job that changes a
//! folder makes it forget that folder (and the ones above it, which hold it).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Storage::FileSystem::WIN32_FIND_DATAW;

use crate::folder;
use crate::search;

/// How long a total is trusted. Files change behind Glint's back too, so not forever.
const KEEP_FOR: Duration = Duration::from_secs(60);
/// Totals remembered at most (about 100 bytes each).
const MAX_KEPT: usize = 4_096;

const ATTR_DIRECTORY: u32 = 0x10;

/// The total size of everything inside `dir` (all subfolders; junctions and symbolic links not
/// followed; every file counted, hidden or not), or None if cancelled.
pub fn size_of(dir: &str, cancel: &AtomicBool) -> Option<u64> {
    // Deep first, so only the folders beside the ones on the way down wait in the list.
    let mut folders = vec![dir.to_string()];
    let mut total = 0u64;
    while let Some(dir) = folders.pop() {
        let listed = search::list(&dir, cancel, &mut |data: &WIN32_FIND_DATAW| {
            let attrs = data.dwFileAttributes;
            if attrs & ATTR_DIRECTORY == 0 {
                total = total.saturating_add(((data.nFileSizeHigh as u64) << 32) | data.nFileSizeLow as u64);
                return true;
            }
            if search::is_link(data) {
                return true;
            }
            let len = data.cFileName.iter().position(|&c| c == 0).unwrap_or(data.cFileName.len());
            let name = &data.cFileName[..len];
            if name != [b'.' as u16] && name != [b'.' as u16, b'.' as u16] {
                folders.push(folder::join(&dir, &String::from_utf16_lossy(name)));
            }
            true
        });
        if !listed {
            return None;
        }
    }
    Some(total)
}

/// Where a result goes: (the asker's key, the folder, its total). Returns false when the results
/// aren't wanted any more.
type Results = Box<dyn FnMut(u64, String, u64) -> bool + Send>;

/// One ask: the folders to size, and where the totals go.
struct Job {
    key: u64,
    dirs: Vec<String>,
    send: Results,
    cancel: Arc<AtomicBool>,
}

/// Totals already worked out, by lower-case path.
#[derive(Default)]
struct Kept {
    totals: HashMap<String, (u64, Instant)>,
}

impl Kept {
    fn get(&self, key: &str) -> Option<u64> {
        self.totals.get(key).filter(|(_, at)| at.elapsed() < KEEP_FOR).map(|&(bytes, _)| bytes)
    }

    fn put(&mut self, key: String, bytes: u64) {
        if self.totals.len() >= MAX_KEPT && !self.totals.contains_key(&key) {
            self.totals.retain(|_, (_, at)| at.elapsed() < KEEP_FOR);
            // Still full: the oldest goes. (A walk through 4,096 items, only now and then.)
            if self.totals.len() >= MAX_KEPT {
                if let Some(oldest) = self.totals.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
                    self.totals.remove(&oldest);
                }
            }
        }
        self.totals.insert(key, (bytes, Instant::now()));
    }
}

/// What the Sizer and its thread share.
#[derive(Default)]
struct Shared {
    kept: Mutex<Kept>,
    /// Goes up with every `forget`, so a total worked out while a folder changed isn't kept.
    changes: AtomicU64,
}

/// Works out folder sizes on a thread of its own (see the top of this file). Dropping it stops
/// the thread.
#[derive(Default)]
pub struct Sizer {
    shared: Arc<Shared>,
    /// Jobs go to the thread this way (None until the first ask starts it).
    jobs: Option<Sender<Job>>,
    /// The ask under way, to cancel it when a new one comes.
    current: Option<Arc<AtomicBool>>,
}

impl Sizer {
    /// Sizes `dirs`, sending each total to `send` (on the Sizer's thread) as it's done; totals
    /// remembered from earlier come first, at once. Replaces (cancels) any earlier ask. `key` comes
    /// back with every result, to tell them from an older ask's.
    pub fn ask(&mut self, key: u64, dirs: Vec<String>, send: impl FnMut(u64, String, u64) -> bool + Send + 'static) {
        self.cancel();
        if self.jobs.is_none() {
            let (tx, rx) = channel::<Job>();
            let shared = self.shared.clone();
            let started = crate::shell::background("folder sizes", move || {
                while let Ok(job) = rx.recv() {
                    work(&shared, job);
                }
            });
            if !started {
                return; // no thread to be had: folders just show no size
            }
            self.jobs = Some(tx);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.current = Some(cancel.clone());
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job { key, dirs, send: Box::new(send), cancel });
        }
    }

    /// A remembered total for `dir`, if there is one still fresh.
    pub fn known(&self, dir: &str) -> Option<u64> {
        self.shared.kept.lock().unwrap_or_else(|e| e.into_inner()).get(&dir.to_lowercase())
    }

    /// Forgets the totals of `path`, everything under it and everything above it (a file job
    /// changed something there, and a folder's total holds its subfolders').
    pub fn forget(&self, path: &str) {
        let path = path.trim_end_matches('\\').to_lowercase();
        self.shared.changes.fetch_add(1, Ordering::SeqCst);
        let inside = |outer: &str, inner: &str| {
            let outer = outer.trim_end_matches('\\');
            inner.len() > outer.len() && inner.starts_with(outer) && inner.as_bytes()[outer.len()] == b'\\'
        };
        let mut kept = self.shared.kept.lock().unwrap_or_else(|e| e.into_inner());
        kept.totals.retain(|k, _| {
            let k = k.trim_end_matches('\\');
            !(k == path || inside(&path, k) || inside(k, &path))
        });
    }

    /// Stops the ask under way (the thread waits for the next).
    pub fn cancel(&mut self) {
        if let Some(c) = self.current.take() {
            c.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Sizer {
    fn drop(&mut self) {
        // Cancelling stops the job; with the sender gone the thread then ends.
        self.cancel();
        self.jobs = None;
    }
}

/// Goes through one ask: remembered totals first, then the rest one by one.
fn work(shared: &Shared, mut job: Job) {
    let mut left = Vec::new();
    for dir in std::mem::take(&mut job.dirs) {
        let known = shared.kept.lock().unwrap_or_else(|e| e.into_inner()).get(&dir.to_lowercase());
        match known {
            Some(bytes) => {
                if job.cancel.load(Ordering::SeqCst) || !(job.send)(job.key, dir, bytes) {
                    return;
                }
            }
            None => left.push(dir),
        }
    }
    for dir in left {
        let changes = shared.changes.load(Ordering::SeqCst);
        let Some(bytes) = size_of(&dir, &job.cancel) else { return };
        if shared.changes.load(Ordering::SeqCst) == changes {
            shared.kept.lock().unwrap_or_else(|e| e.into_inner()).put(dir.to_lowercase(), bytes);
        }
        if job.cancel.load(Ordering::SeqCst) || !(job.send)(job.key, dir, bytes) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// A small tree under the temporary folder, removed afterwards: (path, bytes) pairs.
    struct Tree(std::path::PathBuf);

    impl Tree {
        fn new(name: &str, files: &[(&str, usize)]) -> Tree {
            let root = std::env::temp_dir().join(format!("glint-size-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            for (f, n) in files {
                let p = root.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, vec![7u8; *n]).unwrap();
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

    #[test]
    fn counts_everything_in_all_subfolders() {
        let t = Tree::new("sum", &[("a.bin", 100), ("x/b.bin", 2_000), ("x/y/z/c.bin", 30_000), ("w/empty.bin", 0)]);
        let never = AtomicBool::new(false);
        assert_eq!(size_of(&t.path(), &never), Some(32_100));
        assert_eq!(size_of(&format!("{}\\x", t.path()), &never), Some(32_000));
        // A folder that isn't there is just empty.
        assert_eq!(size_of(&format!("{}\\nope", t.path()), &never), Some(0));
        let cancelled = AtomicBool::new(true);
        assert_eq!(size_of(&t.path(), &cancelled), None);
    }

    #[test]
    fn the_sizer_sends_each_total_and_remembers_them() {
        let t = Tree::new("sizer", &[("a/1.bin", 10), ("b/2.bin", 20), ("b/c/3.bin", 5)]);
        let (a, b) = (format!("{}\\a", t.path()), format!("{}\\b", t.path()));
        let mut sizer = Sizer::default();
        let (tx, rx) = channel();
        sizer.ask(7, vec![a.clone(), b.clone()], move |key, dir, bytes| tx.send((key, dir, bytes)).is_ok());
        let mut got: Vec<(u64, String, u64)> = (0..2).map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        got.sort();
        assert_eq!(got, vec![(7, a.clone(), 10), (7, b.clone(), 25)]);
        assert_eq!(sizer.known(&a.to_uppercase()), Some(10));

        // Remembered: still the old total even though the folder grew (until forgotten).
        std::fs::write(t.0.join("b/more.bin"), [1u8; 75]).unwrap();
        let (tx, rx) = channel();
        sizer.ask(8, vec![b.clone()], move |key, dir, bytes| tx.send((key, dir, bytes)).is_ok());
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), (8, b.clone(), 25));

        // Forgetting something inside `b` forgets `b` (and the tree above it), not `a`.
        sizer.forget(&format!("{b}\\more.bin"));
        assert_eq!(sizer.known(&b), None);
        assert_eq!(sizer.known(&a), Some(10));
        let (tx, rx) = channel();
        sizer.ask(9, vec![b.clone()], move |key, dir, bytes| tx.send((key, dir, bytes)).is_ok());
        assert_eq!(rx.recv_timeout(Duration::from_secs(10)).unwrap(), (9, b.clone(), 100));
        sizer.forget(&t.path());
        assert_eq!(sizer.known(&a), None);
    }

    #[test]
    fn a_new_ask_or_a_cancel_stops_the_old_one() {
        let t = Tree::new("stop", &[("a/1.bin", 1)]);
        let mut sizer = Sizer::default();
        let (tx, rx) = channel();
        let dirs = vec![format!("{}\\a", t.path()); 50];
        sizer.ask(1, dirs, move |key, _, _| {
            std::thread::sleep(Duration::from_millis(20));
            tx.send(key).is_ok()
        });
        sizer.cancel();
        let mut after = 0;
        while rx.recv_timeout(Duration::from_millis(300)).is_ok() {
            after += 1;
        }
        assert!(after <= 1, "at most the one under way finishes, got {after}");
        drop(sizer);
    }

    fn ask_and_wait(sizer: &mut Sizer, key: u64, dirs: Vec<String>) -> Vec<(u64, String, u64)> {
        let n = dirs.len();
        let (tx, rx) = channel();
        sizer.ask(key, dirs, move |key, dir, bytes| tx.send((key, dir, bytes)).is_ok());
        let mut got: Vec<_> = (0..n).map(|_| rx.recv_timeout(Duration::from_secs(10)).expect("a total")).collect();
        got.sort();
        got
    }

    #[test]
    fn empty_folders_and_files_of_no_size_total_nothing() {
        let t = Tree::new("empty", &[("zero.bin", 0), ("a/b/c/zero2.bin", 0)]);
        std::fs::create_dir_all(t.0.join("nothing/at/all")).unwrap();
        let never = AtomicBool::new(false);
        assert_eq!(size_of(&t.path(), &never), Some(0));
        assert_eq!(size_of(&format!("{}\\nothing", t.path()), &never), Some(0));
        // A file named as the folder is not a folder: nothing in it.
        assert_eq!(size_of(&format!("{}\\zero.bin", t.path()), &never), Some(0));
        let mut sizer = Sizer::default();
        let got = ask_and_wait(&mut sizer, 1, vec![format!("{}\\nothing", t.path())]);
        assert_eq!(got[0].2, 0);
        // An empty list of folders asks nothing and ends well.
        sizer.ask(2, Vec::new(), |_, _, _| true);
    }

    #[test]
    fn a_very_deep_tree_is_counted_without_running_out_of_stack() {
        let mut deep = std::path::PathBuf::new();
        for i in 0..120 {
            deep.push(format!("level-{i:03}-with-a-longish-name"));
        }
        // (Far over 260 characters.)
        let t = Tree::new("deep", &[("top.bin", 10)]);
        let bottom = t.0.join(&deep);
        if std::fs::create_dir_all(&bottom).is_err() || std::fs::write(bottom.join("bottom.bin"), vec![1u8; 100]).is_err() {
            return; // (this system can't make paths this long)
        }
        assert!(bottom.to_string_lossy().len() > 260);
        let never = AtomicBool::new(false);
        assert_eq!(size_of(&t.path(), &never), Some(110));
        // Removing it takes the long way too, so the test leaves nothing behind.
        let _ = std::fs::remove_dir_all(&t.0);
    }

    #[test]
    fn odd_folder_names_are_sized_and_remembered_by_any_capitals() {
        if !crate::search::odd_names_work(&std::env::temp_dir()) {
            return;
        }
        let t = Tree::new("oddnames", &[("caf\u{e9} \u{1f600}/a.bin", 7), ("#1 100%/b.bin", 9), ("dots. here/c.bin", 11), ("x y/z/d.bin", 13)]);
        let dirs: Vec<String> = ["caf\u{e9} \u{1f600}", "#1 100%", "dots. here", "x y"].iter().map(|d| format!("{}\\{d}", t.path())).collect();
        let mut sizer = Sizer::default();
        let got = ask_and_wait(&mut sizer, 5, dirs.clone());
        let mut sizes: Vec<u64> = got.iter().map(|g| g.2).collect();
        sizes.sort();
        assert_eq!(sizes, [7, 9, 11, 13]);
        assert_eq!(sizer.known(&dirs[0].to_uppercase()), Some(7));
    }

    #[test]
    fn a_parent_and_a_namesake_folder_are_forgotten_only_when_they_should() {
        let t = Tree::new("namesake", &[("x/a.bin", 1), ("xy/b.bin", 2), ("x y/c.bin", 4), ("x/sub/d.bin", 8)]);
        let p = t.path();
        let dirs: Vec<String> = ["x", "xy", "x y", "x\\sub"].iter().map(|d| format!("{p}\\{d}")).collect();
        let mut sizer = Sizer::default();
        let sizes = |sizer: &mut Sizer, key| ask_and_wait(sizer, key, dirs.clone()).into_iter().map(|g| g.2).collect::<Vec<_>>();
        assert_eq!(sizes(&mut sizer, 1).len(), 4);
        assert_eq!(sizer.known(&dirs[0]), Some(9));
        // Something changed in x\sub: x\sub and x (which holds it) are forgotten; xy and "x y" are not.
        sizer.forget(&format!("{p}\\X\\sub\\new.bin"));
        assert_eq!((sizer.known(&dirs[0]), sizer.known(&dirs[3])), (None, None));
        assert_eq!((sizer.known(&dirs[1]), sizer.known(&dirs[2])), (Some(2), Some(4)));
        // Something changed in x: x goes, x\sub (inside it) goes too, and the namesakes stay.
        ask_and_wait(&mut sizer, 2, dirs.clone());
        sizer.forget(&format!("{p}\\x"));
        assert_eq!((sizer.known(&dirs[0]), sizer.known(&dirs[3])), (None, None));
        assert_eq!((sizer.known(&dirs[1]), sizer.known(&dirs[2])), (Some(2), Some(4)));
        // A trailing backslash, a drive-sized name and an empty name forget what they should (or nothing).
        ask_and_wait(&mut sizer, 3, dirs.clone());
        sizer.forget(&format!("{p}\\xy\\"));
        assert_eq!((sizer.known(&dirs[1]), sizer.known(&dirs[0])), (None, Some(9)));
        sizer.forget("");
        sizer.forget("\\");
        sizer.forget("Z:\\");
        assert_eq!(sizer.known(&dirs[0]), Some(9), "unrelated paths forget nothing");
        // The parent of everything forgets everything.
        sizer.forget(&p);
        assert!(dirs.iter().all(|d| sizer.known(d).is_none()));
    }

    #[test]
    fn totals_are_remembered_at_most_so_many_and_the_oldest_goes_first() {
        let mut kept = Kept::default();
        for i in 0..MAX_KEPT {
            kept.put(format!("c:\\dir{i}"), i as u64);
        }
        assert_eq!(kept.totals.len(), MAX_KEPT);
        kept.put("c:\\new".into(), 1);
        assert_eq!(kept.totals.len(), MAX_KEPT, "one made room for the new one");
        assert_eq!(kept.get("c:\\new"), Some(1));
        // The same folder again is a replacement, not a second entry.
        kept.put("c:\\new".into(), 2);
        assert_eq!((kept.totals.len(), kept.get("c:\\new")), (MAX_KEPT, Some(2)));
        assert!(kept.get("c:\\dir0").is_none(), "the oldest went");
        assert!(kept.get("c:\\dir5").is_some());
        // A total that is too old is not trusted.
        kept.totals.insert("c:\\old".into(), (5, Instant::now() - KEEP_FOR - Duration::from_secs(1)));
        assert_eq!(kept.get("c:\\old"), None);
    }

    #[test]
    fn a_total_worked_out_while_the_folder_changed_is_not_remembered() {
        let t = Tree::new("changed", &[("a/1.bin", 5), ("b/2.bin", 6)]);
        let (a, b) = (format!("{}\\a", t.path()), format!("{}\\b", t.path()));
        let shared = Arc::new(Shared::default());
        // The first folder's total goes out and, as it does, a file job changes the second one's
        // folder: the second is sized after, so its total is kept; a change *during* the sizing
        // bumps the counter and is not.
        let sizer_ref = shared.clone();
        let (tx, rx) = channel();
        let job = Job {
            key: 1,
            dirs: vec![a.clone(), b.clone()],
            send: Box::new(move |key, dir, bytes| {
                sizer_ref.changes.fetch_add(1, Ordering::SeqCst);
                tx.send((key, dir, bytes)).is_ok()
            }),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        work(&shared, job);
        let got: Vec<_> = rx.try_iter().collect();
        assert_eq!(got.len(), 2);
        let kept = shared.kept.lock().unwrap();
        assert_eq!(kept.get(&a.to_lowercase()), Some(5));
        assert_eq!(kept.get(&b.to_lowercase()), Some(6));
        drop(kept);
        // The results aren't wanted any more (the window closed): the rest isn't sized.
        let shared = Arc::new(Shared::default());
        let job = Job { key: 2, dirs: vec![a.clone(), b.clone()], send: Box::new(|_, _, _| false), cancel: Arc::new(AtomicBool::new(false)) };
        work(&shared, job);
        let kept = shared.kept.lock().unwrap();
        assert_eq!(kept.totals.len(), 1, "only the first was sized");
        drop(kept);
        // Cancelled before it began: nothing sized, nothing sent.
        let shared = Arc::new(Shared::default());
        let job = Job { key: 3, dirs: vec![a, b], send: Box::new(|_, _, _| panic!("sent")), cancel: Arc::new(AtomicBool::new(true)) };
        work(&shared, job);
        assert!(shared.kept.lock().unwrap().totals.is_empty());
    }

    #[test]
    fn a_folder_deleted_while_it_is_sized_gives_a_total_not_a_crash() {
        let files: Vec<(String, usize)> = (0..30).map(|i| (format!("d{}/e{}/f{i}.bin", i % 6, i % 4), 100)).collect();
        let refs: Vec<(&str, usize)> = files.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        let t = Tree::new("vanish", &refs);
        let root = t.0.clone();
        let mut sizer = Sizer::default();
        let (tx, rx) = channel();
        sizer.ask(1, vec![t.path(), format!("{}\\d3", t.path())], move |key, dir, bytes| {
            tx.send((key, dir, bytes)).is_ok()
        });
        // Gone while the sizer works (or before): whatever it counted is a total no bigger than the tree.
        let _ = std::fs::remove_dir_all(root.join("d3"));
        for _ in 0..2 {
            let (_, _, bytes) = rx.recv_timeout(Duration::from_secs(10)).expect("a total for each");
            assert!(bytes <= 3_000, "{bytes}");
        }
    }
}
