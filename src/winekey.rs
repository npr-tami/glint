//! Win+E opens Glint - caught by the running Glint itself (also while hidden in the background), so
//! no new glint.exe has to start. Only while "Win+E opens Glint" is on (life.rs, registry.rs: the
//! registry part makes Windows start glint.exe for Win+E when Glint isn't running).
//!
//! First Glint asks Windows for the hotkey (RegisterHotKey on the door window). On Windows 10 and 11
//! that normally fails, because Explorer owns Win+E. Then a low-level keyboard hook watches for Win+E
//! only, on a thread of its own that does nothing else - so typing never waits for a busy Glint
//! window - and tells the door (WM_APP_WIN_E). The hook swallows Win+E so Explorer doesn't open too.
//!
//! GLINT_WIN_E_HOOK=1 skips the hotkey and uses the hook (for testing the hook where the hotkey works).

use std::cell::Cell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Mutex, PoisonError};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MOD_NOREPEAT,
    MOD_WIN, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostMessageW, PostThreadMessageW, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT,
    WM_SYSKEYDOWN,
};

use crate::life::WM_APP_WIN_E;
use crate::log;

/// The hotkey's number (WM_HOTKEY's wparam).
pub const HOTKEY_ID: i32 = 0x5745;
/// The E key.
const VK_E: u32 = 0x45;
/// A key no keyboard has (0xE8 is "unassigned"): pressed and let go while Win is down, it tells
/// Windows that Win was part of a shortcut, so letting go of Win doesn't open the Start menu.
const VK_UNASSIGNED: u16 = 0xE8;

thread_local! {
    /// The door the hotkey is registered on (UI thread).
    static HOTKEY_ON: Cell<Option<HWND>> = const { Cell::new(None) };
}

/// The door the hook tells about Win+E.
static DOOR: AtomicIsize = AtomicIsize::new(0);

/// The keyboard hook's thread, as `start` and `stop` see it.
struct Hook {
    /// Moves on with every start and stop: a thread started for an older number ends by itself
    /// (a stop came before it was under way).
    number: u64,
    /// A hook thread is wanted (started, not stopped, and it didn't fail).
    wanted: bool,
    /// Its thread id once it's under way (0 before), for `stop`.
    thread: u32,
}

static HOOK: Mutex<Hook> = Mutex::new(Hook { number: 0, wanted: false, thread: 0 });

fn hook_state() -> std::sync::MutexGuard<'static, Hook> {
    // panic = abort, so it can't be left poisoned - but never panic over it either way.
    HOOK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether Win+E is being caught.
pub fn running() -> bool {
    HOTKEY_ON.with(Cell::get).is_some() || hook_state().wanted
}

/// Starts catching Win+E for `door` (does nothing if it already does).
pub fn start(door: HWND) {
    if running() {
        return;
    }
    DOOR.store(door.0 as isize, Ordering::SeqCst);
    let force_hook = std::env::var_os("GLINT_WIN_E_HOOK").is_some_and(|v| !v.is_empty());
    // SAFETY: registers a hotkey for our own door window.
    if !force_hook && unsafe { RegisterHotKey(Some(door), HOTKEY_ID, MOD_WIN | MOD_NOREPEAT, VK_E) }.is_ok() {
        HOTKEY_ON.with(|h| h.set(Some(door)));
        log::line("Win+E: caught with a hotkey");
        return;
    }
    // The hook goes on a thread of its own; nothing here waits for it.
    let number = {
        let mut h = hook_state();
        h.number += 1;
        h.wanted = true;
        h.number
    };
    let started = crate::shell::background("Win+E", move || {
        // SAFETY: a low-level hook needs our module and a message loop on this thread, which it
        // has until WM_QUIT; it is removed on the same thread before the thread ends.
        unsafe {
            let module = GetModuleHandleW(None).ok().map(Into::into);
            let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) else {
                // Not allowed here: Win+E keeps starting glint.exe through the registry, as before.
                let mut h = hook_state();
                if h.number == number {
                    h.wanted = false;
                }
                log::line("Win+E: couldn't be caught here; Windows starts glint.exe for it instead");
                return;
            };
            // The thread's message queue exists before `stop` can post WM_QUIT to it.
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let still_wanted = {
                let mut h = hook_state();
                let wanted = h.number == number && h.wanted;
                if wanted {
                    h.thread = GetCurrentThreadId();
                }
                wanted
            };
            if still_wanted {
                log::line("Win+E: caught with a keyboard hook");
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let _ = UnhookWindowsHookEx(hook);
        }
    });
    if !started {
        hook_state().wanted = false;
        log::line("Win+E: couldn't be caught (no thread to be had); Windows starts glint.exe for it instead");
    }
}

/// Stops catching Win+E.
pub fn stop() {
    let was = running();
    if let Some(door) = HOTKEY_ON.with(|h| h.take()) {
        // SAFETY: undoes our own RegisterHotKey.
        unsafe {
            let _ = UnregisterHotKey(Some(door), HOTKEY_ID);
        }
    }
    let id = {
        let mut h = hook_state();
        h.number += 1;
        h.wanted = false;
        std::mem::take(&mut h.thread)
    };
    if was {
        log::line("Win+E: no longer caught");
    }
    if id != 0 {
        // SAFETY: asks the hook thread to end its loop (it removes the hook itself).
        unsafe {
            let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: a plain key-state query.
    unsafe { GetAsyncKeyState(vk.0 as i32) < 0 }
}

/// Whether a key press is Win+E: E typed (not sent by a program) with a Win key down and no other
/// modifier.
fn is_win_e(vk: u32, injected: bool, win: bool, others: bool) -> bool {
    vk == VK_E && !injected && win && !others
}

/// The keyboard hook: Win+E goes to the door and no further; everything else passes untouched.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam.0 == WM_KEYDOWN as usize || wparam.0 == WM_SYSKEYDOWN as usize) {
        // SAFETY: for WH_KEYBOARD_LL, lparam points to a KBDLLHOOKSTRUCT valid during the call.
        if let Some(k) = unsafe { (lparam.0 as *const KBDLLHOOKSTRUCT).as_ref() } {
            let injected = k.flags.0 & LLKHF_INJECTED.0 != 0;
            if k.vkCode == VK_E && !injected {
                let win = down(VK_LWIN) || down(VK_RWIN);
                let others = down(VK_SHIFT) || down(VK_CONTROL) || down(VK_MENU);
                if is_win_e(k.vkCode, injected, win, others) {
                    mask_win_key();
                    let door = HWND(DOOR.load(Ordering::SeqCst) as *mut _);
                    // SAFETY: posting to a window handle is safe even if it's gone.
                    unsafe {
                        let _ = PostMessageW(Some(door), WM_APP_WIN_E, WPARAM(0), LPARAM(0));
                    }
                    return LRESULT(1); // Explorer doesn't see it
                }
            }
        }
    }
    // SAFETY: passes everything else on, as a hook must.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Letting go of Win alone opens the Start menu; a key nobody uses, pressed in between, tells
/// Windows it was a shortcut (the trick other hotkey tools use).
fn mask_win_key() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_UNASSIGNED), dwFlags: flags, ..Default::default() } },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    // SAFETY: two well-formed keyboard inputs.
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_win_e_itself() {
        assert!(is_win_e(VK_E, false, true, false));
        assert!(!is_win_e(VK_E, false, false, false), "E alone is typing");
        assert!(!is_win_e(VK_E, false, true, true), "Win+Shift+E is someone else's");
        assert!(!is_win_e(VK_E, true, true, false), "sent by a program");
        assert!(!is_win_e(0x46, false, true, false), "Win+F");
    }
}
