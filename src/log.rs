//! Two logs. Writing either one never fails loudly.
//!
//! - **The timing log**, %TEMP%\glint.log: start-up and folder-load times, so speed can be
//!   checked without a debugger (`line`). Off unless asked for when Glint starts: GLINT_TIMING_LOG=1
//!   (or another logging switch from main.rs, or --bench). It names the folders that were opened,
//!   so the one an older Glint wrote (it was always on then) is deleted at start (`tidy_timing_log`).
//! - **The error log**, error.log in the logs folder (settings.rs): unexpected problems and crashes,
//!   with the date and time, Glint's version, what happened and where in the code (`error`, and the
//!   panic hook from `install_panic_hook`). It can be switched off (SETTINGS > Error log; on by
//!   default). When it passes 1 MB it starts again, keeping the previous one as error.old.log.
//!
//! Glint is built with panic = abort: a panic (a bug Rust caught) ends the program at once. The hook
//! writes it to the error log first and then tells the person, so Glint never just vanishes.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::core::{w, PCWSTR};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, MB_YESNO, SW_SHOWNORMAL};

use crate::folder::wide;
use crate::settings;

/// Glint's version, as Cargo.toml says ("0.8.0").
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The timing log starts again after this many bytes.
const TIMING_LOG_MAX: u64 = 1024 * 1024;
/// The error log starts again after this many bytes.
const ERROR_LOG_MAX: u64 = 1024 * 1024;

fn path() -> Option<std::path::PathBuf> {
    std::env::var_os("TEMP").or_else(|| std::env::var_os("TMP")).map(|t| std::path::Path::new(&t).join("glint.log"))
}

/// The switches that turn the timing log on (each of the others adds its own details to it).
const TIMING_SWITCHES: [&str; 4] = ["GLINT_TIMING_LOG", "GLINT_FRAME_LOG", "GLINT_WATCH_LOG", "GLINT_THUMB_LOG"];
/// --bench asked for the timing log.
static TIMING_ASKED: AtomicBool = AtomicBool::new(false);

/// Whether the timing log is written: only when a switch or --bench asks for it.
pub fn timing_on() -> bool {
    static SWITCHED: OnceLock<bool> = OnceLock::new();
    TIMING_ASKED.load(Ordering::Relaxed) || *SWITCHED.get_or_init(|| TIMING_SWITCHES.iter().any(|s| std::env::var_os(s).is_some()))
}

/// --bench: start-up timing is what it's for.
pub fn ask_for_timing_log() {
    TIMING_ASKED.store(true, Ordering::Relaxed);
}

/// At start: the timing log goes unless it's wanted, so the folders an older Glint wrote down
/// don't stay in the temp folder.
pub fn tidy_timing_log() {
    if !timing_on() {
        if let Some(p) = path() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Appends one line to the timing log (when it's on), stamped with seconds since 1970
/// (milliseconds precision).
pub fn line(text: &str) {
    if !timing_on() {
        return;
    }
    let Some(p) = path() else { return };
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    // It starts again once it's big, so it never piles up in the temp folder: as a new, empty file
    // (a file opened for appending can't be cut short on Windows).
    let big = std::fs::metadata(&p).is_ok_and(|m| m.len() > TIMING_LOG_MAX);
    let file = if big { std::fs::File::create(&p) } else { std::fs::OpenOptions::new().create(true).append(true).open(&p) };
    if let Ok(mut f) = file {
        let _ = writeln!(f, "{stamp:.3} {text}");
    }
}

/// Where the time of one piece of work went, for the timing log: `mark` after each step, then
/// `summary` ("begin 41.2, titlebar 3.1, ..."). Does nothing (and costs nothing) when `on` is false.
pub struct Marks {
    last: Option<std::time::Instant>,
    steps: Vec<(&'static str, f64)>,
}

impl Marks {
    pub fn new(on: bool) -> Marks {
        Marks { last: on.then(std::time::Instant::now), steps: Vec::new() }
    }

    /// The step `name` just ended.
    pub fn mark(&mut self, name: &'static str) {
        if let Some(last) = self.last {
            let now = std::time::Instant::now();
            self.steps.push((name, (now - last).as_secs_f64() * 1000.0));
            self.last = Some(now);
        }
    }

    /// The steps and their milliseconds, in order (empty when off).
    pub fn summary(&self) -> String {
        self.steps.iter().map(|(n, ms)| format!("{n} {ms:.1}")).collect::<Vec<_>>().join(", ")
    }
}

// ---------------------------------------------------------------- the error log

/// Whether the error log is on (SETTINGS > Error log). On until the settings say otherwise.
static ERROR_LOG: AtomicBool = AtomicBool::new(true);

pub fn set_error_log(on: bool) {
    ERROR_LOG.store(on, Ordering::SeqCst);
}

pub fn error_log_on() -> bool {
    ERROR_LOG.load(Ordering::SeqCst)
}

/// Where the error log is.
pub fn error_log_path() -> PathBuf {
    settings::folders().logs.join("error.log")
}

/// The local date and time: "2026-10-08 14:32:05".
fn now_text() -> String {
    // SAFETY: returns a plain struct.
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// One entry of the error log (without the trailing blank line).
fn entry(kind: &str, text: &str, place: &str) -> String {
    format!("{}  Glint {VERSION}  {kind}: {}\n    at {place}", now_text(), text.trim())
}

/// Appends an entry to the error log if it's on. Returns whether it was written.
fn append_error(entry: &str) -> bool {
    if !error_log_on() {
        return false;
    }
    let file = error_log_path();
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&file).is_ok_and(|m| m.len() > ERROR_LOG_MAX) {
        let _ = std::fs::rename(&file, file.with_file_name("error.old.log"));
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        Ok(mut f) => writeln!(f, "{entry}\n").is_ok(),
        Err(_) => false,
    }
}

/// Writes an unexpected problem to the error log (when it's on): the date and time, the text, and
/// the place in the code that called this. Glint carries on; use it for things that shouldn't
/// happen but aren't worth stopping for (a settings file that can't be saved, say).
#[track_caller]
pub fn error(text: &str) {
    let at = std::panic::Location::caller();
    append_error(&entry("error", text, &format!("{}:{}", at.file(), at.line())));
}

/// From now on, a panic is written to the error log and shown to the person before Glint closes.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let thread = std::thread::current();
        let place = format!(
            "{} (thread \"{}\")",
            info.location().map_or("an unknown place".to_string(), |l| format!("{}:{}", l.file(), l.line())),
            thread.name().unwrap_or("unnamed")
        );
        let saved = append_error(&entry("crash", &message, &place));
        line(&format!("crash: {message} at {place}"));
        // Our window must not handle messages while the box below is up: its state may be half
        // changed (the panic stopped whatever was changing it).
        crate::window::freeze_for_crash();
        tell_about_crash(&message, saved);
    }));
}

/// The message after a crash. With panic = abort, Glint ends as soon as it's closed.
fn tell_about_crash(message: &str, saved: bool) {
    let file = error_log_path();
    // Just after an update, the Glint from before is still next to this one: say how to go back.
    let message = match std::env::current_exe().ok().and_then(|me| crate::update::previous_exe(&me)) {
        Some(old) => format!("{message}\n\nThe Glint you had before the last update is still there, as {}. If this keeps happening, start that one.", old.display()),
        None => message.to_string(),
    };
    let message = message.as_str();
    let text = if saved {
        format!(
            "Glint ran into a problem it can't recover from and has to close.\n\n{message}\n\nThe details were saved in error.log, in:\n{}\n\nOpen error.log now?",
            file.parent().map_or(String::new(), |d| d.display().to_string())
        )
    } else if error_log_on() {
        format!("Glint ran into a problem it can't recover from and has to close.\n\n{message}\n\nThe details couldn't be saved. Press Ctrl+C to copy this message.")
    } else {
        format!("Glint ran into a problem it can't recover from and has to close.\n\n{message}\n\nThe error log is off (SETTINGS > Error log), so the details weren't saved. Press Ctrl+C to copy this message.")
    };
    let text_w = wide(&text);
    let buttons = if saved { MB_YESNO } else { MB_OK };
    // SAFETY: NUL-terminated strings that outlive the calls; no owner window (ours may be the
    // problem). COM is set up for ShellExecute on this thread if it wasn't already.
    unsafe {
        let answer = MessageBoxW(None, PCWSTR(text_w.as_ptr()), w!("Glint"), buttons | MB_ICONERROR | MB_TASKMODAL | MB_SETFOREGROUND);
        if saved && answer == IDYES {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            let file_w = wide(&file.to_string_lossy());
            ShellExecuteW(None, w!("open"), PCWSTR(file_w.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_say_where_the_time_went_only_when_on() {
        let mut off = Marks::new(false);
        off.mark("begin");
        assert_eq!(off.summary(), "");
        let mut on = Marks::new(true);
        on.mark("begin");
        on.mark("end");
        let s = on.summary();
        assert!(s.starts_with("begin ") && s.contains(", end "), "{s}");
    }

    #[test]
    fn entries_have_time_version_text_and_place() {
        let e = entry("error", "  Couldn't save  ", "src\\settings.rs:12");
        let (first, second) = e.split_once('\n').unwrap();
        assert_eq!(first.as_bytes()[4], b'-');
        assert!(first.ends_with(&format!("Glint {VERSION}  error: Couldn't save")), "{first}");
        assert_eq!(second, "    at src\\settings.rs:12");
    }

    #[test]
    fn the_resource_file_has_the_same_version() {
        let rc = include_str!("../res/glint.rc");
        assert!(rc.contains(&format!("\"FileVersion\", \"{VERSION}\"")), "res/glint.rc and Cargo.toml disagree on the version");
        // Windows takes four plain numbers: MAJOR.MINOR.PATCH and the beta build ("1.0.12-beta.2" is
        // 1,0,12,2), or 0 for a version without one.
        let (core, pre) = VERSION.split_once('-').unwrap_or((VERSION, ""));
        let build = pre.rsplit('.').next().filter(|n| n.parse::<u16>().is_ok()).unwrap_or("0");
        let numbers = format!("{},{build}", core.replace('.', ","));
        assert!(rc.contains(&format!("FILEVERSION {numbers}\n")) || rc.contains(&format!("FILEVERSION {numbers}\r\n")), "FILEVERSION in res/glint.rc");
        assert!(rc.contains(&format!("\"ProductVersion\", \"{VERSION}\"")), "ProductVersion in res/glint.rc");
        assert!(rc.contains(&format!("PRODUCTVERSION {numbers}\n")) || rc.contains(&format!("PRODUCTVERSION {numbers}\r\n")), "PRODUCTVERSION in res/glint.rc");
        let manifest = include_str!("../res/glint.manifest");
        assert!(manifest.contains(&format!("name=\"Glint\" version=\"{}.{build}\"", core)), "the version in res/glint.manifest");
    }
}
