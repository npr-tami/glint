//! The copy queue: big copies and moves run one after another instead of all at once (which only
//! makes each of them slower), in a small list at the bottom right of the window (queuebox.rs) with
//! each one's speed and time left, and Pause and Cancel - like a browser's downloads.
//!
//! A copy or move between drives joins the queue (fileops.rs, `transfer`) and waits on its own
//! thread until it is first; so do Extract and Compress (fileops.rs, archive.rs). A move on the same drive never waits: it's only a rename on the disk.
//! While it runs, the copy engine's progress reports (fileops.rs, the sink) come here: they say how
//! far it is, Pause holds the copy right there, and Cancel stops it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One copy or move in the queue.
pub struct Task {
    pub id: u64,
    /// "Copying 3 items to Photos".
    pub label: String,
    /// How many bytes it copies (0 until known).
    total: AtomicU64,
    /// How far it is, in millionths (from the copy engine's reports).
    done_ppm: AtomicU64,
    paused: AtomicBool,
    cancelled: AtomicBool,
    /// When it started running (None while it waits), and how long it has been paused so far.
    timing: Mutex<(Option<Instant>, Duration, Option<Instant>)>,
}

/// What the list shows of a task.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub id: u64,
    pub label: String,
    pub running: bool,
    pub paused: bool,
    /// 0.0 to 1.0, when known.
    pub fraction: Option<f64>,
    /// "12 MB/s · 2 min left", "Waiting", "Paused".
    pub detail: String,
}

struct Queue {
    tasks: Vec<Arc<Task>>,
    next_id: u64,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { tasks: Vec::new(), next_id: 1 });
static TURN: Condvar = Condvar::new();
/// Tells the window something changed (it reads `shown` when it paints).
static WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

thread_local! {
    /// The task this thread is running (the copy engine reports on the job's own thread).
    static CURRENT: std::cell::RefCell<Option<Arc<Task>>> = const { std::cell::RefCell::new(None) };
}

/// How the window is told to paint again (set once, when it exists).
pub fn set_wake(f: impl Fn() + Send + Sync + 'static) {
    let _ = WAKE.set(Box::new(f));
}

fn wake() {
    if let Some(w) = WAKE.get() {
        w();
    }
}

fn lock() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

/// A place in the queue. Dropping it leaves the queue (and lets the next one start).
pub struct Ticket(Arc<Task>);

impl Ticket {
    /// Joins the end of the queue (it shows in the list at once).
    pub fn join(label: String) -> Ticket {
        let mut q = lock();
        let id = q.next_id;
        q.next_id += 1;
        let task = Arc::new(Task {
            id,
            label,
            total: AtomicU64::new(0),
            done_ppm: AtomicU64::new(0),
            paused: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            timing: Mutex::new((None, Duration::ZERO, None)),
        });
        q.tasks.push(task.clone());
        drop(q);
        wake();
        Ticket(task)
    }

    /// How many bytes it will copy, once counted (for its speed).
    pub fn set_total(&self, bytes: u64) {
        self.0.total.store(bytes, Ordering::SeqCst);
    }

    /// Set once CANCEL is clicked (so counting its size can stop too).
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.0.cancelled
    }

    /// Waits until it is first in the queue. False: it was cancelled while it waited.
    pub fn wait_turn(&self) -> bool {
        let mut q = lock();
        loop {
            if self.0.cancelled.load(Ordering::SeqCst) {
                return false;
            }
            if q.tasks.first().is_some_and(|t| t.id == self.0.id) {
                break;
            }
            q = TURN.wait_timeout(q, Duration::from_millis(500)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        drop(q);
        self.0.timing.lock().unwrap_or_else(|e| e.into_inner()).0 = Some(Instant::now());
        CURRENT.with(|c| *c.borrow_mut() = Some(self.0.clone()));
        wake();
        true
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        CURRENT.with(|c| {
            if c.borrow().as_ref().is_some_and(|t| t.id == self.0.id) {
                *c.borrow_mut() = None;
            }
        });
        lock().tasks.retain(|t| t.id != self.0.id);
        TURN.notify_all();
        wake();
    }
}

/// The copy engine's (or archive.rs's) report on this thread's task: `so_far` of `total` work
/// done. Holds the copy while it's paused. False: it was cancelled (the engine is told to stop).
pub fn progress(total: u32, so_far: u32) -> bool {
    let Some(task) = CURRENT.with(|c| c.borrow().clone()) else { return true };
    if total > 0 {
        let ppm = (so_far as u64 * 1_000_000 / total as u64).min(1_000_000);
        let before = task.done_ppm.swap(ppm, Ordering::SeqCst);
        // (About four repaints a second at most: the window repaints the list when it's woken.)
        if ppm / 2_500 != before / 2_500 {
            wake();
        }
    }
    still_on(&task)
}

/// Between items: holds the copy while it's paused. False: cancelled.
pub fn between_items() -> bool {
    match CURRENT.with(|c| c.borrow().clone()) {
        Some(task) => still_on(&task),
        None => true,
    }
}

fn still_on(task: &Task) -> bool {
    if task.paused.load(Ordering::SeqCst) && !task.cancelled.load(Ordering::SeqCst) {
        let at = Instant::now();
        while task.paused.load(Ordering::SeqCst) && !task.cancelled.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
        task.timing.lock().unwrap_or_else(|e| e.into_inner()).1 += at.elapsed();
    }
    !task.cancelled.load(Ordering::SeqCst)
}

/// Pause or carry on (the list's PAUSE / RESUME).
pub fn toggle_pause(id: u64) {
    if let Some(t) = lock().tasks.iter().find(|t| t.id == id) {
        let now = !t.paused.load(Ordering::SeqCst);
        t.paused.store(now, Ordering::SeqCst);
        t.timing.lock().unwrap_or_else(|e| e.into_inner()).2 = now.then(Instant::now);
    }
    wake();
}

/// Stop it (the list's CANCEL): a waiting one leaves the queue, a running one stops.
pub fn cancel(id: u64) {
    if let Some(t) = lock().tasks.iter().find(|t| t.id == id) {
        t.cancelled.store(true, Ordering::SeqCst);
    }
    TURN.notify_all();
    wake();
}

/// Glint is closing: nothing stays paused (the copies finish with no list to resume them from).
pub fn resume_all() {
    for t in &lock().tasks {
        t.paused.store(false, Ordering::SeqCst);
    }
}

/// Whether anything is in the queue.
pub fn busy() -> bool {
    !lock().tasks.is_empty()
}

/// What the list shows, first (running) to last.
pub fn shown() -> Vec<Shown> {
    let q = lock();
    q.tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let (started, paused_for, paused_at) = *t.timing.lock().unwrap_or_else(|e| e.into_inner());
            let running = i == 0 && started.is_some();
            let paused = t.paused.load(Ordering::SeqCst);
            let ppm = t.done_ppm.load(Ordering::SeqCst);
            let fraction = (running && ppm > 0).then_some(ppm as f64 / 1_000_000.0);
            let detail = if t.cancelled.load(Ordering::SeqCst) {
                "Stopping\u{2026}".to_string()
            } else if paused {
                "Paused".to_string()
            } else if !running {
                "Waiting".to_string()
            } else {
                let paused_now = paused_at.map(|a| a.elapsed()).unwrap_or_default();
                let secs = started.map(|s| s.elapsed().saturating_sub(paused_for + paused_now).as_secs_f64()).unwrap_or(0.0);
                speed_and_left(t.total.load(Ordering::SeqCst), fraction.unwrap_or(0.0), secs)
            };
            Shown { id: t.id, label: t.label.clone(), running, paused, fraction, detail }
        })
        .collect()
}

/// "12.5 MB/s · 2 min left", from what's done after `secs` seconds of copying.
fn speed_and_left(total: u64, fraction: f64, secs: f64) -> String {
    if secs < 1.0 || fraction <= 0.0 {
        return "Starting\u{2026}".to_string();
    }
    let left = secs * (1.0 - fraction) / fraction;
    let left = match left.round() as u64 {
        s if s < 60 => format!("{s} sec left"),
        s if s < 3600 => format!("{} min left", s.div_ceil(60)),
        s => format!("{} h {} min left", s / 3600, (s % 3600) / 60),
    };
    if total == 0 {
        return left;
    }
    let per_sec = (total as f64 * fraction / secs) as u64;
    format!("{}/s \u{00b7} {left}", crate::folder::format_size(per_sec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_take_turns_and_a_waiting_one_can_be_cancelled() {
        let first = Ticket::join("one".into());
        let second = Ticket::join("two".into());
        assert!(first.wait_turn());
        let shown = shown();
        let mine: Vec<_> = shown.iter().filter(|s| s.id == first.0.id || s.id == second.0.id).collect();
        assert!(mine[0].running && !mine[1].running);
        assert_eq!(mine[1].detail, "Waiting");
        cancel(second.0.id);
        assert!(!second.wait_turn(), "cancelled while waiting");
        drop(first);
        drop(second);
    }

    #[test]
    fn speed_and_time_left_read_plainly() {
        assert_eq!(speed_and_left(100 * 1024 * 1024, 0.5, 10.0), "5.0 MB/s \u{00b7} 10 sec left");
        assert_eq!(speed_and_left(0, 0.25, 60.0), "3 min left");
    }

    #[test]
    fn speed_and_time_left_with_nothing_or_very_little_to_go_on() {
        // Under a second in, or nothing done yet: no guess (and no dividing by nothing).
        assert_eq!(speed_and_left(1_000_000, 0.5, 0.9), "Starting\u{2026}");
        assert_eq!(speed_and_left(1_000_000, 0.0, 30.0), "Starting\u{2026}");
        assert_eq!(speed_and_left(0, 0.0, 0.0), "Starting\u{2026}");
        // Only empty files (no bytes to go by): the time left alone.
        assert_eq!(speed_and_left(0, 0.5, 4.0), "4 sec left");
        // A few bytes, all done.
        assert_eq!(speed_and_left(3, 1.0, 1.0), "3 B/s \u{00b7} 0 sec left");
        assert_eq!(speed_and_left(1024, 1.0, 1.0), "1 KB/s \u{00b7} 0 sec left");
        // The minute and the hour, either side.
        assert_eq!(speed_and_left(0, 0.5, 59.4), "59 sec left");
        assert_eq!(speed_and_left(0, 0.5, 59.6), "1 min left");
        assert_eq!(speed_and_left(0, 0.5, 90.0), "2 min left");
        assert_eq!(speed_and_left(0, 0.5, 3600.0), "1 h 0 min left");
        assert_eq!(speed_and_left(0, 0.25, 3000.0), "2 h 30 min left");
        // A millionth done after a second: a long time, but a number.
        assert_eq!(speed_and_left(0, 0.000_001, 1.0), "277 h 46 min left");
    }

    /// A copy on a thread of its own (the copy engine reports on the job's own thread): it waits
    /// its turn, then makes each report it is handed, sending back what the report said.
    struct Copy {
        id: u64,
        report: std::sync::mpsc::Sender<(u32, u32)>,
        said: std::sync::mpsc::Receiver<bool>,
        thread: std::thread::JoinHandle<()>,
    }

    impl Copy {
        fn start(label: &str) -> Copy {
            let ticket = Ticket::join(label.into());
            let id = ticket.0.id;
            let (report, reports) = std::sync::mpsc::channel::<(u32, u32)>();
            let (tell, said) = std::sync::mpsc::channel();
            let (turn, got_turn) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                turn.send(ticket.wait_turn()).unwrap();
                for (total, so_far) in reports {
                    tell.send(progress(total, so_far)).unwrap();
                }
            });
            assert_eq!(got_turn.recv_timeout(Duration::from_secs(10)), Ok(true));
            Copy { id, report, said, thread }
        }

        /// Hands the copy a report and waits (up to `wait`) for what it said.
        fn reports(&self, total: u32, so_far: u32, wait: Duration) -> Option<bool> {
            self.report.send((total, so_far)).unwrap();
            self.said.recv_timeout(wait).ok()
        }

        fn shown(&self) -> Shown {
            shown().into_iter().find(|s| s.id == self.id).expect("in the list")
        }

        /// The copy ends (its place in the queue goes with it).
        fn end(self) {
            drop(self.report);
            self.thread.join().unwrap();
            assert!(!shown().iter().any(|s| s.id == self.id), "gone from the list");
        }
    }

    const SOON: Duration = Duration::from_secs(10);

    #[test]
    fn pause_holds_the_copy_until_resumed_and_cancel_ends_a_paused_one() {
        let copy = Copy::start("queue test: pause");
        toggle_pause(copy.id);
        assert_eq!(copy.reports(100, 50, Duration::from_millis(150)), None, "held while paused");
        let s = copy.shown();
        assert!(s.running && s.paused, "{s:?}");
        assert_eq!((s.detail.as_str(), s.fraction), ("Paused", Some(0.5)), "how far it got still shows");
        toggle_pause(copy.id);
        assert_eq!(copy.said.recv_timeout(SOON), Ok(true), "on again");
        assert!(!copy.shown().paused);
        // Paused again, then cancelled: the held report ends, saying stop - and so does every one after.
        toggle_pause(copy.id);
        assert_eq!(copy.reports(100, 60, Duration::from_millis(150)), None);
        cancel(copy.id);
        assert_eq!(copy.said.recv_timeout(SOON), Ok(false));
        assert_eq!(copy.shown().detail, "Stopping\u{2026}");
        assert_eq!(copy.reports(100, 70, SOON), Some(false));
        copy.end();
    }

    #[test]
    fn reports_show_how_far_a_copy_is_and_never_go_past_the_end() {
        let copy = Copy::start("queue test: progress");
        assert_eq!(copy.shown().fraction, None, "nothing reported yet");
        // A report of nothing out of nothing changes nothing (and divides by nothing).
        assert_eq!(copy.reports(0, 0, SOON), Some(true));
        assert_eq!(copy.shown().fraction, None);
        assert_eq!(copy.reports(3, 1, SOON), Some(true));
        assert_eq!(copy.shown().fraction, Some(0.333_333));
        assert_eq!(copy.reports(u32::MAX, u32::MAX - 1, SOON), Some(true));
        assert_eq!(copy.shown().fraction, Some(0.999_999));
        // More than all of it is all of it.
        assert_eq!(copy.reports(10, 20, SOON), Some(true));
        assert_eq!(copy.shown().fraction, Some(1.0));
        // On a thread with no place in the queue, reports never hold or stop anything.
        toggle_pause(copy.id);
        assert!(progress(10, 5) && between_items());
        toggle_pause(copy.id);
        copy.end();
    }

    #[test]
    fn a_cancelled_waiting_copy_leaves_and_the_next_waits_for_the_one_running() {
        let first = Ticket::join("queue test: first".into());
        assert!(first.wait_turn());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut waiting: Vec<(u64, std::thread::JoinHandle<()>)> = ["queue test: second", "queue test: third"]
            .iter()
            .map(|label| {
                let ticket = Ticket::join(label.to_string());
                let (id, tx) = (ticket.0.id, tx.clone());
                (id, std::thread::spawn(move || tx.send((id, ticket.wait_turn())).unwrap()))
            })
            .collect();
        let (second, third) = (waiting[0].0, waiting[1].0);
        let mine = || -> Vec<u64> { shown().iter().map(|s| s.id).filter(|id| [first.0.id, second, third].contains(id)).collect() };
        assert_eq!(mine(), [first.0.id, second, third]);
        // Cancelled while it waits: it stops waiting (at once, not when the one running is done).
        cancel(second);
        assert_eq!(rx.recv_timeout(SOON), Ok((second, false)));
        // (Its ticket leaves the list when its thread lets go of it.)
        waiting.remove(0).1.join().unwrap();
        assert_eq!(mine(), [first.0.id, third]);
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err(), "the third still waits for the first");
        assert_eq!(shown().iter().find(|s| s.id == third).unwrap().detail, "Waiting");
        drop(first);
        assert_eq!(rx.recv_timeout(SOON), Ok((third, true)));
        for (_, t) in waiting {
            t.join().unwrap();
        }
    }
}
