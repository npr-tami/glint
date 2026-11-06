//! Disk space (diskspace.rs) in the app: a look per drive, started when a tab opens Disk space and
//! kept until SCAN AGAIN, so going back to it, or opening it in another tab, shows it at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use super::{wake, AppState, Loaded};
use crate::diskspace::{self, Change, Look, Remembered};
use crate::shell;

/// How often a look under way has the page painted again (to show how far it got).
const PROGRESS_EVERY_MS: u128 = 250;

/// A drive's look: under way, or done.
pub enum DiskLook {
    Looking {
        /// Bytes added up so far.
        counted: Arc<AtomicU64>,
        /// The drive's used space (what "counted" goes up to, near enough).
        used: u64,
        cancel: Arc<AtomicBool>,
    },
    Done {
        look: Arc<Look>,
        /// The look it's compared with ("Changes"), if there was one.
        then: Option<Remembered>,
        grew: Vec<Change>,
    },
    /// The drive couldn't be looked at (not ready, gone).
    Failed,
}

/// The looks by drive ("C:\\").
#[derive(Default)]
pub struct Disks {
    pub looks: HashMap<String, DiskLook>,
}

impl Drop for Disks {
    fn drop(&mut self) {
        for l in self.looks.values() {
            if let DiskLook::Looking { cancel, .. } = l {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}

impl AppState {
    /// Opens Disk space for `drive` (or the drive of the folder on screen, or C:) in the tab.
    pub fn open_disk_space(&mut self, drive: Option<String>) {
        let drive = drive
            .or_else(|| diskspace::drive_of(&self.tab().path))
            .or_else(|| diskspace::is_disk(&self.tab().path).then(|| diskspace::drive_of_location(&self.tab().path)).flatten())
            .unwrap_or_else(|| "C:\\".to_string());
        self.navigate(&diskspace::location(&drive));
    }

    /// Starts looking at `drive` unless a look is there or under way (`again`: a new look anyway,
    /// SCAN AGAIN).
    pub fn look_at_disk(&mut self, drive: &str, again: bool) {
        let drive = drive.to_uppercase();
        match self.disks.looks.get(&drive) {
            Some(DiskLook::Looking { .. }) => return,
            Some(DiskLook::Done { .. } | DiskLook::Failed) if !again => return,
            _ => {}
        }
        let counted = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let used = diskspace::used_space(&drive);
        let (tx, hwnd) = (self.tx.clone(), self.hwnd.clone());
        let (c, stop, d) = (counted.clone(), cancel.clone(), drive.clone());
        let started = shell::background("disk space", move || {
            let mut last = Instant::now();
            let mut tick = || {
                if last.elapsed().as_millis() >= PROGRESS_EVERY_MS {
                    last = Instant::now();
                    if tx.send(Loaded::DiskProgress).is_ok() {
                        wake(&hwnd);
                    }
                }
            };
            let look = diskspace::look(&d, &stop, &c, &mut tick);
            let result = look.map(|look| {
                let then = diskspace::remembered(&d, look.at);
                diskspace::remember(&look);
                (look, then)
            });
            if stop.load(Ordering::SeqCst) {
                return;
            }
            if tx.send(Loaded::Disk { drive: d, result }).is_ok() {
                wake(&hwnd);
            }
        });
        let state = if started { DiskLook::Looking { counted, used, cancel } } else { DiskLook::Failed };
        self.disks.looks.insert(drive, state);
    }

    /// A look came back.
    pub(super) fn disk_looked(&mut self, drive: String, result: Option<(Look, Option<Remembered>)>) {
        let state = match result {
            Some((look, then)) => {
                let grew = then.as_ref().map(|t| diskspace::grew(&look, t)).unwrap_or_default();
                DiskLook::Done { look: Arc::new(look), then, grew }
            }
            None => DiskLook::Failed,
        };
        self.disks.looks.insert(drive, state);
    }
}
