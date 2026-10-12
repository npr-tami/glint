//! Working with files: copy and move (paste), delete (to the Recycle Bin or for good), rename, and
//! new folders and text files - plus files on the clipboard.
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
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
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
        matches!(self, Job::Paste { .. } | Job::Delete { .. } | Job::Bin(_) | Job::Drop { how: DropHow::Copy | DropHow::Move, .. })
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
    // The folders among what moves, to tell afterwards which of them went (tabs inside follow).
    let folders: Vec<&String> = if moving { sources.iter().filter(|s| is_folder(s)).collect() } else { Vec::new() };
    let mut flags = FOF_ALLOWUNDO | FOFX_ADDUNDORECORD | FOF_NOCONFIRMMKDIR;
    if !moving && same_folder {
        flags |= FOF_RENAMEONCOLLISION;
    }
    let (done, made) = match Engine::new(flags) {
        Some(engine) => {
            let Some(target) = shell_item(dest) else {
                return Outcome::note(format!("Couldn't {verb} here: Windows can't open \u{201c}{}\u{201d}.", file_name(dest)));
            };
            let mut queued = 0;
            for s in sources {
                let Some(item) = shell_item(s) else { continue };
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
                return Outcome::note(format!("Couldn't {verb}: {} isn't there any more.", items_text(sources)));
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
            let done = legacy(if moving { FO_MOVE } else { FO_COPY }, sources, &targets, flags);
            let made = match (before, names_in(dest)) {
                (Some(b), Some(a)) => a.difference(&b).cloned().collect(),
                _ => Vec::new(),
            };
            (done, made)
        }
    };
    // What to select: what Windows reported (or what appeared), else the items' own names.
    let names = if !made.is_empty() {
        made
    } else if !same_folder {
        sources.iter().map(|s| file_name(s).to_string()).collect()
    } else {
        Vec::new()
    };
    // A folder that is gone from where it was and is in `dest` now has moved (some may have been
    // skipped, or the move stopped halfway).
    let moved_folders = folders
        .into_iter()
        .map(|s| (s.clone(), join(dest, file_name(s))))
        .filter(|(from, to)| !exists(from) && is_folder(to))
        .collect();
    let mut out = Outcome { ok: done.ok(), changed, created: Some((dest.to_string(), names)), moved_folders, ..Default::default() };
    out.note = done.problem(&format!("Couldn't {verb} everything"));
    out
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
    Outcome { ok: error.is_none(), changed: vec![dest.to_string()], created: Some((dest.to_string(), made)), note: Some(note), ..Default::default() }
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
                let Some(item) = shell_item(p) else { continue };
                // SAFETY: COM call on live objects of this thread.
                queued += unsafe { engine.op.DeleteItem(&item, None) }.is_ok() as usize;
            }
            if queued == 0 {
                return Outcome { changed, ..Outcome::note(format!("{} isn't there any more.", items_text(paths))) };
            }
            engine.run()
        }
        None => legacy(FO_DELETE, paths, &[], flags),
    };
    if recycle && paths.iter().any(|p| !exists(p)) {
        crate::recycle::saw_full(true); // (the sidebar's bin icon)
    }
    let mut out = Outcome { ok: done.ok(), changed, ..Default::default() };
    out.note = done.problem("Couldn't delete everything").or_else(|| {
        Some(if recycle {
            format!("{} moved to the Recycle Bin", items_text(paths))
        } else {
            format!("{} deleted", items_text(paths))
        })
    });
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
        None => Outcome { ok: true, changed: vec![dir.clone()], created: Some((dir, vec![new_name.to_string()])), ..Default::default() },
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
            Outcome { ok: true, changed: vec![dir.to_string()], created: Some((dir.to_string(), vec![name])), rename_new: true, ..Default::default() }
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
}

impl Done {
    fn ok(&self) -> bool {
        !self.aborted && self.error.is_none()
    }

    /// A status-bar line when something didn't go through.
    fn problem(&self, lead: &str) -> Option<String> {
        match self.error {
            Some(hr) => Some(format!("{lead}: {}", explain(hr))),
            None if self.aborted => Some("Stopped before it was finished.".to_string()),
            None => None,
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
        Ok(())
    }
    fn PostMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, hr: HRESULT, made: Ref<IShellItem>) -> WinResult<()> {
        self.after(hr, made.as_ref());
        Ok(())
    }
    fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn PostCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR, hr: HRESULT, made: Ref<IShellItem>) -> WinResult<()> {
        self.after(hr, made.as_ref());
        Ok(())
    }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> WinResult<()> {
        Ok(())
    }
    fn PostDeleteItem(&self, _: u32, _: Ref<IShellItem>, hr: HRESULT, _: Ref<IShellItem>) -> WinResult<()> {
        // What a deleted item "became" is its copy in the Recycle Bin: nothing to select.
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
    fn UpdateProgress(&self, _: u32, _: u32) -> WinResult<()> {
        Ok(())
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

/// A Windows "shell item" for a path (what the copy engine works with), if the item is there.
fn shell_item(path: &str) -> Option<IShellItem> {
    let w = wide(path);
    // SAFETY: `w` is NUL-terminated and outlives the call.
    unsafe { SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok() }
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

fn exists(path: &str) -> bool {
    attributes(path).is_some()
}

fn is_folder(path: &str) -> bool {
    attributes(path).is_some_and(|a| a & 0x10 != 0) // FILE_ATTRIBUTE_DIRECTORY
}

fn attributes(path: &str) -> Option<u32> {
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
}
