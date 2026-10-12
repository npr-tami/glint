//! Playing a video in the Space preview, with Windows' Media Foundation media engine.
//!
//! The engine runs in "frame server" mode: it decodes on threads of its own, and on each repaint
//! the preview asks it for the newest frame (`Video::tick`), which is copied into a WIC bitmap and
//! drawn with Direct2D like any other picture (render.rs). Sound goes straight to the speakers.
//!
//! A video never starts by itself: the engine is made only when the person presses play. Sound
//! fades in over a fifth of a second when playing starts or resumes, and fades out briefly when it
//! pauses, or when the preview closes or moves to another file (then the engine is shut down once
//! it's quiet, see `retire`), so there's no click and no sudden blast.
//!
//! While something is playing, a timer on the window (`wake_in`) repaints it about 30 times a
//! second; nothing runs when nothing plays. Where Media Foundation isn't there (some Windows N
//! editions, Wine), `Video::open` fails and the preview says it can't play the file.

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
    MF_MEDIA_ENGINE_EVENT_LOADEDMETADATA, MF_MEDIA_ENGINE_VIDEO_OUTPUT_FORMAT, MF_VERSION,
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
            })
        }
    }

    /// The engine couldn't read the file.
    pub fn failed(&self) -> bool {
        self.events.error.load(Ordering::SeqCst)
    }

    pub fn is_playing(&self) -> bool {
        self.wants_play && !self.failed()
    }

    /// Plays from where it is (from the start once it has ended), the sound fading in.
    pub fn play(&mut self) {
        // SAFETY: plain calls on a live engine.
        unsafe {
            if self.engine.IsEnded().as_bool() || self.events.ended.swap(false, Ordering::SeqCst) {
                let _ = self.engine.SetCurrentTime(0.0);
            }
            let now = self.engine.GetVolume();
            let _ = self.engine.SetVolume(now.min(engine_volume(self.volume)));
            let _ = self.engine.Play();
            self.fade = Some(Fade { from: now, to: engine_volume(self.volume), start: Instant::now(), ms: FADE_IN_MS, then_pause: false });
        }
        self.wants_play = true;
        wake_in(self.hwnd, FRAME_MS);
    }

    /// Pauses after a short fade out.
    pub fn pause(&mut self) {
        if !self.wants_play {
            return;
        }
        self.wants_play = false;
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
        if d > 0.0 {
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
        if self.events.ended.load(Ordering::SeqCst) && self.wants_play {
            self.wants_play = false;
        }
        self.grab(max_px);
        self.wants_play || self.fade.is_some()
    }

    /// Copies the newest frame out of the engine (when there is a new one).
    fn grab(&mut self, max_px: u32) {
        let Some((w, h)) = self.size() else { return };
        // SAFETY: plain calls on a live engine; the WIC bitmap is ours, locked while copied.
        unsafe {
            let Ok(pts) = self.engine.OnVideoStreamTick() else { return };
            if pts == self.last_pts && self.frame.is_some() {
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
            let row = *tw as usize * 4;
            let mut px = Vec::with_capacity(row * *th as usize);
            for y in 0..*th as usize {
                let start = y * stride as usize;
                if start + row > size as usize {
                    return;
                }
                px.extend_from_slice(std::slice::from_raw_parts(data.add(start), row));
            }
            drop(lock);
            // Video has no transparency.
            for p in px.chunks_exact_mut(4) {
                p[3] = 255;
            }
            self.frame = Picture::new(*tw, *th, px, false);
        }
    }
}

impl Drop for Video {
    /// The preview closed or moved on: the sound fades out, then the engine is shut down.
    fn drop(&mut self) {
        // SAFETY: a plain call on a live engine.
        let now = unsafe { self.engine.GetVolume() };
        retire(self.engine.clone(), if self.wants_play || self.fade.is_some() { now } else { 0.0 }, self.hwnd);
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
    fn videos_by_name() {
        assert!(is_video("mp4") && is_video("mkv") && is_video("webm") && is_video("mov"));
        assert!(!is_video("gif") && !is_video("mp3"));
    }
}
