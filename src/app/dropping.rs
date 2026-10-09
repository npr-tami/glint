//! Files dropped onto Glint (dragdrop.rs, window/drag.rs): copied, moved or linked into a folder by
//! a file job (Windows' copy engine, as a paste), put in the Recycle Bin (with Glint's question, as
//! Del), or pinned to the sidebar. When the job is done the folder dropped into is read again, with
//! what was made selected in the tab that shows it (`after_file_op`, as after a paste).

use super::files::AfterJob;
use super::{wake, AppState, Loaded};
use crate::dragdrop::{Action, Dest, Plan, Signal};
use crate::fileops::{self, is_full_item_path, items_text, Job};

impl AppState {
    /// Carries out a drop the window decided on. Returns true when a file job was started; its end
    /// fires `signal` (the program the drag came from may be waiting for it). Every other way out
    /// starts nothing, so nothing is waited for.
    pub fn drop_files(&mut self, plan: &Plan, action: Action, signal: Option<Signal>) -> bool {
        match (action, &plan.dest) {
            (Action::Transfer(how), Dest::Folder(dest)) => {
                let job = Job::Drop { sources: plan.files.clone(), dest: dest.clone(), how };
                self.start_drop_job(job, AfterJob::new(plan.tab_id), signal)
            }
            (Action::Recycle { permanent }, _) => {
                self.drop_in_bin(&plan.files, permanent);
                false
            }
            (Action::Pin, _) => {
                self.pin_dropped(&plan.files);
                false
            }
            (Action::Transfer(_), _) => false,
        }
    }

    /// Like `start_job`, but also tells `signal` when the job is done.
    fn start_drop_job(&mut self, job: Job, after: AfterJob, signal: Option<Signal>) -> bool {
        let (tx, hwnd) = (self.tx.clone(), self.hwnd.clone());
        let started = fileops::start(job, move |outcome| {
            let ok = outcome.ok;
            if tx.send(Loaded::FileOp { after, outcome }).is_ok() {
                wake(&hwnd);
            }
            if let Some(s) = signal {
                s.fire(ok);
            }
        });
        if !started {
            self.say("Windows couldn't start this right now. Try again.");
        }
        started
    }

    /// Items dropped on the Recycle Bin are deleted like Del deletes them: to the Recycle Bin, or
    /// for good with Shift held, asking first as "Ask before deleting" says.
    fn drop_in_bin(&mut self, files: &[String], permanent: bool) {
        // Only real items: never a whole drive (or a bare name another program put in its list).
        let paths: Vec<String> = files.iter().filter(|p| is_full_item_path(p)).cloned().collect();
        if paths.is_empty() {
            return;
        }
        let what = items_text(&paths);
        let after = AfterJob::new(self.tab().id);
        let job = Job::Delete { paths, recycle: !permanent };
        if !self.settings.ask_before_deleting(permanent) {
            self.start_job(job, after);
        } else if permanent {
            self.ask(format!("Delete {what} permanently?"), "This can't be undone.".into(), "DELETE", job, after);
        } else {
            self.ask(format!("Move {what} to the Recycle Bin?"), String::new(), "DELETE", job, after);
        }
    }

    /// Folders dropped on the sidebar's "Pinned" group are pinned there (files are left out; a
    /// folder pinned already stays where it is). At most 64 items are looked at, as when the drag
    /// was over "Pinned" (window/drag.rs asked the same question then, so the answer is at hand).
    fn pin_dropped(&mut self, files: &[String]) {
        let folders: Vec<&String> = files.iter().take(64).filter(|p| is_full_item_path(p) && std::fs::metadata(p).is_ok_and(|m| m.is_dir())).collect();
        let mut pinned: Vec<&str> = Vec::new();
        for f in &folders {
            if self.pin_folder(f) {
                pinned.push(crate::folder::file_name(f));
            }
        }
        let note = match (pinned.as_slice(), folders.len()) {
            (_, 0) => "Only folders can be pinned.".to_string(),
            ([], _) => "Already pinned.".to_string(),
            ([one], _) => format!("Pinned \u{201c}{one}\u{201d}"),
            (many, _) => format!("Pinned {} folders", many.len()),
        };
        self.say(note);
    }
}
