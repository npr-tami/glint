//! The main window's side of the custom sort order's editor (sortwin.rs): opening it beside the
//! window (closing Settings), keeping it there as the window moves and changes size, and handing it
//! its messages under the window's guard, as for the skin customiser.

use super::*;
use crate::sortwin;

/// Makes the editor, or brings it to the front if it's open (run outside message handling, like
/// `open_settings`).
pub(super) fn open_sort(hwnd: HWND, ptr: *mut Window) {
    // SAFETY: as in open_settings: short borrows under BUSY while the window is alive.
    unsafe {
        if (*ptr).sort_win.as_ref().is_some_and(|s| !IsWindow(Some(s.hwnd())).as_bool()) {
            BUSY.with(|b| b.set(true));
            (*ptr).sort_win = None;
            BUSY.with(|b| b.set(false));
        }
        if (*ptr).sort_win.is_some() {
            BUSY.with(|b| b.set(true));
            if let Some(s) = (*ptr).sort_win.as_mut() {
                let asks = s.reopen();
                (*ptr).sort_asks(asks);
            }
            BUSY.with(|b| b.set(false));
            return;
        }
        // The editor takes over from Settings and the skin customiser (one docked window at a time).
        if let Some(s) = (*ptr).settings_win.as_ref() {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(s.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        if let Some(s) = (*ptr).skin_win.as_ref() {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(Some(s.hwnd()), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        BUSY.with(|b| b.set(true));
        let made = sortwin::create(hwnd, &mut (*ptr).app);
        BUSY.with(|b| b.set(false));
        let Some(win) = made else {
            BUSY.with(|b| b.set(true));
            (*ptr).app.say("Windows couldn't open the sort editor right now");
            (*ptr).invalidate();
            BUSY.with(|b| b.set(false));
            return;
        };
        if !still_alive(hwnd, ptr) {
            return;
        }
        BUSY.with(|b| b.set(true));
        (*ptr).sort_win = Some(win);
        (*ptr).deferred.push(Deferred::Sort(sortwin::Job::Show));
        BUSY.with(|b| b.set(false));
    }
}

/// Runs a job for the editor (a menu, showing, moving, closing) with no borrow of the state held.
pub(super) fn run_sort_job(hwnd: HWND, ptr: *mut Window, job: sortwin::Job) {
    // SAFETY: as above, a short borrow to read where the editor is.
    let Some((swin, scale)) = (unsafe { (*ptr).sort_win.as_ref().map(|s| (s.hwnd(), s.scale())) }) else { return };
    if matches!(job, sortwin::Job::Reopen) {
        // DETACH / DOCK: the editor is made again with its new frame (its state goes with the old
        // window, after using what was typed; the settings say where the new one goes).
        // SAFETY: our own window, with no borrow of the state held.
        unsafe {
            let _ = DestroyWindow(swin);
        }
        if still_alive(hwnd, ptr) {
            open_sort(hwnd, ptr);
        }
        return;
    }
    let done = sortwin::run_job(swin, scale, job, show_menu);
    if done != sortwin::JobDone::Nothing && still_alive(hwnd, ptr) {
        BUSY.with(|b| b.set(true));
        // SAFETY: BUSY guards the only live reference.
        unsafe {
            let Window { sort_win, app, .. } = &mut *ptr;
            if let Some(s) = sort_win.as_mut() {
                let asks = s.job_done(done, app);
                (*ptr).sort_asks(asks);
            }
            (*ptr).note_changes();
        }
        BUSY.with(|b| b.set(false));
    }
}

/// A message of the editor: handled by its state, which the main window keeps, under the same guard
/// as the window's own messages (see `settings_message`).
pub fn sort_message(owner: HWND, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    if owner.is_invalid() {
        return None;
    }
    // SAFETY: the pointer was put there by `create` and is cleared in WM_NCDESTROY (see wndproc).
    let ptr = unsafe { GetWindowLongPtrW(owner, GWLP_USERDATA) } as *mut Window;
    if ptr.is_null() || BUSY.with(|b| b.get()) {
        return None;
    }
    // SAFETY: not BUSY, so no other reference exists; a short borrow.
    if unsafe { (*ptr).sort_win.as_ref().map(|s| s.hwnd()) } != Some(hwnd) {
        return None;
    }
    BUSY.with(|b| b.set(true));
    // SAFETY: BUSY guarantees this is the only live reference; it ends before BUSY is cleared.
    let handled = unsafe { (*ptr).sort_handle(msg, wparam, lparam) };
    // (Its timer uses the rules typed: that's a change to save too.)
    if !QUIET_MESSAGES.contains(&msg) || msg == WM_TIMER {
        // SAFETY: as above.
        unsafe { (*ptr).note_changes() };
    }
    BUSY.with(|b| b.set(false));
    run_deferred(owner, ptr);
    handled
}

impl Window {
    /// One of the editor's messages (see `sort_message`).
    fn sort_handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let Window { sort_win, app, .. } = self;
        let s = sort_win.as_mut()?;
        let (result, asks) = s.handle(msg, wparam, lparam, app);
        self.sort_asks(asks);
        if msg == WM_NCDESTROY {
            // Its last message: the state goes with it.
            self.sort_win = None;
        }
        result
    }

    /// Carries out what the editor asked for.
    fn sort_asks(&mut self, asks: Vec<sortwin::Ask>) {
        for ask in asks {
            match ask {
                sortwin::Ask::Job(job) => self.deferred.push(Deferred::Sort(job)),
                sortwin::Ask::RedrawMain => self.invalidate(),
            }
        }
    }

    /// The window moved or changed size: the editor stays beside it.
    pub(super) fn follow_sort(&mut self) {
        if let Some(job) = self.sort_win.as_ref().and_then(|s| s.dock(self.hwnd)) {
            self.deferred.push(Deferred::Sort(job));
        }
    }
}
