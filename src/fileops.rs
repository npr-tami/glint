//! Working with files: copy and move (paste), delete (to the Recycle Bin or for good), rename, and
//! new folders and text files - plus files on the clipboard. Also unpacking and making .zip and .7z
//! files (Extract here, Compress to zip): archive.rs does that work, in the copy queue.
//!
//! Everything goes through Windows' own copy engine (IFileOperation), the one File Explorer uses:
//! - Windows shows its usual progress window, the "Replace or Skip Files" question when names clash,
//!   and its own messages for read-only discs, missing permission or a full disk;
//! - the change lands on Explorer's undo list, so Ctrl+Z in File Explorer can undo it.
//!
//! Each job runs on its own background thread with COM in a single-threaded apartment (which the
//! copy engine needs), so the window never waits for the disk. When the job is done, the `done`
//! callback gets an [`Outcome`] on that thread; app.rs sends it to the window like a folder listing.
//! Windows' progress and question windows are not tied to Glint's window: a big copy carries on after
//! Glint is closed, and the process stays alive until it has finished ([`FinishOnExit`]).
//!
//! When the copy engine isn't there (Wine has only empty stubs of it), the older SHFileOperation
//! copies, moves and deletes, and plain file-system calls rename and make new items. That fallback
//! is only chosen before anything was done, so nothing ever runs twice.
//!
//! The clipboard: files go on it the way Explorer puts them there (CF_HDROP, plus "Preferred
//! DropEffect" saying copy or cut), so Explorer pastes what Glint copied and the other way round.

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use windows::core::{implement, w, Interface, Ref, Result as WinResult, HRESULT, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, E_ABORT, HANDLE, HGLOBAL, HWND};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetFileAttributesW, MoveFileExW, INVALID_FILE_ATTRIBUTES, MOVE_FILE_FLAGS};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_ALL, CLSCTX_INPROC_SERVER};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl, IShellItem, IShellLinkW, ShellLink,
    SHCreateItemFromParsingName, SHFileOperationW, FILEOPERATION_FLAGS, FOFX_ADDUNDORECORD, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO,
    FOF_MULTIDESTFILES, FOF_NOCONFIRMATION, FOF_NOCONFIRMMKDIR, FOF_NOERRORUI, FOF_RENAMEONCOLLISION, FOF_SILENT, FOF_WANTNUKEWARNING, FO_COPY,
    FO_DELETE, FO_MOVE, SHFILEOPSTRUCTW, SIGDN_FILESYSPATH,
};

use crate::folder::{file_name, format_count, join, parent, wide, wide_path};
use crate::log;
use crate::shell::{self, ComGuard};

/// Something to do with files. Each runs on its own background thread (see [`start`]).
#[derive(Clone, Debug)]
pub enum Job {
    /// Copy the files on the clipboard into `dest` - or move them, when they were cut.
    Paste { dest: String },
    /// To the Recycle Bin (`recycle`) or deleted for good.
    Delete { paths: Vec<String>, recycle: bool },
    /// Give `path` the name `new_name` (already checked with [`check_name`]).
    Rename { path: String, new_name: String },
    /// "New folder" (or "New folder (2)"...) in `dir`.
    NewFolder { dir: String },
    /// "New Text Document.txt" (or "... (2).txt") in `dir`.
    NewTextFile { dir: String },
    /// Restore, delete for good or empty in the Recycle Bin (recycle.rs).
    Bin(crate::recycle::BinJob),
    /// Items dragged and dropped into `dest` (dragdrop.rs): copied, moved, or shortcuts made to them.
    Drop { sources: Vec<String>, dest: String, how: DropHow },
    /// Move each item to exactly this path, as (from, to) - Ctrl+Z after a move (undo.rs). Never
    /// replaces anything.
    MoveEach { pairs: Vec<(String, String)> },
    /// Bring these items back from the Recycle Bin to where they were (undo.rs).
    Restore { paths: Vec<String> },
    /// Rename many items at once, as (full path, full new path) in the same folder (app/forms.rs).
    /// Never replaces anything; names swapped among them work.
    RenameMany { pairs: Vec<(String, String)> },
    /// Unpack the .zip or .7z `archive` into its own folder (Extract here), or into a new folder
    /// named after it there (`own_folder`: Extract to "Name"). Never replaces anything.
    Extract { archive: String, own_folder: bool },
    /// Pack these items (all of one folder) into a new .zip or .7z next to them (Compress to zip).
    Compress { paths: Vec<String>, format: crate::archive::Format },
}

/// What dropping items into a folder does (Explorer's rules decide which: dragdrop.rs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropHow {
    Copy,
    Move,
    /// Make shortcuts to them ("report.pdf - Shortcut.lnk").
    Link,
}

impl Job {
    /// Copies, moves and deletes show Windows' progress window and can take long; a person can see
    /// them and stop them. Renames and new items are quick and quiet.
    fn shows_progress(&self) -> bool {
        // (A Recycle Bin job counts too: closing Glint mustn't stop a restore halfway.)
        matches!(
            self,
            Job::Paste { .. }
                | Job::Delete { .. }
                | Job::Bin(_)
                | Job::Drop { how: DropHow::Copy | DropHow::Move, .. }
                | Job::MoveEach { .. }
                | Job::Restore { .. }
                | Job::Extract { .. }
                | Job::Compress { .. }
        )
    }
}

/// What a finished job changed, for the window to show.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// It all went through: nothing was stopped, skipped or refused.
    pub ok: bool,
    /// Folders whose contents changed: tabs showing them read them again.
    pub changed: Vec<String>,
    /// New items (copies, moved or renamed items, new folders) as (folder, names), to select them.
    pub created: Option<(String, Vec<String>)>,
    /// The new item should get its name box at once (New folder, New text file).
    pub rename_new: bool,
    /// A rename that can be fixed by typing another name: (the item's path, what was typed). The
    /// name box opens on it again.
    pub retry: Option<(String, String)>,
    /// Cut items were moved: the clipboard was emptied and Glint forgets its "cut" mark.
    pub moved_cut: bool,
    /// Folders that were moved somewhere else, as (old path, new path): tabs inside them follow.
    pub moved_folders: Vec<(String, String)>,
    /// A short line for the status bar.
    pub note: Option<String>,
    /// What was done, so Ctrl+Z can undo it (undo.rs). None when nothing was done, or it can't be
    /// undone (deleted for good).
    pub undo: Option<crate::undo::Step>,
}

impl Outcome {
    fn note(text: impl Into<String>) -> Outcome {
        Outcome { note: Some(text.into()), ..Default::default() }
    }
}

// ---------------------------------------------------------------- running jobs

/// Jobs still running, so the process can wait for them before it ends (see [`FinishOnExit`]).
struct Running {
    /// Copies, moves and deletes (Windows shows their progress; they may take long).
    shown: usize,
    /// Renames and new items.
    quiet: usize,
}

static RUNNING: Mutex<Running> = Mutex::new(Running { shown: 0, quiet: 0 });
static FINISHED: Condvar = Condvar::new();

/// How long the end of the program waits for a quiet job (a rename on a sleeping network drive).
const QUIET_WAIT: Duration = Duration::from_secs(10);

fn count(shown: bool, delta: isize) {
    if let Ok(mut r) = RUNNING.lock() {
        let n = if shown { &mut r.shown } else { &mut r.quiet };
        *n = n.saturating_add_signed(delta);
    }
    FINISHED.notify_all();
}

/// Runs `job` on a new background thread and calls `done` there with what happened. Returns false
/// (and calls nothing) if Windows can't give us a thread right now.
pub fn start(job: Job, done: impl FnOnce(Outcome) + Send + 'static) -> bool {
    let shown = job.shows_progress();
    count(shown, 1);
    let started = shell::background("file operation", move || {
        // The copy engine and its progress window need COM in a single-threaded apartment.
        let _com = ComGuard::init();
        let t0 = Instant::now();
        let what = format!("{job:?}");
        let outcome = run(job);
        log::line(&format!("file job {what}: {:.1} ms, ok {}, note {:?}", t0.elapsed().as_secs_f64() * 1000.0, outcome.ok, outcome.note));
        done(outcome);
        count(shown, -1);
    });
    if !started {
        count(shown, -1);
    }
    started
}

/// Keep one of these alive in `main`: when it is dropped (the program ends), it waits for file
/// jobs still running, so closing Glint never cuts a copy short. Windows' progress window stays on
/// screen meanwhile. A quiet job (rename, new folder) is given at most QUIET_WAIT.
pub struct FinishOnExit;

impl Drop for FinishOnExit {
    fn drop(&mut self) {
        crate::copyqueue::resume_all();
        let deadline = Instant::now() + QUIET_WAIT;
        let Ok(mut r) = RUNNING.lock() else { return };
        let mut said = false;
        loop {
            if r.shown == 0 && (r.quiet == 0 || Instant::now() >= deadline) {
                return;
            }
            if !said {
                said = true;
                log::line(&format!("waiting for {} file operation(s) to finish before ending", r.shown + r.quiet));
            }
            let wait = if r.shown > 0 { Duration::from_secs(1) } else { deadline.saturating_duration_since(Instant::now()) };
            match FINISHED.wait_timeout(r, wait) {
                Ok((guard, _)) => r = guard,
                Err(_) => return,
            }
        }
    }
}

fn run(job: Job) -> Outcome {
    match job {
        Job::Paste { dest } => paste(&dest),
        Job::Delete { paths, recycle } => delete(&paths, recycle),
        Job::Rename { path, new_name } => rename(&path, &new_name),
        Job::NewFolder { dir } => new_item(&dir, true),
        Job::NewTextFile { dir } => new_item(&dir, false),
        Job::Bin(job) => crate::recycle::run(job),
        Job::Drop { sources, dest, how } => dropped(&sources, &dest, how),
        Job::MoveEach { pairs } if pairs.iter().all(|(a, b)| parent(a).zip(parent(b)).is_some_and(|(x, y)| same_path(&x, &y))) => rename_many(&pairs),
        Job::MoveEach { pairs } => move_each(&pairs),
        Job::RenameMany { pairs } => rename_many(&pairs),
        Job::Restore { paths } => crate::recycle::restore_paths(&paths),
        Job::Extract { archive, own_folder } => extract(&archive, own_folder),
        Job::Compress { paths, format } => compress(&paths, format),
    }
}

/// "“report.pdf”" for one item, "3 items" for more.
pub fn items_text(paths: &[String]) -> String {
    match paths {
        [one] => format!("\u{201c}{}\u{201d}", file_name(one)),
        _ => format!("{} items", format_count(paths.len())),
    }
}

/// The clipboard contents (by the clipboard's change counter) whose cut files a paste is moving
/// right now. Pasting the same cut again meanwhile (Ctrl+V pressed twice, or in a second folder
/// before the first move is done) would have two jobs moving the same files at once.
static MOVING: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Marks a cut as being moved for as long as it lives (see `MOVING`).
struct MovingCut(u32);

impl MovingCut {
    /// None when this cut is being moved already.
    fn begin(seq: u32) -> Option<MovingCut> {
        let mut moving = MOVING.lock().unwrap_or_else(|e| e.into_inner());
        if moving.contains(&seq) {
            return None;
        }
        moving.push(seq);
        Some(MovingCut(seq))
    }
}

impl Drop for MovingCut {
    fn drop(&mut self) {
        MOVING.lock().unwrap_or_else(|e| e.into_inner()).retain(|&s| s != self.0);
    }
}

fn paste(dest: &str) -> Outcome {
    let mut clip = match read_clipboard() {
        Ok(c) => c,
        Err(msg) => return Outcome::note(msg),
    };
    // Only full paths of real items: another program's list could hold a bare name (which the
    // older copy engine would look for in Glint's own current folder) or a whole drive.
    clip.files.retain(|p| is_full_item_path(p));
    if clip.files.is_empty() {
        return Outcome::note("There are no files on the clipboard to paste.");
    }
    let verb = if clip.cut { "moved" } else { "copied" };
    if let Some(s) = clip.files.iter().find(|s| is_same_or_inside(dest, s)) {
        return Outcome::note(format!("\u{201c}{}\u{201d} can't be {verb} into itself.", file_name(s)));
    }
    let in_dest = |s: &String| parent(s).is_some_and(|p| same_path(&p, dest));
    let same_folder = clip.files.iter().all(in_dest);
    // Cut items that are in this folder already stay where they are; the rest move here.
    let files: Vec<String> = if clip.cut { clip.files.iter().filter(|s| !in_dest(s)).cloned().collect() } else { clip.files.clone() };
    if files.is_empty() {
        return Outcome { ok: true, ..Default::default() }; // moving things to where they are: nothing to do
    }
    let _moving = if clip.cut {
        match MovingCut::begin(clip.seq) {
            Some(m) => Some(m),
            None => return Outcome::note("Those items are still being moved. Wait until that's done."),
        }
    } else {
        None
    };
    let mut out = transfer(&files, dest, clip.cut, same_folder);
    if out.ok && clip.cut {
        // Trust, but check: a move that says it's done but left items where they were (Wine's
        // older engine does that with folders moved to another drive) keeps the clipboard as it
        // is, so they can be pasted again, and says so.
        let left: Vec<String> = files.iter().filter(|s| exists(s)).cloned().collect();
        if !left.is_empty() {
            out.ok = false;
            out.note = Some(format!("Couldn't move {}: still where it was.", items_text(&left)));
            return out;
        }
    }
    if out.ok {
        out.note = Some(format!("{} {verb}", items_text(&files)));
        if clip.cut {
            // Like Explorer: what was cut and has now moved is gone from the clipboard.
            clear_clipboard_if(clip.seq);
            out.moved_cut = true;
        }
    }
    out
}

/// Copies or moves `sources` into `dest`. `same_folder`: they are all in `dest` already (a copy then
/// gets "- Copy" names).
fn transfer(sources: &[String], dest: &str, moving: bool, same_folder: bool) -> Outcome {
    let mut changed = vec![dest.to_string()];
    if moving {
        for s in sources {
            if let Some(p) = parent(s) {
                if !changed.iter().any(|c| same_path(c, &p)) {
                    changed.push(p);
                }
            }
        }
    }
    let verb = if moving { "move" } else { "copy" };
    // The folder may be gone since the tab showed it (the older engine would quietly make it again).
    if !is_folder(dest) {
        return Outcome::note(format!("Couldn't {verb} here: \u{201c}{}\u{201d} isn't there any more.", file_name(dest)));
    }
    let mut flags = FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_NOCONFIRMMKDIR;
    // The copy queue (copyqueue.rs): a copy, or a move to another drive, waits its turn and shows
    // in Glint's own list - instead of Windows' progress window (its questions still come).
    let _ticket = if !moving || sources.iter().any(|s| !same_drive(s, dest)) {
        let doing = if moving { "Moving" } else { "Copying" };
        let ticket = crate::copyqueue::Ticket::join(format!("{doing} {} to \u{201c}{}\u{201d}", items_text(sources), file_name(dest)));
        // (CANCEL stops the count too: size_of then gives None, and wait_turn says cancelled.)
        let bytes = sources
            .iter()
            .map(|s| if is_folder(s) { crate::foldersize::size_of(s, ticket.cancel_flag()).unwrap_or(0) } else { std::fs::metadata(s).map(|m| m.len()).unwrap_or(0) })
            .sum();
        ticket.set_total(bytes);
        if !ticket.wait_turn() {
            let were = if sources.len() == 1 { "wasn't" } else { "weren't" };
            return Outcome { changed, note: Some(format!("Cancelled: {} {were} {}.", items_text(sources), if moving { "moved" } else { "copied" })), ..Default::default() };
        }
        flags |= FOF_SILENT;
        Some(ticket)
    } else {
        None
    };
    // An item deleted meanwhile (it may have waited its turn a while) is left out and named in the
    // note; the rest is still done. (The older engine would refuse the lot, the newer say nothing.)
    let (sources, gone): (Vec<String>, Vec<String>) = sources.iter().cloned().partition(|s| exists(s));
    let gone_note = (!gone.is_empty()).then(|| format!("Couldn't {verb}: {} {} there any more.", items_text(&gone), if gone.len() == 1 { "isn't" } else { "aren't" }));
    if sources.is_empty() {
        return Outcome { note: gone_note, ..Default::default() };
    }
    // The folders among what moves, to tell afterwards which of them went (tabs inside follow).
    let folders: Vec<&String> = if moving { sources.iter().filter(|s| is_folder(s)).collect() } else { Vec::new() };
    if !moving && same_folder {
        flags |= FOF_RENAMEONCOLLISION;
    }
    // The names in `dest` already taken before it runs (by lower case): an item that lands on one
    // was merged into, replaced or skipped - it isn't what this job made or moved, for Ctrl+Z.
    let there: HashSet<String> = sources.iter().map(|s| file_name(s)).filter(|n| exists(&join(dest, n))).map(str::to_lowercase).collect();
    let was_there = |p: &str| parent(p).is_some_and(|d| same_path(&d, dest)) && there.contains(&file_name(p).to_lowercase());
    let (done, made) = match Engine::new(flags) {
        Some(engine) => {
            let Some(target) = shell_item(dest) else {
                return Outcome::note(format!("Couldn't {verb} here: Windows can't open \u{201c}{}\u{201d}.", file_name(dest)));
            };
            let mut queued = 0;
            for s in &sources {
                let Some(item) = engine.item(s) else { continue };
                // SAFETY: COM calls on live objects of this thread; a null name keeps the item's own.
                let added = unsafe {
                    if moving {
                        engine.op.MoveItem(&item, &target, PCWSTR::null(), None)
                    } else {
                        engine.op.CopyItem(&item, &target, PCWSTR::null(), None)
                    }
                };
                queued += added.is_ok() as usize;
            }
            if queued == 0 {
                return Outcome::note(match engine.unopened() {
                    Some(name) => format!("Couldn't {verb}: Windows can't open \u{201c}{name}\u{201d}."),
                    None => format!("Couldn't {verb}: {} isn't there any more.", items_text(&sources)),
                });
            }
            let done = engine.run();
            let made = names_in_folder(&done.made, dest);
            (done, made)
        }
        None => {
            let before = names_in(dest);
            // Each item gets its full target path: Wine's old engine refuses to move several
            // items into one folder, and it asks to overwrite instead of making "- Copy" names -
            // so a copy into its own folder is named here, as Windows would ("report - Copy.pdf").
            let mut taken: HashSet<String> = before.iter().flatten().map(|n| n.to_lowercase()).collect();
            let targets: Vec<String> = sources
                .iter()
                .map(|s| {
                    let here = parent(s).is_some_and(|p| same_path(&p, dest));
                    let name = if !moving && here { copy_name(file_name(s), is_folder(s), |n| taken.contains(&n.to_lowercase())) } else { file_name(s).to_string() };
                    taken.insert(name.to_lowercase());
                    join(dest, &name)
                })
                .collect();
            let flags = FILEOPERATION_FLAGS((flags.0 | FOF_MULTIDESTFILES.0) & !FOF_RENAMEONCOLLISION.0);
            let done = legacy(if moving { FO_MOVE } else { FO_COPY }, &sources, &targets, flags);
            let made = match (before, names_in(dest)) {
                (Some(b), Some(a)) => a.difference(&b).cloned().collect(),
                _ => Vec::new(),
            };
            (done, made)
        }
    };
    // What to select: what Windows reported (or what appeared), else the items' own names.
    let names = if !made.is_empty() {
        made.clone()
    } else if !same_folder {
        sources.iter().map(|s| file_name(s).to_string()).collect()
    } else {
        Vec::new()
    };
    // Where each moved item went: as the copy engine reported it ("a (2).txt" after "keep both"),
    // else its own name in `dest`. (true: reported.)
    let went = |s: &String| match done.moved.iter().find(|(from, _)| same_path(from, s)) {
        Some((_, to)) => (to.clone(), true),
        None => (join(dest, file_name(s)), false),
    };
    // A folder that is gone from where it was and is in `dest` now has moved (some may have been
    // skipped, or the move stopped halfway). One merged into a folder there counts too: what was in
    // it is there now.
    let moved_folders = folders
        .into_iter()
        .map(|s| (s.clone(), went(s).0))
        .filter(|(from, to)| !exists(from) && is_folder(to))
        .collect();
    // What Ctrl+Z undoes: the items that really moved (gone from where they were, there now), or
    // the copies made - never an item that was in `dest` before (a folder merged into; a file only
    // when the engine said this one replaced it).
    let undo = if moving {
        let pairs: Vec<(String, String)> = sources
            .iter()
            .filter_map(|s| {
                let (to, sure) = went(s);
                (!exists(s) && exists(&to) && (!was_there(&to) || (sure && !is_folder(&to)))).then(|| (s.clone(), to))
            })
            .collect();
        (!pairs.is_empty()).then_some(crate::undo::Step::Moved { pairs })
    } else {
        // Only what the copy really made (not the items' own names: those may be what was skipped).
        let made: Vec<String> = made.iter().map(|n| join(dest, n)).filter(|p| !was_there(p) && exists(p)).collect();
        (!made.is_empty()).then_some(crate::undo::Step::Made { paths: made, what: "copy" })
    };
    let mut out = Outcome { ok: done.ok() && gone.is_empty(), changed, created: Some((dest.to_string(), names)), moved_folders, undo, ..Default::default() };
    out.note = done.problem(&format!("Couldn't {verb} everything")).or(gone_note);
    out
}

/// Extract here / Extract to "Name" (see [`Job::Extract`]), in the copy queue. A cancel removes
/// what was made; a damaged archive leaves what came out before it.
fn extract(archive: &str, own_folder: bool) -> Outcome {
    let Some(dir) = parent(archive).filter(|d| !d.is_empty()) else { return Outcome::note("This archive isn't in a folder.") };
    let name = file_name(archive).to_string();
    if !exists(archive) || is_folder(archive) {
        return Outcome::note(format!("\u{201c}{name}\u{201d} isn't there any more."));
    }
    let ticket = crate::copyqueue::Ticket::join(format!("Extracting \u{201c}{name}\u{201d}"));
    // (The speed shown is then how fast the archive is gone through.)
    ticket.set_total(std::fs::metadata(archive).map(|m| m.len()).unwrap_or(0));
    if !ticket.wait_turn() {
        return Outcome::note(format!("Cancelled: \u{201c}{name}\u{201d} wasn't extracted."));
    }
    let mut made = Vec::new();
    let result = if own_folder {
        // The folder's name is picked now, at its turn: the one free right now.
        let folder = join(&dir, &crate::archive::extract_folder_name(&name, |n| exists(&join(&dir, n))));
        match std::fs::create_dir(&folder) {
            Ok(()) => {
                made.push(folder.clone());
                crate::archive::extract_all(archive, &folder, &mut Vec::new())
            }
            Err(e) => Err(format!("\u{201c}{}\u{201d} couldn't be made ({e}).", file_name(&folder))),
        }
    } else {
        crate::archive::extract_all(archive, &dir, &mut made)
    };
    let cancelled = ticket.cancel_flag().load(std::sync::atomic::Ordering::SeqCst);
    drop(ticket);
    if cancelled {
        remove_made(&made);
        return Outcome { changed: vec![dir], note: Some(format!("Cancelled: \u{201c}{name}\u{201d} wasn't extracted.")), ..Default::default() };
    }
    // After a problem, folders left empty go again (the one Extract to made, for a damaged archive).
    made.retain(|p| exists(p) && !(result.is_err() && is_folder(p) && std::fs::remove_dir(p).is_ok()));
    let names: Vec<String> = made.iter().map(|p| file_name(p).to_string()).collect();
    let undo = (!made.is_empty()).then(|| crate::undo::Step::Made { paths: made.clone(), what: "extract" });
    let note = match &result {
        Ok(()) if made.is_empty() => format!("\u{201c}{name}\u{201d} is empty: there was nothing to take out."),
        Ok(()) => format!("Extracted \u{201c}{name}\u{201d}"),
        Err(why) => why.clone(),
    };
    Outcome { ok: result.is_ok(), changed: vec![dir.clone()], created: Some((dir, names)), undo, note: Some(note), ..Default::default() }
}

/// Compress to zip / 7z (see [`Job::Compress`]), in the copy queue: one new archive next to the
/// first item, named after it (or after its folder, for several), numbered when that's taken.
fn compress(paths: &[String], format: crate::archive::Format) -> Outcome {
    let Some(dir) = paths.first().and_then(|p| parent(p)).filter(|d| !d.is_empty()) else { return Outcome::note("Select what to compress first.") };
    let what = items_text(paths);
    let cancelled_note = format!("Cancelled: {what} {} compressed.", if paths.len() == 1 { "wasn't" } else { "weren't" });
    let ticket = crate::copyqueue::Ticket::join(format!("Compressing {what}"));
    // (Looking through the folders stops at CANCEL too.)
    let plan = match crate::archive::plan_pack(paths, ticket.cancel_flag()) {
        Ok(Some(plan)) => plan,
        Ok(None) => return Outcome::note(cancelled_note),
        Err(why) => return Outcome::note(why),
    };
    ticket.set_total(plan.total);
    if !ticket.wait_turn() {
        return Outcome::note(cancelled_note);
    }
    let name = unique_name(&crate::archive::pack_name(paths), format.ext(), |n| exists(&join(&dir, n)));
    let out = join(&dir, &name);
    let result = crate::archive::pack(&plan, &out, format);
    let cancelled = ticket.cancel_flag().load(std::sync::atomic::Ordering::SeqCst);
    drop(ticket);
    let changed = vec![dir.clone()];
    if cancelled {
        // (pack removed the half-made file. One finished just as CANCEL was clicked goes too.)
        if result.is_ok() {
            let _ = std::fs::remove_file(&out);
        }
        return Outcome { changed, note: Some(cancelled_note), ..Default::default() };
    }
    match result {
        Ok(()) => Outcome {
            ok: true,
            changed,
            created: Some((dir, vec![name.clone()])),
            undo: Some(crate::undo::Step::Made { paths: vec![out], what: "new archive" }),
            note: Some(format!("Compressed {what} to \u{201c}{name}\u{201d}")),
            ..Default::default()
        },
        Err(why) => Outcome { changed, note: Some(format!("Couldn't compress {what}: {why}")), ..Default::default() },
    }
}

/// Removes what a cancelled job made (all new: nothing that was there before).
fn remove_made(paths: &[String]) {
    for p in paths {
        let _ = if is_folder(p) { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
    }
}

/// Renames each item to the path given (all in their own folder), one after the other in an order
/// that lets a name be handed on ("2.jpg" to "3.jpg" after "3.jpg" to "4.jpg"); names that go round
/// in a circle pass through a temporary name. Nothing is ever replaced: an item whose new name is
/// taken by something else stays as it is.
fn rename_many(pairs: &[(String, String)]) -> Outcome {
    let mut changed: Vec<String> = Vec::new();
    for (a, _) in pairs {
        if let Some(d) = parent(a) {
            if !changed.iter().any(|c| same_path(c, &d)) {
                changed.push(d);
            }
        }
    }
    // Each folder renamed, in the order it was done (through a temporary name too): followed one
    // after the other, tabs inside them land where they went.
    let mut moved_folders: Vec<(String, String)> = Vec::new();
    let mut rename = |from: &str, to: &str| {
        let (f, t) = (wide_path(from), wide_path(to));
        // SAFETY: both paths are NUL-terminated and outlive the call. No flags: never replace.
        let ok = unsafe { MoveFileExW(PCWSTR(f.as_ptr()), PCWSTR(t.as_ptr()), MOVE_FILE_FLAGS(0)) }.is_ok();
        if ok && is_folder(to) {
            moved_folders.push((from.to_string(), to.to_string()));
        }
        ok
    };
    // (where it was, where it is now, where it goes)
    let mut todo: Vec<(String, String, String)> = pairs.iter().filter(|(a, _)| exists(a)).map(|(a, b)| (a.clone(), a.clone(), b.clone())).collect();
    let mut done: Vec<(String, String)> = Vec::new();
    let mut hops = 0;
    loop {
        let mut moved_any = false;
        let mut i = 0;
        while i < todo.len() {
            let (_, now, to) = &todo[i];
            // (A change of capitals only is the same item: its "taken" name is itself.)
            if (!exists(to) || same_path(now, to)) && rename(now, to) {
                let (was, _, to) = todo.remove(i);
                done.push((was, to));
                moved_any = true;
            } else {
                i += 1;
            }
        }
        if todo.is_empty() || moved_any {
            if todo.is_empty() {
                break;
            }
            continue;
        }
        // Stuck: a circle of names (or names taken by other items). One whose new name is another's
        // present name steps aside to a temporary name, which frees that one.
        let Some(i) = todo.iter().position(|(_, _, to)| todo.iter().any(|(_, now, _)| same_path(now, to))) else { break };
        let dir = parent(&todo[i].1).unwrap_or_default();
        let tmp = unique_name(&format!("glint-renaming-{}", std::process::id()), ".tmp", |n| exists(&join(&dir, n)));
        let tmp = join(&dir, &tmp);
        hops += 1;
        if hops > pairs.len() + 1 || !rename(&todo[i].1, &tmp) {
            break;
        }
        todo[i].1 = tmp;
    }
    // Something left at a temporary name goes back to its old one.
    for (was, now, _) in &todo {
        if now != was {
            rename(now, was);
        }
    }
    let created = match done.first().and_then(|(_, to)| parent(to)) {
        Some(d) if done.iter().all(|(_, to)| parent(to).is_some_and(|p| same_path(&p, &d))) => {
            let names = done.iter().map(|(_, to)| file_name(to).to_string()).collect();
            Some((d, names))
        }
        _ => None,
    };
    let note = if todo.is_empty() {
        format!("Renamed {}", items_text(&done.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>()))
    } else {
        let left: Vec<String> = todo.iter().map(|(a, _, _)| a.clone()).collect();
        format!("Couldn't rename {}: the name is taken, or it's in use", items_text(&left))
    };
    let undo = (!done.is_empty()).then(|| crate::undo::Step::Moved { pairs: done.clone() });
    Outcome { ok: todo.is_empty() && done.len() == pairs.len(), changed, created, moved_folders, note: Some(note), undo, ..Default::default() }
}

/// Moves each item to exactly the path given (Ctrl+Z after a move puts things back where they
/// were, Ctrl+Y moves them again). An item whose place is taken meanwhile stays where it is: nothing
/// is replaced.
fn move_each(pairs: &[(String, String)]) -> Outcome {
    let mut changed: Vec<String> = Vec::new();
    for (a, b) in pairs {
        for p in [a, b] {
            if let Some(d) = parent(p) {
                if !changed.iter().any(|c| same_path(c, &d)) {
                    changed.push(d);
                }
            }
        }
    }
    let (todo, taken): (Vec<&(String, String)>, Vec<&(String, String)>) = pairs.iter().filter(|(from, _)| exists(from)).partition(|(_, to)| !exists(to));
    if todo.is_empty() {
        let note = match taken.first() {
            Some((_, to)) => format!("Couldn't move it back: something called \u{201c}{}\u{201d} is there now.", file_name(to)),
            None => "It isn't where it was moved to any more.".to_string(),
        };
        return Outcome { changed, ..Outcome::note(note) };
    }
    // The folder it goes back to may be gone meanwhile: made again, as a restore from the bin does.
    for (_, to) in &todo {
        if let Some(d) = parent(to) {
            if !is_folder(&d) {
                let _ = std::fs::create_dir_all(&d);
            }
        }
    }
    let flags = FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_NOCONFIRMMKDIR;
    let done = match Engine::new(flags) {
        Some(engine) => {
            for (from, to) in &todo {
                let (Some(item), Some(dir)) = (shell_item(from), parent(to).and_then(|d| shell_item(&d))) else { continue };
                let name = wide(file_name(to));
                // SAFETY: COM call on live objects of this thread; `name` outlives it.
                let _ = unsafe { engine.op.MoveItem(&item, &dir, PCWSTR(name.as_ptr()), None) };
            }
            engine.run()
        }
        None => {
            let from: Vec<String> = todo.iter().map(|(a, _)| a.clone()).collect();
            let to: Vec<String> = todo.iter().map(|(_, b)| b.clone()).collect();
            legacy(FO_MOVE, &from, &to, FILEOPERATION_FLAGS(flags.0 | FOF_MULTIDESTFILES.0))
        }
    };
    let moved: Vec<(String, String)> = todo.iter().filter(|(from, to)| !exists(from) && exists(to)).map(|&p| p.clone()).collect();
    let moved_folders = moved.iter().filter(|(_, to)| is_folder(to)).cloned().collect();
    // Selected where they went, when that's one folder.
    let created = match moved.first().and_then(|(_, to)| parent(to)) {
        Some(d) if moved.iter().all(|(_, to)| parent(to).is_some_and(|p| same_path(&p, &d))) => {
            let names = moved.iter().map(|(_, to)| file_name(to).to_string()).collect();
            Some((d, names))
        }
        _ => None,
    };
    let ok = done.ok() && moved.len() == pairs.len();
    let left: Vec<String> = pairs.iter().filter(|(a, _)| !moved.iter().any(|(m, _)| m == a)).map(|(a, _)| a.clone()).collect();
    let note = done.problem("Couldn't move everything").or_else(|| {
        (!left.is_empty()).then(|| format!("{} stayed where it was: its place is taken, or it's gone", items_text(&left)))
    });
    let undo = (!moved.is_empty()).then_some(crate::undo::Step::Moved { pairs: moved });
    Outcome { ok, changed, created, moved_folders, note, undo, ..Default::default() }
}

/// Items dragged and dropped into `dest` (dragdrop.rs decided how, by Explorer's rules). Checked
/// again here, as a paste is: only full paths of real items (not a bare name, not a whole drive),
/// never a folder into itself, never into a folder that's gone (`transfer`).
fn dropped(sources: &[String], dest: &str, how: DropHow) -> Outcome {
    let sources: Vec<String> = sources.iter().filter(|p| is_full_item_path(p)).cloned().collect();
    if sources.is_empty() {
        return Outcome::note("Only files and folders can be dropped here.");
    }
    let verb = match how {
        DropHow::Copy => "copied",
        DropHow::Move => "moved",
        DropHow::Link => "linked",
    };
    if let Some(s) = sources.iter().find(|s| is_same_or_inside(dest, s)) {
        return Outcome::note(format!("\u{201c}{}\u{201d} can't be {verb} into itself.", file_name(s)));
    }
    if how == DropHow::Link {
        return shortcuts(&sources, dest);
    }
    let moving = how == DropHow::Move;
    let in_dest = |s: &String| parent(s).is_some_and(|p| same_path(&p, dest));
    let same_folder = sources.iter().all(in_dest);
    // Items moved to the folder they're in already stay where they are.
    let files: Vec<String> = if moving { sources.iter().filter(|s| !in_dest(s)).cloned().collect() } else { sources.clone() };
    if files.is_empty() {
        return Outcome { ok: true, ..Default::default() };
    }
    let mut out = transfer(&files, dest, moving, same_folder);
    if out.ok && moving {
        // As after a paste: a move that says it's done but left items where they were says so.
        let left: Vec<String> = files.iter().filter(|s| exists(s)).cloned().collect();
        if !left.is_empty() {
            out.ok = false;
            out.note = Some(format!("Couldn't move {}: still where it was.", items_text(&left)));
            return out;
        }
    }
    if out.ok {
        out.note = Some(format!("{} {verb} to \u{201c}{}\u{201d}", items_text(&files), file_name(dest)));
    }
    out
}

/// Makes a shortcut in `dest` to each of `sources` ("report.pdf - Shortcut.lnk", numbered when the
/// name is taken), as Explorer's "Create shortcuts here" does. Never replaces anything.
fn shortcuts(sources: &[String], dest: &str) -> Outcome {
    if !is_folder(dest) {
        return Outcome::note(format!("Couldn't make shortcuts here: \u{201c}{}\u{201d} isn't there any more.", file_name(dest)));
    }
    let mut made: Vec<String> = Vec::new();
    let mut error = None;
    for s in sources {
        let taken = |n: &str| exists(&join(dest, n)) || made.iter().any(|m| m.eq_ignore_ascii_case(n));
        let name = unique_name(&format!("{} - Shortcut", file_name(s)), ".lnk", taken);
        let path = join(dest, &name);
        if exists(&path) {
            continue; // every numbered name was taken: leave it
        }
        match make_shortcut(s, &path) {
            Ok(()) => made.push(name),
            Err(hr) => {
                error.get_or_insert(hr);
            }
        }
    }
    let n = made.len();
    let note = match error {
        Some(hr) => format!("Couldn't make every shortcut: {}", explain(hr)),
        None if n == 1 => format!("Made a shortcut in \u{201c}{}\u{201d}", file_name(dest)),
        None => format!("Made {} shortcuts in \u{201c}{}\u{201d}", format_count(n), file_name(dest)),
    };
    let paths: Vec<String> = made.iter().map(|n| join(dest, n)).collect();
    let undo = (!paths.is_empty()).then_some(crate::undo::Step::Made { paths, what: "shortcut" });
    Outcome { ok: error.is_none(), changed: vec![dest.to_string()], created: Some((dest.to_string(), made)), note: Some(note), undo, ..Default::default() }
}

/// Writes the shortcut file `lnk` pointing at `target` (Windows' own shortcut object).
pub(crate) fn make_shortcut(target: &str, lnk: &str) -> Result<(), HRESULT> {
    // SAFETY: COM calls in this job thread's apartment; the strings outlive the calls.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.code())?;
        let t = wide(target);
        link.SetPath(PCWSTR(t.as_ptr())).map_err(|e| e.code())?;
        if !is_folder(target) {
            // A program started from the shortcut starts in its own folder, as Explorer sets it.
            if let Some(dir) = parent(target) {
                let d = wide(&dir);
                let _ = link.SetWorkingDirectory(PCWSTR(d.as_ptr()));
            }
        }
        let file: IPersistFile = link.cast().map_err(|e| e.code())?;
        let l = wide(lnk);
        file.Save(PCWSTR(l.as_ptr()), true).map_err(|e| e.code())
    }
}

fn delete(paths: &[String], recycle: bool) -> Outcome {
    let mut changed: Vec<String> = Vec::new();
    for p in paths {
        if let Some(dir) = parent(p) {
            if !changed.iter().any(|c| same_path(c, &dir)) {
                changed.push(dir);
            }
        }
    }
    if recycle {
        // Tabs showing the Recycle Bin read it again too.
        changed.push(crate::recycle::LOCATION.to_string());
    }
    // Glint has already asked (or was told not to). Windows still warns when something can't go to
    // the Recycle Bin (a network drive, a USB stick without one) and would be gone for good.
    let flags = if recycle { FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOFX_ADDUNDORECORD | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING } else { FOF_NOCONFIRMATION };
    let done = match Engine::new(flags) {
        Some(engine) => {
            let mut queued = 0;
            for p in paths {
                let Some(item) = engine.item(p) else { continue };
                // SAFETY: COM call on live objects of this thread.
                queued += unsafe { engine.op.DeleteItem(&item, None) }.is_ok() as usize;
            }
            if queued == 0 {
                let note = match engine.unopened() {
                    Some(name) => format!("Couldn't delete: Windows can't open \u{201c}{name}\u{201d}."),
                    None => format!("{} isn't there any more.", items_text(paths)),
                };
                return Outcome { changed, ..Outcome::note(note) };
            }
            engine.run()
        }
        None => legacy(FO_DELETE, paths, &[], flags),
    };
    if recycle && paths.iter().any(|p| !exists(p)) {
        crate::recycle::saw_full(true); // (the sidebar's bin icon)
    }
    // Sent to the Recycle Bin: Ctrl+Z brings back what really went. (Deleted for good - Windows
    // asks first when something can't go there - there's no way back.)
    let for_good = |p: &String| done.gone_for_good.iter().any(|g| same_path(g, p));
    let gone: Vec<String> = paths.iter().filter(|p| !exists(p) && !for_good(p)).cloned().collect();
    let note = if !recycle || gone.is_empty() {
        format!("{} deleted", items_text(paths))
    } else if done.gone_for_good.is_empty() {
        format!("{} moved to the Recycle Bin", items_text(paths))
    } else {
        format!("{} moved to the Recycle Bin, {} deleted", items_text(&gone), items_text(&done.gone_for_good))
    };
    let undo = (recycle && !gone.is_empty()).then_some(crate::undo::Step::Recycled { paths: gone });
    let mut out = Outcome { ok: done.ok(), changed, undo, ..Default::default() };
    out.note = done.problem("Couldn't delete everything").or(Some(note));
    out
}

fn rename(path: &str, new_name: &str) -> Outcome {
    let Some(dir) = parent(path).filter(|d| !d.is_empty()) else { return Outcome::note("This can't be renamed.") };
    let old = file_name(path).to_string();
    let target = join(&dir, new_name);
    // Never replace another item. (Changing only capitals - "readme" to "README" - is the same item.)
    if exists(&target) && old.to_lowercase() != new_name.to_lowercase() {
        return Outcome {
            retry: Some((path.to_string(), new_name.to_string())),
            ..Outcome::note(format!("There's already an item called \u{201c}{new_name}\u{201d} here."))
        };
    }
    // Errors go to the status bar (FOF_NOERRORUI) instead of a Windows message box.
    let flags = FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_NOERRORUI | FOF_SILENT;
    let error = match Engine::new(flags) {
        Some(engine) => {
            let Some(item) = shell_item(path) else {
                return Outcome { changed: vec![dir], ..Outcome::note(format!("\u{201c}{old}\u{201d} isn't there any more.")) };
            };
            let name_w = wide(new_name);
            // SAFETY: COM call on live objects of this thread; `name_w` outlives it.
            if let Err(e) = unsafe { engine.op.RenameItem(&item, PCWSTR(name_w.as_ptr()), None) } {
                Some(e.code())
            } else {
                let done = engine.run();
                if done.aborted && done.error.is_none() {
                    return Outcome { changed: vec![dir], ..Default::default() }; // stopped by the person
                }
                done.error
            }
        }
        None => {
            let (from, to) = (wide_path(path), wide_path(&target));
            // SAFETY: both strings are NUL-terminated and outlive the call. No flags: never replace.
            unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVE_FILE_FLAGS(0)) }.err().map(|e| e.code())
        }
    };
    match error {
        None => {
            let undo = Some(crate::undo::Step::Renamed { dir: dir.clone(), from: old.clone(), to: new_name.to_string() });
            Outcome { ok: true, changed: vec![dir.clone()], created: Some((dir, vec![new_name.to_string()])), undo, ..Default::default() }
        }
        Some(hr) => {
            // Someone made an item with that name in the meantime: let the person pick another.
            // (Windows' copy engine says so with its own codes, COPYENGINE_E_ALREADY_EXISTS_*.)
            let taken = matches!(win32_code(hr), Some(80 | 183)) || matches!(engine_code(hr), Some(0x29..=0x2C));
            Outcome {
                changed: vec![dir],
                retry: taken.then(|| (path.to_string(), new_name.to_string())),
                ..Outcome::note(format!("Couldn't rename \u{201c}{old}\u{201d}: {}", explain(hr)))
            }
        }
    }
}

fn new_item(dir: &str, folder: bool) -> Outcome {
    let (base, ext, what) = if folder { ("New folder", "", "folder") } else { ("New Text Document", ".txt", "text file") };
    let name = unique_name(base, ext, |n| exists(&join(dir, n)));
    // Windows shows its own message if this can't be done (a read-only disc, no permission).
    let flags = FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_SILENT | FOF_NOCONFIRMMKDIR;
    let (error, made) = match Engine::new(flags) {
        Some(engine) => {
            let Some(target) = shell_item(dir) else {
                return Outcome::note(format!("Couldn't make a new {what}: Windows can't open this folder."));
            };
            let name_w = wide(&name);
            // FILE_ATTRIBUTE_DIRECTORY makes a folder; FILE_ATTRIBUTE_NORMAL with no template an empty file.
            let attributes = if folder { 0x10 } else { 0x80 };
            // SAFETY: COM call on live objects of this thread; `name_w` outlives it.
            match unsafe { engine.op.NewItem(&target, attributes, PCWSTR(name_w.as_ptr()), PCWSTR::null(), None) } {
                Err(e) => (Some(e.code()), Vec::new()),
                Ok(()) => {
                    let done = engine.run();
                    let made = names_in_folder(&done.made, dir);
                    if done.aborted && done.error.is_none() {
                        return Outcome { changed: vec![dir.to_string()], ..Default::default() };
                    }
                    (done.error, made)
                }
            }
        }
        None => {
            let path = join(dir, &name);
            let made = if folder { std::fs::create_dir(&path) } else { std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ()) };
            (made.err().map(|e| HRESULT::from_win32(e.raw_os_error().unwrap_or(31) as u32)), Vec::new())
        }
    };
    match error {
        None => {
            let name = made.into_iter().next().unwrap_or(name);
            let undo = Some(crate::undo::Step::Made { paths: vec![join(dir, &name)], what: if folder { "new folder" } else { "new text file" } });
            Outcome { ok: true, changed: vec![dir.to_string()], created: Some((dir.to_string(), vec![name])), rename_new: true, undo, ..Default::default() }
        }
        Some(hr) => Outcome::note(format!("Couldn't make a new {what} here: {}", explain(hr))),
    }
}

// ---------------------------------------------------------------- the copy engine

/// What happened when the copy engine (or the fallback) ran.
#[derive(Debug, Default)]
struct Done {
    /// The person stopped it, or skipped something.
    aborted: bool,
    /// The first thing that went wrong (stopping doesn't count).
    error: Option<HRESULT>,
    /// Full paths of what was made: copies, moved and renamed items, new items.
    made: Vec<String>,
    /// Items moved, as (where it was, where it is now), as the engine reported them.
    moved: Vec<(String, String)>,
    /// The name of an item that is there but that Windows couldn't open, so it was left out.
    unopened: Option<String>,
    /// Deleted items that didn't go to the Recycle Bin: Windows asked, and they were deleted for good.
    gone_for_good: Vec<String>,
}

impl Done {
    fn ok(&self) -> bool {
        !self.aborted && self.error.is_none() && self.unopened.is_none()
    }

    /// A status-bar line when something didn't go through.
    fn problem(&self, lead: &str) -> Option<String> {
        match self.error {
            Some(hr) => Some(format!("{lead}: {}", explain(hr))),
            None if self.aborted => Some("Stopped before it was finished.".to_string()),
            None => self.unopened.as_ref().map(|name| format!("{lead}: Windows can't open \u{201c}{name}\u{201d}.")),
        }
    }
}

/// Collects what the copy engine reports while it works. The reports come on the job's own thread
/// while PerformOperations runs; the record is still behind a Mutex (not a RefCell), because COM
/// objects made with `implement` say they may be called from any thread, and the engine may let go
/// of the sink from another one.
#[implement(IFileOperationProgressSink)]
struct Sink(Arc<Mutex<Done>>);

impl Sink {
    fn after(&self, hr: HRESULT, made: Option<&IShellItem>) {
        let mut done = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if hr.is_err() {
            if is_cancel(hr) {
                done.aborted = true;
            } else if done.error.is_none() {
                done.error = Some(hr);
            }
            return;
        }
        if let Some(path) = made.and_then(item_path) {
            done.made.push(path);
        }
    }
}

impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> WinResult<()> {
        Ok(())
    }
    fn FinishOperations(&self, hr: HRESULT) -> WinResult<()> {
        if hr.is_err() {
            self.after(hr, None);
        }
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn PostRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, hr: HRESULT, made: Ref<IShellItem>) -> WinResult<()> {
        self.after(hr, made.as_ref());
        Ok(())
    }
    fn PreMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        go_on()
    }
    fn PostMoveItem(&self, _: u32, item: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, hr: HRESULT, made: Ref<IShellItem>) -> WinResult<()> {
        self.after(hr, made.as_ref());
        // Where it went, for Ctrl+Z (its name may have changed: "keep both").
        if hr.is_ok() {
            if let (Some(from), Some(to)) = (item.as_ref().and_then(item_path), made.as_ref().and_then(item_path)) {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).moved.push((from, to));
            }
        }
        Ok(())
    }
    fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        go_on()
    }
    fn PostCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, hr: HRESULT, made: Ref<IShellItem>) -> WinResult<()> {
        self.after(hr, made.as_ref());
        Ok(())
    }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> WinResult<()> {
        Ok(())
    }
    fn PostDeleteItem(&self, _: u32, item: Ref<IShellItem>, hr: HRESULT, in_bin: Ref<IShellItem>) -> WinResult<()> {
        // What a deleted item "became" is its copy in the Recycle Bin: nothing to select. Without
        // one, it was deleted for good.
        if hr.is_ok() && in_bin.as_ref().is_none() {
            if let Some(path) = item.as_ref().and_then(item_path) {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).gone_for_good.push(path);
            }
        }
        self.after(hr, None);
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn PostNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: &PCWSTR, _: u32, hr: HRESULT, made: Ref<IShellItem>) -> WinResult<()> {
        self.after(hr, made.as_ref());
        Ok(())
    }
    fn UpdateProgress(&self, total: u32, so_far: u32) -> WinResult<()> {
        // The copy queue's list shows how far it is; Pause holds it here, Cancel stops it.
        if crate::copyqueue::progress(total, so_far) { Ok(()) } else { Err(E_ABORT.into()) }
    }
    fn ResetTimer(&self) -> WinResult<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> WinResult<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> WinResult<()> {
        Ok(())
    }
}

/// Before each item: on (Pause holds it here), or stopped by the copy queue's Cancel.
fn go_on() -> WinResult<()> {
    if crate::copyqueue::between_items() { Ok(()) } else { Err(E_ABORT.into()) }
}

/// Whether two paths are on the same drive (or network share): a move between them is only a
/// rename on the disk.
fn same_drive(a: &str, b: &str) -> bool {
    let root = |p: &str| -> String {
        let p = p.to_lowercase();
        match p.strip_prefix("\\\\") {
            // \\server\share
            Some(rest) => rest.split('\\').take(2).collect::<Vec<_>>().join("\\"),
            None => p.chars().take(2).collect(),
        }
    };
    root(a) == root(b)
}

/// One run of Windows' copy engine: queue items on `op`, then [`Engine::run`].
struct Engine {
    op: IFileOperation,
    done: Arc<Mutex<Done>>,
}

impl Engine {
    /// The copy engine with these flags - or None when Windows doesn't offer it (Wine), and the
    /// caller uses the fallback. Nothing has been done yet when this returns None.
    fn new(flags: FILEOPERATION_FLAGS) -> Option<Engine> {
        // SAFETY: COM calls in this thread's apartment (the job thread made one with ComGuard).
        unsafe {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).ok()?;
            // Wine's stubs answer "not implemented" here, before anything happens.
            op.SetOperationFlags(flags).ok()?;
            let done = Arc::new(Mutex::new(Done::default()));
            let sink: IFileOperationProgressSink = Sink(done.clone()).into();
            // Without the reports Glint only can't select what was made; the job itself still works.
            let _ = op.Advise(&sink);
            Some(Engine { op, done })
        }
    }

    /// The shell item for one of the job's items. One that is still there but that Windows can't
    /// open (a very long path, an odd name) is noted, so the job doesn't say everything went through.
    fn item(&self, path: &str) -> Option<IShellItem> {
        let item = shell_item(path);
        if item.is_none() && exists(path) {
            self.done.lock().unwrap_or_else(|e| e.into_inner()).unopened.get_or_insert_with(|| file_name(path).to_string());
        }
        item
    }

    /// The name of an item `item` couldn't open although it's there, if any.
    fn unopened(&self) -> Option<String> {
        self.done.lock().unwrap_or_else(|e| e.into_inner()).unopened.clone()
    }

    /// Does everything queued, showing Windows' own progress, questions and messages.
    fn run(self) -> Done {
        // SAFETY: COM calls on our own object, on the thread that made it.
        let (performed, aborted) = unsafe { (self.op.PerformOperations(), self.op.GetAnyOperationsAborted().map(|b| b.as_bool())) };
        drop(self.op); // lets go of the sink (unless the engine still holds it somewhere)
        let mut done = std::mem::take(&mut *self.done.lock().unwrap_or_else(|e| e.into_inner()));
        if let Err(e) = performed {
            if is_cancel(e.code()) {
                done.aborted = true;
            } else if done.error.is_none() {
                done.error = Some(e.code());
            }
        }
        done.aborted |= aborted.unwrap_or(false);
        done
    }
}

/// A Windows "shell item" for a path (what the copy engine works with), if the item is there. A
/// long path Windows can't open as it is gets a second try with the "\\?\" prefix (see `wide_path`).
fn shell_item(path: &str) -> Option<IShellItem> {
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let parse = |w: &[u16]| unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok() };
    let w = wide(path);
    parse(&w).or_else(|| Some(wide_path(path)).filter(|long| long.len() > w.len()).and_then(|long| parse(&long)))
}

/// The full path of a shell item (None for things that aren't files, such as Recycle Bin entries).
fn item_path(item: &IShellItem) -> Option<String> {
    // SAFETY: the returned string is copied, then freed with CoTaskMemFree as the API requires.
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// The names of those `paths` that are directly inside `dir`.
fn names_in_folder(paths: &[String], dir: &str) -> Vec<String> {
    paths.iter().filter(|p| parent(p).is_some_and(|d| same_path(&d, dir))).map(|p| file_name(p).to_string()).collect()
}

/// The fallback for copy, move and delete: SHFileOperation, the engine before IFileOperation.
/// `to`: nothing (delete), or one full target path per item (with FOF_MULTIDESTFILES).
fn legacy(func: u32, from: &[String], to: &[String], flags: FILEOPERATION_FLAGS) -> Done {
    let from_w = nul_list(from.iter().map(String::as_str));
    let to_w = (!to.is_empty()).then(|| nul_list(to.iter().map(String::as_str)));
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND::default(),
        wFunc: func,
        pFrom: PCWSTR(from_w.as_ptr()),
        pTo: to_w.as_ref().map_or(PCWSTR::null(), |t| PCWSTR(t.as_ptr())),
        // Only the old FOF_ flags fit (the FOFX_ ones are the copy engine's own).
        fFlags: (flags.0 & 0xFFFF) as u16,
        ..Default::default()
    };
    // SAFETY: the struct and both lists live across the call; the lists are double-NUL-terminated.
    let code = unsafe { SHFileOperationW(&mut op) };
    let mut done = Done { aborted: op.fAnyOperationsAborted.as_bool(), ..Default::default() };
    log::line(&format!("SHFileOperation (fallback) {func}: code {code:#x}, aborted {}", done.aborted));
    match code {
        0 => {}
        // ERROR_CANCELLED, DE_OPCANCELLED
        1223 | 0x75 => done.aborted = true,
        // SHFileOperation's own old codes, roughly as Windows errors.
        0x78 => done.error = Some(HRESULT::from_win32(5)),
        0x7C => done.error = Some(HRESULT::from_win32(2)),
        0x81 => done.error = Some(HRESULT::from_win32(206)),
        c if c > 0 && c < 0x70 => done.error = Some(HRESULT::from_win32(c as u32)),
        c => done.error = Some(HRESULT((0x8000_0000u32 | c as u32) as i32)),
    }
    done
}

/// Paths for SHFileOperation: each ends with a NUL, and one more NUL ends the list.
fn nul_list<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<u16> {
    paths.into_iter().flat_map(|p| p.encode_utf16().chain([0])).chain([0]).collect()
}

/// The names in a folder (for the fallback, to find what a copy made). None if it can't be read.
fn names_in(dir: &str) -> Option<HashSet<String>> {
    let entries = std::fs::read_dir(dir).ok()?;
    Some(entries.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
}

pub(crate) fn exists(path: &str) -> bool {
    attributes(path).is_some()
}

pub(crate) fn is_folder(path: &str) -> bool {
    attributes(path).is_some_and(|a| a & 0x10 != 0) // FILE_ATTRIBUTE_DIRECTORY
}

pub(crate) fn attributes(path: &str) -> Option<u32> {
    let w = wide_path(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    let a = unsafe { GetFileAttributesW(PCWSTR(w.as_ptr())) };
    (a != INVALID_FILE_ATTRIBUTES).then_some(a)
}

/// The Windows error number inside an HRESULT made from one (0x8007xxxx), if it is one.
fn win32_code(hr: HRESULT) -> Option<u32> {
    let v = hr.0 as u32;
    (v & 0xFFFF_0000 == 0x8007_0000).then_some(v & 0xFFFF)
}

/// The number inside one of the copy engine's own errors (COPYENGINE_E_..., 0x8027xxxx), if it is one.
fn engine_code(hr: HRESULT) -> Option<u32> {
    let v = hr.0 as u32;
    (v & 0xFFFF_0000 == 0x8027_0000).then_some(v & 0xFFFF)
}

/// The person stopped it ("Cancel" in Windows' progress window, or a question answered with Cancel).
fn is_cancel(hr: HRESULT) -> bool {
    // ERROR_CANCELLED, E_ABORT, COPYENGINE_E_USER_CANCELLED, COPYENGINE_E_CANCELLED
    matches!(hr.0 as u32, 0x8007_04C7 | 0x8000_4004 | 0x8027_0000 | 0x8027_0001)
}

/// A Windows error in plain words.
pub fn explain(hr: HRESULT) -> String {
    let text = match (win32_code(hr), engine_code(hr)) {
        (Some(5), _) | (_, Some(0x21 | 0x22 | 0x3F)) => "Windows says you don't have permission.",
        (Some(32 | 33), _) | (_, Some(0x27 | 0x28)) => "it's open in another program.",
        (Some(2 | 3), _) | (_, Some(0x23 | 0x24)) => "it isn't there any more.",
        (Some(19), _) | (_, Some(0x0F..=0x14)) => "the disk is write-protected.",
        (Some(21), _) => "the drive isn't ready.",
        (Some(39 | 112), _) | (_, Some(0x0E | 0x32 | 0x33)) => "there isn't enough free space.",
        (Some(53 | 64 | 67), _) | (_, Some(0x25 | 0x26)) => "the network location can't be reached.",
        (Some(80 | 183), _) | (_, Some(0x29..=0x2C)) => "there's already an item with that name.",
        (Some(123), _) | (_, Some(0x1B | 0x1C)) => "that name isn't allowed.",
        (Some(145), _) | (_, Some(0x3D)) => "the folder isn't empty.",
        (Some(206), _) | (_, Some(0x1D | 0x1E | 0x3B | 0x3C)) => "the name or path is too long.",
        (_, Some(0x09 | 0x0A)) => "a folder can't go into itself.",
        (_, Some(0x02)) => "Windows needs administrator permission for this.",
        _ => "",
    };
    if !text.is_empty() {
        return text.to_string();
    }
    let system = windows::core::Error::from(hr).message();
    let system = system.trim();
    if system.is_empty() {
        format!("Windows reported error 0x{:08X}.", hr.0 as u32)
    } else {
        system.to_string()
    }
}

// ---------------------------------------------------------------- names

/// The name Windows gives a copy in the same folder: "report - Copy.pdf", then "report - Copy (2).pdf".
pub fn copy_name(name: &str, is_folder: bool, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_folder => name.split_at(i),
        _ => (name, ""),
    };
    unique_name(&format!("{stem} - Copy"), ext, taken)
}

/// `base` + `ext`, or "base (2)ext", "base (3)ext"... - the first that `taken` says is free.
pub fn unique_name(base: &str, ext: &str, taken: impl Fn(&str) -> bool) -> String {
    let first = format!("{base}{ext}");
    if !taken(&first) {
        return first;
    }
    (2..10_000).map(|i| format!("{base} ({i}){ext}")).find(|n| !taken(n)).unwrap_or(first)
}

/// What a typed name turns into.
#[derive(Debug, PartialEq, Eq)]
pub enum NameCheck {
    /// Nothing left after tidying: leave the name as it was.
    Empty,
    /// Can't be used; the text says why.
    Bad(String),
    /// Use this (spaces around it and dots at the end taken off, as Windows would anyway).
    Good(String),
}

/// Checks a name typed for a file or folder, like Explorer does.
pub fn check_name(typed: &str) -> NameCheck {
    let name = typed.trim().trim_end_matches(['.', ' ']);
    if name.is_empty() {
        return NameCheck::Empty;
    }
    if name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32) {
        return NameCheck::Bad("A name can't contain any of these: \\ / : * ? \" < > |".to_string());
    }
    // Names Windows keeps for devices, also with an extension ("con.txt").
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        return NameCheck::Bad(format!("\u{201c}{name}\u{201d} is a name Windows keeps for itself. Pick another one."));
    }
    if name.encode_utf16().count() > 255 {
        return NameCheck::Bad("That name is too long (255 characters at most).".to_string());
    }
    NameCheck::Good(name.to_string())
}

/// How much of a name the name box selects at first: all of a folder's name, a file's name up to
/// its extension ("report" of "report.pdf"). In UTF-16 units, as the text box counts.
pub fn stem_len(name: &str, is_folder: bool) -> usize {
    match name.rfind('.') {
        Some(i) if i > 0 && !is_folder => name[..i].encode_utf16().count(),
        _ => name.encode_utf16().count(),
    }
}

/// The extension of a file name with its dot, in lower case (".pdf"), or "".
pub fn dotted_extension(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_lowercase(),
        _ => String::new(),
    }
}

/// Whether "to the Recycle Bin" really keeps `path` there. A USB stick, a network drive or a disc
/// has no Recycle Bin, so deleting there is for good either way.
pub fn has_recycle_bin(path: &str) -> bool {
    let root: String = path.chars().take(3).collect();
    if !(root.len() == 3 && root.ends_with(":\\")) {
        return false; // (\\server\share)
    }
    let w = wide(&root);
    // SAFETY: a NUL-terminated root path that outlives the call.
    let kind = unsafe { GetDriveTypeW(PCWSTR(w.as_ptr())) };
    !matches!(kind, 2 | 4 | 5) // DRIVE_REMOVABLE, DRIVE_REMOTE, DRIVE_CDROM
}

/// Whether `path` is a full path ("C:\..." or "\\server\share\...") of something inside a folder -
/// not a drive or a share itself, and not a bare name.
pub fn is_full_item_path(path: &str) -> bool {
    let b = path.as_bytes();
    let full = path.starts_with("\\\\") || (b.get(1) == Some(&b':') && b.get(2) == Some(&b'\\'));
    full && parent(path).is_some_and(|p| !p.is_empty())
}

/// Whether two paths name the same folder (Windows ignores capitals).
pub fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches('\\').to_lowercase() == b.trim_end_matches('\\').to_lowercase()
}

/// Whether `dest` is `src` itself or somewhere inside it (a folder can't go into itself).
pub fn is_same_or_inside(dest: &str, src: &str) -> bool {
    let (d, s) = (dest.trim_end_matches('\\').to_lowercase(), src.trim_end_matches('\\').to_lowercase());
    !s.is_empty() && (d == s || d.starts_with(&format!("{s}\\")))
}

// ---------------------------------------------------------------- the clipboard

/// "Preferred DropEffect": the clipboard format in which Explorer says whether files were cut.
fn drop_effect_format() -> u32 {
    // SAFETY: a constant string; registering an existing name just returns its number.
    unsafe { RegisterClipboardFormatW(w!("Preferred DropEffect")) }
}

/// The clipboard's change counter: it moves whenever anything is put on the clipboard.
pub fn clipboard_sequence() -> u32 {
    // SAFETY: no arguments.
    unsafe { GetClipboardSequenceNumber() }
}

/// Whether there are files on the clipboard to paste (copied or cut in Glint or File Explorer).
pub fn clipboard_has_files() -> bool {
    // SAFETY: a plain query; it doesn't open the clipboard or ask its owner for the data.
    unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32).is_ok() }
}

/// Puts files on the clipboard to be pasted (in Glint, File Explorer or any other program): copied,
/// or cut (`cut`: the paste moves them). Returns the clipboard's change counter afterwards, or None
/// if the clipboard was busy.
pub fn put_files(owner: HWND, paths: &[String], cut: bool) -> Option<u32> {
    let files = drop_files_bytes(paths);
    // DROPEFFECT_MOVE for cut; DROPEFFECT_COPY | DROPEFFECT_LINK for copy, as Explorer does.
    let effect: u32 = if cut { 2 } else { 5 };
    // SAFETY: the clipboard is opened and always closed. Each memory block is either handed to the
    // clipboard (which then owns it) or freed here.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let mut ok = EmptyClipboard().is_ok() && set_clipboard_bytes(CF_HDROP.0 as u32, &files);
        if ok {
            let format = drop_effect_format();
            ok = format != 0 && set_clipboard_bytes(format, &effect.to_le_bytes());
        }
        let _ = CloseClipboard();
        ok.then(clipboard_sequence)
    }
}

/// Copies `bytes` into a new memory block and gives it to the (open) clipboard.
///
/// # Safety
/// The clipboard must be open on this thread.
unsafe fn set_clipboard_bytes(format: u32, bytes: &[u8]) -> bool {
    // SAFETY: the block is allocated with room for `bytes`, filled while locked, and then owned by
    // the clipboard - or freed if the clipboard refused it.
    unsafe {
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len()) else { return false };
        let dst = GlobalLock(mem) as *mut u8;
        if dst.is_null() {
            let _ = GlobalFree(Some(mem));
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        let _ = GlobalUnlock(mem);
        if SetClipboardData(format, Some(HANDLE(mem.0))).is_ok() {
            true
        } else {
            let _ = GlobalFree(Some(mem));
            false
        }
    }
}

/// Files on the clipboard: their paths, whether they were cut, and the clipboard's change counter.
struct Clip {
    files: Vec<String>,
    cut: bool,
    seq: u32,
}

/// Opens the clipboard for this thread, trying for a moment if another program has it open.
fn open_clipboard() -> bool {
    for _ in 0..10 {
        // SAFETY: no owner window (this is a background thread); closed by the caller.
        if unsafe { OpenClipboard(None) }.is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

/// Reads the files on the clipboard (on a job thread: the program that put them there may take a
/// moment to hand them over).
fn read_clipboard() -> Result<Clip, String> {
    if !open_clipboard() {
        return Err("The clipboard is busy. Try again.".to_string());
    }
    // SAFETY: the clipboard is open on this thread until CloseClipboard below; the handles it gives
    // stay owned by it and are only read while locked.
    unsafe {
        let files = GetClipboardData(CF_HDROP.0 as u32).ok().and_then(|h| global_bytes(h)).map(|b| parse_drop_files(&b)).unwrap_or_default();
        let format = drop_effect_format();
        let effect = if format == 0 { None } else { GetClipboardData(format).ok().and_then(|h| global_bytes(h)) };
        let effect = effect.and_then(|b| b.get(..4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))).unwrap_or(5);
        let seq = clipboard_sequence();
        let _ = CloseClipboard();
        // Cut means "move" without "copy" (Explorer: DROPEFFECT_MOVE alone).
        Ok(Clip { files, cut: effect & 2 != 0 && effect & 1 == 0, seq })
    }
}

/// A copy of the bytes in a clipboard memory block.
///
/// # Safety
/// `h` must be a memory handle from GetClipboardData, with the clipboard still open.
unsafe fn global_bytes(h: HANDLE) -> Option<Vec<u8>> {
    // SAFETY: as the caller promises; the block is read only while locked, within its size.
    unsafe {
        let mem = HGLOBAL(h.0);
        let size = GlobalSize(mem);
        let p = GlobalLock(mem) as *const u8;
        if p.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(mem);
        Some(bytes)
    }
}

/// Empties the clipboard (Ctrl+C inside an archive, until its items are out).
pub fn clear_clipboard(owner: HWND) {
    // SAFETY: the clipboard is opened here and closed again.
    unsafe {
        if OpenClipboard(Some(owner)).is_ok() {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

/// Empties the clipboard if nothing else was put on it since `seq` (after cut files were moved).
fn clear_clipboard_if(seq: u32) {
    if !open_clipboard() {
        return;
    }
    // SAFETY: the clipboard is open on this thread.
    unsafe {
        if clipboard_sequence() == seq {
            let _ = EmptyClipboard();
        }
        let _ = CloseClipboard();
    }
}

/// The size of Windows' DROPFILES header, which comes before the file list in CF_HDROP.
const DROPFILES_SIZE: usize = 20;

/// CF_HDROP data for `paths`: a DROPFILES header (file list at byte 20, wide characters), then each
/// path in UTF-16 ending with a NUL, then one more NUL.
pub fn drop_files_bytes(paths: &[String]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DROPFILES_SIZE + paths.iter().map(|p| p.len() * 2 + 2).sum::<usize>() + 2);
    bytes.extend_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes()); // pFiles
    bytes.extend_from_slice(&[0; 8]); // pt
    bytes.extend_from_slice(&0u32.to_le_bytes()); // fNC
    bytes.extend_from_slice(&1u32.to_le_bytes()); // fWide
    for p in paths {
        for unit in p.encode_utf16().chain([0]) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// The paths in CF_HDROP data (wide or, from very old programs, 8-bit). Never reads past the end.
pub fn parse_drop_files(bytes: &[u8]) -> Vec<String> {
    let word = |at: usize| bytes.get(at..at + 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]));
    let (Some(start), Some(wide)) = (word(0), word(16)) else { return Vec::new() };
    let Some(body) = bytes.get(start as usize..) else { return Vec::new() };
    let mut paths = Vec::new();
    if wide != 0 {
        let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        for part in units.split(|&u| u == 0) {
            if part.is_empty() {
                break;
            }
            paths.push(String::from_utf16_lossy(part));
        }
    } else {
        for part in body.split(|&b| b == 0) {
            if part.is_empty() {
                break;
            }
            paths.push(String::from_utf8_lossy(part).into_owned());
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own in the temp folder, for a test that works with real files.
    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("glint-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn compress_and_extract_jobs_pick_free_names() {
        use crate::archive::Format;
        let dir = scratch("archive-jobs");
        let docs = join(&dir, "Docs");
        std::fs::create_dir_all(join(&docs, "inner")).unwrap();
        std::fs::write(join(&docs, "a.txt"), b"alpha").unwrap();
        std::fs::write(join(&join(&docs, "inner"), "b.txt"), b"beta").unwrap();
        // One folder: named after it; taken, so numbered.
        let first = compress(&[docs.clone()], Format::Zip);
        assert!(first.ok, "{:?}", first.note);
        assert_eq!(first.created, Some((dir.clone(), vec!["Docs.zip".to_string()])));
        assert_eq!(first.undo, Some(crate::undo::Step::Made { paths: vec![join(&dir, "Docs.zip")], what: "new archive" }));
        assert_eq!(compress(&[docs.clone()], Format::Zip).created.unwrap().1, ["Docs (2).zip"]);
        // Several items: named after their folder.
        let seven = compress(&[join(&docs, "a.txt"), join(&docs, "inner")], Format::SevenZ);
        assert_eq!(seven.created, Some((docs.clone(), vec!["Docs.7z".to_string()])));

        // Extract to: "Docs" is taken (by the folder itself), so "Docs (2)".
        let zip = join(&dir, "Docs.zip");
        let to = extract(&zip, true);
        assert!(to.ok, "{:?}", to.note);
        assert_eq!(to.created, Some((dir.clone(), vec!["Docs (2)".to_string()])));
        assert_eq!(std::fs::read(join(&join(&join(&dir, "Docs (2)"), "Docs"), "a.txt")).unwrap(), b"alpha");
        // Extract here: the zip's "Docs" lands as the next free name, "Docs (3)".
        let here = extract(&zip, false);
        assert!(here.ok, "{:?}", here.note);
        assert_eq!(here.created, Some((dir.clone(), vec!["Docs (3)".to_string()])));
        assert_eq!(std::fs::read(join(&join(&join(&dir, "Docs (3)"), "inner"), "b.txt")).unwrap(), b"beta");
        assert_eq!(std::fs::read(join(&join(&dir, "Docs"), "a.txt")).unwrap(), b"alpha", "the original is untouched");
        assert_eq!(here.undo, Some(crate::undo::Step::Made { paths: vec![join(&dir, "Docs (3)")], what: "extract" }));
        // A 7z made of two items comes out as those two, beside the originals.
        let mut back = extract(&join(&docs, "Docs.7z"), false).created.unwrap().1;
        back.sort();
        assert_eq!(back, ["a (2).txt", "inner (2)"]);
        assert!(!extract(&join(&dir, "missing.zip"), false).ok);
    }

    #[test]
    fn a_rename_and_a_move_can_be_undone_and_redone() {
        let _com = ComGuard::init();
        let dir = scratch("undo");
        let (a, b) = (join(&dir, "a"), join(&dir, "b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(join(&a, "note.txt"), "hi").unwrap();
        // Renamed, then undone: the old name is back.
        let out = run(Job::Rename { path: join(&a, "note.txt"), new_name: "memo.txt".into() });
        let step = out.undo.expect("a rename can be undone");
        assert!(exists(&join(&a, "memo.txt")));
        assert!(run(step.undo_job()).ok);
        assert!(exists(&join(&a, "note.txt")) && !exists(&join(&a, "memo.txt")));
        assert!(run(step.redo_job()).ok);
        assert!(exists(&join(&a, "memo.txt")));
        // Moved to b, then back.
        let step = crate::undo::Step::Moved { pairs: vec![(join(&a, "memo.txt"), join(&b, "memo.txt"))] };
        assert!(run(step.redo_job()).ok);
        assert!(exists(&join(&b, "memo.txt")) && !exists(&join(&a, "memo.txt")));
        assert!(run(step.undo_job()).ok);
        assert!(exists(&join(&a, "memo.txt")) && !exists(&join(&b, "memo.txt")));
        // Never over something that took its place meanwhile.
        std::fs::write(join(&b, "memo.txt"), "other").unwrap();
        let out = run(step.redo_job());
        assert!(!out.ok && exists(&join(&a, "memo.txt")));
        assert_eq!(std::fs::read_to_string(join(&b, "memo.txt")).unwrap(), "other");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_renames_hand_names_on_and_swap() {
        let dir = scratch("many");
        for (n, text) in [("1.txt", "one"), ("2.txt", "two"), ("a.txt", "a"), ("b.txt", "b")] {
            std::fs::write(join(&dir, n), text).unwrap();
        }
        let p = |n: &str| join(&dir, n);
        // 1 -> 2 and 2 -> 3 (a chain), a <-> b (a circle).
        let pairs = vec![(p("1.txt"), p("2.txt")), (p("2.txt"), p("3.txt")), (p("a.txt"), p("b.txt")), (p("b.txt"), p("a.txt"))];
        let out = run(Job::RenameMany { pairs });
        assert!(out.ok, "{:?}", out.note);
        let read = |n: &str| std::fs::read_to_string(p(n)).unwrap();
        assert_eq!((read("2.txt"), read("3.txt"), read("b.txt"), read("a.txt")), ("one".into(), "two".into(), "a".into(), "b".into()));
        assert!(!exists(&p("1.txt")));
        // Ctrl+Z puts every name back.
        assert!(run(out.undo.unwrap().undo_job()).ok);
        assert_eq!((read("1.txt"), read("2.txt"), read("a.txt"), read("b.txt")), ("one".into(), "two".into(), "a".into(), "b".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A network share has no Recycle Bin, so deleting there is asked as deleting for good.
    #[test]
    fn network_shares_have_no_recycle_bin() {
        assert!(!has_recycle_bin("\\\\server\\share\\notes.txt"));
        assert!(has_recycle_bin("C:\\Users\\notes.txt"));
    }

    #[test]
    fn unique_names() {
        let taken = ["New folder", "New folder (2)", "New Text Document.txt"];
        let is_taken = |n: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(n));
        assert_eq!(unique_name("New folder", "", is_taken), "New folder (3)");
        assert_eq!(unique_name("New Text Document", ".txt", is_taken), "New Text Document (2).txt");
        assert_eq!(unique_name("Fresh", "", is_taken), "Fresh");
        let copies = ["report - Copy.pdf"];
        let copy_taken = |n: &str| copies.contains(&n);
        assert_eq!(copy_name("report.pdf", false, copy_taken), "report - Copy (2).pdf");
        assert_eq!(copy_name("v1.2", true, copy_taken), "v1.2 - Copy");
        assert_eq!(copy_name(".gitignore", false, copy_taken), ".gitignore - Copy");
    }

    #[test]
    fn names_are_checked_like_explorer() {
        assert_eq!(check_name("  report.pdf  "), NameCheck::Good("report.pdf".into()));
        assert_eq!(check_name("notes..."), NameCheck::Good("notes".into()));
        assert_eq!(check_name(" ... "), NameCheck::Empty);
        assert!(matches!(check_name("a/b"), NameCheck::Bad(_)));
        assert!(matches!(check_name("what?"), NameCheck::Bad(_)));
        assert!(matches!(check_name("CON"), NameCheck::Bad(_)));
        assert!(matches!(check_name("lpt1.txt"), NameCheck::Bad(_)));
        assert_eq!(check_name("COM10"), NameCheck::Good("COM10".into()));
        assert_eq!(check_name("console.log"), NameCheck::Good("console.log".into()));
        assert!(matches!(check_name(&"x".repeat(256)), NameCheck::Bad(_)));
    }

    #[test]
    fn stems() {
        assert_eq!(stem_len("report.pdf", false), 6);
        assert_eq!(stem_len("archive.tar.gz", false), 11);
        assert_eq!(stem_len(".gitignore", false), 10);
        assert_eq!(stem_len("v1.2", true), 4);
        assert_eq!(stem_len("Ünïcode.txt", false), 7);
        assert_eq!(dotted_extension("Photo.JPG"), ".jpg");
        assert_eq!(dotted_extension("README"), "");
        assert_eq!(dotted_extension(".bashrc"), "");
    }

    #[test]
    fn one_cut_is_moved_by_one_paste_at_a_time() {
        let first = MovingCut::begin(0xC0FFEE).expect("nothing moves yet");
        assert!(MovingCut::begin(0xC0FFEE).is_none(), "a second paste of the same cut waits");
        let other = MovingCut::begin(0xBEEF);
        assert!(other.is_some(), "another cut may move meanwhile");
        drop(first);
        assert!(MovingCut::begin(0xC0FFEE).is_some(), "once done (or stopped), it can be pasted again");
    }

    #[test]
    fn only_full_item_paths_are_pasted() {
        assert!(is_full_item_path("C:\\Users\\me\\a.txt"));
        assert!(is_full_item_path("\\\\server\\share\\a.txt"));
        assert!(!is_full_item_path("a.txt"), "a bare name");
        assert!(!is_full_item_path("C:a.txt"), "relative to C:'s current folder");
        assert!(!is_full_item_path("E:\\"), "a whole drive");
        assert!(!is_full_item_path("\\\\server\\share"), "a whole share");
    }

    #[test]
    fn paths_inside_paths() {
        assert!(is_same_or_inside("C:\\a\\b", "C:\\A"));
        assert!(is_same_or_inside("C:\\a", "c:\\a\\"));
        assert!(!is_same_or_inside("C:\\ab", "C:\\a"));
        assert!(!is_same_or_inside("C:\\", "C:\\a"));
        assert!(same_path("C:\\Users\\", "c:\\users"));
        assert!(!same_path("C:\\Users", "C:\\Users2"));
    }

    #[test]
    fn clipboard_file_lists_round_trip() {
        let paths = vec!["C:\\Users\\me\\report.pdf".to_string(), "D:\\Ünïcode folder".to_string()];
        let bytes = drop_files_bytes(&paths);
        assert_eq!(&bytes[..4], &20u32.to_le_bytes());
        assert_eq!(DROPFILES_SIZE, std::mem::size_of::<windows::Win32::UI::Shell::DROPFILES>());
        assert_eq!(parse_drop_files(&bytes), paths);
        // Cut short or nonsense: never more than what is there.
        assert!(parse_drop_files(&bytes[..10]).is_empty());
        let mut odd = bytes.clone();
        odd[0] = 200;
        assert!(parse_drop_files(&odd).is_empty());
        // An old 8-bit list.
        let mut ansi = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        ansi.extend_from_slice(b"C:\\a.txt\0C:\\b.txt\0\0");
        assert_eq!(parse_drop_files(&ansi), vec!["C:\\a.txt", "C:\\b.txt"]);
    }


    #[test]
    fn errors_in_plain_words() {
        assert_eq!(explain(HRESULT::from_win32(32)), "it's open in another program.");
        assert_eq!(win32_code(HRESULT::from_win32(183)), Some(183));
        assert!(is_cancel(HRESULT::from_win32(1223)));
        // The copy engine's own codes (real Windows).
        use windows::Win32::UI::Shell::{COPYENGINE_E_ALREADY_EXISTS_NORMAL, COPYENGINE_E_CANCELLED, COPYENGINE_E_SHARING_VIOLATION_SRC};
        assert_eq!(explain(COPYENGINE_E_ALREADY_EXISTS_NORMAL), "there's already an item with that name.");
        assert_eq!(explain(COPYENGINE_E_SHARING_VIOLATION_SRC), "it's open in another program.");
        assert!(is_cancel(COPYENGINE_E_CANCELLED));
    }

    /// A `scratch` folder that goes again when the test ends (also when it fails).
    struct Scratch(String);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            Scratch(scratch(name))
        }
        fn at(&self, name: &str) -> String {
            join(&self.0, name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Writes `text` to `name` in `dir`; its path.
    fn put(dir: &str, name: &str, text: &str) -> String {
        let p = join(dir, name);
        std::fs::write(&p, text).unwrap();
        p
    }

    fn read(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// The copy queue's id of the job shown as `label`, once it has joined.
    fn listed(label: &str) -> u64 {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(t) = crate::copyqueue::shown().into_iter().find(|t| t.label == label) {
                return t.id;
            }
            assert!(Instant::now() < until, "{label} never joined the queue");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether this disk changes capitals in a rename (Windows does; Wine keeps the old ones).
    fn renames_capitals(dir: &str) -> bool {
        let probe = put(dir, "capitals-probe", "");
        let _ = std::fs::rename(&probe, join(dir, "CAPITALS-PROBE"));
        let found = names_in(dir).unwrap().contains("CAPITALS-PROBE");
        let _ = std::fs::remove_file(join(dir, "CAPITALS-PROBE"));
        found
    }

    #[test]
    fn undoing_a_rename_never_replaces_what_took_the_old_name() {
        use crate::undo::Step;
        let _com = ComGuard::init();
        let s = Scratch::new("undo-taken");
        put(&s.0, "a.txt", "mine");
        let step = run(Job::Rename { path: s.at("a.txt"), new_name: "b.txt".into() }).undo.expect("renamed");
        assert_eq!(step, Step::Renamed { dir: s.0.clone(), from: "a.txt".into(), to: "b.txt".into() });
        // Meanwhile something else got the old name (in other capitals).
        put(&s.0, "A.TXT", "theirs");
        let out = run(step.undo_job());
        assert!(!out.ok && out.undo.is_none());
        assert_eq!((read(&s.at("a.txt")), read(&s.at("b.txt"))), ("theirs".into(), "mine".into()));
        assert_eq!(out.retry, Some((s.at("b.txt"), "a.txt".to_string())), "the name box opens again to pick another");
        // Moved away elsewhere meanwhile: nothing happens, and it says so.
        std::fs::create_dir(s.at("elsewhere")).unwrap();
        std::fs::rename(s.at("b.txt"), s.at("elsewhere\\b.txt")).unwrap();
        std::fs::remove_file(s.at("A.TXT")).unwrap();
        let out = run(step.undo_job());
        assert!(!out.ok && out.undo.is_none());
        assert!(out.note.as_deref().is_some_and(|n| n.contains("isn't there any more")), "{:?}", out.note);
        assert!(!exists(&s.at("a.txt")));
        assert_eq!(read(&s.at("elsewhere\\b.txt")), "mine");
    }

    #[test]
    fn a_change_of_capitals_only_is_undone_and_redone() {
        let _com = ComGuard::init();
        let s = Scratch::new("undo-capitals");
        put(&s.0, "readme.txt", "text");
        // The name "taken" is the item itself: not refused.
        let out = run(Job::Rename { path: s.at("readme.txt"), new_name: "README.txt".into() });
        assert!(out.ok, "{:?}", out.note);
        let capitals = renames_capitals(&s.0);
        let names = || names_in(&s.0).unwrap();
        if capitals {
            assert!(names().contains("README.txt"));
        }
        let step = out.undo.unwrap();
        assert!(run(step.undo_job()).ok);
        if capitals {
            assert!(names().contains("readme.txt"));
        }
        assert!(run(step.redo_job()).ok);
        assert_eq!(names().len(), 1, "never two, never none");
        assert_eq!(read(&s.at("readme.txt")), "text");
    }

    #[test]
    fn moving_back_makes_the_folder_it_came_from_again() {
        use crate::undo::Step;
        let _com = ComGuard::init();
        let s = Scratch::new("undo-folder-gone");
        let (from, to) = (s.at("src\\deep"), s.at("dst"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        put(&from, "a.txt", "a");
        let step = Step::Moved { pairs: vec![(join(&from, "a.txt"), join(&to, "a.txt"))] };
        assert!(run(step.redo_job()).ok);
        // The folder it came from was deleted since.
        std::fs::remove_dir_all(s.at("src")).unwrap();
        let out = run(step.undo_job());
        assert!(out.ok, "{:?}", out.note);
        assert_eq!(read(&join(&from, "a.txt")), "a");
        assert!(!exists(&join(&to, "a.txt")));
        // Gone from where it was moved to: nothing to move back.
        let out = run(step.undo_job());
        assert!(!out.ok && out.undo.is_none());
        assert_eq!(out.note.as_deref(), Some("It isn't where it was moved to any more."));
    }

    #[test]
    fn moving_back_with_one_place_taken_moves_the_rest_and_replaces_nothing() {
        use crate::undo::Step;
        let _com = ComGuard::init();
        let s = Scratch::new("undo-place-taken");
        let (from, to) = (s.at("src"), s.at("dst"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::create_dir_all(&to).unwrap();
        let pairs: Vec<(String, String)> = ["a.txt", "b.txt"].iter().map(|n| (put(&from, n, n), join(&to, n))).collect();
        let step = Step::Moved { pairs };
        assert!(run(step.redo_job()).ok);
        // Something new is called b.txt where b.txt came from.
        put(&from, "b.txt", "newcomer");
        let out = run(step.undo_job());
        assert!(!out.ok);
        assert_eq!(read(&join(&from, "a.txt")), "a.txt");
        assert_eq!(read(&join(&from, "b.txt")), "newcomer", "never replaced");
        assert_eq!(read(&join(&to, "b.txt")), "b.txt", "stays where it is");
        assert_eq!(out.undo, Some(Step::Moved { pairs: vec![(join(&to, "a.txt"), join(&from, "a.txt"))] }), "only what really moved");
        assert!(out.note.as_deref().is_some_and(|n| n.contains("b.txt") && n.contains("stayed where it was")), "{:?}", out.note);
    }

    #[test]
    fn deleting_for_good_leaves_nothing_to_undo() {
        let _com = ComGuard::init();
        let s = Scratch::new("for-good");
        let file = put(&s.0, "a.txt", "a");
        std::fs::create_dir(s.at("F")).unwrap();
        put(&s.at("F"), "in.txt", "in");
        let out = run(Job::Delete { paths: vec![file.clone(), s.at("F")], recycle: false });
        assert!(out.ok, "{:?}", out.note);
        assert!(out.undo.is_none(), "there's no way back");
        assert!(!exists(&file) && !exists(&s.at("F")));
        assert_eq!(out.note.as_deref(), Some("2 items deleted"));
    }

    #[test]
    fn renaming_many_never_replaces_anything_and_does_the_rest() {
        use crate::undo::Step;
        let s = Scratch::new("many-taken");
        for n in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            put(&s.0, n, n);
        }
        put(&s.0, "outsider.txt", "keep me");
        // a onto an item that isn't being renamed (in other capitals); b and c onto one name.
        let pairs = vec![(s.at("a.txt"), s.at("Outsider.TXT")), (s.at("b.txt"), s.at("same.txt")), (s.at("c.txt"), s.at("same.txt")), (s.at("d.txt"), s.at("e.txt"))];
        let out = run(Job::RenameMany { pairs });
        assert!(!out.ok);
        assert_eq!(read(&s.at("outsider.txt")), "keep me", "never replaced");
        assert_eq!(read(&s.at("a.txt")), "a.txt", "left as it was");
        assert_eq!((read(&s.at("same.txt")), read(&s.at("c.txt"))), ("b.txt".into(), "c.txt".into()), "the first gets the name, the other keeps its own");
        assert_eq!(read(&s.at("e.txt")), "d.txt");
        assert_eq!(names_in(&s.0).unwrap().len(), 5, "nothing lost, no temporary name left");
        assert_eq!(out.undo, Some(Step::Moved { pairs: vec![(s.at("b.txt"), s.at("same.txt")), (s.at("d.txt"), s.at("e.txt"))] }), "Ctrl+Z undoes only what was done");
        assert_eq!(out.note.as_deref(), Some("Couldn't rename 2 items: the name is taken, or it's in use"));
    }

    #[test]
    fn names_going_round_in_a_circle_all_arrive_and_go_back() {
        let s = Scratch::new("many-circle");
        put(&s.0, "1", "one");
        put(&s.0, "2", "two");
        std::fs::create_dir(s.at("3")).unwrap();
        put(&s.at("3"), "in.txt", "three");
        // 1 -> 2 -> 3 -> 1, with a folder among them.
        let out = run(Job::RenameMany { pairs: vec![(s.at("1"), s.at("2")), (s.at("2"), s.at("3")), (s.at("3"), s.at("1"))] });
        assert!(out.ok, "{:?}", out.note);
        assert_eq!((read(&s.at("2")), read(&s.at("3")), read(&s.at("1\\in.txt"))), ("one".into(), "two".into(), "three".into()));
        assert_eq!(names_in(&s.0).unwrap().len(), 3, "no temporary name left");
        // Ctrl+Z, Ctrl+Y, Ctrl+Z: the same three each time.
        let step = out.undo.unwrap();
        for job in [step.undo_job(), step.redo_job(), step.undo_job()] {
            let out = run(job);
            assert!(out.ok, "{:?}", out.note);
        }
        assert_eq!((read(&s.at("1")), read(&s.at("2")), read(&s.at("3\\in.txt"))), ("one".into(), "two".into(), "three".into()));
        assert_eq!(names_in(&s.0).unwrap().len(), 3);
    }

    #[test]
    fn folders_renamed_together_say_where_each_went() {
        let s = Scratch::new("many-folders");
        for (n, text) in [("Day 1", "first"), ("Day 2", "second"), ("Other", "other")] {
            std::fs::create_dir(s.at(n)).unwrap();
            put(&s.at(n), "in.txt", text);
        }
        // The two days swap names, and "Other" gets a new one.
        let out = run(Job::RenameMany { pairs: vec![(s.at("Day 2"), s.at("Day 1")), (s.at("Day 1"), s.at("Day 2")), (s.at("Other"), s.at("Day 3"))] });
        assert!(out.ok, "{:?}", out.note);
        let text = |n: &str| read(&join(&s.at(n), "in.txt"));
        assert_eq!((text("Day 1"), text("Day 2"), text("Day 3")), ("second".into(), "first".into(), "other".into()));
        // Followed one after the other, as tabs inside them follow (app.rs), each lands where it went.
        let follow = |path: &str| out.moved_folders.iter().fold(path.to_string(), |p, (from, to)| crate::app::moved_path(&p, from, to).unwrap_or(p));
        assert_eq!(follow(&s.at("Day 1\\in.txt")), s.at("Day 2\\in.txt"));
        assert_eq!(follow(&s.at("Day 2")), s.at("Day 1"));
        assert_eq!(follow(&s.at("Other")), s.at("Day 3"));
    }

    #[test]
    fn a_swap_with_one_item_in_use_leaves_both_as_they_were() {
        use std::os::windows::fs::OpenOptionsExt;
        let s = Scratch::new("many-in-use");
        put(&s.0, "a.txt", "a");
        put(&s.0, "b.txt", "b");
        // b is open in a program that lets nothing else at it.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(s.at("b.txt")).unwrap();
        let out = run(Job::RenameMany { pairs: vec![(s.at("a.txt"), s.at("b.txt")), (s.at("b.txt"), s.at("a.txt"))] });
        drop(held);
        assert!(!out.ok && out.undo.is_none());
        assert_eq!((read(&s.at("a.txt")), read(&s.at("b.txt"))), ("a".into(), "b".into()), "each back under its own name");
        assert_eq!(names_in(&s.0).unwrap().len(), 2, "no temporary name left");
    }

    #[test]
    fn renaming_many_works_in_a_folder_past_260_characters() {
        let s = Scratch::new("many-long");
        let mut deep = s.0.clone();
        while deep.len() < 300 {
            deep = join(&deep, "a folder with a long name");
        }
        std::fs::create_dir_all(&deep).unwrap();
        let p = |n: &str| join(&deep, n);
        put(&deep, "1.txt", "one");
        put(&deep, "2.txt", "two");
        let out = run(Job::RenameMany { pairs: vec![(p("1.txt"), p("2.txt")), (p("2.txt"), p("1.txt"))] });
        assert!(out.ok, "{:?}", out.note);
        assert_eq!((read(&p("1.txt")), read(&p("2.txt"))), ("two".into(), "one".into()));
        assert!(run(out.undo.unwrap().undo_job()).ok);
        assert_eq!((read(&p("1.txt")), read(&p("2.txt"))), ("one".into(), "two".into()));
    }

    #[test]
    fn a_copy_cancelled_while_it_waits_copies_nothing() {
        let s = Scratch::new("queue-cancel");
        let dest = s.at("queue-cancel-dest");
        std::fs::create_dir(&dest).unwrap();
        let file = put(&s.0, "a.txt", "x");
        // Another copy is running: this one waits its turn, and is cancelled meanwhile.
        let running = crate::copyqueue::Ticket::join("queue-cancel: the copy before".into());
        assert!(running.wait_turn());
        let job = {
            let (file, dest) = (file.clone(), dest.clone());
            std::thread::spawn(move || {
                let _com = ComGuard::init();
                transfer(&[file], &dest, false, false)
            })
        };
        crate::copyqueue::cancel(listed("Copying \u{201c}a.txt\u{201d} to \u{201c}queue-cancel-dest\u{201d}"));
        let out = job.join().unwrap();
        drop(running);
        assert!(!out.ok && out.undo.is_none());
        assert_eq!(out.note.as_deref(), Some("Cancelled: \u{201c}a.txt\u{201d} wasn't copied."));
        assert!(names_in(&dest).unwrap().is_empty(), "nothing was copied");
        assert_eq!(read(&file), "x");
    }

    #[test]
    fn two_copies_into_one_folder_take_turns_and_each_undoes_only_its_own() {
        use crate::undo::Step;
        let s = Scratch::new("queue-two");
        let dest = s.at("queue-two-dest");
        std::fs::create_dir(&dest).unwrap();
        let copy = |file: String| {
            let dest = dest.clone();
            std::thread::spawn(move || {
                let _com = ComGuard::init();
                transfer(&[file], &dest, false, false)
            })
        };
        // Something else is copying: both wait, in the order they came.
        let running = crate::copyqueue::Ticket::join("queue-two: the copy before".into());
        assert!(running.wait_turn());
        let first = copy(put(&s.0, "a.txt", "a"));
        let one = listed("Copying \u{201c}a.txt\u{201d} to \u{201c}queue-two-dest\u{201d}");
        let second = copy(put(&s.0, "b.txt", "b"));
        let two = listed("Copying \u{201c}b.txt\u{201d} to \u{201c}queue-two-dest\u{201d}");
        let order: Vec<u64> = crate::copyqueue::shown().iter().map(|t| t.id).filter(|id| [one, two].contains(id)).collect();
        assert_eq!(order, [one, two]);
        assert!(names_in(&dest).unwrap().is_empty(), "neither starts while another copy runs");
        drop(running);
        let (first, second) = (first.join().unwrap(), second.join().unwrap());
        assert!(first.ok && second.ok, "{:?} {:?}", first.note, second.note);
        assert_eq!(first.undo, Some(Step::Made { paths: vec![join(&dest, "a.txt")], what: "copy" }));
        assert_eq!(second.undo, Some(Step::Made { paths: vec![join(&dest, "b.txt")], what: "copy" }), "not the other copy's file");
    }

    #[test]
    fn a_copy_whose_item_vanished_while_it_waited_says_so() {
        use crate::undo::Step;
        let s = Scratch::new("queue-gone");
        let dest = s.at("queue-gone-dest");
        std::fs::create_dir(&dest).unwrap();
        let sources = vec![put(&s.0, "kept.txt", "k"), put(&s.0, "gone.txt", "g")];
        let running = crate::copyqueue::Ticket::join("queue-gone: the copy before".into());
        assert!(running.wait_turn());
        let job = {
            let (sources, dest) = (sources.clone(), dest.clone());
            std::thread::spawn(move || {
                let _com = ComGuard::init();
                transfer(&sources, &dest, false, false)
            })
        };
        listed("Copying 2 items to \u{201c}queue-gone-dest\u{201d}");
        // Deleted elsewhere while it waited its turn.
        std::fs::remove_file(&sources[1]).unwrap();
        drop(running);
        let out = job.join().unwrap();
        assert_eq!(read(&join(&dest, "kept.txt")), "k", "what's still there is copied");
        assert!(!exists(&join(&dest, "gone.txt")));
        assert!(!out.ok, "not everything was copied: {:?}", out.note);
        assert!(out.note.as_deref().is_some_and(|n| n.contains("gone.txt") && n.contains("isn't there any more")), "{:?}", out.note);
        assert_eq!(out.undo, Some(Step::Made { paths: vec![join(&dest, "kept.txt")], what: "copy" }));
    }
}
