//! Playing a video in the Space preview, with Windows' Media Foundation media engine.
//!
//! The engine runs in "frame server" mode: it decodes on threads of its own, and on each repaint
//! the preview asks it for the newest frame (`Video::tick`), which is copied into a WIC bitmap and
//! drawn with Direct2D like any other picture (render.rs). Sound goes straight to the speakers.
//!
//! A video never starts by itself: the engine is made only when the person presses play. Sound
//! fades in over a fifth of a second when playing starts or resumes, and fades out briefly when it
//! pauses, or when the preview closes or moves to another file (then the engine is shut down once
//! it's quiet, see `retire`), so there's no click and no sudden blast. Only deleting, renaming or
//! moving the file, or ejecting its drive, shuts the engine down at once (`let_go`): it has the
//! file open until then.
//!
//! While something is playing, a timer on the window (`wake_in`) repaints it about 30 times a
//! second; nothing runs when nothing plays. Where Media Foundation isn't there (some Windows N
//! editions, Wine), `Video::open` fails and the preview says it can't play the file. It says so too
//! for a file the engine reports an error on, or never gets going on (some broken files make it go
//! quiet instead, see `Watch`); play and seek then do nothing.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use windows::core::{implement, BSTR};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::Graphics::Imaging::{GUID_WICPixelFormat32bppBGRA, IWICBitmap, IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapLockWrite, WICRect};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaEngine, IMFMediaEngineClassFactory, IMFMediaEngineNotify, IMFMediaEngineNotify_Impl, MFCreateAttributes, MFStartup,
    CLSID_MFMediaEngineClassFactory, MFSTARTUP_LITE, MF_MEDIA_ENGINE_CALLBACK, MF_MEDIA_ENGINE_EVENT_ENDED, MF_MEDIA_ENGINE_EVENT_ERROR,
    MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA, MF_MEDIA_ENGINE_EVENT_SEEKED, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::thumbs::Picture;

/// Files the preview offers to play (lower case, no dot).
pub const VIDEOS: &[&str] = &["mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "asf", "mpg", "mpeg", "3gp", "3g2", "ts", "m2ts", "mts", "flv"];

/// Whether a file with this extension is a video the preview can try to play.
pub fn is_video(ext: &str) -> bool {
    VIDEOS.contains(&ext)
}

/// Sound fades in over this long when playing starts or resumes (milliseconds)...
const FADE_IN_MS: f64 = 200.0;
/// ...and out over this long before a pause or a stop.
const FADE_OUT_MS: f64 = 120.0;
/// While playing, the window is repainted this often (milliseconds).
pub const FRAME_MS: u32 = 30;
/// The window's timers (any number the window's own timers don't use).
const WAKE_TIMER: usize = 0x6c70;
const FADE_TIMER: usize = 0x6c71;
/// A file that hasn't said how long it is and how big its picture is after this long (from being
/// opened) can't be played: the engine goes quiet on some broken files instead of reporting them.
const LOAD_TIMEOUT_MS: f64 = 8000.0;
/// Nor one that has played this long without a single frame and without moving on.
const START_TIMEOUT_MS: f64 = 5000.0;
/// DXGI_FORMAT_B8G8R8A8_UNORM: frames come out as BGRA, as Direct2D draws them.
const BGRA: u32 = 87;

/// The volume the engine is given for a slider at `v` (0 to 1): squared, so the slider's middle
/// sounds like the middle (loudness isn't linear).
pub fn engine_volume(v: f32) -> f64 {
    let v = v.clamp(0.0, 1.0) as f64;
    v * v
}

/// Where a fade is: the volume `elapsed_ms` into a fade from `from` to `to` over `length_ms`, and
/// whether it's over.
pub fn fade_step(from: f64, to: f64, elapsed_ms: f64, length_ms: f64) -> (f64, bool) {
    if elapsed_ms >= length_ms || length_ms <= 0.0 {
        return (to, true);
    }
    let t = (elapsed_ms / length_ms).clamp(0.0, 1.0);
    (from + (to - from) * t, false)
}

/// Watching a video get going, to give up on one that never does (see `LOAD_TIMEOUT_MS`).
#[derive(Clone, Copy, Debug)]
struct Watch {
    opened: Instant,
    /// Since when it's been meant to play (None while paused).
    playing_since: Option<Instant>,
    /// It has shown a frame or moved on: it plays.
    started: bool,
    /// Given up on: it can't be played.
    gave_up: bool,
}

impl Watch {
    fn new(now: Instant) -> Watch {
        Watch { opened: now, playing_since: None, started: false, gave_up: false }
    }

    fn playing(&mut self, on: bool, now: Instant) {
        if !on {
            self.playing_since = None;
        } else if self.playing_since.is_none() {
            self.playing_since = Some(now);
        }
    }

    /// Looks again at `now`: whether the engine reported an error, has read the file's size and
    /// length (`loaded`) and has shown a frame or moved on (`moving`). Returns whether it can't be
    /// played (and once not, never again).
    fn check(&mut self, error: bool, loaded: bool, moving: bool, now: Instant) -> bool {
        self.started |= moving;
        let ms = |since: Instant| now.saturating_duration_since(since).as_secs_f64() * 1000.0;
        let never_loaded = !loaded && ms(self.opened) >= LOAD_TIMEOUT_MS;
        let never_started = loaded && !self.started && self.playing_since.is_some_and(|t| ms(t) >= START_TIMEOUT_MS);
        self.gave_up |= error || never_loaded || never_started;
        self.gave_up
    }
}

/// What the engine told us (from its own threads).
#[derive(Default)]
struct Events {
    loaded: AtomicBool,
    error: AtomicBool,
    ended: AtomicBool,
    hwnd: AtomicIsize,
}

#[implement(IMFMediaEngineNotify)]
struct Notify(Arc<Events>);

impl IMFMediaEngineNotify_Impl for Notify_Impl {
    fn EventNotify(&self, event: u32, _param1: usize, _param2: u32) -> windows::core::Result<()> {
        let e = &self.0;
        let event = event as i32;
        if event == MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA.0 {
            e.loaded.store(true, Ordering::SeqCst);
        } else if event == MF_MEDIA_ENGINE_EVENT_ERROR.0 {
            e.error.store(true, Ordering::SeqCst);
        } else if event == MF_MEDIA_ENGINE_EVENT_ENDED.0 {
            e.ended.store(true, Ordering::SeqCst);
        } else if event == MF_MEDIA_ENGINE_EVENT_SEEKED.0 {
            // A jump is done: the frame there is drawn (nothing repaints a paused video by itself).
        } else {
            return Ok(());
        }
        let hwnd = HWND(e.hwnd.load(Ordering::SeqCst) as *mut _);
        // SAFETY: InvalidateRect may be called from any thread; a window that's gone just fails.
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
        }
        Ok(())
    }
}

/// A fade under way.
#[derive(Clone, Copy)]
struct Fade {
    from: f64,
    to: f64,
    start: Instant,
    ms: f64,
    /// Pause when it's over (a fade out).
    then_pause: bool,
}

/// One video being played.
pub struct Video {
    engine: IMFMediaEngine,
    events: Arc<Events>,
    hwnd: HWND,
    wic: Option<IWICImagingFactory>,
    /// The bitmap frames are copied into, and its size.
    target: Option<(IWICBitmap, u32, u32)>,
    /// The newest frame.
    frame: Option<Arc<Picture>>,
    last_pts: i64,
    /// The slider's volume (0 to 1) and mute.
    volume: f32,
    muted: bool,
    fade: Option<Fade>,
    /// Playing, or about to (play was pressed and no pause since).
    wants_play: bool,
    watch: Watch,
}

thread_local! {
    static STARTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Engines fading out before they are shut down: the engine, its volume at the start, when.
    static RETIRED: RefCell<Vec<(IMFMediaEngine, f64, Instant)>> = const { RefCell::new(Vec::new()) };
}

impl Video {
    /// Opens `path` without playing it. None where Media Foundation isn't there or won't take it.
    pub fn open(path: &str, hwnd: HWND, volume: f32, muted: bool) -> Option<Video> {
        let events = Arc::new(Events::default());
        events.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
        // SAFETY: plain COM calls on this (the UI) thread, which has COM; every object is an owned
        // wrapper released when dropped.
        unsafe {
            if !STARTED.with(|s| s.get()) {
                MFStartup(MF_VERSION, MFSTARTUP_LITE).ok()?;
                STARTED.with(|s| s.set(true));
            }
            let factory: IMFMediaEngineClassFactory = CoCreateInstance(&CLSID_MFMediaEngineClassFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let mut attrs: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attrs, 2).ok()?;
            let attrs = attrs?;
            let notify: IMFMediaEngineNotify = Notify(events.clone()).into();
            attrs.SetUnknown(&MF_MEDIA_ENGINE_CALLBACK, &notify).ok()?;
            attrs.SetUINT32(&MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, BGRA).ok()?;
            let engine = factory.CreateInstance(0, &attrs).ok()?;
            let _ = engine.SetAutoPlay(false);
            let _ = engine.SetLoop(false);
            // Silent until play is pressed (then it fades in).
            let _ = engine.SetVolume(0.0);
            let _ = engine.SetMuted(muted);
            if engine.SetSource(&BSTR::from(path)).is_err() {
                let _ = engine.Shutdown();
                return None;
            }
            Some(Video {
                engine,
                events,
                hwnd,
                wic: crate::thumbs::wic_factory(),
                target: None,
                frame: None,
                last_pts: -1,
                volume,
                muted,
                fade: None,
                wants_play: false,
                watch: Watch::new(Instant::now()),
            })
        }
    }

    /// The engine couldn't read the file: it said so, or never got going (`Watch`). Play and seek
    /// then do nothing.
    pub fn failed(&self) -> bool {
        self.watch.gave_up || self.events.error.load(Ordering::SeqCst)
    }

    pub fn is_playing(&self) -> bool {
        self.wants_play && !self.failed()
    }

    /// Plays from where it is (from the start once it has ended), the sound fading in.
    pub fn play(&mut self) {
        if self.failed() {
            return;
        }
        // The "it ended" note goes either way: left behind, `tick` would stop it again at once.
        let ended = self.events.ended.swap(false, Ordering::SeqCst);
        // SAFETY: plain calls on a live engine.
        unsafe {
            if ended || self.engine.IsEnded().as_bool() {
                let _ = self.engine.SetCurrentTime(0.0);
            }
            let now = self.engine.GetVolume();
            let _ = self.engine.SetVolume(now.min(engine_volume(self.volume)));
            let _ = self.engine.Play();
            self.fade = Some(Fade { from: now, to: engine_volume(self.volume), start: Instant::now(), ms: FADE_IN_MS, then_pause: false });
        }
        self.wants_play = true;
        self.watch.playing(true, Instant::now());
        wake_in(self.hwnd, FRAME_MS);
    }

    /// Pauses after a short fade out.
    pub fn pause(&mut self) {
        if !self.wants_play {
            return;
        }
        self.wants_play = false;
        self.watch.playing(false, Instant::now());
        // SAFETY: a plain call on a live engine.
        let now = unsafe { self.engine.GetVolume() };
        self.fade = Some(Fade { from: now, to: 0.0, start: Instant::now(), ms: FADE_OUT_MS, then_pause: true });
        wake_in(self.hwnd, FRAME_MS);
    }

    pub fn toggle(&mut self) {
        if self.wants_play {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Where it is and how long it is, in seconds (0 while not known).
    pub fn position(&self) -> f64 {
        // SAFETY: a plain call on a live engine.
        let t = unsafe { self.engine.GetCurrentTime() };
        if t.is_finite() { t.max(0.0) } else { 0.0 }
    }

    pub fn duration(&self) -> f64 {
        // SAFETY: a plain call on a live engine.
        let d = unsafe { self.engine.GetDuration() };
        if d.is_finite() { d.max(0.0) } else { 0.0 }
    }

    /// Jumps to `secs`.
    pub fn seek(&mut self, secs: f64) {
        let d = self.duration();
        if d > 0.0 && !self.failed() {
            self.events.ended.store(false, Ordering::SeqCst);
            // SAFETY: a plain call on a live engine.
            unsafe {
                let _ = self.engine.SetCurrentTime(secs.clamp(0.0, d));
            }
            self.last_pts = -1;
            wake_in(self.hwnd, FRAME_MS);
        }
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        if self.wants_play && self.fade.is_none() {
            // SAFETY: a plain call on a live engine.
            unsafe {
                let _ = self.engine.SetVolume(engine_volume(self.volume));
            }
        } else if let Some(f) = self.fade.as_mut().filter(|f| !f.then_pause) {
            f.to = engine_volume(self.volume);
        }
    }

    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        // SAFETY: a plain call on a live engine.
        unsafe {
            let _ = self.engine.SetMuted(m);
        }
    }

    /// The video's own size in pixels, once known.
    pub fn size(&self) -> Option<(u32, u32)> {
        if !self.events.loaded.load(Ordering::SeqCst) {
            return None;
        }
        let (mut w, mut h) = (0u32, 0u32);
        // SAFETY: plain out-parameters.
        unsafe { self.engine.GetNativeVideoSize(Some(&mut w), Some(&mut h)).ok()? };
        (w > 0 && h > 0).then_some((w, h))
    }

    /// The newest frame there is.
    pub fn frame(&self) -> Option<Arc<Picture>> {
        self.frame.clone()
    }

    /// Called on each repaint: moves the fade on and takes the newest frame, made to fit a
    /// `max_px` square. Returns whether to repaint again soon.
    pub fn tick(&mut self, max_px: u32) -> bool {
        if let Some(f) = self.fade {
            let (v, done) = fade_step(f.from, f.to, f.start.elapsed().as_secs_f64() * 1000.0, f.ms);
            // SAFETY: plain calls on a live engine.
            unsafe {
                let _ = self.engine.SetVolume(v);
                if done {
                    self.fade = None;
                    if f.then_pause {
                        let _ = self.engine.Pause();
                    }
                }
            }
        }
        self.grab(max_px);
        // Whether it can't be played after all: the engine said so (asked here too, in case its
        // event went missing), or it never got going.
        // SAFETY: plain calls on a live engine.
        let error = self.events.error.load(Ordering::SeqCst) || unsafe { self.engine.GetError() }.is_ok_and(|e| unsafe { e.GetErrorCode() } != 0);
        let loaded = self.events.loaded.load(Ordering::SeqCst);
        let moving = self.frame.is_some() || self.position() > 0.0;
        let failed = self.watch.check(error, loaded, moving, Instant::now());
        // At the end, or when the file can't be played, it stops (and nothing repaints for it).
        if self.wants_play && (self.events.ended.load(Ordering::SeqCst) || failed) {
            self.wants_play = false;
            self.watch.playing(false, Instant::now());
        }
        // Until it's known whether the file plays, it's looked at again soon (so a file the engine
        // goes quiet on is given up on in time, even paused).
        self.wants_play || self.fade.is_some() || (!loaded && !failed)
    }

    /// Copies the newest frame out of the engine (when there is a new one).
    fn grab(&mut self, max_px: u32) {
        let Some((w, h)) = self.size() else { return };
        // SAFETY: plain calls on a live engine; the WIC bitmap is ours, locked while copied.
        unsafe {
            let Ok(pts) = self.engine.OnVideoStreamTick() else { return };
            // (A frame whose bitmap went with a lost graphics device is copied out again, so a
            // paused video doesn't go blank.)
            if pts == self.last_pts && self.frame.as_ref().is_some_and(|f| !f.is_lost()) {
                return;
            }
            let (tw, th) = crate::thumbs::fit_size(w, h, max_px.max(64), u64::MAX);
            if self.target.as_ref().is_none_or(|t| (t.1, t.2) != (tw, th)) {
                let Some(wic) = &self.wic else { return };
                let Ok(bmp) = wic.CreateBitmap(tw, th, &GUID_WICPixelFormat32bppBGRA, WICBitmapCacheOnLoad) else { return };
                self.target = Some((bmp, tw, th));
            }
            let Some((bmp, tw, th)) = &self.target else { return };
            let dst = RECT { left: 0, top: 0, right: *tw as i32, bottom: *th as i32 };
            if self.engine.TransferVideoFrame(bmp, None, &dst, None).is_err() {
                return;
            }
            self.last_pts = pts;
            let rect = WICRect { X: 0, Y: 0, Width: *tw as i32, Height: *th as i32 };
            let Ok(lock) = bmp.Lock(&rect, WICBitmapLockWrite.0 as u32) else { return };
            let (Ok(stride), mut size, mut data) = (lock.GetStride(), 0u32, std::ptr::null_mut::<u8>()) else { return };
            if lock.GetDataPointer(&mut size, &mut data).is_err() || data.is_null() {
                return;
            }
            let locked = std::slice::from_raw_parts(data as *const u8, size as usize);
            let fill = |out: &mut [u8]| copy_rows(locked, stride as usize, *tw as usize * 4, out);
            // The same frame picture is filled again while the size stays (the renderer copies it
            // into the bitmap it has: no new memory, no new bitmap, every frame). Video has no
            // see-through parts: the renderer ignores the fourth byte of a frame.
            let refilled = self.frame.as_ref().is_some_and(|f| (f.width(), f.height()) == (*tw, *th) && f.refill(fill));
            if !refilled {
                self.frame = Picture::frame(*tw, *th, fill);
            }
            drop(lock);
        }
    }
}

/// Copies rows of `row` bytes, `stride` apart in `from`, one after another into `out` (which
/// holds a whole number of rows). False when `from` is too short for them.
fn copy_rows(from: &[u8], stride: usize, row: usize, out: &mut [u8]) -> bool {
    if row == 0 || stride < row || out.len() % row != 0 {
        return false;
    }
    for (y, line) in out.chunks_exact_mut(row).enumerate() {
        let start = y * stride;
        let Some(src) = from.get(start..start + row) else { return false };
        line.copy_from_slice(src);
    }
    true
}

impl Drop for Video {
    /// The preview closed or moved on: the sound fades out, then the engine is shut down.
    fn drop(&mut self) {
        // SAFETY: a plain call on a live engine.
        let now = unsafe { self.engine.GetVolume() };
        retire(self.engine.clone(), if self.wants_play || self.fade.is_some() { now } else { 0.0 }, self.hwnd);
    }
}

/// Lets go of the files videos have open, at once: `video`'s engine and those still fading out are
/// shut down now, with no fade. An engine keeps its file open (even paused), and Windows won't
/// delete, rename or move a file that's open, nor eject the drive it's on.
pub fn let_go(video: Option<Video>) {
    if let Some(mut v) = video {
        // Silent now, so dropping it shuts it down straight away (`retire`).
        v.wants_play = false;
        v.fade = None;
    }
    let fading = RETIRED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for (engine, _, _) in fading {
        // SAFETY: plain calls on engines we no longer use.
        unsafe {
            let _ = engine.Pause();
            let _ = engine.Shutdown();
        }
    }
}

/// Fades an engine out and shuts it down (on a timer: the preview may already be gone).
fn retire(engine: IMFMediaEngine, volume: f64, hwnd: HWND) {
    if volume <= 0.0 {
        // SAFETY: plain calls on a live engine we no longer use.
        unsafe {
            let _ = engine.Pause();
            let _ = engine.Shutdown();
        }
        return;
    }
    RETIRED.with(|r| r.borrow_mut().push((engine, volume, Instant::now())));
    // SAFETY: a timer on the window, calling `fade_out` on this thread.
    unsafe {
        SetTimer(Some(hwnd), FADE_TIMER, 15, Some(fade_out));
    }
}

unsafe extern "system" fn fade_out(hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    let left = RETIRED.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|(engine, from, start)| {
            let (v, done) = fade_step(*from, 0.0, start.elapsed().as_secs_f64() * 1000.0, FADE_OUT_MS);
            // SAFETY: plain calls on engines kept here until they're shut down.
            unsafe {
                let _ = engine.SetVolume(v);
                if done {
                    let _ = engine.Pause();
                    let _ = engine.Shutdown();
                }
            }
            !done
        });
        r.len()
    });
    if left == 0 {
        // SAFETY: our own timer.
        unsafe {
            let _ = KillTimer(Some(hwnd), FADE_TIMER);
        }
    }
}

/// Repaints the window in `ms` milliseconds (again and again until `stop_waking`).
pub fn wake_in(hwnd: HWND, ms: u32) {
    // SAFETY: a timer on the window; setting it again just moves it.
    unsafe {
        SetTimer(Some(hwnd), WAKE_TIMER, ms.max(10), Some(wake));
    }
}

pub fn stop_waking(hwnd: HWND) {
    // SAFETY: our own timer (fine if it isn't running).
    unsafe {
        let _ = KillTimer(Some(hwnd), WAKE_TIMER);
    }
}

unsafe extern "system" fn wake(hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    // SAFETY: our own window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// A time in seconds as "1:05", or "1:02:03" from an hour on.
pub fn clock(secs: f64) -> String {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    crate::details::duration_text((secs * 10_000_000.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame's rows come out of the locked bitmap (whose rows can be further apart than a row)
    /// one after another, into the frame's own buffer; a short bitmap copies nothing.
    #[test]
    fn frame_rows_are_copied_whole() {
        // 2 x 3 pixels, rows 12 bytes apart (8 of pixels, 4 of padding).
        let locked: Vec<u8> = (0..36).collect();
        let mut out = vec![0u8; 24];
        assert!(copy_rows(&locked, 12, 8, &mut out));
        assert_eq!(&out[..8], &locked[..8]);
        assert_eq!(&out[8..16], &locked[12..20]);
        assert_eq!(&out[16..], &locked[24..32]);
        // Into a refilled frame picture: the same id, the new pixels.
        let f = Picture::frame(2, 3, |b| copy_rows(&locked, 12, 8, b)).unwrap();
        let id = f.id();
        let _ = f.take_pixels();
        let other: Vec<u8> = (100..136).collect();
        assert!(f.refill(|b| copy_rows(&other, 12, 8, b)));
        assert_eq!(f.id(), id);
        assert_eq!(&f.take_fresh().unwrap()[..8], &other[..8]);
        // Too short (the last row missing), rows wider than the stride, nothing at all.
        assert!(!copy_rows(&locked[..30], 12, 8, &mut out));
        assert!(!copy_rows(&locked, 4, 8, &mut out));
        assert!(!copy_rows(&locked, 12, 0, &mut out));
        assert!(!copy_rows(&locked, 12, 8, &mut [0u8; 7]));
        assert!(copy_rows(&[], 12, 8, &mut []), "no rows: nothing to copy");
    }

    #[test]
    fn times_read_like_a_player() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(65.4), "1:05");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(f64::NAN), "0:00");
        assert_eq!(clock(-3.0), "0:00");
    }

    #[test]
    fn fades_are_smooth_and_end() {
        assert_eq!(fade_step(0.0, 0.25, 0.0, 200.0), (0.0, false));
        assert_eq!(fade_step(0.0, 0.25, 100.0, 200.0), (0.125, false));
        assert_eq!(fade_step(0.0, 0.25, 250.0, 200.0), (0.25, true));
        assert_eq!(fade_step(0.5, 0.0, 120.0, 120.0), (0.0, true));
    }

    #[test]
    fn half_way_on_the_slider_is_a_moderate_volume() {
        assert_eq!(engine_volume(0.5), 0.25);
        assert_eq!(engine_volume(2.0), 1.0);
        assert_eq!(engine_volume(-1.0), 0.0);
    }

    #[test]
    fn a_video_that_never_gets_going_cant_be_played() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
        // The engine says so: at once.
        let mut w = Watch::new(t0);
        assert!(w.check(true, false, false, t0));
        // It never reads the file's size and length: given up on after a while, not before.
        let mut w = Watch::new(t0);
        assert!(!w.check(false, false, false, at(LOAD_TIMEOUT_MS as u64 - 1)));
        assert!(w.check(false, false, false, at(LOAD_TIMEOUT_MS as u64)));
        // And once given up on, it stays so (whatever comes later).
        assert!(w.check(false, true, true, at(LOAD_TIMEOUT_MS as u64 + 10)));
        // Read, played, but no frame and not moving: given up on after a while of playing.
        let mut w = Watch::new(t0);
        assert!(!w.check(false, true, false, at(100)));
        w.playing(true, at(100));
        assert!(!w.check(false, true, false, at(100 + START_TIMEOUT_MS as u64 - 1)));
        assert!(w.check(false, true, false, at(100 + START_TIMEOUT_MS as u64)));
    }

    #[test]
    fn a_paused_or_playing_video_is_not_given_up_on() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + std::time::Duration::from_millis(ms);
        // Read and paused (never played): left alone however long it waits.
        let mut w = Watch::new(t0);
        assert!(!w.check(false, true, false, at(60_000)));
        // Played, paused, played again: only the time playing counts, from the new start.
        w.playing(true, at(60_000));
        w.playing(false, at(61_000));
        w.playing(true, at(70_000));
        assert!(!w.check(false, true, false, at(70_000 + START_TIMEOUT_MS as u64 - 1)));
        // A video that showed a frame (or an audio-only one that moved on) plays, however slow.
        let mut w = Watch::new(t0);
        w.playing(true, at(10));
        assert!(!w.check(false, true, true, at(20)));
        assert!(!w.check(false, true, false, at(60_000)));
    }

    #[test]
    fn videos_by_name() {
        assert!(is_video("mp4") && is_video("mkv") && is_video("webm") && is_video("mov"));
        assert!(!is_video("gif") && !is_video("mp3"));
    }
}
