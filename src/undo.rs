//! Undo and redo for file actions (Ctrl+Z and Ctrl+Y in the file list).
//!
//! Every file job Glint runs says what it did as a [`Step`] (fileops.rs fills it in from what really
//! happened, not from what was asked). The app keeps the last few steps (app/files.rs); Ctrl+Z runs
//! the job that turns the newest one around, and Ctrl+Y runs it again:
//! - a rename is renamed back;
//! - a move is moved back to where each item came from;
//! - copies, new folders and new files, new zip and 7z files and what Extract took out go to the
//!   Recycle Bin (and Ctrl+Y brings them back from it);
//! - items sent to the Recycle Bin come back from it (and Ctrl+Y sends them there again).
//!
//! Deleting for good can't be undone (Glint asks first). Nothing is ever replaced: an item that
//! would land on a name taken meanwhile is left where it is, and the status bar says so.

use crate::fileops::{items_text, Job};

/// How many steps Ctrl+Z can go back.
pub const KEPT: usize = 30;

/// One file action, as it was done.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// `from` in `dir` was renamed to `to`.
    Renamed { dir: String, from: String, to: String },
    /// These items were made: copies, shortcuts, a new folder or file, a new archive or what was
    /// extracted from one (`what` says which, for the status bar: "copy", "new folder", "extract").
    Made { paths: Vec<String>, what: &'static str },
    /// Items moved, as (where it was, where it is now).
    Moved { pairs: Vec<(String, String)> },
    /// These items went to the Recycle Bin.
    Recycled { paths: Vec<String> },
}

impl Step {
    /// The job that turns this step around.
    pub fn undo_job(&self) -> Job {
        match self {
            Step::Renamed { dir, from, to } => Job::Rename { path: crate::folder::join(dir, to), new_name: from.clone() },
            Step::Made { paths, .. } => Job::Delete { paths: paths.clone(), recycle: true },
            Step::Moved { pairs } => Job::MoveEach { pairs: pairs.iter().map(|(a, b)| (b.clone(), a.clone())).collect() },
            Step::Recycled { paths } => Job::Restore { paths: paths.clone() },
        }
    }

    /// The job that does this step again, after it was undone.
    pub fn redo_job(&self) -> Job {
        match self {
            Step::Renamed { dir, from, to } => Job::Rename { path: crate::folder::join(dir, from), new_name: to.clone() },
            Step::Made { paths, .. } => Job::Restore { paths: paths.clone() },
            Step::Moved { pairs } => Job::MoveEach { pairs: pairs.clone() },
            Step::Recycled { paths } => Job::Delete { paths: paths.clone(), recycle: true },
        }
    }

    /// What this step moved or renamed, as (where it was, where it is now): what colour tags follow
    /// (tags.rs). Nothing for the others.
    pub fn moves(&self) -> Vec<(String, String)> {
        match self {
            Step::Renamed { dir, from, to } => vec![(crate::folder::join(dir, from), crate::folder::join(dir, to))],
            Step::Moved { pairs } => pairs.clone(),
            Step::Made { .. } | Step::Recycled { .. } => Vec::new(),
        }
    }

    /// What it was, for the status bar: "the rename of “a.txt”", "the move of 3 items".
    pub fn describe(&self) -> String {
        match self {
            Step::Renamed { from, .. } => format!("the rename of \u{201c}{from}\u{201d}"),
            Step::Made { paths, what } => format!("the {what} ({})", items_text(paths)),
            Step::Moved { pairs } => {
                let from: Vec<String> = pairs.iter().map(|(a, _)| a.clone()).collect();
                // Renamed many at once (app/forms.rs): each stayed in its folder.
                let renamed = pairs.iter().all(|(a, b)| crate::folder::parent(a) == crate::folder::parent(b));
                format!("the {} of {}", if renamed { "rename" } else { "move" }, items_text(&from))
            }
            Step::Recycled { paths } => format!("the delete of {}", items_text(paths)),
        }
    }
}

/// The undo and redo lists.
#[derive(Default)]
pub struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// An undo or redo is still being done: the next one waits for it, as each step expects what
    /// the one after it left on disk.
    pub busy: bool,
}

impl History {
    /// A new action was done: it can be undone, and what was undone before can't be redone any more
    /// (as in every editor).
    pub fn done(&mut self, step: Step) {
        self.undo.push(step);
        if self.undo.len() > KEPT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// The step Ctrl+Z turns around, taken off the list (it goes on the redo list once the job
    /// worked: [`History::undone`]).
    pub fn take_undo(&mut self) -> Option<Step> {
        self.undo.pop()
    }

    pub fn take_redo(&mut self) -> Option<Step> {
        self.redo.pop()
    }

    /// Ctrl+Z worked: Ctrl+Y can do it again.
    pub fn undone(&mut self, step: Step) {
        self.redo.push(step);
    }

    /// Ctrl+Y worked: Ctrl+Z can undo it again (the redo list stays).
    pub fn redone(&mut self, step: Step) {
        self.undo.push(step);
        if self.undo.len() > KEPT {
            self.undo.remove(0);
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// What Ctrl+Z would undo now.
    #[cfg(test)]
    pub fn next_undo(&self) -> Option<&Step> {
        self.undo.last()
    }

    /// A folder was renamed or moved (by Glint or elsewhere): steps about items inside it follow it,
    /// so Ctrl+Z still finds them.
    pub fn folder_moved(&mut self, from: &str, to: &str) {
        let fix = |p: &mut String| {
            if let Some(m) = crate::app::moved_path(p, from, to) {
                *p = m;
            }
        };
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            match s {
                Step::Renamed { dir, .. } => fix(dir),
                Step::Made { paths, .. } | Step::Recycled { paths } => paths.iter_mut().for_each(fix),
                Step::Moved { pairs } => pairs.iter_mut().for_each(|(a, b)| {
                    fix(a);
                    fix(b);
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_turn_each_step_around() {
        let r = Step::Renamed { dir: "C:\\x".into(), from: "a.txt".into(), to: "b.txt".into() };
        assert!(matches!(r.undo_job(), Job::Rename { ref path, ref new_name } if path == "C:\\x\\b.txt" && new_name == "a.txt"));
        assert!(matches!(r.redo_job(), Job::Rename { ref path, ref new_name } if path == "C:\\x\\a.txt" && new_name == "b.txt"));
        let m = Step::Moved { pairs: vec![("C:\\x\\a".into(), "D:\\y\\a".into())] };
        assert!(matches!(m.undo_job(), Job::MoveEach { ref pairs } if pairs[0] == ("D:\\y\\a".to_string(), "C:\\x\\a".to_string())));
        let c = Step::Made { paths: vec!["C:\\x\\a - Copy.txt".into()], what: "copy" };
        assert!(matches!(c.undo_job(), Job::Delete { recycle: true, .. }));
        assert!(matches!(c.redo_job(), Job::Restore { .. }));
        let d = Step::Recycled { paths: vec!["C:\\x\\a.txt".into()] };
        assert!(matches!(d.undo_job(), Job::Restore { .. }));
        assert!(matches!(d.redo_job(), Job::Delete { recycle: true, .. }));
    }

    #[test]
    fn renames_and_moves_say_what_went_where() {
        let r = Step::Renamed { dir: "C:\\x".into(), from: "a.txt".into(), to: "b.txt".into() };
        assert_eq!(r.moves(), vec![("C:\\x\\a.txt".to_string(), "C:\\x\\b.txt".to_string())]);
        let m = Step::Moved { pairs: vec![("C:\\x\\a".into(), "D:\\y\\a".into())] };
        assert_eq!(m.moves(), vec![("C:\\x\\a".to_string(), "D:\\y\\a".to_string())]);
        // A copy moved nothing.
        assert!(Step::Made { paths: vec!["C:\\x\\a - Copy.txt".into()], what: "copy" }.moves().is_empty());
        assert!(Step::Recycled { paths: vec!["C:\\x\\a.txt".into()] }.moves().is_empty());
    }

    #[test]
    fn a_new_action_clears_redo_and_the_list_is_kept_short() {
        let mut h = History::default();
        let step = |n: usize| Step::Recycled { paths: vec![format!("C:\\{n}.txt")] };
        for n in 0..KEPT + 5 {
            h.done(step(n));
        }
        assert_eq!(h.undo.len(), KEPT);
        let s = h.take_undo().unwrap();
        h.undone(s);
        assert!(h.can_redo());
        h.done(step(99));
        assert!(!h.can_redo(), "a new action ends what could be redone");
    }

    #[test]
    fn steps_follow_a_renamed_folder() {
        let mut h = History::default();
        h.done(Step::Made { paths: vec!["C:\\x\\sub\\a.txt".into()], what: "copy" });
        h.folder_moved("C:\\x", "C:\\y");
        assert_eq!(h.next_undo(), Some(&Step::Made { paths: vec!["C:\\y\\sub\\a.txt".into()], what: "copy" }));
    }

    /// Ctrl+Z, Ctrl+Y and Ctrl+Z again, back and forth: each key takes the newest step of its list.
    #[test]
    fn a_long_chain_of_undo_and_redo_keeps_the_order() {
        let mut h = History::default();
        let step = |n: usize| Step::Renamed { dir: "C:\\x".into(), from: format!("{n}.txt"), to: format!("{}.txt", n + 1) };
        for n in 0..3 {
            h.done(step(n));
        }
        // (As app/files.rs does once each job worked.)
        let undo = |h: &mut History| {
            let s = h.take_undo().expect("something to undo");
            h.undone(s.clone());
            s
        };
        let redo = |h: &mut History| {
            let s = h.take_redo().expect("something to redo");
            h.redone(s.clone());
            s
        };
        assert_eq!(undo(&mut h), step(2));
        assert_eq!(undo(&mut h), step(1));
        assert_eq!(redo(&mut h), step(1));
        assert_eq!(undo(&mut h), step(1), "undo after redo turns the same step around again");
        assert_eq!(undo(&mut h), step(0));
        assert!(!h.can_undo() && h.take_undo().is_none());
        assert_eq!((redo(&mut h), redo(&mut h), redo(&mut h)), (step(0), step(1), step(2)));
        assert!(!h.can_redo() && h.take_redo().is_none());
        // An undo whose job didn't work goes on neither list: the next Ctrl+Z is the one before.
        h.take_undo();
        assert_eq!(h.next_undo(), Some(&step(1)));
        assert!(!h.can_redo());
        // Redoing never makes the list longer than KEPT either.
        for n in 10..10 + KEPT {
            h.done(step(n));
        }
        h.undone(step(99));
        let s = h.take_redo().unwrap();
        h.redone(s);
        assert_eq!(h.undo.len(), KEPT);
        assert_eq!(h.next_undo(), Some(&step(99)));
    }

    #[test]
    fn steps_follow_a_moved_folder_but_not_one_whose_name_only_starts_the_same() {
        let mut h = History::default();
        h.done(Step::Moved { pairs: vec![("C:\\x\\a.txt".into(), "D:\\X\\in\\a.txt".into()), ("C:\\x2\\b.txt".into(), "D:\\x\\b.txt".into())] });
        h.done(Step::Renamed { dir: "D:\\x\\in".into(), from: "c".into(), to: "d".into() });
        h.done(Step::Recycled { paths: vec!["D:\\xy\\e.txt".into(), "D:\\x".into()] });
        // The newest was undone: it's on the redo list, which follows too.
        let s = h.take_undo().unwrap();
        h.undone(s);
        h.folder_moved("D:\\x", "E:\\new");
        assert_eq!(h.take_redo(), Some(Step::Recycled { paths: vec!["D:\\xy\\e.txt".into(), "E:\\new".into()] }), "D:\\xy isn't inside D:\\x");
        assert_eq!(h.take_undo(), Some(Step::Renamed { dir: "E:\\new\\in".into(), from: "c".into(), to: "d".into() }));
        // Both ends of a move, in any capitals; the item that came from elsewhere keeps that.
        let moved = vec![("C:\\x\\a.txt".to_string(), "E:\\new\\in\\a.txt".to_string()), ("C:\\x2\\b.txt".to_string(), "E:\\new\\b.txt".to_string())];
        assert_eq!(h.take_undo(), Some(Step::Moved { pairs: moved }));
    }

    #[test]
    fn the_status_bar_says_what_was_undone() {
        let r = Step::Renamed { dir: "C:\\x".into(), from: "a.txt".into(), to: "b.txt".into() };
        assert_eq!(r.describe(), "the rename of \u{201c}a.txt\u{201d}");
        // Renamed many at once: each stayed in its folder.
        let many = Step::Moved { pairs: vec![("C:\\x\\a.jpg".into(), "C:\\x\\Holiday 1.jpg".into()), ("C:\\x\\b.jpg".into(), "C:\\x\\Holiday 2.jpg".into())] };
        assert_eq!(many.describe(), "the rename of 2 items");
        // Up to the drive's root is a move.
        assert_eq!(Step::Moved { pairs: vec![("C:\\x\\a.jpg".into(), "C:\\a.jpg".into())] }.describe(), "the move of \u{201c}a.jpg\u{201d}");
        assert_eq!(Step::Made { paths: vec!["C:\\x\\New folder".into()], what: "new folder" }.describe(), "the new folder (\u{201c}New folder\u{201d})");
        assert_eq!(Step::Recycled { paths: vec!["C:\\a".into(), "C:\\b".into(), "C:\\c".into()] }.describe(), "the delete of 3 items");
    }
}
