//! Win+E on newer Windows 11 (25H2, build 26200): Explorer no longer runs the registry key that
//! "Win+E opens Glint" sets (registry.rs) - it first hands Win+E to File Explorer's own "direct
//! launch" window, whose handle it keeps in a per-session registry value:
//!
//!   HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\<session>\
//!     DirectLaunchHandlers, value "microsoft.windows.explorer" (a window handle)
//!
//! Explorer reads that value on every Win+E. When it's missing (or its window is gone), Explorer
//! falls back to the old way: it runs the {52205fd8} "opennewwindow" key - Glint's. So taking that
//! value away gives Win+E back to Glint: no admin, no keyboard hook, nothing running while Glint is
//! quit, and it works while an admin app (a game) is in front, because Explorer itself gets Win+E.
//! (Read in explorer.exe 10.0.26100.6713: 0xdf2b2 tries the direct launch, 0xdf2d9 falls back to
//! 0x70c44 = ShellExecuteEx "opennewwindow" on shell:::{52205fd8-...}. Proven with real presses,
//! 17 Nov 2026: Glint opened on the desktop and with an admin game in front, no File Explorer.)
//!
//! Explorer writes the value again each time it starts (sign-in, an Explorer restart), so whoever
//! of Glint runs then takes it away again: the main Glint at start and while it runs, and the
//! background part ("Temp Win+E fix", catcher.rs), which watches the value. Only Explorer's own
//! window is taken away: any other program's value is left alone. The taskbar's File Explorer icon
//! keeps opening File Explorer (proven the same day).

use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TokenSessionId, TOKEN_QUERY};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_QUERY_VALUE,
    KEY_SET_VALUE, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_DWORD,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};

use crate::folder::wide;
use crate::log;

/// The value Explorer reads on Win+E.
const VALUE: &str = "microsoft.windows.explorer";

/// The key that holds it, for this Windows session.
fn key_path() -> Option<String> {
    let mut token = HANDLE::default();
    let mut session = 0u32;
    let mut size = 0u32;
    // SAFETY: our own process token, read into a u32 of the right size, closed once.
    let read = unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_ok() && {
            let ok = GetTokenInformation(token, TokenSessionId, Some(&mut session as *mut u32 as *mut _), 4, &mut size).is_ok();
            let _ = CloseHandle(token);
            ok
        }
    };
    read.then(|| format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{session}\DirectLaunchHandlers"))
}

/// Opens the key (it may not be there yet, early at sign-in).
fn open(path: &str, access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Option<HKEY> {
    let p = wide(path);
    let mut key = HKEY::default();
    // SAFETY: NUL-terminated path; `key` is closed by the caller.
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(p.as_ptr()), None, access, &mut key) }.is_ok().then_some(key)
}

/// The window handle in the value, if there is one.
fn handler(key: HKEY) -> Option<u32> {
    let name = wide(VALUE);
    let (mut v, mut size) = (0u32, 4u32);
    // SAFETY: a DWORD read into a u32.
    unsafe { RegGetValueW(key, PCWSTR::null(), PCWSTR(name.as_ptr()), RRF_RT_REG_DWORD, None, Some(&mut v as *mut u32 as *mut _), Some(&mut size)) }
        .is_ok()
        .then_some(v)
}

/// Whether `h` is a live window of explorer.exe.
fn is_explorers(h: u32) -> bool {
    let hwnd = HWND(h as usize as *mut _);
    let mut pid = 0u32;
    // SAFETY: window lookups by handle (a stale one just fails); the process handle is closed once.
    unsafe {
        if h == 0 || !IsWindow(Some(hwnd)).as_bool() || GetWindowThreadProcessId(hwnd, Some(&mut pid)) == 0 {
            return false;
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        ok && String::from_utf16_lossy(&buf[..len as usize]).rsplit('\\').next().is_some_and(|n| n.eq_ignore_ascii_case("explorer.exe"))
    }
}

/// Takes Explorer's Win+E window out of the value, so Win+E runs Glint's registry key. Does nothing
/// if it isn't there, or if another program put its own window there. Whether it took it away.
pub fn release() -> bool {
    let Some(path) = key_path() else { return false };
    let Some(key) = open(&path, KEY_QUERY_VALUE | KEY_SET_VALUE) else { return false };
    let taken = handler(key).is_some_and(is_explorers) && {
        let name = wide(VALUE);
        // SAFETY: deletes one value of a key we opened for writing.
        unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) }.is_ok()
    };
    // SAFETY: closes the key opened above, once.
    unsafe {
        let _ = RegCloseKey(key);
    }
    if taken {
        log::line("Win+E: took File Explorer's direct launch away, Win+E opens Glint again");
    }
    taken
}

/// Whether this process watches already.
static WATCHING: AtomicBool = AtomicBool::new(false);

/// Takes it away now and each time Explorer puts it back, on a thread of its own, for as long as
/// this process runs and `wanted()` says so (checked each time something changes). Once per process.
pub fn keep_released(wanted: fn() -> bool) {
    if WATCHING.swap(true, Ordering::SeqCst) {
        return;
    }
    let started = crate::shell::background("Win+E release", move || {
        loop {
            if !wanted() {
                break;
            }
            release();
            let Some(path) = key_path() else { break };
            match open(&path, KEY_NOTIFY) {
                Some(key) => {
                    // SAFETY: waits on a key we opened, until a value or subkey changes (or the
                    // key goes, at sign-out); closed once afterwards.
                    unsafe {
                        let _ = RegNotifyChangeKeyValue(key, false, REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME, None, false);
                        let _ = RegCloseKey(key);
                    }
                    // Explorer may write the value a moment after making the key.
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
                // Not there (yet): look again shortly.
                None => std::thread::sleep(std::time::Duration::from_secs(2)),
            }
        }
        WATCHING.store(false, Ordering::SeqCst);
    });
    if !started {
        WATCHING.store(false, Ordering::SeqCst);
    }
}
