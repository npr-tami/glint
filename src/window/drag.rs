//! The window's side of drag and drop (dragdrop.rs is Windows' side): where something dragged over
//! the window would go and with what effect, the highlight on that place, the list scrolling while
//! the drag is near its top or bottom edge, switching to a tab the drag rests on, the drop itself -
//! and starting a drag out of the file list.
//!
//! Each step of a drag over the window comes as a WM_APP_DRAG message carrying a `DragMsg`, so it
//! is handled like any other message: never while another one is (BUSY), never with freed state.
//!
//! What is under the mouse is asked from the parts (`Component::drop_target`): a folder's row or
//! the list's empty space (the folder on screen), a tab, a step of the path, a place in the
//! sidebar. The place is highlighted (`Component::drop_hover`) when something can be dropped there.
//! A drop goes only where the highlight was: if what is under the mouse changed between the last
//! step and the drop (the folder was read again and the rows moved), nothing is dropped and the
//! status bar says so.

use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Ole::DROPEFFECT_LINK;
use windows::Win32::System::SystemServices::MK_SHIFT;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use super::{still_alive, Deferred, Part, Window, BUSY};
use crate::dragdrop::{self, Action, Dest, DragMsg, Phase, Plan};
use crate::ui::{Command, Component, DragOut, DropTarget, Effect, Point};

/// The timer that scrolls the list while a drag is near its top or bottom edge.
pub(super) const DRAG_SCROLL_TIMER: usize = 60;
/// The timer that switches to the tab a drag rests on.
pub(super) const TAB_SWITCH_TIMER: usize = 61;
/// How long a drag rests on a tab before Glint switches to it.
const TAB_SWITCH_MS: u32 = 600;
/// How often the list moves while a drag is at its edge (about 60 times a second).
const SCROLL_TICK_MS: u32 = 16;

/// A drag over the window, as far as the window is concerned.
#[derive(Default)]
pub(super) struct DragState {
    /// The dragged files (None: nothing is being dragged over the window).
    files: Option<Vec<String>>,
    /// What the program the drag came from allows (DROPEFFECT flags).
    allowed: u32,
    /// The drag's key state (MK_ flags) and where it is (DIPs), as of the last step.
    keys: u32,
    at: Point,
    /// What is highlighted, in which part.
    shown: Option<(Part, DropTarget)>,
    /// Where a drop on the highlighted place goes, as of the last step (the folder whose name
    /// Windows' drag picture shows). A tab is a number, so the folder behind it can change while
    /// the mouse rests on it (a folder from another program opens a tab before it): the drop goes
    /// only where this still says.
    shown_dest: Option<Dest>,
    /// The tab the drag rests on (TAB_SWITCH_TIMER runs).
    waiting_tab: Option<usize>,
    /// The list scrolls (DRAG_SCROLL_TIMER runs); when it last moved.
    scrolling: Option<Instant>,
    /// Whether any dropped item is a folder (asked once per drag, for "Pinned").
    has_folder: Option<bool>,
    /// A drag out of the list is under way: Glint's own drag loop is running.
    out: bool,
}

impl Window {
    /// WM_APP_DRAG: one step of a drag over the window (see dragdrop.rs).
    pub(super) fn drag_message(&mut self, lparam: LPARAM) -> LRESULT {
        // SAFETY: dragdrop.rs sends a pointer to a DragMsg that lives across this SendMessage and
        // isn't used by anything else meanwhile.
        let Some(msg) = (unsafe { (lparam.0 as *mut DragMsg).as_mut() }) else { return LRESULT(0) };
        msg.handled = true;
        match msg.phase {
            Phase::Enter => {
                self.drag_ended();
                self.drag.files = Some(std::mem::take(&mut msg.files));
                self.drag.allowed = msg.allowed;
                self.drag_moved(msg);
            }
            Phase::Over => self.drag_moved(msg),
            Phase::Leave => self.drag_ended(),
            Phase::Drop => self.drag_dropped(msg),
            Phase::Perform => {
                if let (Some(plan), Some(action)) = (msg.plan.take(), msg.action) {
                    msg.started = self.app.drop_files(&plan, action, msg.signal.take());
                    // Dropped on the Recycle Bin with "Ask before deleting": Enter / Esc answer.
                    self.keyboard_to_question();
                    self.invalidate();
                }
            }
        }
        LRESULT(1)
    }

    /// A folder was read again while something is dragged over the window (the folder watch saw
    /// a change): the rows may have moved under the mouse, so where a drop would go is worked
    /// out again (and the highlight with it).
    pub(super) fn drag_listing_changed(&mut self) {
        if self.drag.files.is_some() {
            let _ = self.drag_update();
        }
    }

    /// The drag moved (or a key changed): what dropping there would do.
    fn drag_moved(&mut self, msg: &mut DragMsg) {
        if self.drag.files.is_none() {
            return;
        }
        self.drag.at = self.screen_dips(msg.at);
        self.drag.keys = msg.keys;
        let (effect, label) = self.drag_update();
        msg.effect = effect;
        msg.label = label;
    }

    /// Where the drag would go now: highlights it and starts or stops the tab and scroll timers.
    /// Returns the effect (DROPEFFECT) and the place's name for Windows' drag picture.
    fn drag_update(&mut self) -> (u32, Option<String>) {
        let mut answer = (0, None);
        let mut show = None;
        if let Some((part, target)) = self.drop_spot(self.drag.at) {
            answer = self.drop_effect(&target, self.drag.keys);
            // A tab lights up even when nothing can go into its folder: it's switched to anyway.
            if answer.0 != 0 || matches!(target, DropTarget::Tab(_)) {
                show = Some((part, target));
            }
        }
        self.drag.shown_dest = show.as_ref().and_then(|(_, t)| self.dest_of(t));
        self.show_drop(show);
        self.wait_on_tab();
        self.scroll_near_edge();
        answer
    }

    /// The part under `p` and where it says a drop would go.
    fn drop_spot(&self, p: Point) -> Option<(Part, DropTarget)> {
        // Nothing is dropped while a question is open (it gets all input).
        if self.drag.files.is_none() || self.app.question.is_some() {
            return None;
        }
        let part = self.part_at(p)?;
        let target = match part {
            Part::Title => self.titlebar.drop_target(p, &self.app),
            Part::Toolbar => self.toolbar.drop_target(p, &self.app),
            Part::Sidebar => self.sidebar.drop_target(p, &self.app),
            Part::List => self.list.drop_target(p, &self.app),
            Part::Status => self.status.drop_target(p, &self.app),
            Part::Side => self.side.drop_target(p, &self.app),
        }?;
        Some((part, target))
    }

    /// Where a drop onto `target` really goes. None: nowhere (This PC holds only drives).
    fn dest_of(&self, target: &DropTarget) -> Option<Dest> {
        let folder = |path: &str| {
            if crate::recycle::is_bin(path) {
                Some(Dest::RecycleBin)
            } else {
                (!path.is_empty()).then(|| Dest::Folder(path.to_string()))
            }
        };
        match target {
            DropTarget::Folder(path) => folder(path),
            DropTarget::Tab(i) => folder(&self.app.tabs.get(*i)?.path),
            DropTarget::RecycleBin => Some(Dest::RecycleBin),
            DropTarget::Pin => Some(Dest::Pin),
        }
    }

    /// What dropping onto `target` with `keys` does (DROPEFFECT), and the place's name.
    fn drop_effect(&mut self, target: &DropTarget, keys: u32) -> (u32, Option<String>) {
        let Some(dest) = self.dest_of(target) else { return (0, None) };
        let Some(files) = self.drag.files.as_ref() else { return (0, None) };
        let allowed = self.drag.allowed;
        let effect = match &dest {
            Dest::Folder(path) => dragdrop::folder_effect(files, path, keys, allowed).map_or(0, dragdrop::effect_of),
            Dest::RecycleBin => dragdrop::bin_effect(files, allowed),
            Dest::Pin if self.has_folder() => DROPEFFECT_LINK.0 & allowed,
            Dest::Pin => 0,
        };
        let label = (effect != 0).then(|| match &dest {
            Dest::Folder(path) => self.app.display_name(path),
            Dest::RecycleBin => "Recycle Bin".to_string(),
            Dest::Pin => "Pinned".to_string(),
        });
        (effect, label)
    }

    /// Whether a folder is among the dragged items (only folders can be pinned). Asked once per
    /// drag, and only of the first 64 items, so a huge drag of files costs nothing.
    fn has_folder(&mut self) -> bool {
        if let Some(h) = self.drag.has_folder {
            return h;
        }
        let h = self.drag.files.as_ref().is_some_and(|f| f.iter().take(64).any(|p| std::fs::metadata(p).is_ok_and(|m| m.is_dir())));
        self.drag.has_folder = Some(h);
        h
    }

    /// Moves the highlight to `new` (or takes it away).
    fn show_drop(&mut self, new: Option<(Part, DropTarget)>) {
        if self.drag.shown == new {
            return;
        }
        if let Some((old, _)) = self.drag.shown.take() {
            let e = self.with_part(old, |c, _| c.drop_hover(None));
            self.apply(old, e);
        }
        if let Some((part, target)) = &new {
            let e = self.with_part(*part, |c, _| c.drop_hover(Some(target)));
            self.apply(*part, e);
        }
        self.drag.shown = new;
    }

    /// Resting on another tab starts the timer that switches to it.
    fn wait_on_tab(&mut self) {
        let on = match &self.drag.shown {
            Some((Part::Title, DropTarget::Tab(i))) if *i != self.app.active => Some(*i),
            _ => None,
        };
        if on == self.drag.waiting_tab {
            return;
        }
        self.drag.waiting_tab = on;
        // SAFETY: timers on our own window.
        unsafe {
            if on.is_some() {
                SetTimer(Some(self.hwnd), TAB_SWITCH_TIMER, TAB_SWITCH_MS, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TAB_SWITCH_TIMER);
            }
        }
    }

    /// Near the list's top or bottom edge the list scrolls (a timer moves it); elsewhere it stops.
    fn scroll_near_edge(&mut self) {
        let near = self.drag.files.is_some() && self.list.drag_scroll_speed(self.drag.at, &self.app) != 0.0;
        if near == self.drag.scrolling.is_some() {
            return;
        }
        // SAFETY: timers on our own window.
        unsafe {
            if near {
                self.drag.scrolling = Some(Instant::now());
                SetTimer(Some(self.hwnd), DRAG_SCROLL_TIMER, SCROLL_TICK_MS, None);
            } else {
                self.drag.scrolling = None;
                let _ = KillTimer(Some(self.hwnd), DRAG_SCROLL_TIMER);
            }
        }
    }

    /// DRAG_SCROLL_TIMER and TAB_SWITCH_TIMER.
    pub(super) fn drag_timer(&mut self, id: usize) {
        match id {
            TAB_SWITCH_TIMER => {
                // SAFETY: stops our own one-off timer.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TAB_SWITCH_TIMER);
                }
                let Some(i) = self.drag.waiting_tab.take() else { return };
                let still_there = matches!(&self.drag.shown, Some((Part::Title, DropTarget::Tab(t))) if *t == i);
                if self.drag.files.is_some() && still_there {
                    self.apply(Part::Title, Effect::Command(Command::SwitchTab(i)));
                    let _ = self.drag_update();
                }
            }
            DRAG_SCROLL_TIMER => {
                let speed = if self.drag.files.is_some() { self.list.drag_scroll_speed(self.drag.at, &self.app) } else { 0.0 };
                let (Some(since), true) = (self.drag.scrolling, speed != 0.0) else {
                    self.drag.scrolling = None;
                    // SAFETY: stops our own timer.
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), DRAG_SCROLL_TIMER);
                    }
                    return;
                };
                let now = Instant::now();
                // Steady speed whatever the timer's real pace (Windows rounds timers to ~16 ms).
                let dt = now.duration_since(since).as_secs_f32().min(0.1);
                self.drag.scrolling = Some(now);
                self.list.drag_scroll(speed * dt, &mut self.app);
                self.invalidate();
                // Other rows are under the mouse now.
                let _ = self.drag_update();
            }
            _ => {}
        }
    }

    /// The drop: decides what it does (the plan; dragdrop.rs shows the right-button menu, then
    /// asks for it to be carried out with Phase::Perform).
    fn drag_dropped(&mut self, msg: &mut DragMsg) {
        if self.drag.files.is_none() {
            return; // no Enter came first (or the window was busy then): nothing was shown
        }
        self.drag.at = self.screen_dips(msg.at);
        // The files as the drag holds them now (read again at the drop), else as at the start.
        if !msg.files.is_empty() {
            self.drag.files = Some(std::mem::take(&mut msg.files));
        }
        let target = self.drop_spot(self.drag.at).map(|(_, t)| t);
        let shown = self.drag.shown.as_ref().map(|(_, t)| t.clone());
        // The same place, and still the same folder behind it (see `shown_dest`).
        let same = |t: &DropTarget, s: &DropTarget| t == s && self.dest_of(t) == self.drag.shown_dest;
        let decided = match (&target, &shown) {
            (Some(t), Some(s)) if same(t, s) => self.plan_for(t, msg.keys),
            (_, Some(_)) => {
                self.app.say("The list changed, so nothing was dropped. Try again.");
                None
            }
            _ => None,
        };
        self.drag_ended();
        if let Some((plan, effect)) = decided {
            msg.effect = effect;
            msg.plan = Some(plan);
        }
        self.invalidate();
    }

    /// What dropping onto `target` with `keys` does, and its effect (DROPEFFECT).
    fn plan_for(&mut self, target: &DropTarget, keys: u32) -> Option<(Plan, u32)> {
        let dest = self.dest_of(target)?;
        let (effect, _) = self.drop_effect(target, keys);
        if effect == 0 {
            return None;
        }
        let files = self.drag.files.clone()?;
        let allowed = self.drag.allowed;
        let (action, choices) = match &dest {
            Dest::Folder(path) => {
                let how = dragdrop::folder_effect(&files, path, keys, allowed)?;
                (Action::Transfer(how), dragdrop::folder_choices(&files, path, allowed))
            }
            Dest::RecycleBin => (Action::Recycle { permanent: keys & MK_SHIFT.0 != 0 }, Vec::new()),
            Dest::Pin => (Action::Pin, Vec::new()),
        };
        // The tab whose folder it is gets the new items selected.
        let tab_id = match target {
            DropTarget::Tab(i) => self.app.tabs.get(*i)?.id,
            _ => self.app.tab().id,
        };
        Some((Plan { dest, files, action, choices, tab_id, from_glint: self.drag.out }, effect))
    }

    /// The drag left the window, was dropped or was called off: highlight and timers go.
    fn drag_ended(&mut self) {
        self.show_drop(None);
        self.drag.shown_dest = None;
        self.drag.files = None;
        self.drag.has_folder = None;
        self.drag.waiting_tab = None;
        self.drag.scrolling = None;
        // SAFETY: stops our own timers (harmless when they don't run).
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TAB_SWITCH_TIMER);
            let _ = KillTimer(Some(self.hwnd), DRAG_SCROLL_TIMER);
        }
    }

    /// A point in screen pixels as DIPs in the window.
    fn screen_dips(&self, at: POINT) -> Point {
        let mut pt = at;
        // SAFETY: converts in place for our own window.
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        let s = self.scale();
        Point::new(pt.x as f32 / s, pt.y as f32 / s)
    }

    /// The list asks to drag its selected items out. Windows' drag loop runs after this message
    /// (it has a message loop of its own) and takes the mouse: the part loses it now, and hover
    /// highlights go, since no mouse moves come while it runs.
    pub(super) fn start_drag_out(&mut self, d: DragOut) {
        self.capture = None;
        // SAFETY: releasing our own capture (the WM_CAPTURECHANGED it sends finds us busy).
        unsafe {
            let _ = ReleaseCapture();
        }
        self.mouse_leave();
        self.deferred.push(Deferred::DragOut(d));
    }
}

/// Runs a drag out of the list (Deferred::DragOut), with no borrow of the window held: Windows'
/// drag loop handles messages meanwhile, our own drops among them.
pub(super) fn run_drag_out(hwnd: HWND, ptr: *mut Window, d: DragOut) {
    BUSY.with(|b| b.set(true));
    // SAFETY: called from run_deferred with the window alive and no other borrow; BUSY guards it.
    unsafe { (*ptr).drag.out = true };
    BUSY.with(|b| b.set(false));
    let result = dragdrop::drag_out(hwnd, &d.folder, &d.names, d.right);
    if !still_alive(hwnd, ptr) {
        return;
    }
    BUSY.with(|b| b.set(true));
    // SAFETY: as above; the window is still alive.
    unsafe {
        let w = &mut *ptr;
        w.drag.out = false;
        if let Err(why) = result {
            w.app.say(why);
        }
        w.invalidate();
        w.note_changes();
    }
    BUSY.with(|b| b.set(false));
}
