//! Updates: Glint looks for a new version now and then, says when there is one, and puts it in place
//! when the person says so (the prompt is updatewin.rs). New versions come from Glint's repository
//! on GitHub ("UpdateRepo", owner/name): each release there is tagged with its version ("v0.8.0")
//! and has a ready-built Glint.exe attached.
//!
//! **Looking** (`start_check`, on a thread of its own; nothing here ever runs on the UI thread):
//! - GitHub is asked for the newest release (`api.github.com/repos/<owner>/<name>/releases/latest`,
//!   which leaves out drafts and pre-releases), over HTTPS with WinHTTP: the system's proxy
//!   settings, HTTPS certificates always checked, and a time limit on every step.
//! - Its tag (a leading "v" left out) is compared with Glint's own version (`Version`, semver).
//!   Newer: the CHANGELOG.md sections since Glint's own version (the file as it is at that tag) go
//!   into the prompt, or the release's own text when CHANGELOG.md can't be read.
//! - A newer release without its Glint.exe yet (still being built) isn't offered.
//! - No release yet, or GitHub's limit on checks reached: About says Glint couldn't check right
//!   now, and nothing else nags.
//!
//! **When** (`Schedule`, the main window's UPDATE_TIMER): about 30 seconds after start and then every
//! 12 hours while Glint runs ("CheckForUpdates", on) - never while it's hidden in the background
//! (the timer stops; when the window is back a check that's due runs 30 seconds later). Settings >
//! About > CHECK FOR UPDATES checks at once and shows the result there. Only the main Glint checks
//! (an extra window says so in About).
//!
//! **Asking**: "Remind me again later" asks again at the first check after 12 hours
//! ("RemindUpdateAfter"); "Don't remind me about this update" remembers the version
//! ("SkippedUpdate"): a newer one asks again; "Turn off automatic updates" turns "CheckForUpdates"
//! off. A check from About always shows what it found, automatic checks on or off.
//!
//! **Installing** (`start_install`, on a thread of its own; one check or install at a time),
//! never leaving the person without a working glint.exe:
//! 1. the release's Glint.exe is downloaded into the cache folder (`Glint-<version>.exe`; WinHTTP
//!    follows GitHub's redirect to its download server, never from HTTPS to plain HTTP) and must be
//!    exactly as big as GitHub said;
//! 2. it's copied next to ours (`Glint.update-<pid>.exe`) - a folder Glint can't write to (Program
//!    Files) gets a plain message with its path, and nothing changes;
//! 3. the copy is checked before anything else (`verify`; on any failure it's deleted and nothing
//!    changes): its version info, read without starting it, is the release's version; only then is
//!    it started with `--version` and must say so too;
//! 4. it's renamed once more (`Glint.update-<pid>-2.exe`), which waits out a virus scanner still
//!    holding it while ours keeps its name; then our own exe is renamed to `Glint.old.exe` (Windows
//!    allows that while it runs) and the copy takes its name at once - if that fails, the old one
//!    gets its name back;
//! 5. the window saves the settings and quits, and main.rs starts the new exe with `--after-update`,
//!    which opens the same tabs (and starts hidden if Glint was hidden by then).
//!
//! The download is deleted once the new Glint is in place; one left by an install that didn't work
//! goes with the next download (or Settings > About > Cache). The Glint from before stays as
//! `Glint.old.exe` while the new one runs for the first time; a crash message then says it's there.
//! A later start of the main Glint deletes it (`remove_old_exes`, on a thread, trying again for a
//! minute while the old process may still be finishing).
//!
//! `glint.exe --version` prints the version and ends (no window). `GLINT_TEST_VERSION=0.2.9` makes Glint
//! take itself for that version (for testing; never passed on to the exe it starts).

use std::ffi::{c_void, OsStr, OsString};
use std::io::{Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts, ERROR_WINHTTP_CANNOT_CONNECT, ERROR_WINHTTP_CONNECTION_ERROR,
    ERROR_WINHTTP_NAME_NOT_RESOLVED, ERROR_WINHTTP_SECURE_CERT_CN_INVALID, ERROR_WINHTTP_SECURE_CERT_DATE_INVALID,
    ERROR_WINHTTP_SECURE_CERT_REVOKED, ERROR_WINHTTP_SECURE_FAILURE, ERROR_WINHTTP_SECURE_INVALID_CA, ERROR_WINHTTP_TIMEOUT,
    INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_FLAG_SECURE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE,
};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO};
use windows::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer, WM_APP};

use crate::folder::wide;
use crate::log;
use crate::settings::{self, Settings};
use crate::shell;

/// Posted to the main window: wparam says what (CHECKED, INSTALLED, ANSWERED), lparam more.
pub const WM_APP_UPDATE: u32 = WM_APP + 70;
/// A check is done (lparam: 1 when the person asked for it). The result is in `status()`.
pub const CHECKED: usize = 1;
/// Installing is done (`take_installed`).
pub const INSTALLED: usize = 2;
/// The prompt was answered (lparam: one of the ANSWER_ values).
pub const ANSWERED: usize = 3;
pub const ANSWER_UPDATE: isize = 1;
pub const ANSWER_LATER: isize = 2;
pub const ANSWER_SKIP: isize = 3;
/// "Turn off automatic updates".
pub const ANSWER_OFF: isize = 4;
/// The main window's timer for the next check.
pub const UPDATE_TIMER: usize = 70;

/// The first check after start (or after coming back from the background, when one is due).
const FIRST_CHECK: Duration = Duration::from_secs(30);
/// Then every so long, and "Remind me again later" waits as long.
pub const EVERY: Duration = Duration::from_secs(12 * 60 * 60);
/// How long one question to GitHub may take, all in all (the newest release; the changelog).
const CHECK_LIMIT: Duration = Duration::from_secs(60);
/// How long downloading the new Glint.exe may take, all in all.
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(10 * 60);
/// WinHTTP's time limits for each step, in milliseconds: finding the server, connecting, sending,
/// and each wait for more of the answer.
const STEP_LIMITS: [i32; 4] = [15_000, 15_000, 30_000, 30_000];
/// The new exe has this long to say its version.
const VERIFY_LIMIT: Duration = Duration::from_secs(20);
/// The most of the changelog the prompt shows.
const MAX_CHANGELOG: usize = 40_000;
/// The most an answer about a release, or a CHANGELOG.md, may hold.
const MAX_ANSWER: usize = 4 * 1024 * 1024;
/// The biggest Glint.exe Glint downloads (it's a few MB).
const MAX_EXE: u64 = 128 * 1024 * 1024;
/// The name of the release's file that is Glint (any case).
const ASSET: &str = "Glint.exe";
/// What GitHub's API is asked for: its JSON, as the version of the API Glint is written for has it.
const API_HEADERS: &str = "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28";
/// CREATE_NO_WINDOW: the new exe, asked its version, never shows a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------- versions

/// A version number as Cargo.toml has it: major.minor.patch, maybe with a pre-release part
/// ("0.4.0-beta.2"), compared as semver does (a pre-release comes before its release; "+build" is
/// ignored).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub numbers: [u64; 3],
    pub pre: Vec<String>,
}

impl Version {
    /// "0.3.1", "v0.3.1", "0.3" (= 0.3.0), "0.4.0-beta.2+abc". None for anything else.
    pub fn parse(text: &str) -> Option<Version> {
        let t = text.trim().trim_start_matches(['v', 'V']);
        let t = t.split('+').next()?;
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (t, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let mut numbers = [0u64; 3];
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            numbers[i] = p.parse().ok()?;
        }
        let pre = match pre {
            Some(p) if p.is_empty() || p.split('.').any(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')) => return None,
            Some(p) => p.split('.').map(str::to_string).collect(),
            None => Vec::new(),
        };
        Some(Version { numbers, pre })
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        match self.numbers.cmp(&other.numbers) {
            Ordering::Equal => {}
            o => return o,
        }
        // A release is newer than its pre-releases.
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => a.cmp(b),
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let [a, b, c] = self.numbers;
        write!(f, "{a}.{b}.{c}")?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

/// Glint's own version (Cargo.toml's), or GLINT_TEST_VERSION's when that is set (testing).
pub fn ours() -> Version {
    let test = std::env::var("GLINT_TEST_VERSION").ok().and_then(|v| Version::parse(&v));
    test.or_else(|| Version::parse(log::VERSION)).unwrap_or(Version { numbers: [0, 0, 0], pre: Vec::new() })
}


/// The sections of a CHANGELOG.md ("## 0.3.1 (2026-10-12): ...") for the versions after `ours` up to
/// `theirs`, newest first as the file has them. At most MAX_CHANGELOG characters.
pub fn changelog_since(md: &str, ours: &Version, theirs: &Version) -> String {
    let mut out = String::new();
    let mut taking = false;
    for line in md.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let v = heading.split(|c: char| c.is_whitespace() || c == ':').next().and_then(Version::parse);
            taking = v.is_some_and(|v| &v > ours && &v <= theirs);
        }
        if taking {
            if out.len() + line.len() + 1 > MAX_CHANGELOG {
                out.push_str("\n…");
                break;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out.trim_end().to_string()
}

// ---------------------------------------------------------------- what's known

/// What a check found.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    /// Couldn't check right now, nothing to worry about: there's no release yet, or GitHub's limit
    /// on checks was reached (why, for the log).
    Unavailable(String),
    /// Couldn't check (no network, an answer that makes no sense...): why, in a few words.
    Failed(String),
    /// Nothing newer: the newest version there is.
    UpToDate(String),
    /// A newer version, ready to install.
    Newer(Release),
    /// A newer version, but without its Glint.exe (not built yet).
    NotReady(String),
}

/// A newer Glint, ready to install.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Release {
    pub version: String,
    /// What's new: CHANGELOG.md's sections since Glint's own version, or the release's own text
    /// (Markdown).
    pub changelog: String,
    /// The release's tag, as written ("v0.8.0").
    pub tag: String,
    /// Where its Glint.exe is downloaded from (GitHub's https:// address for it).
    pub url: String,
    /// How big that Glint.exe is, in bytes: the download must be exactly this.
    pub size: u64,
}

/// Everything the windows show about updates.
#[derive(Clone, Debug, Default)]
pub struct Status {
    /// A check is under way.
    pub checking: bool,
    /// Installing is under way.
    pub installing: bool,
    /// What the last check found, and when ("14:02").
    pub last: Option<(Found, String)>,
    /// What the last install brought (taken by the window).
    installed: Option<Result<PathBuf, String>>,
}

static STATUS: Mutex<Status> = Mutex::new(Status { checking: false, installing: false, last: None, installed: None });

fn status_lock() -> std::sync::MutexGuard<'static, Status> {
    STATUS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What's known about updates now.
pub fn status() -> Status {
    status_lock().clone()
}

/// The result of the install that just finished (INSTALLED): the exe now in place, or what went wrong.
pub fn take_installed() -> Option<Result<PathBuf, String>> {
    status_lock().installed.take()
}

/// The time now as "14:02".
fn clock_text() -> String {
    // SAFETY: a plain query of the local time.
    let t = unsafe { GetLocalTime() };
    format!("{}:{:02}", t.wHour, t.wMinute)
}

/// Seconds since 1970 now.
pub fn now_seconds() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// What Settings > About says about updates (the row's explanation), and whether CHECK FOR UPDATES
/// can be pressed now. `main`: this is the main Glint (only it checks); `auto`: "CheckForUpdates".
pub fn about(main: bool, auto: bool) -> (String, bool) {
    if !main {
        return ("Only the main Glint window checks for updates.".into(), false);
    }
    let st = status_lock();
    let ours = ours();
    let said = if st.installing {
        "Installing the new Glint…".to_string()
    } else if st.checking {
        "Looking for a new version…".to_string()
    } else {
        match &st.last {
            None if auto => format!("You have Glint {ours}."),
            None => format!("You have Glint {ours}. Press CHECK FOR UPDATES to look now."),
            Some((Found::Unavailable(_), at)) => format!("Couldn't check for updates right now. Tried at {at}."),
            Some((Found::Failed(why), at)) => format!("Couldn't check for updates at {at}: {why}."),
            Some((Found::UpToDate(theirs), at)) => match Version::parse(theirs) {
                Some(v) if v < ours => format!("You have Glint {ours}, newer than the newest release ({theirs}). Checked at {at}."),
                _ => format!("You have the newest Glint ({ours}). Checked at {at}."),
            },
            Some((Found::Newer(r), at)) => format!("Glint {} is ready (you have {ours}). Found at {at}.", r.version),
            Some((Found::NotReady(v), at)) => format!("Glint {v} is coming but isn't ready yet. Checked at {at}."),
        }
    };
    (said, !st.checking && !st.installing)
}

// ---------------------------------------------------------------- when to look

/// When the next automatic check is due; the main window keeps one and calls it at the moments
/// below. The timer runs only while the window is on screen.
#[derive(Debug, Default)]
pub struct Schedule {
    /// When the next check is due (None: never - not the main Glint, or --bench).
    due: Option<Instant>,
}

impl Schedule {
    /// At start (the main Glint): the first check is due in about 30 seconds; the timer starts
    /// unless the window starts hidden.
    pub fn start(&mut self, hwnd: HWND, shown: bool) {
        self.due = Some(Instant::now() + FIRST_CHECK);
        if shown {
            self.arm(hwnd);
        }
    }

    /// Sets the timer for when the next check is due (at least FIRST_CHECK from now when it's
    /// overdue: the window just came back from the background).
    fn arm(&self, hwnd: HWND) {
        let Some(due) = self.due else { return };
        let now = Instant::now();
        let wait = if due > now { due - now } else { FIRST_CHECK };
        // SAFETY: a timer on our own window (setting it again just moves it).
        unsafe {
            SetTimer(Some(hwnd), UPDATE_TIMER, wait.as_millis().min(u32::MAX as u128 / 2) as u32, None);
        }
    }

    /// The window went to the background: no checks while hidden.
    pub fn hidden(&self, hwnd: HWND) {
        // SAFETY: stops our own timer (fine if it isn't running).
        unsafe {
            let _ = KillTimer(Some(hwnd), UPDATE_TIMER);
        }
    }

    /// The window is on screen again: a check that came due meanwhile runs a little later.
    pub fn shown(&self, hwnd: HWND) {
        self.arm(hwnd);
    }

    /// A new version was found but not answered (the prompt went with the window into the
    /// background, or the check ended while it was hidden): look again soon after the window is
    /// back, so the prompt comes back too.
    pub fn ask_again_when_shown(&mut self) {
        if self.due.is_some() {
            self.due = Some(Instant::now());
        }
    }

    /// UPDATE_TIMER went off: whether a check is wanted now. The next one is due in 12 hours either
    /// way (the timer is set again when the check is done, or now when none is made).
    pub fn due_now(&mut self, hwnd: HWND, s: &Settings) -> bool {
        // SAFETY: stops our own one-off timer.
        unsafe {
            let _ = KillTimer(Some(hwnd), UPDATE_TIMER);
        }
        if self.due.is_none() {
            return false;
        }
        self.due = Some(Instant::now() + EVERY);
        let busy = {
            let st = status_lock();
            st.checking || st.installing
        };
        if !s.check_for_updates || busy {
            self.arm(hwnd);
            return false;
        }
        true
    }

    /// A check is done (automatic or asked for): the next one is due in 12 hours.
    pub fn checked(&mut self, hwnd: HWND) {
        if self.due.is_some() {
            self.due = Some(Instant::now() + EVERY);
            self.arm(hwnd);
        }
    }
}

/// Whether what a check found should be shown in the prompt: a newer version, unless the person
/// said not to be reminded about it, or to be reminded later (automatic checks only).
pub fn should_prompt(found: &Found, s: &Settings, asked: bool, now: u64) -> bool {
    let Found::Newer(r) = found else { return false };
    if asked {
        return true;
    }
    let skipped = s.skipped_update.as_deref().and_then(Version::parse);
    let newer_than_skipped = match (skipped, Version::parse(&r.version)) {
        (Some(skip), Some(v)) => v > skip,
        _ => true,
    };
    newer_than_skipped && now >= s.remind_update_after
}

// ---------------------------------------------------------------- GitHub

/// Whether `repo` names a GitHub repository as "owner/name" ("UpdateRepo"): two parts of letters,
/// digits, ".", "-" and "_", neither of them only dots. settings.rs puts the standard one back for
/// anything else.
pub fn valid_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    [owner, name].iter().all(|p| p.len() <= 100 && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) && !p.bytes().all(|b| b == b'.'))
}

/// Where GitHub lists `repo`'s recent releases, newest first. Glint picks the highest version
/// there itself (`read_releases`): the release GitHub marks "latest" can be an older one.
fn releases_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases?per_page={RELEASES_LISTED}")
}

/// How many recent releases `releases_url` asks for.
const RELEASES_LISTED: u32 = 20;

/// Where `repo`'s CHANGELOG.md is, as it is at the release tagged `tag`.
fn changelog_url(repo: &str, tag: &str) -> String {
    format!("https://raw.githubusercontent.com/{repo}/{tag}/CHANGELOG.md")
}

/// Whether `tag` may name a release: a version ("v0.8.0", "0.8.0-beta.1") written only with what an
/// address can carry as it is.
fn valid_tag(tag: &str) -> bool {
    tag.len() <= 64 && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)) && Version::parse(tag).is_some()
}

/// The server and the path of an https:// address: ("github.com", "/a/b"). None for any other
/// address, and for one with a user name, a port of its own, spaces or line breaks.
fn split_https(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(at) => rest.split_at(at),
        None => (rest, "/"),
    };
    let host_ok = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    let path_ok = !path.chars().any(|c| c.is_control() || c.is_whitespace());
    (host_ok && path_ok).then_some((host, path))
}

/// Whether the new Glint.exe may be downloaded from `url`: GitHub's own address for a release's
/// file (https://github.com/<owner>/<name>/releases/download/...).
fn download_allowed(url: &str) -> bool {
    split_https(url).is_some_and(|(host, path)| host.eq_ignore_ascii_case("github.com") && path.contains("/releases/download/"))
}

/// Why a question to GitHub got no answer to use.
#[derive(Debug, PartialEq)]
enum NetError {
    /// It answered with this HTTP status instead of 200.
    Status(u32),
    /// What went wrong, in a few words.
    Failed(String),
}

/// A WinHTTP handle, closed when dropped.
struct Internet(*mut c_void);

impl Internet {
    /// Takes `handle`, or says why there is none (null: Windows' last error says why).
    fn new(handle: *mut c_void) -> Result<Internet, NetError> {
        if handle.is_null() {
            // SAFETY: reads this thread's last error, set by the call that just failed.
            Err(NetError::Failed(net_problem(unsafe { GetLastError() }.0)))
        } else {
            Ok(Internet(handle))
        }
    }
}

impl Drop for Internet {
    fn drop(&mut self) {
        // SAFETY: a handle WinHTTP gave us, closed once.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// What a WinHTTP error code means, in a few words.
fn net_problem(code: u32) -> String {
    match code {
        ERROR_WINHTTP_NAME_NOT_RESOLVED | ERROR_WINHTTP_CANNOT_CONNECT | ERROR_WINHTTP_CONNECTION_ERROR => "GitHub couldn't be reached (is the computer online?)".into(),
        ERROR_WINHTTP_TIMEOUT => "GitHub took too long to answer".into(),
        ERROR_WINHTTP_SECURE_FAILURE | ERROR_WINHTTP_SECURE_INVALID_CA | ERROR_WINHTTP_SECURE_CERT_CN_INVALID | ERROR_WINHTTP_SECURE_CERT_DATE_INVALID | ERROR_WINHTTP_SECURE_CERT_REVOKED => {
            "the connection to GitHub wasn't safe (its certificate was refused)".into()
        }
        _ => format!("the connection to GitHub failed (error {code})"),
    }
}

/// What a failed WinHTTP call means (`net_problem`).
fn net_failed(e: windows::core::Error) -> NetError {
    NetError::Failed(net_problem(e.code().0 as u32 & 0xFFFF))
}

/// Asks for `url` (https:// only) and hands the answer's body to `take`, piece by piece. `headers`:
/// more request headers ("Name: value" lines), besides "User-Agent: Glint/<version>". All of it
/// may take at most `limit`. Redirects are followed (never from HTTPS to plain HTTP); an answer
/// other than 200 in the end is `NetError::Status`.
fn get(url: &str, headers: &str, limit: Duration, mut take: impl FnMut(&[u8]) -> Result<(), String>) -> Result<(), NetError> {
    let (host, path) = split_https(url).ok_or_else(|| NetError::Failed(format!("\"{url}\" isn't an https:// address")))?;
    let started = Instant::now();
    let agent = wide(&format!("Glint/{}", log::VERSION));
    let (host, path) = (wide(host), wide(path));
    let headers: Vec<u16> = headers.encode_utf16().collect();
    // SAFETY: the strings are NUL-terminated (the headers go with their length) and outlive the
    // calls; each handle is used only while it's open (they close in reverse order: the request,
    // the connection, the session); the buffers are as big as said.
    unsafe {
        // The system's proxy settings, automatic ones included (older Windows: its plain default).
        let mut session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            session = WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        }
        let session = Internet::new(session)?;
        let [resolve, connect, send, receive] = STEP_LIMITS;
        let _ = WinHttpSetTimeouts(session.0, resolve, connect, send, receive);
        // Redirects never go from HTTPS to plain HTTP (WinHTTP's own default, made sure of).
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_ne_bytes();
        let _ = WinHttpSetOption(Some(session.0), WINHTTP_OPTION_REDIRECT_POLICY, Some(&policy));
        let connection = Internet::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        // HTTPS, with the server's certificate checked (nothing here ever turns that off).
        let request = Internet::new(WinHttpOpenRequest(connection.0, w!("GET"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE))?;
        WinHttpSendRequest(request.0, (!headers.is_empty()).then_some(&headers[..]), None, 0, 0, 0).map_err(net_failed)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(net_failed)?;
        let mut status = 0u32;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut())
            .map_err(net_failed)?;
        if status != 200 {
            return Err(NetError::Status(status));
        }
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if started.elapsed() > limit {
                return Err(NetError::Failed(format!("it took longer than {} seconds", limit.as_secs())));
            }
            let mut read = 0u32;
            WinHttpReadData(request.0, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32, &mut read).map_err(net_failed)?;
            if read == 0 {
                return Ok(());
            }
            take(&buffer[..read as usize]).map_err(NetError::Failed)?;
        }
    }
}

/// The body of the answer to `url` (see `get`), at most MAX_ANSWER bytes.
fn fetch(url: &str, headers: &str) -> Result<Vec<u8>, NetError> {
    let mut body = Vec::new();
    get(url, headers, CHECK_LIMIT, |piece| {
        if body.len() + piece.len() > MAX_ANSWER {
            return Err("GitHub's answer was far too long".into());
        }
        body.extend_from_slice(piece);
        Ok(())
    })?;
    Ok(body)
}

/// GitHub's answer about a release: the parts Glint reads.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ReleaseJson {
    tag_name: String,
    /// The release's own text (Markdown).
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<AssetJson>,
}

/// One of a release's files, as GitHub tells about it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AssetJson {
    name: String,
    browser_download_url: String,
    size: u64,
    /// "uploaded" once it's all there.
    state: Option<String>,
}

/// `text`, cut to at most `max` bytes (at a character's edge, with "…" after it).
fn shorten(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &text[..end])
}

/// What GitHub's answer about one release (`json`) means for Glint at version `ours` (the tests
/// read single releases with it).
#[cfg(test)]
fn read_latest(json: &[u8], ours: &Version) -> Found {
    let Ok(release) = serde_json::from_slice::<ReleaseJson>(json) else { return Found::Failed("GitHub's answer about the newest release couldn't be read".into()) };
    read_release(release, ours)
}

/// What GitHub's list of recent releases (`json`) means for Glint at version `ours`: the release
/// with the highest version number, drafts and pre-releases left out, judged as `read_latest` does.
fn read_releases(json: &[u8], ours: &Version) -> Found {
    let Ok(list) = serde_json::from_slice::<Vec<ReleaseJson>>(json) else { return Found::Failed("GitHub's list of releases couldn't be read".into()) };
    let best = list
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease && valid_tag(r.tag_name.trim()))
        .filter_map(|r| Version::parse(r.tag_name.trim()).map(|v| (v, r)))
        .max_by(|a, b| a.0.cmp(&b.0));
    match best {
        Some((_, release)) => read_release(release, ours),
        None => Found::Unavailable("no release yet".into()),
    }
}

/// What one release means for Glint at version `ours`. A newer release's changelog is its own
/// text for now (`whats_new` may find better).
fn read_release(release: ReleaseJson, ours: &Version) -> Found {
    if release.draft || release.prerelease {
        return Found::Unavailable("the newest release isn't out yet".into());
    }
    let tag = release.tag_name.trim();
    let theirs = match Version::parse(tag) {
        Some(v) if valid_tag(tag) => v,
        _ => return Found::Failed(format!("the newest release's tag (\"{}\") isn't a version number", shorten(tag, 40))),
    };
    if theirs <= *ours {
        return Found::UpToDate(theirs.to_string());
    }
    // Its Glint.exe, once it's all there.
    let exe = release.assets.iter().find(|a| a.name.eq_ignore_ascii_case(ASSET) && a.size > 0 && a.state.as_deref().is_none_or(|s| s == "uploaded"));
    let Some(exe) = exe else { return Found::NotReady(theirs.to_string()) };
    if !download_allowed(&exe.browser_download_url) {
        return Found::Failed("the new Glint.exe isn't where GitHub keeps a release's files".into());
    }
    if exe.size > MAX_EXE {
        return Found::Failed(format!("the new Glint.exe is far too big ({} MB)", exe.size / (1024 * 1024)));
    }
    let text = release.body.as_deref().unwrap_or("").trim();
    Found::Newer(Release { version: theirs.to_string(), changelog: shorten(text, MAX_CHANGELOG), tag: tag.to_string(), url: exe.browser_download_url.clone(), size: exe.size })
}

/// What's new in a release (version `theirs`) for Glint at `ours`: the sections of `changelog`
/// (CHANGELOG.md at the release's tag, when it could be read) since then, otherwise the release's
/// own text as it is.
fn whats_new(changelog: Option<&str>, release_text: &str, ours: &Version, theirs: &Version) -> String {
    match changelog.map(|md| changelog_since(md, ours, theirs)) {
        Some(sections) if !sections.is_empty() => sections,
        _ => release_text.to_string(),
    }
}

// ---------------------------------------------------------------- checking

/// Starts a check on a thread of its own (unless one is under way, or an install). `owner` (the
/// main window) gets WM_APP_UPDATE / CHECKED when it's done; `asked`: the person pressed CHECK FOR
/// UPDATES. Returns whether one started.
pub fn start_check(owner: shell::Owner, s: &Settings, asked: bool) -> bool {
    {
        let mut st = status_lock();
        if st.checking || st.installing {
            return false;
        }
        st.checking = true;
    }
    let repo = s.update_repo.clone();
    let started = shell::background("update: check", move || {
        let t0 = Instant::now();
        let found = check(&repo);
        log::line(&format!("update check ({}): {} in {:.1} s", if asked { "asked" } else { "automatic" }, describe(&found), t0.elapsed().as_secs_f64()));
        {
            let mut st = status_lock();
            st.checking = false;
            st.last = Some((found, clock_text()));
        }
        // SAFETY: posting to a window handle is safe even if the window is gone.
        unsafe {
            let _ = PostMessageW(Some(owner.hwnd()), WM_APP_UPDATE, WPARAM(CHECKED), LPARAM(asked as isize));
        }
    });
    if !started {
        status_lock().checking = false;
    }
    started
}

/// For the log: what a check found, in a few words.
fn describe(f: &Found) -> String {
    match f {
        Found::Unavailable(why) => format!("couldn't check right now ({why})"),
        Found::Failed(why) => format!("failed ({why})"),
        Found::UpToDate(v) => format!("up to date (newest {v})"),
        Found::Newer(r) => format!("{} is ready", r.version),
        Found::NotReady(v) => format!("{v} has no Glint.exe yet"),
    }
}

/// Looks for a new version (on a background thread): asks GitHub for `repo`'s newest release and,
/// when it's newer, what's new in it.
fn check(repo: &str) -> Found {
    let repo = if valid_repo(repo) { repo } else { settings::UPDATE_REPO };
    let json = match fetch(&releases_url(repo), API_HEADERS) {
        Ok(json) => json,
        // No release yet (or a repository GitHub doesn't show).
        Err(NetError::Status(404)) => return Found::Unavailable("no release yet".into()),
        // Too many checks from this network for now: GitHub's limit (60 an hour) was reached.
        Err(NetError::Status(403 | 429)) => return Found::Unavailable("GitHub's limit on checks was reached".into()),
        Err(NetError::Status(code)) => return Found::Failed(format!("GitHub answered with error {code}")),
        Err(NetError::Failed(why)) => return Found::Failed(why),
    };
    let ours = ours();
    let mut found = read_releases(&json, &ours);
    if let Found::Newer(r) = &mut found {
        let theirs = Version::parse(&r.version).unwrap_or_else(|| ours.clone());
        let md = fetch(&changelog_url(repo, &r.tag), "").ok().map(|b| String::from_utf8_lossy(&b).into_owned());
        r.changelog = whats_new(md.as_deref(), &r.changelog, &ours, &theirs);
    }
    found
}

/// The version baked into an exe (its version info's file version, major.minor.patch: res/glint.rc
/// has Cargo.toml's), read without starting it. None when it has none or it can't be read.
pub(crate) fn exe_version(exe: &Path) -> Option<[u64; 3]> {
    let name = wide(&exe.to_string_lossy());
    // SAFETY: the name is NUL-terminated and lives across the calls; the buffer is as big as Windows
    // said; VerQueryValueW gives a pointer into that buffer, read (unaligned) while it's alive.
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(name.as_ptr()), None);
        if size == 0 || size > 1 << 20 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(name.as_ptr()), None, size, data.as_mut_ptr() as *mut _).ok()?;
        let mut info: *mut std::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(data.as_ptr() as *const _, w!("\\"), &mut info, &mut len).as_bool() || info.is_null() || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
            return None;
        }
        let fixed = std::ptr::read_unaligned(info as *const VS_FIXEDFILEINFO);
        if fixed.dwSignature != 0xFEEF_04BD {
            return None;
        }
        Some([(fixed.dwFileVersionMS >> 16) as u64, (fixed.dwFileVersionMS & 0xffff) as u64, (fixed.dwFileVersionLS >> 16) as u64])
    }
}

// ---------------------------------------------------------------- installing

/// Starts putting `release` in place, on a thread of its own. `owner` gets WM_APP_UPDATE /
/// INSTALLED when it's done (`take_installed`). False if a check or another install is under way.
pub fn start_install(owner: shell::Owner, release: Release) -> bool {
    {
        let mut st = status_lock();
        if st.checking || st.installing {
            return false;
        }
        st.installing = true;
    }
    let started = shell::background("update: install", move || {
        let result = install(&release);
        match &result {
            Ok(exe) => log::line(&format!("update: Glint {} is in place ({})", release.version, exe.display())),
            Err(why) => {
                log::line(&format!("update: Glint {} couldn't be installed: {why}", release.version));
                log::error(&format!("Updating to Glint {} didn't work: {why}", release.version));
            }
        }
        {
            let mut st = status_lock();
            st.installing = false;
            st.installed = Some(result);
        }
        // SAFETY: posting to a window handle is safe even if the window is gone.
        unsafe {
            let _ = PostMessageW(Some(owner.hwnd()), WM_APP_UPDATE, WPARAM(INSTALLED), LPARAM(0));
        }
    });
    if !started {
        status_lock().installing = false;
    }
    started
}

/// Whether glint.exe is a build of its own (in a cargo `target` folder): it's built again from the
/// source code, not replaced by a ready-built exe.
pub fn built_from_source(exe: &Path) -> bool {
    let low = exe.to_string_lossy().to_lowercase().replace('/', "\\");
    ["\\target\\release\\", "\\target\\debug\\"].iter().any(|t| low.contains(t)) || low.contains("\\target\\x86_64-pc-windows-")
}

/// Puts the new Glint.exe in place of ours (see the module comment). Returns the exe now in place.
fn install(release: &Release) -> Result<PathBuf, String> {
    let ours = std::env::current_exe().map_err(|e| format!("Glint couldn't find its own glint.exe ({e})"))?;
    if built_from_source(&ours) {
        return Err(format!("This Glint was built from its source code ({}), so it doesn't replace itself. Build the new version from the source code too.", ours.display()));
    }
    let expected = Version::parse(&release.version).ok_or("the new version number can't be read")?;
    let downloaded = download(release, &settings::folders().cache)?;
    let done = install_into(&downloaded, &ours, |exe| verify(exe, &expected));
    if done.is_ok() {
        let _ = std::fs::remove_file(&downloaded);
    }
    done
}

/// Downloads the release's Glint.exe into `cache` (`Glint-<version>.exe`, after deleting what an
/// earlier download left there) and makes sure it came whole: exactly as big as GitHub said.
/// Returns where it is.
fn download(release: &Release, cache: &Path) -> Result<PathBuf, String> {
    use std::os::windows::fs::OpenOptionsExt;
    if !download_allowed(&release.url) || release.size == 0 || release.size > MAX_EXE {
        return Err("The new Glint.exe isn't where GitHub keeps a release's files. Nothing was changed.".into());
    }
    std::fs::create_dir_all(cache).map_err(|e| format!("The cache folder ({}) couldn't be made ({e}). Nothing was changed.", cache.display()))?;
    remove_downloads(cache);
    let file = cache.join(format!("Glint-{}.exe", release.version));
    // Nothing else may open it while it's written (another Glint using the same cache folder).
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0)
        .open(&file)
        .map_err(|e| format!("The new Glint.exe couldn't be saved in {} ({e}). Nothing was changed.", cache.display()))?;
    let mut written = 0u64;
    let got = get(&release.url, "", DOWNLOAD_LIMIT, |piece| {
        written += piece.len() as u64;
        if written > release.size {
            return Err(format!("it's bigger than GitHub said ({} bytes)", release.size));
        }
        out.write_all(piece).map_err(|e| format!("it couldn't be saved ({e})"))
    });
    let saved = out.sync_all();
    drop(out);
    let size = std::fs::metadata(&file).map_or(0, |m| m.len());
    let problem = match got {
        Err(NetError::Status(code)) => Some(format!("GitHub answered with error {code}")),
        Err(NetError::Failed(why)) => Some(why),
        Ok(()) if saved.is_err() || size != release.size => Some(format!("only {size} of its {} bytes arrived", release.size)),
        Ok(()) => None,
    };
    match problem {
        None => Ok(file),
        Some(why) => {
            let _ = std::fs::remove_file(&file);
            Err(format!("The new Glint.exe couldn't be downloaded: {why}. Nothing was changed."))
        }
    }
}

/// Deletes the downloads earlier updates left in `cache` (`Glint-<version>.exe`). One that another
/// Glint is writing right now can't be deleted and stays.
fn remove_downloads(cache: &Path) {
    let Ok(entries) = std::fs::read_dir(cache) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("glint-") && name.ends_with(".exe") && entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The steps of `install`, with the check of the new exe passed in (so they can be tested).
fn install_into(new_exe: &Path, ours: &Path, verify: impl Fn(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    let dir = ours.parent().ok_or("glint.exe has no folder")?;
    let stem = ours.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Glint".into());
    let pid = std::process::id();
    let tmp = dir.join(format!("{stem}.update-{pid}.exe"));
    // 1. The copy, next to ours.
    let size = std::fs::metadata(new_exe).map_err(|e| format!("the new Glint.exe isn't there any more ({e}). Press CHECK FOR UPDATES and try again."))?.len();
    if let Err(e) = std::fs::copy(new_exe, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "Glint can't write to its own folder ({}), so it can't update itself there. Move the Glint folder somewhere you can write to (your Documents, say), or copy the new Glint.exe there yourself: it's in {}.",
                dir.display(),
                new_exe.parent().map_or(String::new(), |p| p.display().to_string())
            )
        } else {
            format!("the new Glint.exe couldn't be copied to {} ({e})", dir.display())
        });
    }
    if std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0) != size {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("the new Glint.exe wasn't copied whole to {} (is the disk full?)", dir.display()));
    }
    // 2. It must be the very file of the release, and start and say its version.
    if let Err(why) = verify(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{why} Nothing was changed."));
    }
    // 3. Ready under a second name first: a virus scanner that holds the file just after it ran
    // keeps the rename waiting here, while ours still has its name. The two renames that follow
    // are then back to back, so Glint.exe is never missing for more than a moment.
    let ready = dir.join(format!("{stem}.update-{pid}-2.exe"));
    if let Err(e) = rename_patiently(&tmp, &ready) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("the new Glint.exe is still in use ({e}). Nothing was changed; try again in a moment."));
    }
    // 4. Ours steps aside (allowed while it runs), the new one takes its name.
    let old = old_name(dir, &stem);
    if let Err(e) = rename_patiently(ours, &old) {
        let _ = std::fs::remove_file(&ready);
        return Err(format!("{} couldn't be renamed ({e}). Nothing was changed.", ours.display()));
    }
    if let Err(e) = rename_patiently(&ready, ours) {
        // Back as it was.
        let back = rename_patiently(&old, ours).or_else(|_| std::fs::copy(&old, ours).map(|_| ()));
        let _ = std::fs::remove_file(&ready);
        return Err(match back {
            Ok(()) => format!("the new Glint.exe couldn't be put in place ({e}). Glint stays as it was."),
            Err(b) => format!(
                "the new Glint.exe couldn't be put in place ({e}), and the old one couldn't get its name back ({b}). Your Glint is now called {}: rename it back to {}.",
                old.display(),
                ours.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())
            ),
        });
    }
    Ok(ours.to_path_buf())
}

/// Renames `from` to `to`, trying again for a moment: a virus scanner often holds a new exe just
/// after it was written (and a file that was just started, for a moment after it ended).
fn rename_patiently(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if tries >= 10 || e.kind() == std::io::ErrorKind::NotFound => return Err(e),
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

/// A free name for our old exe: Glint.old.exe (an older one is deleted first), or Glint.old2.exe...
/// when that one is still running somewhere.
fn old_name(dir: &Path, stem: &str) -> PathBuf {
    for n in 1..100 {
        let name = if n == 1 { format!("{stem}.old.exe") } else { format!("{stem}.old{n}.exe") };
        let p = dir.join(name);
        if !p.exists() || std::fs::remove_file(&p).is_ok() {
            return p;
        }
    }
    dir.join(format!("{stem}.old-{}.exe", std::process::id()))
}

/// Checks the copy of the new exe before it's put in place, starting it only once its version info
/// is right:
/// 1. its version info (read without starting it) is the release's version, `expected`;
/// 2. started with `--version` (no window), it says that version too.
fn verify(exe: &Path, expected: &Version) -> Result<(), String> {
    match exe_version(exe) {
        Some(n) if n == expected.numbers => {}
        Some([a, b, c]) => return Err(format!("The new Glint.exe says it's {a}.{b}.{c}, not {expected}.")),
        None => return Err("The new Glint.exe has no version information, so it isn't a Glint.".into()),
    }
    let out = run(exe, &[OsStr::new("--version")], VERIFY_LIMIT).map_err(|why| format!("The new Glint.exe didn't start as it should ({why})."))?;
    match out.split_whitespace().find_map(Version::parse) {
        Some(v) if v == *expected => Ok(()),
        Some(v) => Err(format!("The new Glint.exe says it's {v}, not {expected}.")),
        None => Err(format!("The new Glint.exe didn't say its version (it said \"{}\").", out.trim().chars().take(80).collect::<String>())),
    }
}

/// A job object that ends every process in it when it's closed (a program that ran out of time,
/// and what it started).
struct Job(HANDLE);

impl Job {
    fn new() -> Option<Job> {
        // SAFETY: a fresh unnamed job object with a properly sized limit struct.
        unsafe {
            let h = CreateJobObjectW(None, PCWSTR::null()).ok()?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
            if SetInformationJobObject(h, JobObjectExtendedLimitInformation, &info as *const _ as *const _, size).is_err() {
                let _ = CloseHandle(h);
                return None;
            }
            Some(Job(h))
        }
    }

    fn stop_all(&self) {
        // SAFETY: our own job handle.
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: closed once; anything still in the job ends with it (KILL_ON_JOB_CLOSE).
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Runs `exe` with `args` (in its own folder), without a window, for at most `limit`. Returns what
/// it wrote on its standard output, or why it failed.
fn run(exe: &Path, args: &[&OsStr], limit: Duration) -> Result<String, String> {
    let mut cmd = Command::new(exe);
    cmd.args(args).creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).env_remove("GLINT_TEST_VERSION");
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    let job = Job::new();
    let mut child = cmd.spawn().map_err(|e| format!("it couldn't be started: {e}"))?;
    let process = HANDLE(child.as_raw_handle());
    if let Some(j) = &job {
        // SAFETY: both handles are alive (the child isn't waited for yet). Best effort: without it
        // only the program itself is stopped at the time limit.
        unsafe {
            let _ = AssignProcessToJobObject(j.0, process);
        }
    }
    // Its output is read on a thread of its own while waiting, so it never blocks on a full pipe.
    let pipe = child.stdout.take();
    let reader = std::thread::Builder::new().name("update: version check".into()).spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut bytes);
        }
        bytes
    });
    // SAFETY: waiting on the child's own process handle, which lives until `child` is dropped.
    let finished = unsafe { WaitForSingleObject(process, limit.as_millis().min(u32::MAX as u128 - 1) as u32) } == WAIT_OBJECT_0;
    if !finished {
        match &job {
            Some(j) => j.stop_all(),
            None => {
                let _ = child.kill();
            }
        }
    }
    let status = child.wait();
    let stdout = reader.ok().and_then(|h| h.join().ok()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    if !finished {
        return Err(format!("it took longer than {} seconds and was stopped", limit.as_secs()));
    }
    match status {
        Ok(s) if s.success() => Ok(stdout),
        Ok(s) => Err(format!("it stopped with code {}", s.code().map_or("?".to_string(), |c| c.to_string()))),
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------- after installing

/// The new exe to start once Glint has quit (set when an install succeeded), and whether it starts
/// hidden.
static RESTART: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);

/// After a successful install: once the window is gone, main.rs starts `exe --after-update`
/// (`hidden`: Glint was hidden in the background by then, so the new one starts hidden too).
pub fn restart_after_quit(exe: PathBuf, hidden: bool) {
    *RESTART.lock().unwrap_or_else(PoisonError::into_inner) = Some((exe, hidden));
}

/// Called by main.rs after the message loop (the main Glint has let go of being the main one, so the
/// new exe becomes it): starts the new exe, if an install asked for it.
pub fn restart_if_installed() {
    let Some((exe, hidden)) = RESTART.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
    let mut cmd = Command::new(&exe);
    cmd.arg(AFTER_UPDATE).env_remove("GLINT_TEST_VERSION");
    if hidden {
        cmd.arg(crate::registry::BACKGROUND_SWITCH);
    }
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    match cmd.spawn() {
        Ok(_) => log::line(&format!("update: started {} {AFTER_UPDATE}", exe.display())),
        Err(e) => log::error(&format!("The new Glint is in place, but it couldn't be started ({e}). Start Glint again yourself.")),
    }
}

/// The switch the new exe is started with after an update: it opens the same tabs.
pub const AFTER_UPDATE: &str = "--after-update";

/// At the main Glint's start: deletes what an update left next to glint.exe (`Glint.old.exe`, and a
/// copy that never got put in place), on a thread of its own. The old process may still be ending
/// (waiting for a copy to finish), so a file still in use is tried again for a minute.
/// `just_updated` (started with --after-update): the Glint from before the update stays as
/// Glint.old.exe until a later start, so it's still there if the new one turns out not to work.
pub fn remove_old_exes(just_updated: bool) {
    let Ok(ours) = std::env::current_exe() else { return };
    shell::background("update: remove old exe", move || {
        for attempt in 0..12 {
            let left = old_exes(&ours, just_updated).into_iter().filter(|p| std::fs::remove_file(p).is_err() && p.exists()).count();
            if left == 0 {
                return;
            }
            if attempt == 11 {
                log::line(&format!("update: {left} old exe(s) next to glint.exe are still in use; deleted at a later start"));
                return;
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

/// The Glint from before the last update, when it's still next to `ours` (for the crash message).
pub fn previous_exe(ours: &Path) -> Option<PathBuf> {
    let stem = ours.file_stem()?.to_string_lossy().into_owned();
    let old = ours.with_file_name(format!("{stem}.old.exe"));
    (old.is_file() && !old.as_os_str().eq_ignore_ascii_case(ours.as_os_str())).then_some(old)
}

/// The files next to `ours` that an update leaves: "<name>.old.exe", "<name>.old2.exe"... and
/// "<name>.update-<pid>.exe" (never `ours` itself, whatever it's called). `keep_old`: only the
/// copies, not the old Glints.
fn old_exes(ours: &Path, keep_old: bool) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (ours.parent(), ours.file_stem()) else { return Vec::new() };
    let stem = stem.to_string_lossy().to_lowercase();
    // Glint started as "Glint.old.exe" itself (an update that couldn't start the new one): the
    // leftovers are still named after "Glint".
    let stem = stem.strip_suffix(".old").unwrap_or(&stem).to_string();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let Some(middle) = name.strip_prefix(&format!("{stem}.")).and_then(|r| r.strip_suffix(".exe")) else { return false };
            let old = middle.strip_prefix("old").is_some_and(|n| n.is_empty() || n.bytes().all(|b| b.is_ascii_digit()) || n.starts_with('-'));
            // "update-1234" and "update-1234-2" (the copy, ready to take our name).
            let copy = middle.strip_prefix("update-").is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()) && n.bytes().all(|b| b.is_ascii_digit() || b == b'-'));
            ((old && !keep_old) || copy) && !p.as_os_str().eq_ignore_ascii_case(ours.as_os_str())
        })
        .collect()
}

// ---------------------------------------------------------------- --version

/// `glint.exe --version`: prints "Glint 0.3.0" and returns (no window). To the standard output when
/// it goes somewhere (a pipe, a file - how the updater asks), otherwise to the console Glint was
/// started from (glint.exe is a windowed program, which Windows starts without one).
pub fn print_version() {
    use std::io::Write;
    let line = format!("Glint {}\n", ours());
    // SAFETY: plain queries of this process's standard output handle.
    let has_stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.is_ok_and(|h| !h.is_invalid());
    if has_stdout {
        let mut out = std::io::stdout();
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
        return;
    }
    // SAFETY: attaching to the parent's console (if it has one) only lets this process write to it.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
        if let Ok(mut console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let _ = console.write_all(format!("\n{line}").as_bytes());
        }
    }
}

/// Whether the command line asks for the version (looked at before anything else starts).
pub fn wants_version(args: &[OsString]) -> bool {
    args.iter().any(|a| a.eq_ignore_ascii_case("--version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn versions_compare_like_semver() {
        assert!(v("0.3.1") > v("0.3.0"));
        assert!(v("0.10.0") > v("0.9.9"), "numbers, not text");
        assert!(v("1.0.0") > v("0.99.99"));
        assert_eq!(v("v0.3"), v("0.3.0"));
        assert_eq!(v("0.3.0+build.5"), v("0.3.0"), "build data doesn't count");
        // Pre-releases come before their release, and among themselves by their parts.
        assert!(v("0.4.0-beta") < v("0.4.0"));
        assert!(v("0.4.0-beta") > v("0.3.9"));
        assert!(v("0.4.0-alpha") < v("0.4.0-alpha.1"));
        assert!(v("0.4.0-alpha.2") < v("0.4.0-alpha.10"));
        assert!(v("0.4.0-alpha.9") < v("0.4.0-beta"));
        assert!(v("0.4.0-1") < v("0.4.0-alpha"), "numbers before words");
        assert_eq!(v("0.4.0-rc.1").to_string(), "0.4.0-rc.1");
        for bad in ["", "x", "1.2.3.4", "1..2", "1.2.x", "-1.2.3", "1.2.3-", "1.2.3-a..b", " "] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn the_changelog_shows_what_came_since() {
        let md = "# What changed\n\nIntro.\n\n## 0.3.2 (2026-10-20): more\n\n- c\n\n## 0.3.1 (2026-10-12): fixes\n\n- b\n  more b\n\n## 0.3.0 (2026-10-10): updates\n\n- a\n";
        assert_eq!(changelog_since(md, &v("0.3.0"), &v("0.3.2")), "## 0.3.2 (2026-10-20): more\n\n- c\n\n## 0.3.1 (2026-10-12): fixes\n\n- b\n  more b");
        assert_eq!(changelog_since(md, &v("0.3.0"), &v("0.3.1")), "## 0.3.1 (2026-10-12): fixes\n\n- b\n  more b");
        assert_eq!(changelog_since(md, &v("0.3.2"), &v("0.3.2")), "");
        // Glint's own changelog has a section for its own version.
        let own = include_str!("../CHANGELOG.md");
        assert!(changelog_since(own, &v("0.0.1"), &v(log::VERSION)).starts_with(&format!("## {}", log::VERSION)));
        // What's new: the changelog's sections, or the release's own text when there are none.
        assert_eq!(whats_new(Some(md), "text", &v("0.3.1"), &v("0.3.2")), "## 0.3.2 (2026-10-20): more\n\n- c");
        assert_eq!(whats_new(Some("# nothing here\n"), "text", &v("0.3.1"), &v("0.3.2")), "text");
        assert_eq!(whats_new(None, "text", &v("0.3.1"), &v("0.3.2")), "text");
    }

    /// GitHub's answer about the newest release, as its API gives it (shortened).
    fn latest_json(tag: &str, assets: &str) -> String {
        format!(
            r#"{{
  "url": "https://api.github.com/repos/npr-tami/glint/releases/1",
  "html_url": "https://github.com/npr-tami/glint/releases/tag/{tag}",
  "id": 1,
  "author": {{"login": "tamara", "id": 2}},
  "tag_name": "{tag}",
  "target_commitish": "main",
  "name": "Glint {tag}",
  "draft": false,
  "prerelease": false,
  "created_at": "2026-10-20T10:00:00Z",
  "published_at": "2026-10-20T10:05:00Z",
  "assets": [{assets}],
  "tarball_url": "https://api.github.com/repos/npr-tami/glint/tarball/{tag}",
  "zipball_url": "https://api.github.com/repos/npr-tami/glint/zipball/{tag}",
  "body": "  ## What's new\r\n\r\n- faster  "
}}"#
        )
    }

    fn asset(name: &str, size: u64, state: &str) -> String {
        format!(
            r#"{{"url": "https://api.github.com/repos/npr-tami/glint/releases/assets/7", "id": 7, "name": "{name}", "label": null, "uploader": {{"login": "github-actions[bot]"}},
               "content_type": "application/x-msdownload", "state": "{state}", "size": {size}, "download_count": 3,
               "browser_download_url": "https://github.com/npr-tami/glint/releases/download/v0.8.0/{name}"}}"#
        )
    }

    #[test]
    fn the_highest_version_in_the_list_wins() {
        let ours = v("0.8.0");
        let exe = |tag: &str| asset("Glint.exe", 3_000_000, "uploaded").replace("v0.8.0", tag);
        // GitHub lists newest first, but its "latest" can be an older release: the highest version
        // counts, whatever the order.
        let list = format!("[{},{}]", latest_json("v0.8.0", &exe("v0.8.0")), latest_json("v0.9.0", &exe("v0.9.0")));
        assert!(matches!(read_releases(list.as_bytes(), &ours), Found::Newer(r) if r.version == "0.9.0" && r.url.ends_with("/v0.9.0/Glint.exe")));
        // Drafts and pre-releases are left out.
        let pre = latest_json("v1.0.0", &exe("v1.0.0")).replace("\"prerelease\": false", "\"prerelease\": true");
        let draft = latest_json("v0.9.5", &exe("v0.9.5")).replace("\"draft\": false", "\"draft\": true");
        let list = format!("[{pre},{draft},{}]", latest_json("v0.9.0", &exe("v0.9.0")));
        assert!(matches!(read_releases(list.as_bytes(), &ours), Found::Newer(r) if r.version == "0.9.0"));
        // Nothing newer, nothing at all, or something that isn't a list.
        assert_eq!(read_releases(format!("[{}]", latest_json("v0.8.0", &exe("v0.8.0"))).as_bytes(), &ours), Found::UpToDate("0.8.0".into()));
        assert_eq!(read_releases(b"[]", &ours), Found::Unavailable("no release yet".into()));
        assert!(matches!(read_releases(b"{}", &ours), Found::Failed(_)));
    }

    #[test]
    fn the_newest_release_is_read() {
        let ours = v("0.7.0");
        let exe = asset("Glint.exe", 3_000_000, "uploaded");
        let others = format!("{},{}", asset("Glint.zip", 1_000_000, "uploaded"), asset("Glint.exe.sha256", 64, "uploaded"));
        match read_latest(latest_json("v0.8.0", &format!("{others},{exe}")).as_bytes(), &ours) {
            Found::Newer(r) => {
                assert_eq!((r.version.as_str(), r.tag.as_str(), r.size), ("0.8.0", "v0.8.0", 3_000_000));
                assert_eq!(r.url, "https://github.com/npr-tami/glint/releases/download/v0.8.0/Glint.exe");
                assert_eq!(r.changelog, "## What's new\r\n\r\n- faster", "the release's own text until CHANGELOG.md is read");
            }
            other => panic!("{other:?}"),
        }
        // Any case; a tag without its "v".
        let lower = asset("glint.EXE", 5, "uploaded");
        assert!(matches!(read_latest(latest_json("0.8.0", &lower).as_bytes(), &ours), Found::Newer(r) if r.size == 5 && r.tag == "0.8.0"));
        // No Glint.exe (yet), only other files, or one still being uploaded: not ready.
        assert_eq!(read_latest(latest_json("v0.8.0", "").as_bytes(), &ours), Found::NotReady("0.8.0".into()));
        assert_eq!(read_latest(latest_json("v0.8.0", &others).as_bytes(), &ours), Found::NotReady("0.8.0".into()));
        assert_eq!(read_latest(latest_json("v0.8.0", &asset("Glint.exe", 3_000_000, "open")).as_bytes(), &ours), Found::NotReady("0.8.0".into()));
        assert_eq!(read_latest(latest_json("v0.8.0", &asset("Glint.exe", 0, "uploaded")).as_bytes(), &ours), Found::NotReady("0.8.0".into()));
        assert_eq!(read_latest(latest_json("v0.8.0", &asset("Glint-setup.exe", 9, "uploaded")).as_bytes(), &ours), Found::NotReady("0.8.0".into()));
        // The same version, or an older one: up to date.
        assert_eq!(read_latest(latest_json("v0.7.0", &exe).as_bytes(), &ours), Found::UpToDate("0.7.0".into()));
        assert_eq!(read_latest(latest_json("v0.6.2", &exe).as_bytes(), &ours), Found::UpToDate("0.6.2".into()));
        // A Glint.exe that isn't GitHub's, or far too big, is never offered.
        let elsewhere = exe.replace("https://github.com/", "https://example.com/");
        assert!(matches!(read_latest(latest_json("v0.8.0", &elsewhere).as_bytes(), &ours), Found::Failed(_)));
        let plain = exe.replace("https://", "http://");
        assert!(matches!(read_latest(latest_json("v0.8.0", &plain).as_bytes(), &ours), Found::Failed(_)));
        assert!(matches!(read_latest(latest_json("v0.8.0", &asset("Glint.exe", 1 << 40, "uploaded")).as_bytes(), &ours), Found::Failed(_)));
        // Drafts and pre-releases don't count; nonsense is said to be nonsense.
        let draft = latest_json("v0.8.0", &exe).replace("\"draft\": false", "\"draft\": true");
        assert!(matches!(read_latest(draft.as_bytes(), &ours), Found::Unavailable(_)));
        let pre = latest_json("v0.8.0", &exe).replace("\"prerelease\": false", "\"prerelease\": true");
        assert!(matches!(read_latest(pre.as_bytes(), &ours), Found::Unavailable(_)));
        for bad in ["", "not json", "[]", "{\"message\": \"Not Found\"}"] {
            assert!(matches!(read_latest(bad.as_bytes(), &ours), Found::Failed(_)), "{bad:?}");
        }
        assert!(matches!(read_latest(latest_json("nightly", &exe).as_bytes(), &ours), Found::Failed(_)));
        // A body that's null is no text.
        let no_body = latest_json("v0.8.0", &exe).replace("\"  ## What's new\\r\\n\\r\\n- faster  \"", "null");
        assert!(matches!(read_latest(no_body.as_bytes(), &ours), Found::Newer(r) if r.changelog.is_empty()));
    }

    #[test]
    fn tags_and_addresses() {
        for good in ["v0.8.0", "0.8.0", "V1.2.3", "v0.9.0-beta.1", "v1.0.0+build.5", "v0.8"] {
            assert!(valid_tag(good), "{good}");
        }
        for bad in ["", "nightly", "v0.8.0/../x", "v0.8.0 x", "v1.0.0+a b", "v1.0.0+a/b", "v1.0.0?x", "release-1"] {
            assert!(!valid_tag(bad), "{bad:?}");
        }
        assert_eq!(releases_url("npr-tami/glint"), "https://api.github.com/repos/npr-tami/glint/releases?per_page=20");
        assert_eq!(changelog_url("npr-tami/glint", "v0.8.0"), "https://raw.githubusercontent.com/npr-tami/glint/v0.8.0/CHANGELOG.md");
        assert_eq!(split_https("https://api.github.com/repos/a/b/releases/latest"), Some(("api.github.com", "/repos/a/b/releases/latest")));
        assert_eq!(split_https("https://github.com"), Some(("github.com", "/")));
        for bad in ["http://github.com/x", "ftp://github.com/x", "https://", "https://user@github.com/x", "https://github.com:8443/x", "https://github.com/a b", "https://github.com/a\nb", "github.com/x"] {
            assert_eq!(split_https(bad), None, "{bad:?}");
        }
        assert!(download_allowed("https://github.com/npr-tami/glint/releases/download/v0.8.0/Glint.exe"));
        assert!(download_allowed("https://GitHub.com/someone/fork/releases/download/v1.0.0/Glint.exe"));
        for bad in ["http://github.com/npr-tami/glint/releases/download/v0.8.0/Glint.exe", "https://github.com.example.com/a/b/releases/download/v1/Glint.exe", "https://example.com/a/b/releases/download/v1/Glint.exe", "https://github.com/npr-tami/glint/archive/main.zip"] {
            assert!(!download_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn only_owner_and_name_name_a_repository() {
        let long = "x".repeat(101);
        for good in ["npr-tami/glint", "a/b", "Some.User_1/my-repo.rs", "x/.github"] {
            assert!(valid_repo(good), "{good}");
        }
        for bad in ["", "glint", "/glint", "npr-tami/", "a/b/c", "a//b", "../glint", "a/..", "./b", "a b/c", "a/b?x", "a/b#c", "https://github.com/npr-tami/glint.git", "git@github.com:a/b.git", "a\\b", "a/b\n", long.as_str()] {
            assert!(!valid_repo(bad), "{bad:?}");
        }
        assert!(valid_repo(settings::UPDATE_REPO), "the standard one passes");
    }

    #[test]
    fn when_to_ask() {
        let release = |ver: &str| Found::Newer(Release { version: ver.into(), ..Default::default() });
        let mut s = Settings::default();
        assert!(should_prompt(&release("9.0.0"), &s, false, 100));
        assert!(!should_prompt(&Found::UpToDate("9.0.0".into()), &s, true, 100));
        assert!(!should_prompt(&Found::Unavailable("no release yet".into()), &s, true, 100));
        // "Don't remind me again" for 9.0.0: not that one, but a newer one; asking always shows it.
        s.skipped_update = Some("9.0.0".into());
        assert!(!should_prompt(&release("9.0.0"), &s, false, 100));
        assert!(should_prompt(&release("9.0.1"), &s, false, 100));
        assert!(should_prompt(&release("9.0.0"), &s, true, 100));
        // "Remind me later": not before the time.
        s.remind_update_after = 1000;
        assert!(!should_prompt(&release("9.0.1"), &s, false, 999));
        assert!(should_prompt(&release("9.0.1"), &s, false, 1000));
        assert!(should_prompt(&release("9.0.1"), &s, true, 999));
    }

    #[test]
    fn builds_from_source_dont_replace_themselves() {
        assert!(built_from_source(Path::new("C:\\code\\glint\\target\\release\\glint.exe")));
        assert!(built_from_source(Path::new("C:\\x\\TARGET\\Debug\\glint.exe")));
        assert!(built_from_source(Path::new("D:\\w\\target\\x86_64-pc-windows-gnu\\release\\glint.exe")));
        assert!(!built_from_source(Path::new("C:\\Users\\me\\Glint\\Glint.exe")));
        assert!(!built_from_source(Path::new("C:\\targets\\Glint.exe")));
    }

    /// A folder of its own for one test.
    fn test_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("glint-update-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn installing_swaps_the_exe_and_keeps_the_old_one() {
        let d = test_dir("swap");
        let (new, ours) = (d.join("Glint-0.8.0.exe"), d.join("Glint.exe"));
        std::fs::write(&new, b"new glint").unwrap();
        std::fs::write(&ours, b"old glint").unwrap();
        std::fs::write(d.join("Glint.old.exe"), b"older glint").unwrap();
        let checked = std::cell::Cell::new(false);
        let done = install_into(&new, &ours, |tmp| {
            assert_eq!(std::fs::read(tmp).unwrap(), b"new glint", "the copy is checked, next to ours");
            assert_eq!(tmp.parent(), ours.parent());
            checked.set(true);
            Ok(())
        });
        assert_eq!(done, Ok(ours.clone()));
        assert!(checked.get());
        assert_eq!(std::fs::read(&ours).unwrap(), b"new glint");
        assert_eq!(std::fs::read(d.join("Glint.old.exe")).unwrap(), b"old glint", "the one before goes, ours is kept");
        assert_eq!(std::fs::read(&new).unwrap(), b"new glint", "the download stays (install deletes it)");
        // What the next start deletes.
        let mut left: Vec<String> = old_exes(&ours, false).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, vec!["Glint.old.exe"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_exe_that_fails_its_check_changes_nothing() {
        let d = test_dir("refused");
        let (new, ours) = (d.join("new.exe"), d.join("Glint.exe"));
        std::fs::write(&new, b"broken").unwrap();
        std::fs::write(&ours, b"working glint").unwrap();
        let done = install_into(&new, &ours, |_| Err("The new Glint.exe says it's 0.2.0, not 0.3.1.".into()));
        assert!(done.as_ref().is_err_and(|e| e.ends_with("Nothing was changed.")), "{done:?}");
        assert_eq!(std::fs::read(&ours).unwrap(), b"working glint");
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "no copy left behind: {names:?}");
        // A new exe that's gone: nothing changes either.
        let gone = install_into(&d.join("missing.exe"), &ours, |_| Ok(()));
        assert!(gone.is_err());
        assert_eq!(std::fs::read(&ours).unwrap(), b"working glint");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_what_an_update_leaves_is_deleted() {
        let d = test_dir("leftovers");
        for name in ["Glint.exe", "Glint.old.exe", "Glint.old2.exe", "Glint.update-1234.exe", "Glint.update-1234-2.exe", "Glint.oldish.exe", "Glint.update-x.exe", "Glint.update--1.exe", "Other.old.exe", "notes.txt"] {
            std::fs::write(d.join(name), b"x").unwrap();
        }
        let names = |ours: &str, keep_old: bool| {
            let mut found: Vec<String> = old_exes(&d.join(ours), keep_old).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
            found.sort();
            found
        };
        assert_eq!(names("Glint.exe", false), vec!["Glint.old.exe", "Glint.old2.exe", "Glint.update-1234-2.exe", "Glint.update-1234.exe"]);
        // Started as Glint.old.exe itself: never itself.
        assert_eq!(names("Glint.old.exe", false), vec!["Glint.old2.exe", "Glint.update-1234-2.exe", "Glint.update-1234.exe"]);
        // Just updated: the Glint from before stays for now.
        assert_eq!(names("Glint.exe", true), vec!["Glint.update-1234-2.exe", "Glint.update-1234.exe"]);
        assert_eq!(previous_exe(&d.join("Glint.exe")), Some(d.join("Glint.old.exe")));
        assert_eq!(previous_exe(&d.join("Other.exe")), Some(d.join("Other.old.exe")));
        assert_eq!(previous_exe(&d.join("notes.exe")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn downloads_go_only_where_they_should() {
        let d = test_dir("download");
        // Earlier downloads are deleted before a new one; nothing else in the cache is touched.
        for name in ["Glint-0.7.1.exe", "glint-0.8.0.EXE", "thumbs.db", "Glint.exe.txt"] {
            std::fs::write(d.join(name), b"x").unwrap();
        }
        std::fs::create_dir_all(d.join("Glint-folder.exe")).unwrap();
        remove_downloads(&d);
        let mut names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, vec!["Glint-folder.exe", "Glint.exe.txt", "thumbs.db"]);
        // An address that isn't GitHub's, or a size that makes no sense, is refused before anything
        // is asked or written.
        let release = |url: &str, size: u64| Release { version: "0.8.0".into(), url: url.into(), size, ..Default::default() };
        let good = "https://github.com/npr-tami/glint/releases/download/v0.8.0/Glint.exe";
        for r in [release("https://example.com/releases/download/v0.8.0/Glint.exe", 5), release(good, 0), release(good, MAX_EXE + 1)] {
            let said = download(&r, &d);
            assert!(said.as_ref().is_err_and(|e| e.ends_with("Nothing was changed.")), "{said:?}");
        }
        assert!(!d.join("Glint-0.8.0.exe").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_exe_is_checked_before_it_is_started() {
        let d = test_dir("verify");
        // A file with no version information is never started.
        std::fs::write(d.join("plain.exe"), b"not a program").unwrap();
        let said = verify(&d.join("plain.exe"), &v("0.3.0"));
        assert!(said.as_ref().is_err_and(|e| e.contains("no version information")), "{said:?}");
        assert_eq!(exe_version(&d.join("plain.exe")), None, "not an exe");
        assert_eq!(exe_version(&d.join("missing.exe")), None);
        // This test program carries Glint's version information when it's built with it: then a
        // version other than that is refused without it being run.
        let me = std::env::current_exe().unwrap();
        if let Some(n) = exe_version(&me) {
            assert_eq!(n, v(log::VERSION).numbers);
            for other in ["99.0.0", "0.0.1"] {
                let said = verify(&me, &v(other));
                assert!(said.as_ref().is_err_and(|e| e.contains(&format!("not {other}"))), "{said:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn about_says_it_briefly() {
        let at = "18:48".to_string();
        let ours = ours();
        let said = |found: Found| {
            status_lock().last = Some((found, at.clone()));
            let (text, ready) = about(true, true);
            status_lock().last = None;
            assert!(ready);
            text
        };
        assert_eq!(said(Found::UpToDate(ours.to_string())), format!("You have the newest Glint ({ours}). Checked at 18:48."));
        assert_eq!(said(Found::Unavailable("no release yet".into())), "Couldn't check for updates right now. Tried at 18:48.");
        assert_eq!(said(Found::Failed("GitHub couldn't be reached (is the computer online?)".into())), "Couldn't check for updates at 18:48: GitHub couldn't be reached (is the computer online?).");
        assert_eq!(said(Found::NotReady("99.0.0".into())), "Glint 99.0.0 is coming but isn't ready yet. Checked at 18:48.");
        assert_eq!(about(false, true).0, "Only the main Glint window checks for updates.");
    }
}
