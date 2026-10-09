//! Glint in Windows' registry: "Open folders and drives in Glint", "Win+E opens Glint" and "Start with
//! Windows". Everything is per user, under HKEY_CURRENT_USER: no admin rights are needed, other
//! accounts are never touched, and HKEY_LOCAL_MACHINE is never written.
//!
//! The keys other file explorers use too:
//! - **Folders and drives**: a "glint" verb under `Software\Classes\Directory\shell` and
//!   `...\Drive\shell` ("Open in Glint", `"glint.exe" "%1"`), made the default verb. Only Directory and
//!   Drive, not Folder, so Control Panel, the Recycle Bin, phones and the like keep opening in File
//!   Explorer.
//! - **Win+E**: the command of `Software\Classes\CLSID\{52205fd8-...}\shell\opennewwindow`, marked
//!   with a "GlintOwned" value so Glint knows it's its own.
//! - Whatever was there before (another program's Win+E, say) is backed up under
//!   `Software\Glint\RegistryBackup` and put back when the setting is turned off.
//!
//! There is only one "default file explorer": turning it on points it at this glint.exe, and turning
//! it off (or "Restore File Explorer.bat") gives everything back to File Explorer, whichever
//! glint.exe it pointed at. A setting counts as "on" only when it points at *this* glint.exe.
//!
//! **Start with Windows** is the "Glint" value of `...\CurrentVersion\Run`:
//! `"glint.exe" --background`.
//!
//! If glint.exe is moved, `repair_if_moved` (at start) points everything that pointed at the old
//! place, which no longer exists, at the new one.
//!
//! GLINT_TEST_REGISTRY=Software\SomeKey puts all of it under that key instead (for testing: nothing
//! real changes then, and Windows isn't told about new associations).

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW,
    RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ,
};
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLongPathNameW};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

use crate::folder::wide;

/// The verb Glint adds to folders and drives.
const VERB: &str = "glint";
/// What Win+E opens: "Home" / "This PC", depending on Windows' choice.
const HOME_CLSID: &str = "{52205fd8-5dfb-447d-801a-d0b52f2e83e1}";
/// In the backup: "there was nothing".
const NONE: &str = "<none>";
/// On the Win+E command, so Glint knows it's its own.
const MARKER: &str = "GlintOwned";
/// The folder types Glint opens.
const FOLDER_TYPES: [&str; 2] = ["Directory", "Drive"];
/// The value under Run for "Start with Windows".
const RUN_VALUE: &str = "Glint";
/// The switch "Start with Windows" adds: start hidden in the background.
pub const BACKGROUND_SWITCH: &str = "--background";

/// Where everything goes: the real places, or a test key (GLINT_TEST_REGISTRY).
#[derive(Clone, Debug)]
pub struct Places {
    classes: String,
    backup: String,
    run: String,
    /// The real places: Windows is told when folder associations change.
    real: bool,
    /// Tell Windows right away instead of on a thread of its own (the process is about to end).
    at_once: bool,
}

impl Places {
    /// The places for this run: the real ones, unless GLINT_TEST_REGISTRY names a test key.
    pub fn current() -> Places {
        match std::env::var("GLINT_TEST_REGISTRY").ok().map(|r| r.trim().trim_matches('\\').to_string()).filter(|r| !r.is_empty()) {
            Some(root) => Places::under(&root),
            None => Places {
                classes: r"Software\Classes".into(),
                backup: r"Software\Glint\RegistryBackup".into(),
                run: r"Software\Microsoft\Windows\CurrentVersion\Run".into(),
                real: true,
                at_once: false,
            },
        }
    }

    /// Everything under `root` (in HKEY_CURRENT_USER), for tests.
    pub fn under(root: &str) -> Places {
        Places { classes: format!(r"{root}\Classes"), backup: format!(r"{root}\Backup"), run: format!(r"{root}\Run"), real: false, at_once: false }
    }

    /// The same places, but Windows is told about changes before the call returns (for the
    /// command-line switches, whose process ends right after).
    pub fn telling_the_shell_at_once(mut self) -> Places {
        self.at_once = true;
        self
    }

    fn shell(&self, kind: &str) -> String {
        format!(r"{}\{kind}\shell", self.classes)
    }

    fn win_e(&self) -> String {
        format!(r"{}\CLSID\{HOME_CLSID}\shell\opennewwindow\command", self.classes)
    }
}

// ---------------------------------------------------------------- a registry key

/// An open key of HKEY_CURRENT_USER, closed when dropped.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW / RegCreateKeyExW and is closed once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// A registry error in plain words.
fn problem(what: &str, path: &str, e: WIN32_ERROR) -> String {
    format!("Windows didn't let Glint {what} HKEY_CURRENT_USER\\{path} (error {}).", e.0)
}

impl Key {
    /// Opens an existing key; None if it isn't there (or can't be opened).
    fn open(path: &str, write: bool) -> Option<Key> {
        let p = wide(path);
        let mut h = HKEY::default();
        let access = if write { KEY_READ | KEY_WRITE } else { KEY_READ };
        // SAFETY: `p` is NUL-terminated and `h` a valid out-pointer.
        let e = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), None, access, &mut h) };
        (e == ERROR_SUCCESS).then_some(Key(h))
    }

    /// Opens a key, making it (and the keys above it) if needed.
    fn create(path: &str) -> Result<Key, String> {
        let p = wide(path);
        let mut h = HKEY::default();
        // SAFETY: `p` is NUL-terminated and `h` a valid out-pointer; no class, default security.
        let e = unsafe { RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_READ | KEY_WRITE, None, &mut h, None) };
        if e == ERROR_SUCCESS {
            Ok(Key(h))
        } else {
            Err(problem("change", path, e))
        }
    }

    /// A text value ("" = the key's default value); None if it isn't there or isn't text.
    fn string(&self, name: &str) -> Option<String> {
        let n = wide(name);
        let mut bytes = 0u32;
        // SAFETY: asks for the size first; `n` is NUL-terminated.
        let e = unsafe { RegGetValueW(self.0, PCWSTR::null(), PCWSTR(n.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut bytes)) };
        if e != ERROR_SUCCESS || bytes > 64 * 1024 {
            return None;
        }
        let mut buf = vec![0u16; (bytes as usize).div_ceil(2) + 1];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size` bytes; RegGetValueW NUL-terminates text values.
        let e = unsafe { RegGetValueW(self.0, PCWSTR::null(), PCWSTR(n.as_ptr()), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size)) };
        if e != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// Whether a value of any kind is there.
    fn has(&self, name: &str) -> bool {
        let n = wide(name);
        // SAFETY: only asks whether it exists; `n` is NUL-terminated.
        unsafe { RegQueryValueExW(self.0, PCWSTR(n.as_ptr()), None, None, None, None) == ERROR_SUCCESS }
    }

    fn set_string(&self, name: &str, value: &str, path: &str) -> Result<(), String> {
        let n = wide(name);
        let v = wide(value);
        // SAFETY: the data is the UTF-16 text with its NUL, as REG_SZ wants it.
        let bytes = unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), v.len() * 2) };
        // SAFETY: `n` is NUL-terminated and `bytes` lives across the call.
        let e = unsafe { RegSetValueExW(self.0, PCWSTR(n.as_ptr()), None, REG_SZ, Some(bytes)) };
        if e == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(problem("change", path, e))
        }
    }

    fn set_dword(&self, name: &str, value: u32, path: &str) -> Result<(), String> {
        let n = wide(name);
        let bytes = value.to_le_bytes();
        // SAFETY: `n` is NUL-terminated and `bytes` lives across the call.
        let e = unsafe { RegSetValueExW(self.0, PCWSTR(n.as_ptr()), None, REG_DWORD, Some(&bytes)) };
        if e == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(problem("change", path, e))
        }
    }

    /// Deletes a value; fine if it isn't there.
    fn delete_value(&self, name: &str, path: &str) -> Result<(), String> {
        let n = wide(name);
        // SAFETY: `n` is NUL-terminated.
        let e = unsafe { RegDeleteValueW(self.0, PCWSTR(n.as_ptr())) };
        if e == ERROR_SUCCESS || e == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(problem("change", path, e))
        }
    }

    /// Deletes a subkey with everything in it; fine if it isn't there.
    fn delete_tree(&self, sub: &str, path: &str) -> Result<(), String> {
        let s = wide(sub);
        // SAFETY: `s` is NUL-terminated and names a subkey of this key.
        let e = unsafe { RegDeleteTreeW(self.0, PCWSTR(s.as_ptr())) };
        if e == ERROR_SUCCESS || e == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(problem("remove", &format!(r"{path}\{sub}"), e))
        }
    }

    /// Whether it holds no subkeys and no values.
    fn is_empty(&self) -> bool {
        let (mut keys, mut values) = (0u32, 0u32);
        // SAFETY: valid out-pointers; everything else isn't asked for.
        let e = unsafe { RegQueryInfoKeyW(self.0, None, None, None, Some(&mut keys), None, None, Some(&mut values), None, None, None, None) };
        e == ERROR_SUCCESS && keys == 0 && values == 0
    }
}

/// Deletes `path` and the keys above it while they're empty, stopping at `stop_at` (which stays).
fn delete_empty_keys(path: &str, stop_at: &str) {
    let mut path = path.to_string();
    while path.len() > stop_at.len() && path.to_lowercase().starts_with(&stop_at.to_lowercase()) {
        match Key::open(&path, false) {
            Some(k) if !k.is_empty() => return,
            _ => {}
        }
        let p = wide(&path);
        // SAFETY: `p` is NUL-terminated; RegDeleteKeyW only deletes a key without subkeys.
        let e = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr())) };
        if e != ERROR_SUCCESS && e != ERROR_FILE_NOT_FOUND {
            return;
        }
        match path.rfind('\\') {
            Some(cut) => path.truncate(cut),
            None => return,
        }
    }
}

/// Tells Explorer that folder associations changed (only for the real places). Done on a thread of
/// its own (it's a broadcast to the shell, which the UI thread shouldn't wait for), unless the
/// places say "at once".
fn notify_shell(places: &Places) {
    // SAFETY: SHCNE_ASSOCCHANGED takes no items.
    let tell = || unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    if !places.real {
        return;
    }
    if places.at_once || !crate::shell::background("tell the shell", tell) {
        tell();
    }
}

// ---------------------------------------------------------------- commands and exe paths

/// The command that opens a folder in Glint: `"glint.exe" "%1"`.
pub fn folder_command(exe: &str) -> String {
    format!("\"{exe}\" \"%1\"")
}

/// The command Win+E runs: `"glint.exe"`.
pub fn win_e_command(exe: &str) -> String {
    format!("\"{exe}\"")
}

/// The command that starts Glint at sign-in: `"glint.exe" --background`.
pub fn run_command(exe: &str) -> String {
    format!("\"{exe}\" {BACKGROUND_SWITCH}")
}

/// The exe a command starts: the first quoted part ("\"C:\\a b\\glint.exe\" \"%1\"" -> C:\a b\glint.exe).
pub fn exe_of(command: &str) -> Option<String> {
    let rest = command.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    (end > 0).then(|| rest[..end].to_string())
}

/// Whether two exe paths are the same file (Windows paths ignore case; "\\?\" makes no difference).
pub fn same_exe(a: &str, b: &str) -> bool {
    let clean = |p: &str| p.trim_start_matches(r"\\?\").replace('/', "\\").to_lowercase();
    clean(a) == clean(b)
}

/// Why `exe` shouldn't become what folders, drives or Win+E open: it sits where it may soon be gone.
/// Pointed at an exe that is gone, Windows can't open any folder with a double-click (on the
/// desktop, in a shortcut, from another program) until "Restore File Explorer.bat" is run. So it's
/// refused for a temporary folder (a glint.exe started from inside a zip file runs from one, and
/// Windows empties those), a removable drive, a CD, or a network drive. None: a place it can stay.
pub fn risky_place(exe: &str) -> Option<&'static str> {
    if exe.starts_with(r"\\") {
        return Some("glint.exe is on a network drive, so folders could stop opening. Move Glint to this PC's own drive first.");
    }
    let lower = exe.to_lowercase();
    let temp = std::env::temp_dir().to_string_lossy().into_owned();
    let in_temp = [temp.clone(), long_path(&temp)].iter().map(|t| t.trim_end_matches('\\').to_lowercase()).any(|t| !t.is_empty() && lower.starts_with(&format!("{t}\\")));
    if in_temp {
        return Some("glint.exe is in a temporary folder (inside a zip file?), so folders could stop opening. Move Glint to a folder of its own first.");
    }
    let root: String = exe.chars().take(3).collect();
    if root.len() == 3 && root.ends_with(":\\") {
        let root_w = wide(&root);
        // SAFETY: a NUL-terminated root path that outlives the call.
        let kind = unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) };
        // DRIVE_REMOVABLE, DRIVE_REMOTE, DRIVE_CDROM
        if matches!(kind, 2 | 4 | 5) {
            return Some("glint.exe is on a removable or network drive, so folders could stop opening. Move Glint to this PC's own drive first.");
        }
    }
    None
}

/// The long form of a path that may be written in 8.3 short names ("C:\Users\JOHNSM~1\..."), as
/// Windows' own TEMP often is. The path itself when Windows can't say.
fn long_path(path: &str) -> String {
    let w = wide(path);
    let mut buf = vec![0u16; 1024];
    // SAFETY: `w` is NUL-terminated and `buf` a writable buffer of the size given.
    let n = unsafe { GetLongPathNameW(PCWSTR(w.as_ptr()), Some(&mut buf)) } as usize;
    if n == 0 || n >= buf.len() {
        return path.to_string();
    }
    String::from_utf16_lossy(&buf[..n])
}

/// This glint.exe, as a plain full path.
pub fn this_exe() -> Option<String> {
    let p = std::env::current_exe().ok()?.to_string_lossy().into_owned();
    Some(p.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(p))
}

// ---------------------------------------------------------------- what's on

/// The exe that folders and drives open in, when Glint's verb is the default ("Open folders and
/// drives in Glint" is on for some glint.exe).
pub fn folders_exe(places: &Places) -> Option<String> {
    let shell = Key::open(&places.shell("Directory"), false)?;
    if !shell.string("").is_some_and(|v| v.eq_ignore_ascii_case(VERB)) {
        return None;
    }
    let cmd = Key::open(&format!(r"{}\{VERB}\command", places.shell("Directory")), false)?;
    exe_of(&cmd.string("")?)
}

/// The exe Win+E opens, when it's Glint's (marked).
pub fn win_e_exe(places: &Places) -> Option<String> {
    let cmd = Key::open(&places.win_e(), false)?;
    if !cmd.has(MARKER) {
        return None;
    }
    exe_of(&cmd.string("")?)
}

/// The exe that starts at sign-in ("Start with Windows"), if any.
pub fn run_exe(places: &Places) -> Option<String> {
    exe_of(&Key::open(&places.run, false)?.string(RUN_VALUE)?)
}

/// "Open folders and drives in Glint" is on, for this glint.exe.
pub fn folders_on(places: &Places, exe: &str) -> bool {
    folders_exe(places).is_some_and(|e| same_exe(&e, exe))
}

/// "Win+E opens Glint" is on, for this glint.exe.
pub fn win_e_on(places: &Places, exe: &str) -> bool {
    win_e_exe(places).is_some_and(|e| same_exe(&e, exe))
}

// ---------------------------------------------------------------- changes

/// Turns "Open folders and drives in Glint" on (for `exe`) or off.
pub fn set_folders(places: &Places, on: bool, exe: &str) -> Result<(), String> {
    let backup = Key::create(&places.backup)?;
    let result = folders(places, on, exe, &backup);
    drop(backup);
    if !on {
        delete_empty_keys(&places.backup, "Software");
    }
    notify_shell(places);
    result
}

fn folders(places: &Places, on: bool, exe: &str, backup: &Key) -> Result<(), String> {
    for kind in FOLDER_TYPES {
        let shell_path = places.shell(kind);
        if on {
            let shell = Key::create(&shell_path)?;
            let previous = shell.string("");
            if !previous.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(VERB)) {
                backup.set_string(kind, previous.as_deref().unwrap_or(NONE), &places.backup)?;
            }
            let verb_path = format!(r"{shell_path}\{VERB}");
            let verb = Key::create(&verb_path)?;
            verb.set_string("", "Open in Glint", &verb_path)?;
            verb.set_string("Icon", &format!("\"{exe}\",0"), &verb_path)?;
            let cmd_path = format!(r"{verb_path}\command");
            Key::create(&cmd_path)?.set_string("", &folder_command(exe), &cmd_path)?;
            // Last: only once the command is in place does it become what a double-click does.
            shell.set_string("", VERB, &shell_path)?;
        } else {
            if let Some(shell) = Key::open(&shell_path, true) {
                if shell.string("").is_some_and(|v| v.eq_ignore_ascii_case(VERB)) {
                    // First: double-clicking goes back to what it was before the verb goes.
                    match backup.string(kind).filter(|p| p != NONE && !p.is_empty()) {
                        Some(previous) => shell.set_string("", &previous, &shell_path)?,
                        None => shell.delete_value("", &shell_path)?,
                    }
                }
                shell.delete_tree(VERB, &shell_path)?;
            }
            backup.delete_value(kind, &places.backup)?;
            delete_empty_keys(&shell_path, &places.classes);
        }
    }
    Ok(())
}

/// Turns "Win+E opens Glint" (in Windows' registry) on (for `exe`) or off.
pub fn set_win_e(places: &Places, on: bool, exe: &str) -> Result<(), String> {
    let backup = Key::create(&places.backup)?;
    let result = win_e(places, on, exe, &backup);
    drop(backup);
    if !on {
        delete_empty_keys(&places.backup, "Software");
    }
    notify_shell(places);
    result
}

fn win_e(places: &Places, on: bool, exe: &str, backup: &Key) -> Result<(), String> {
    let path = places.win_e();
    if on {
        // Another app's Win+E (File Pilot, say) is remembered, so turning Glint's off gives it back.
        if let Some(existing) = Key::open(&path, false) {
            if !existing.has(MARKER) {
                backup.set_string("WinE.Command", existing.string("").as_deref().unwrap_or(NONE), &places.backup)?;
                backup.set_string("WinE.DelegateExecute", existing.string("DelegateExecute").as_deref().unwrap_or(NONE), &places.backup)?;
            }
        }
        let cmd = Key::create(&path)?;
        cmd.set_string("", &win_e_command(exe), &path)?;
        // The empty value is needed, or Windows ignores the command.
        cmd.set_string("DelegateExecute", "", &path)?;
        cmd.set_dword(MARKER, 1, &path)?;
    } else {
        let old_command = backup.string("WinE.Command");
        let old_delegate = backup.string("WinE.DelegateExecute");
        if let Some(cmd) = Key::open(&path, true) {
            if cmd.has(MARKER) {
                cmd.delete_value(MARKER, &path)?;
                match old_command.filter(|c| c != NONE) {
                    Some(c) => cmd.set_string("", &c, &path)?,
                    None => cmd.delete_value("", &path)?,
                }
                match old_delegate.filter(|d| d != NONE) {
                    Some(d) => cmd.set_string("DelegateExecute", &d, &path)?,
                    None => cmd.delete_value("DelegateExecute", &path)?,
                }
            }
        }
        backup.delete_value("WinE.Command", &places.backup)?;
        backup.delete_value("WinE.DelegateExecute", &places.backup)?;
        delete_empty_keys(&path, &format!(r"{}\CLSID", places.classes));
    }
    Ok(())
}

/// Turns "Start with Windows" on (for `exe`, started hidden in the background) or off.
pub fn set_start_with_windows(places: &Places, on: bool, exe: &str) -> Result<(), String> {
    if on {
        Key::create(&places.run)?.set_string(RUN_VALUE, &run_command(exe), &places.run)
    } else {
        match Key::open(&places.run, true) {
            Some(run) => run.delete_value(RUN_VALUE, &places.run),
            None => Ok(()),
        }
    }
}

/// Folders, drives and Win+E go back to File Explorer (whichever Glint they pointed at). Used by
/// `glint.exe --restore-explorer`, which needs no settings at all.
pub fn restore_explorer(places: &Places, exe: &str) -> Result<(), String> {
    let a = set_win_e(places, false, exe);
    let b = set_folders(places, false, exe);
    a.and(b)
}

/// glint.exe was moved (or copied elsewhere and the old copy deleted): what points at the old place,
/// which is gone, is pointed at `exe`. Nothing else is touched - a setting that's off stays off, and
/// one pointing at another glint.exe that still exists is left to that one.
pub fn repair_if_moved(places: &Places, exe: &str) {
    let gone = |old: &str| !same_exe(old, exe) && !std::path::Path::new(old).exists();
    if !std::path::Path::new(exe).exists() {
        return;
    }
    let mut fixed = Vec::new();
    if folders_exe(places).is_some_and(|old| gone(&old)) && set_folders(places, true, exe).is_ok() {
        fixed.push("folders and drives");
    }
    if win_e_exe(places).is_some_and(|old| gone(&old)) && set_win_e(places, true, exe).is_ok() {
        fixed.push("Win+E");
    }
    if run_exe(places).is_some_and(|old| gone(&old)) && set_start_with_windows(places, true, exe).is_ok() {
        fixed.push("Start with Windows");
    }
    if !fixed.is_empty() {
        crate::log::line(&format!("glint.exe was moved: {} now point at {exe}", fixed.join(", ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh test key per test (they run side by side), removed at the end.
    struct TestRoot(String, Places);

    impl TestRoot {
        fn new(name: &str) -> TestRoot {
            let root = format!(r"Software\GlintTest\{name}-{}", std::process::id());
            if let Some(k) = Key::open(r"Software", true) {
                let _ = k.delete_tree(&root["Software\\".len()..], "Software");
            }
            let places = Places::under(&root);
            TestRoot(root, places)
        }
        fn key(&self, path: &str) -> Option<Key> {
            Key::open(&format!(r"{}\{path}", self.0), false)
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            if let Some(k) = Key::open("Software", true) {
                let _ = k.delete_tree(&self.0["Software\\".len()..], "Software");
            }
        }
    }

    const EXE: &str = r"C:\Tools\Glint\glint.exe";

    #[test]
    fn commands_and_paths() {
        assert_eq!(folder_command(EXE), r#""C:\Tools\Glint\glint.exe" "%1""#);
        assert_eq!(exe_of(&folder_command(EXE)).as_deref(), Some(EXE));
        assert_eq!(exe_of(&run_command(r"C:\a b\glint.exe")).as_deref(), Some(r"C:\a b\glint.exe"));
        assert_eq!(exe_of("glint.exe %1"), None);
        assert_eq!(exe_of("\"\""), None);
        assert!(same_exe(r"c:\tools\GLINT\Glint.EXE", EXE));
        assert!(same_exe(r"\\?\C:\Tools\Glint\glint.exe", EXE));
        assert!(!same_exe(r"D:\glint.exe", EXE));
    }

    #[test]
    fn folders_on_and_off_puts_back_what_was_there() {
        let t = TestRoot::new("folders");
        let p = &t.1;
        // Another program had made "explore" the default for Directory; Drive had nothing.
        Key::create(&p.shell("Directory")).unwrap().set_string("", "explore", "").unwrap();
        assert!(!folders_on(p, EXE));
        set_folders(p, true, EXE).unwrap();
        assert!(folders_on(p, EXE));
        assert!(!folders_on(p, r"D:\other\glint.exe"), "on only for the exe it points at");
        let cmd = t.key(r"Classes\Drive\shell\glint\command").unwrap();
        assert_eq!(cmd.string("").as_deref(), Some(r#""C:\Tools\Glint\glint.exe" "%1""#));
        assert_eq!(t.key(r"Backup").unwrap().string("Directory").as_deref(), Some("explore"));
        // Turning it on again (another exe) keeps the first backup.
        set_folders(p, true, r"D:\New\glint.exe").unwrap();
        assert_eq!(t.key(r"Backup").unwrap().string("Directory").as_deref(), Some("explore"));
        set_folders(p, false, EXE).unwrap();
        assert!(folders_exe(p).is_none());
        assert_eq!(t.key(r"Classes\Directory\shell").unwrap().string("").as_deref(), Some("explore"));
        assert!(t.key(r"Classes\Directory\shell\glint").is_none());
        assert!(t.key(r"Classes\Drive").is_none(), "empty keys are tidied away");
        assert!(t.key("Backup").is_none());
    }

    #[test]
    fn win_e_on_and_off_gives_another_apps_back() {
        let t = TestRoot::new("wine");
        let p = &t.1;
        let other = Key::create(&p.win_e()).unwrap();
        other.set_string("", r#""C:\Pilot\FPilot.exe""#, "").unwrap();
        drop(other);
        set_win_e(p, true, EXE).unwrap();
        assert!(win_e_on(p, EXE));
        let cmd = Key::open(&p.win_e(), false).unwrap();
        assert_eq!(cmd.string("DelegateExecute").as_deref(), Some(""));
        drop(cmd);
        set_win_e(p, false, EXE).unwrap();
        assert!(!win_e_on(p, EXE));
        let cmd = Key::open(&p.win_e(), false).unwrap();
        assert_eq!(cmd.string("").as_deref(), Some(r#""C:\Pilot\FPilot.exe""#));
        assert!(!cmd.has("DelegateExecute") && !cmd.has(MARKER));
    }

    #[test]
    fn restore_explorer_and_start_with_windows() {
        let t = TestRoot::new("restore");
        let p = &t.1;
        set_folders(p, true, EXE).unwrap();
        set_win_e(p, true, EXE).unwrap();
        set_start_with_windows(p, true, EXE).unwrap();
        assert_eq!(run_exe(p).as_deref(), Some(EXE));
        assert_eq!(t.key("Run").unwrap().string(RUN_VALUE).as_deref(), Some(r#""C:\Tools\Glint\glint.exe" --background"#));
        // Restoring works for whichever exe it pointed at, and leaves nothing behind.
        restore_explorer(p, r"D:\Elsewhere\glint.exe").unwrap();
        assert!(folders_exe(p).is_none() && win_e_exe(p).is_none());
        assert!(t.key(r"Classes\Directory").is_none() && t.key(r"Classes\Drive").is_none() && t.key("Backup").is_none());
        assert!(t.key(r"Classes\CLSID").is_some_and(|k| k.is_empty()), "CLSID itself stays (in the real registry it's never empty)");
        assert!(run_exe(p).is_some(), "Start with Windows is a separate setting");
        set_start_with_windows(p, false, EXE).unwrap();
        assert!(run_exe(p).is_none());
        set_start_with_windows(p, false, EXE).unwrap(); // off twice is fine
    }

    #[test]
    fn never_pointed_at_an_exe_that_may_soon_be_gone() {
        let temp = std::env::temp_dir().to_string_lossy().trim_end_matches('\\').to_string();
        assert!(risky_place(&format!(r"{temp}\Temp1_glint.zip\glint.exe")).is_some(), "unpacked from a zip file");
        assert!(risky_place(&format!(r"{}\x\glint.exe", temp.to_uppercase())).is_some(), "any capitals");
        assert!(risky_place(r"\\nas\tools\glint.exe").is_some(), "a network share");
        assert!(risky_place(EXE).is_none(), "a folder of its own on this PC's drive");
        assert!(risky_place(&format!(r"{temp}x\glint.exe")).is_none(), "only inside the temporary folder itself");
    }

    #[test]
    fn moved_exe_is_repaired_but_nothing_else() {
        let t = TestRoot::new("repair");
        let p = &t.1;
        let me = this_exe().unwrap();
        // Nothing on: nothing is turned on.
        repair_if_moved(p, &me);
        assert!(folders_exe(p).is_none() && win_e_exe(p).is_none() && run_exe(p).is_none());
        // On, for an exe that no longer exists: pointed at this one.
        set_folders(p, true, r"C:\Gone\Away\glint.exe").unwrap();
        set_start_with_windows(p, true, r"C:\Gone\Away\glint.exe").unwrap();
        repair_if_moved(p, &me);
        assert!(folders_on(p, &me));
        assert!(run_exe(p).is_some_and(|e| same_exe(&e, &me)));
        assert!(win_e_exe(p).is_none(), "Win+E was off and stays off");
        // On for another exe that exists: left alone.
        set_folders(p, true, r"C:\windows\system32\notepad.exe").unwrap();
        repair_if_moved(p, &me);
        assert!(folders_on(p, r"C:\windows\system32\notepad.exe"));
        restore_explorer(p, &me).unwrap();
    }
}
