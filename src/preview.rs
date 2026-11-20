//! The Space preview: a wide pane on the right of the file list (about half the window, resizable),
//! like File Pilot's - not a pop-up window.
//!
//! - A top bar: close ×, fit / 1:1 / fill, the zoom in %, the file's name, and ‹ › to step to the
//!   item before or after it in the list.
//! - Pictures are decoded at full size with WIC on a background thread (peek.rs) and drawn on a
//!   checkerboard. Until the full picture is in, its thumbnail (thumbs.rs) is drawn in the same
//!   place, so it only gets sharper. Ctrl+wheel or + / - zoom, dragging pans, and fit (the whole
//!   picture, never blown up), 1:1 (one pixel of the picture on one pixel of the screen) and fill
//!   (the pane filled, cut where it must) are one click. The zoom mode stays while stepping
//!   through pictures.
//! - Text files show their first 200 KB as monospaced text with line numbers; the wheel scrolls.
//!   Long lines wrap inside the pane (a line's number is on its first row only). The WRAP switch
//!   in the top bar turns that off ("PreviewWrap" in settings.json): lines then run on to the
//!   right, and Shift+wheel scrolls sideways.
//! - Animated GIFs play by themselves, looping (anim.rs decodes their frames on peek.rs's thread).
//! - Videos show their thumbnail with a big play button; they play only when the person clicks it
//!   (or the picture), with play/pause, a seek bar with the time, mute and volume under them
//!   (video.rs). Space still closes the pane. One that turns out broken says it can't be played,
//!   in place of the play button, and play and the seek bar then do nothing.
//! - The "i" button in the top bar shows a strip of facts under pictures, GIFs and videos: the
//!   size in pixels, format, bit depth, DPI, camera and date taken, frames, frame rate, bitrate
//!   ("PreviewInfo" in settings.json).
//! - Anything else shows its big thumbnail (a video's frame, a PDF's first page, or its icon) and
//!   its details, as the details panel says them (details.rs).
//!
//! The arrow keys keep moving the list's selection while the pane is open (the keyboard stays with
//! the list), and the pane follows. Esc or Space closes it. It shows the item with the keyboard
//! cursor (when it's selected), else the first selected one. Only the picture on screen is kept at
//! full size; closing the pane or going to another file lets it go.

use std::sync::Arc;
use std::time::Instant;

use windows::Win32::Foundation::HWND;

use crate::app::AppState;
use crate::details::{self, One, Sel};
use crate::anim::{self, Frames};
use crate::folder;
use crate::pathbar::{chevron_right, fill_box, fill_box_paint};
use crate::peek::{self, Answer, FactsCache, Peeker};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{Color, Theme};
use crate::thumbs::{self, Picture};
use crate::ui::{Command, Ctx, CursorKind, Effect, MouseButton, MouseEvent, Point, Rect};
use crate::video::{self, Video};

/// The top bar's height.
pub const TOP_H: f32 = 40.0;
/// A picture decoded for the preview has at most this many pixels (36 million: a big camera's
/// photo); bigger ones are shrunk to it, which keeps the memory sane (4 bytes a pixel).
const MAX_PIXELS: u64 = 36_000_000;
/// The zoom stays between these (as a share of the picture's own pixels).
const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 32.0;
/// Each wheel notch or + / - zooms by this much.
const ZOOM_STEP: f32 = 1.25;
/// Text files.
const MONO_FONT: f32 = 12.5;
const MONO_LINE: f32 = 18.0;
/// Text lines scrolled per wheel notch, and columns per notch sideways (Shift, with wrap off).
const WHEEL_LINES: f32 = 3.0;
const WHEEL_COLS: f32 = 8.0;
/// A row of text holds at least this many columns, however narrow the pane is.
const MIN_COLS: usize = 8;
/// The video controls' height, and one line of the info strip.
const CONTROLS_H: f32 = 40.0;
const INFO_LINE: f32 = 18.0;
/// Said in the player for a video that can't be played (broken, or not a video Windows knows).
const CANT_PLAY: &str = "This video can't be played";

/// How a picture is sized.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// The whole picture in the pane, never bigger than its own size.
    Fit,
    /// One pixel of the picture on one pixel of the screen.
    Actual,
    /// The pane filled (the picture cut where it's longer than the pane).
    Fill,
    /// Zoomed by hand (Ctrl+wheel, + / -), as a share of the picture's own pixels.
    Custom(f32),
}

/// The zoom (picture pixels to screen pixels) a mode means for a picture of `natural` pixels in a
/// view of `view_px` screen pixels.
pub fn zoom_value(zoom: Zoom, natural: (f32, f32), view_px: (f32, f32)) -> f32 {
    let (nw, nh) = (natural.0.max(1.0), natural.1.max(1.0));
    let (sx, sy) = (view_px.0 / nw, view_px.1 / nh);
    match zoom {
        Zoom::Fit => sx.min(sy).min(1.0),
        Zoom::Actual => 1.0,
        Zoom::Fill => sx.max(sy),
        Zoom::Custom(z) => z,
    }
    .max(MIN_ZOOM.min(sx.min(sy)))
}

/// Keeps a picture of `draw` size (DIPs) from being panned out of a `view` (DIPs): centred along a
/// side where it's smaller than the view, and never showing past its edges where it's bigger.
pub fn clamp_pan(pan: Point, draw: (f32, f32), view: (f32, f32)) -> Point {
    let limit = |d: f32, v: f32| ((d - v) / 2.0).max(0.0);
    let (lx, ly) = (limit(draw.0, view.0), limit(draw.1, view.1));
    Point::new(pan.x.clamp(-lx, lx), pan.y.clamp(-ly, ly))
}

/// The pan that keeps the point `at` (relative to the view's centre) on the same spot of the
/// picture when the zoom goes from `old` to `new`.
pub fn pan_for_zoom(pan: Point, at: Point, old: f32, new: f32) -> Point {
    let k = new / old.max(f32::MIN_POSITIVE);
    Point::new(at.x - (at.x - pan.x) * k, at.y - (at.y - pan.y) * k)
}

/// What's under the mouse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    None,
    Close,
    Fit,
    Actual,
    Fill,
    Prev,
    Next,
    /// The "i" button: the info strip on or off.
    Info,
    /// The WRAP switch (text files): long lines wrap, or run on to the right.
    Wrap,
    /// The video's play / pause button.
    Play,
    Seek,
    Mute,
    Volume,
    /// The picture (or text, or thumbnail) area.
    View,
}

/// Dragging on a video's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    Seek,
    Volume,
}

/// Where a video's controls are.
#[derive(Clone, Copy, Debug)]
struct Controls {
    bar: Rect,
    play: Rect,
    time: Rect,
    seek: Rect,
    mute: Rect,
    volume: Rect,
}

impl Controls {
    fn new(bar: Rect) -> Controls {
        let b = 32.0;
        let y = bar.y + (bar.h - b) / 2.0;
        let play = Rect::new(bar.x + 6.0, y, b, b);
        let time = Rect::new(play.right() + 6.0, bar.y, 96.0, bar.h);
        let volume = Rect::new(bar.right() - 10.0 - 72.0, bar.y, 72.0, bar.h);
        let mute = Rect::new(volume.x - 4.0 - b, y, b, b);
        let seek_x = time.right() + 6.0;
        let seek = Rect::new(seek_x, bar.y, (mute.x - 10.0 - seek_x).max(0.0), bar.h);
        Controls { bar, play, time, seek, mute, volume }
    }
}

/// An animated GIF on screen.
struct Anim {
    key: String,
    frames: Frames,
    start: Instant,
}

/// The picture at full size, for one item.
struct Full {
    key: String,
    picture: Option<Arc<Picture>>,
    /// Its size on disk (it may have been decoded smaller).
    original: (u32, u32),
}

/// What the pane showed at the last frame, for the mouse and the keys.
#[derive(Clone, Copy, Debug)]
struct Shown {
    view: Rect,
    /// A picture: its own size in pixels, and the zoom it was drawn at.
    picture: Option<((f32, f32), f32)>,
    /// Text: its rows, and how far it scrolls.
    text: Option<TextShown>,
    /// A video: its controls.
    controls: Option<Controls>,
}

/// A text file as the last frame showed it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TextShown {
    /// How many rows it takes (a wrapped line takes several), and how many fit in the pane.
    rows: usize,
    fits: usize,
    /// How many columns it can be scrolled sideways (0 when it wraps, or no line is too long).
    across: f32,
}

/// Where the top bar's buttons are.
struct Bar {
    close: Rect,
    fit: Rect,
    actual: Rect,
    fill: Rect,
    zoom: Rect,
    name: Rect,
    prev: Rect,
    next: Rect,
    info: Rect,
    wrap: Rect,
}

pub struct PreviewPane {
    rect: Rect,
    peeker: Peeker,
    facts: FactsCache,
    /// The key last asked of peek.rs.
    asked: String,
    full: Option<Full>,
    zoom: Zoom,
    /// Where the picture's centre sits, from the view's centre (DIPs).
    pan: Point,
    /// The item zoom, pan and scroll belong to.
    view_key: String,
    /// The first row of text shown (fractions while scrolling), and with wrap off how many
    /// columns it's scrolled sideways.
    scroll: f32,
    scroll_x: f32,
    /// The text on screen cut into rows (made again when the pane's width or WRAP changes).
    rows: Option<Rows>,
    /// Colours for those rows (highlight.rs), for a code or settings file: made with them.
    colours: Option<crate::highlight::Painter>,
    /// The coloured runs of the row being drawn (kept to save making a list for every row).
    runs: Vec<(usize, usize, crate::highlight::Kind)>,
    hover: Hit,
    pressed: Hit,
    /// Dragging the picture: where the mouse went down and the pan then.
    panning: Option<(Point, Point)>,
    scale: f32,
    shown: Option<Shown>,
    /// The animated GIF on screen, its frames and when it started.
    anim: Option<Anim>,
    /// The video being played (only after play was pressed), and the item it's for.
    video: Option<Video>,
    video_key: String,
    /// The video on screen couldn't be played.
    video_failed: bool,
    /// Videos' volume (0 to 1, read from the settings at first) and mute.
    volume: Option<f32>,
    muted: bool,
    dragging: Option<Drag>,
    /// The item shown last, and whether it is inside a .zip or .7z (found out once for each: it
    /// may ask the disk).
    archived: (String, bool),
    hwnd: HWND,
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewPane {
    pub fn new() -> PreviewPane {
        PreviewPane {
            rect: Rect::default(),
            peeker: Peeker::new("space preview"),
            // (Text files are read 200 KB at a time here: the last few big ones are kept, or many
            // small ones, not 32 big ones.)
            facts: FactsCache::with_budget(32, 2 * 1024 * 1024),
            asked: String::new(),
            full: None,
            zoom: Zoom::Fit,
            pan: Point::default(),
            view_key: String::new(),
            scroll: 0.0,
            scroll_x: 0.0,
            rows: None,
            colours: None,
            runs: Vec::new(),
            hover: Hit::None,
            pressed: Hit::None,
            panning: None,
            scale: 1.0,
            shown: None,
            anim: None,
            video: None,
            video_key: String::new(),
            video_failed: false,
            volume: None,
            muted: false,
            dragging: None,
            archived: (String::new(), false),
            hwnd: HWND::default(),
        }
    }

    pub fn layout(&mut self, rect: Rect) {
        self.rect = rect;
        // No room for the pane next to the list (side.rs), or it isn't showing: a video stops, and
        // nothing repaints the window for it or a GIF meanwhile (the GIF goes on once it shows).
        if rect.is_empty() {
            self.stop_video();
        }
    }

    /// The pane was closed: the full-size picture and whatever was waiting go.
    pub fn closed(&mut self) {
        self.peeker.cancel();
        self.full = None;
        self.asked.clear();
        self.view_key.clear();
        self.facts.clear();
        self.rows = None;
        self.panning = None;
        self.hover = Hit::None;
        self.pressed = Hit::None;
        self.shown = None;
        self.stop_media();
        if !matches!(self.zoom, Zoom::Custom(_)) {
            return;
        }
        self.zoom = Zoom::Fit;
    }

    fn bar(&self) -> Bar {
        let r = self.rect;
        let y = r.y + 4.0;
        let b = 32.0;
        let close = Rect::new(r.x + 6.0, y, b, b);
        let fit = Rect::new(close.right() + 10.0, y, b, b);
        let actual = Rect::new(fit.right() + 2.0, y, b, b);
        let fill = Rect::new(actual.right() + 2.0, y, b, b);
        let zoom = Rect::new(fill.right() + 6.0, r.y, 52.0, TOP_H);
        let next = Rect::new(r.right() - 6.0 - b, y, b, b);
        let prev = Rect::new(next.x - 2.0 - b, y, b, b);
        let info = Rect::new(prev.x - 8.0 - b, y, b, b);
        let name_x = zoom.right() + 8.0;
        let name = Rect::new(name_x, r.y, (info.x - 8.0 - name_x).max(0.0), TOP_H);
        // WRAP (text files only) sits where a picture's zoom is said; in the narrowest pane it
        // gives way to the "i" button.
        let wrap_x = fill.right() + 4.0;
        let wrap = Rect::new(wrap_x, y, 46.0_f32.min((info.x - 2.0 - wrap_x).max(0.0)), b);
        Bar { close, fit, actual, fill, zoom, name, prev, next, info, wrap }
    }

    fn view_rect(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y + TOP_H + 1.0, self.rect.w, (self.rect.h - TOP_H - 1.0).max(0.0))
    }

    fn hit(&self, p: Point) -> Hit {
        let b = self.bar();
        let picture = self.shown.is_some_and(|s| s.picture.is_some());
        if b.close.contains(p) {
            Hit::Close
        } else if picture && b.fit.contains(p) {
            Hit::Fit
        } else if picture && b.actual.contains(p) {
            Hit::Actual
        } else if picture && b.fill.contains(p) {
            Hit::Fill
        } else if b.prev.contains(p) {
            Hit::Prev
        } else if b.next.contains(p) {
            Hit::Next
        } else if b.info.contains(p) {
            Hit::Info
        } else if self.shown.is_some_and(|s| s.text.is_some()) && b.wrap.contains(p) {
            Hit::Wrap
        } else if let Some(c) = self.shown.and_then(|s| s.controls).filter(|c| c.bar.contains(p)) {
            if c.play.contains(p) {
                Hit::Play
            } else if c.mute.contains(p) {
                Hit::Mute
            } else if c.seek.contains(p) {
                Hit::Seek
            } else if c.volume.contains(p) {
                Hit::Volume
            } else {
                Hit::None
            }
        } else if self.shown.map_or(self.view_rect(), |s| s.view).contains(p) {
            Hit::View
        } else {
            Hit::None
        }
    }

    fn take_answers(&mut self) {
        for a in self.peeker.take() {
            match a {
                Answer::Facts { key, facts } => self.facts.insert(key, facts),
                // Only the item on screen keeps its picture at full size.
                Answer::Picture { key, picture, original } if key == self.view_key => self.full = Some(Full { key, picture, original }),
                Answer::Frames { key, frames } if key == self.view_key => self.anim = Some(Anim { key, frames, start: Instant::now() }),
                Answer::Picture { .. } | Answer::Frames { .. } => {}
            }
        }
    }

    /// A new item came on screen: start from its top, at the same zoom mode, nothing panned.
    fn new_item(&mut self, key: &str) {
        self.view_key = key.to_string();
        self.full = None;
        self.stop_media();
        self.pan = Point::default();
        self.scroll = 0.0;
        self.scroll_x = 0.0;
        self.rows = None;
        self.panning = None;
        if matches!(self.zoom, Zoom::Custom(_)) {
            self.zoom = Zoom::Fit;
        }
    }

    pub fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        let rect = self.rect;
        if rect.is_empty() {
            return;
        }
        self.scale = ctx.scale;
        self.hwnd = ctx.hwnd;
        if self.volume.is_none() {
            self.volume = Some(app.settings.preview_volume);
        }
        self.take_answers();
        r.fill_paint(rect, 0.0, &theme.through(&theme.panel_paint));
        r.push_clip(rect);
        let one = details::focused(app);
        if let Some(o) = &one {
            if o.key != self.view_key {
                self.new_item(&o.key);
            }
            // The full picture was lost with the graphics device: decode it again.
            let anim_lost = self.anim.as_ref().is_some_and(|a| a.frames.pictures.iter().any(|p| p.is_lost()));
            if anim_lost || self.full.as_ref().is_some_and(|f| f.picture.as_ref().is_some_and(|p| p.is_lost())) {
                self.full = None;
                self.anim = None;
                self.asked.clear();
            }
            let picture = o.is_file() && thumbs::is_picture(&o.entry.ext);
            let need_facts = self.facts.get(&o.key).is_none();
            let need_full = picture && self.full.is_none();
            if (need_facts || need_full) && self.asked != o.key {
                let pic = picture.then(|| (r.max_bitmap_size().min(16384), MAX_PIXELS));
                // (Not asked, with no thread to be had: asked again at the next frame.)
                if self.peeker.ask(o.ask(Some(peek::PREVIEW_TEXT), pic, app.settings.show_hidden, app.settings.show_system), ctx.hwnd) {
                    self.asked = o.key.clone();
                }
            }
        } else {
            // Nothing selected (a click on empty space, a folder opened): a video stops too, and an
            // item that comes back is asked about again.
            self.view_key.clear();
            self.full = None;
            self.rows = None;
            self.asked.clear();
            self.stop_media();
        }
        let view = self.view_rect();
        self.shown = Some(Shown { view, picture: None, text: None, controls: None });
        let mut wake: Option<u64> = None;
        match &one {
            None => {
                r.text("Select a file to preview it", view, &TextStyle::new(theme.muted).align(Align::Center));
            }
            Some(o) => wake = self.paint_item(r, theme, app, o, view),
        }
        self.paint_bar(r, theme, app, one.as_ref());
        r.pop_clip();
        // A GIF's next frame, or the video's: the window is repainted then.
        match wake {
            Some(ms) => video::wake_in(self.hwnd, ms.min(60_000) as u32),
            None => video::stop_waking(self.hwnd),
        }
    }

    /// The GIF and the video go (another item, or the pane closed): the video's sound fades out.
    fn stop_media(&mut self) {
        self.anim = None;
        self.stop_video();
    }

    /// Something is about to delete, rename or move the selection, or a drive is being ejected: the
    /// video lets go of its file now, without the fade (Windows refuses those while it's open).
    pub fn release_files(&mut self) {
        video::let_go(self.video.take());
        self.stop_video();
    }

    /// The video goes (its sound fades out), and nothing repaints the window for it or a GIF.
    fn stop_video(&mut self) {
        self.video = None;
        self.video_key.clear();
        self.video_failed = false;
        self.dragging = None;
        if self.hwnd != HWND::default() {
            video::stop_waking(self.hwnd);
        }
    }

    /// Draws the item; returns in how many milliseconds to draw again (a GIF or a video playing).
    fn paint_item(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, one: &One, view: Rect) -> Option<u64> {
        let s = self.scale;
        let facts = self.facts.get(&one.key).cloned();
        let is_picture = one.is_file() && thumbs::is_picture(&one.entry.ext);
        let full_failed = self.full.as_ref().is_some_and(|f| f.key == one.key && f.picture.is_none());
        // The text of a text file (by name, or sniffed when there's no real thumbnail).
        let by_name = one.is_file() && peek::is_text_file(&one.entry.ext);
        let thumb_px = ((view.w.max(view.h) * s) as u32).min(1024);
        let waiting_for_text = by_name && facts.as_ref().is_none_or(|f| f.text.is_some());
        let thumb = if waiting_for_text { None } else { thumbs::request_versioned(&one.shell_path, one.version, thumb_px) };
        let real_thumb = thumb.as_ref().filter(|t| !t.is_icon()).cloned();
        let is_video = self.plays(one);
        if (is_picture && !(full_failed && real_thumb.is_none())) || is_video {
            // The info strip and the video's controls go under the picture.
            let gif = self.anim.as_ref().filter(|a| a.key == one.key).map(|a| (a.frames.count, a.frames.total_ms()));
            let video_dims = self.video.as_ref().filter(|_| self.video_key == one.key).and_then(|v| v.size());
            let decoded = self.full.as_ref().filter(|f| f.key == one.key && f.original.0 > 0).map(|f| f.original);
            let dims = facts.as_ref().and_then(|f| f.dims).or(video_dims).or(decoded);
            let lines = if app.settings.preview_info {
                let taken = facts.as_ref().and_then(|f| f.media.taken).map(|t| folder::format_date_as(t, app.settings.date_format));
                let duration = facts.as_ref().and_then(|f| f.duration).filter(|_| is_video);
                media_lines(dims, duration, facts.as_ref().map(|f| &f.media), gif, taken)
            } else {
                Vec::new()
            };
            let info_h = if lines.is_empty() { 0.0 } else { lines.len() as f32 * INFO_LINE + 12.0 };
            let controls_h = if is_video { CONTROLS_H } else { 0.0 };
            let pic = Rect::new(view.x, view.y, view.w, (view.h - info_h - controls_h).max(0.0));
            if let Some(sh) = self.shown.as_mut() {
                sh.view = pic;
            }
            let thumb = real_thumb.or(thumb);
            let wake = if is_video {
                self.paint_video(r, theme, pic, one, thumb)
            } else {
                self.paint_picture(r, theme, pic, facts.as_ref().and_then(|f| f.dims), thumb)
            };
            if !lines.is_empty() {
                let strip = Rect::new(view.x, pic.bottom(), view.w, info_h);
                r.fill_rect(strip, theme.window.with_alpha(0.5));
                r.fill_rect(Rect::new(strip.x, strip.y, strip.w, 1.0), theme.line);
                let mut y = strip.y + 6.0;
                for (i, line) in lines.iter().enumerate() {
                    let color = if i == 0 { theme.text } else { theme.muted };
                    r.text(line, Rect::new(strip.x + 12.0, y, (strip.w - 24.0).max(0.0), INFO_LINE), &TextStyle::new(color).size(12.0));
                    y += INFO_LINE;
                }
            }
            if is_video {
                let duration = facts.as_ref().and_then(|f| f.duration).map_or(0.0, |t| t as f64 / 10_000_000.0);
                self.paint_controls(r, theme, Rect::new(view.x, view.bottom() - CONTROLS_H, view.w, CONTROLS_H), duration);
            }
            return wake;
        }
        let text = facts.as_ref().and_then(|f| f.text.clone()).filter(|_| by_name || real_thumb.is_none());
        if let Some(text) = text {
            self.paint_text(r, theme, view, &text, app.settings.preview_wrap, crate::highlight::lang_for(&one.entry.ext));
            return None;
        }
        if waiting_for_text {
            return None; // its lines are on their way
        }
        // Anything else: the big thumbnail, and the details under it.
        let pad = 18.0;
        let img = Rect::new(view.x + pad, view.y + pad, (view.w - 2.0 * pad).max(0.0), (view.h * 0.55).max(80.0).min(view.h - 2.0 * pad).max(0.0));
        if let Some(t) = &thumb {
            details::draw_fitted(r, t, img, s, 256.0);
        }
        let mut info = details::info(&Sel::One(Box::new(one.clone())), facts.as_ref(), app);
        // Inside a .zip or .7z its size is the unpacked size, as the details panel says.
        if one.is_file() {
            if self.archived.0 != one.path {
                let inside = folder::parent(&one.path).is_some_and(|p| crate::archive::split(&p).is_some());
                self.archived = (one.path.clone(), inside);
            }
            if self.archived.1 {
                details::mark_unpacked(&mut info, &folder::format_size_as(one.entry.size, app.settings.size_format));
            }
        }
        let area = Rect::new(view.x + pad, img.bottom() + 16.0, (view.w - 2.0 * pad).max(0.0), (view.bottom() - img.bottom() - 16.0 - pad).max(0.0));
        details::draw_info(r, theme, area, &info);
        None
    }

    /// A picture on the checkerboard, zoomed and panned.
    fn paint_picture(&mut self, r: &mut Renderer, theme: &Theme, view: Rect, dims: Option<(u32, u32)>, thumb: Option<Arc<Picture>>) -> Option<u64> {
        let s = self.scale;
        let (a, b) = if theme.dark { (Color::hex(0x1c1c1f), Color::hex(0x242428)) } else { (Color::hex(0xf4f4f6), Color::hex(0xe4e4e8)) };
        r.fill_checker(view, 8.0, a, b);
        let mut wake = None;
        let mut full = self.full.as_ref().filter(|f| f.key == self.view_key).and_then(|f| f.picture.clone().map(|p| (p, f.original)));
        // An animated GIF: the frame for now, and when the next one is due.
        if let Some(a) = self.anim.as_ref().filter(|a| a.key == self.view_key) {
            let (i, next) = anim::frame_at(&a.frames.delays, a.start.elapsed().as_millis() as u64);
            if let Some(p) = a.frames.pictures.get(i) {
                full = Some((p.clone(), a.frames.size));
                wake = Some(next.max(10));
            }
        }
        // The picture's own size: from the full decode, the shell, or (for now) the thumbnail.
        let natural = match (&full, dims, &thumb) {
            (Some((_, (w, h))), _, _) if *w > 0 => (*w as f32, *h as f32),
            (_, Some((w, h)), _) => (w as f32, h as f32),
            (_, _, Some(t)) if !t.is_icon() => (t.width() as f32, t.height() as f32),
            _ => {
                if let Some(t) = &thumb {
                    details::draw_fitted(r, t, view.inset(24.0, 24.0), s, 256.0);
                }
                return wake;
            }
        };
        let z = zoom_value(self.zoom, natural, (view.w * s, view.h * s));
        let draw = (natural.0 * z / s, natural.1 * z / s);
        self.pan = clamp_pan(self.pan, draw, (view.w, view.h));
        let c = Point::new(view.x + view.w / 2.0 + self.pan.x, view.y + view.h / 2.0 + self.pan.y);
        let dest = Rect::new(c.x - draw.0 / 2.0, c.y - draw.1 / 2.0, draw.0, draw.1);
        r.push_clip(view);
        let mut drawn = false;
        if let Some((pic, _)) = &full {
            // Zoomed in a lot, each pixel stays a sharp square.
            let per_pixel = draw.0 * s / pic.width().max(1) as f32;
            drawn = r.picture(pic, dest, None, 1.0, per_pixel < 2.0);
        }
        if !drawn {
            if let Some(t) = thumb.filter(|t| !t.is_icon()) {
                r.picture(&t, dest, None, 1.0, true);
            }
        }
        r.pop_clip();
        if let Some(sh) = self.shown.as_mut() {
            sh.picture = Some((natural, z));
        }
        wake
    }

    /// A video: its newest frame once it plays, before that its thumbnail with a big play button.
    fn paint_video(&mut self, r: &mut Renderer, theme: &Theme, area: Rect, one: &One, thumb: Option<Arc<Picture>>) -> Option<u64> {
        let s = self.scale;
        r.fill_rect(area, theme.window);
        let mut wake = None;
        let mut playing = false;
        let mut frame = None;
        if let Some(v) = self.video.as_mut().filter(|_| self.video_key == one.key) {
            let px = (area.w.max(area.h) * s) as u32;
            if v.tick(px.clamp(64, r.max_bitmap_size().min(4096))) {
                wake = Some(video::FRAME_MS as u64);
            }
            if v.failed() {
                self.video_failed = true;
            }
            playing = v.is_playing();
            frame = v.frame();
        }
        if self.cant_play(one) && self.video.is_some() {
            // Its engine goes (it would keep the file open, and Windows wouldn't delete it).
            self.video = None;
        }
        let shown = frame.or(thumb.filter(|t| !t.is_icon()));
        r.push_clip(area);
        match &shown {
            Some(p) => {
                let dest = fit_rect((p.width() as f32, p.height() as f32), area);
                r.picture(p, dest, None, 1.0, true);
            }
            None => {
                if let Some(t) = thumbs::request_versioned(&one.shell_path, one.version, 256) {
                    details::draw_fitted(r, &t, area.inset(24.0, 24.0), s, 128.0);
                }
            }
        }
        let c = Point::new(area.x + area.w / 2.0, area.y + area.h / 2.0);
        if self.cant_play(one) {
            // No play button (pressing it does nothing now), just why.
            let note = Rect::new(area.x + 12.0, c.y - 11.0, (area.w - 24.0).max(0.0), 22.0);
            r.fill_rect(note.inset(0.0, -6.0), theme.window.with_alpha(0.75));
            r.text(CANT_PLAY, note, &TextStyle::new(theme.text_strong).align(Align::Center));
        } else if !playing {
            // The big play button.
            let rad = 30.0f32.min(area.h / 3.0).max(0.0);
            if rad > 8.0 {
                let hover = self.hover == Hit::View;
                r.fill_round_rect(Rect::new(c.x - rad, c.y - rad, rad * 2.0, rad * 2.0), rad, theme.accent.with_alpha(if hover { 1.0 } else { 0.85 }));
                let t = rad * 0.45;
                r.fill_polygon(&[Point::new(c.x - t * 0.7, c.y - t), Point::new(c.x + t, c.y), Point::new(c.x - t * 0.7, c.y + t)], theme.on_accent);
            }
        }
        r.pop_clip();
        wake
    }

    /// Play / pause, the time, the seek bar, mute and the volume.
    fn paint_controls(&mut self, r: &mut Renderer, theme: &Theme, bar: Rect, known_duration: f64) {
        let c = Controls::new(bar);
        if let Some(sh) = self.shown.as_mut() {
            sh.controls = Some(c);
        }
        r.fill_paint(bar, 0.0, &theme.through(&theme.list_header));
        r.fill_rect(Rect::new(bar.x, bar.y, bar.w, 1.0), theme.line);
        let v = self.video.as_ref().filter(|_| self.video_key == self.view_key);
        let playing = v.is_some_and(|v| v.is_playing());
        let (pos, dur) = match v {
            Some(v) if v.duration() > 0.0 => (v.position(), v.duration()),
            _ => (0.0, known_duration),
        };
        let (hover, pressed) = (self.hover, self.pressed);
        let hover_bg = |r: &mut Renderer, rect: Rect, hit: Hit| {
            if hover == hit {
                fill_box(r, theme, rect, 0.0, if pressed == hit { theme.button_pressed } else { theme.button_hover });
            }
        };
        // Play / pause.
        hover_bg(r, c.play, Hit::Play);
        let m = Point::new(c.play.x + c.play.w / 2.0, c.play.y + c.play.h / 2.0);
        if playing {
            r.fill_rect(Rect::new(m.x - 5.0, m.y - 6.0, 3.5, 12.0), theme.text);
            r.fill_rect(Rect::new(m.x + 1.5, m.y - 6.0, 3.5, 12.0), theme.text);
        } else {
            r.fill_polygon(&[Point::new(m.x - 4.0, m.y - 7.0), Point::new(m.x + 7.0, m.y), Point::new(m.x - 4.0, m.y + 7.0)], theme.text);
        }
        // The time.
        r.text(&format!("{} / {}", video::clock(pos), video::clock(dur)), c.time, &TextStyle::new(theme.muted).size(12.0));
        // The seek bar.
        let share = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
        slider(r, theme, c.seek, share, hover == Hit::Seek || self.dragging == Some(Drag::Seek));
        // Mute: a speaker, with waves or a cross.
        hover_bg(r, c.mute, Hit::Mute);
        let m = Point::new(c.mute.x + c.mute.w / 2.0 - 3.0, c.mute.y + c.mute.h / 2.0);
        let color = theme.text;
        r.fill_polygon(
            &[
                Point::new(m.x - 6.0, m.y - 3.0),
                Point::new(m.x - 3.0, m.y - 3.0),
                Point::new(m.x + 2.0, m.y - 7.0),
                Point::new(m.x + 2.0, m.y + 7.0),
                Point::new(m.x - 3.0, m.y + 3.0),
                Point::new(m.x - 6.0, m.y + 3.0),
            ],
            color,
        );
        let volume = self.volume.unwrap_or(0.5);
        if self.muted || volume <= 0.0 {
            r.line(Point::new(m.x + 5.0, m.y - 3.5), Point::new(m.x + 12.0, m.y + 3.5), 1.4, color);
            r.line(Point::new(m.x + 12.0, m.y - 3.5), Point::new(m.x + 5.0, m.y + 3.5), 1.4, color);
        } else {
            r.polyline(&[Point::new(m.x + 5.0, m.y - 3.0), Point::new(m.x + 6.5, m.y), Point::new(m.x + 5.0, m.y + 3.0)], 1.3, color);
            if volume > 0.5 {
                r.polyline(&[Point::new(m.x + 8.0, m.y - 6.0), Point::new(m.x + 10.5, m.y), Point::new(m.x + 8.0, m.y + 6.0)], 1.3, color);
            }
        }
        // The volume.
        slider(r, theme, c.volume, if self.muted { 0.0 } else { volume }, hover == Hit::Volume || self.dragging == Some(Drag::Volume));
    }

    /// Whether the item is a video the pane offers to play. A .ts is a TV recording (an MPEG
    /// transport stream) or TypeScript: it's a video once peek.rs has read its start and found no
    /// text there (until then the pane waits, as for any text file).
    fn plays(&self, one: &One) -> bool {
        one.is_file() && video::is_video(&one.entry.ext) && (one.entry.ext != "ts" || self.facts.get(&one.key).is_some_and(|f| f.text.is_none()))
    }

    /// Whether the video `one` was found not to play (it isn't tried again until it changes or
    /// another item is shown).
    fn cant_play(&self, one: &One) -> bool {
        self.video_failed && self.video_key == one.key
    }

    /// Plays or pauses the video on screen (it's opened the first time). Nothing for one that
    /// can't be played.
    fn toggle_video(&mut self, app: &mut AppState) -> Effect {
        let Some(one) = details::focused(app).filter(|o| self.plays(o)) else { return Effect::None };
        if self.cant_play(&one) {
            return Effect::None;
        }
        if self.video_key != one.key || self.video.is_none() {
            self.video = None;
            self.video_key = one.key.clone();
            self.video_failed = false;
            match Video::open(&one.path, self.hwnd, self.volume.unwrap_or(app.settings.preview_volume), self.muted) {
                Some(v) => self.video = Some(v),
                None => {
                    self.video_failed = true;
                    return Effect::Redraw;
                }
            }
        }
        if let Some(v) = self.video.as_mut() {
            v.toggle();
        }
        Effect::Redraw
    }

    /// The seek bar or the volume was clicked or dragged to `x`.
    fn drag_to(&mut self, drag: Drag, x: f32) {
        let Some(c) = self.shown.and_then(|s| s.controls) else { return };
        let track = if drag == Drag::Seek { c.seek } else { c.volume };
        let share = slider_share(track, x);
        match drag {
            Drag::Seek => {
                if let Some(v) = self.video.as_mut().filter(|_| self.video_key == self.view_key && !self.video_failed) {
                    let d = v.duration();
                    v.seek(d * share as f64);
                }
            }
            Drag::Volume => {
                self.volume = Some(share);
                if share > 0.0 && self.muted {
                    self.muted = false;
                    if let Some(v) = self.video.as_mut() {
                        v.set_muted(false);
                    }
                }
                if let Some(v) = self.video.as_mut() {
                    v.set_volume(share);
                }
            }
        }
    }

    /// The first 200 KB of a text file, with line numbers. Long lines wrap inside the pane; with
    /// `wrap` off they run on to the right, and Shift+wheel scrolls to the rest of them.
    fn paint_text(&mut self, r: &mut Renderer, theme: &Theme, view: Rect, text: &Arc<peek::Text>, wrap: bool, lang: Option<&'static crate::highlight::Lang>) {
        let inner = view.inset(12.0, 10.0);
        let fits = (inner.h / MONO_LINE).floor().max(1.0) as usize;
        if text.lines.is_empty() && !text.cut {
            self.rows = None;
            if let Some(sh) = self.shown.as_mut() {
                sh.text = Some(TextShown { rows: 0, fits, across: 0.0 });
            }
            r.text("Empty file", view, &TextStyle::new(theme.muted).align(Align::Center));
            return;
        }
        let digits = text.lines.len().max(1).to_string().len();
        let cw = r.mono_char_width(MONO_FONT).max(1.0);
        let gutter = digits as f32 * cw + 14.0;
        // Where the text itself goes: right of the numbers, up to the scrollbar.
        let area = Rect::new(inner.x + gutter, view.y, (inner.w - gutter - 8.0).max(0.0), view.h);
        let cols = ((area.w / cw) as usize).max(MIN_COLS);
        self.lay_out(text, cols, wrap, lang);
        let Some((rows, across)) = self.rows.as_ref().map(|rows| (rows.len(), rows.across())) else { return };
        let total = rows + usize::from(text.cut);
        if let Some(sh) = self.shown.as_mut() {
            sh.text = Some(TextShown { rows: total, fits, across });
        }
        let max_scroll = total.saturating_sub(fits) as f32;
        self.scroll = self.scroll.clamp(0.0, max_scroll);
        self.scroll_x = self.scroll_x.clamp(0.0, across);
        let (scroll, scroll_x) = (self.scroll, self.scroll_x);
        let Some(rows) = self.rows.as_ref() else { return };
        let (colours, runs) = (&mut self.colours, &mut self.runs);
        let first = scroll.floor() as usize;
        let top = inner.y - scroll.fract() * MONO_LINE;
        let on_screen = (first..total).map(|n| (n, top + (n - first) as f32 * MONO_LINE)).take_while(|&(_, y)| y <= inner.bottom());
        let color = details::blend(theme.text, theme.panel, 0.08);
        r.push_clip(view);
        r.fill_rect(Rect::new(view.x, view.y, inner.x - view.x + gutter - 6.0, view.h), theme.window.with_alpha(0.35));
        // The numbers are the file's own line numbers: a line has its number on its first row,
        // and the rows it wraps onto have none.
        for (n, y) in on_screen.clone() {
            if let Some(number) = rows.number(n) {
                let number = format!("{number:>digits$}");
                r.mono_text(&number, Rect::new(inner.x, y, gutter, MONO_LINE), MONO_FONT, theme.muted);
            }
        }
        // The text, kept off the numbers while it's scrolled sideways. Only the part of a row
        // that's on screen is handed over to be drawn, however long its line is.
        r.push_clip(area);
        for (n, y) in on_screen {
            match rows.row(n) {
                Some((line, start, end)) => {
                    let (piece, col) = columns(&text.lines[line][start..end], scroll_x.floor() as usize, cols + 2);
                    if piece.is_empty() {
                        continue;
                    }
                    let x = area.x + (col as f32 - scroll_x) * cw;
                    let Some(colours) = colours.as_mut() else {
                        mono_run(r, piece, x, y, cw, area.right(), color);
                        continue;
                    };
                    // In colour: each run where its columns start. (Only what's on screen is
                    // coloured: the row's runs from its first byte shown to its last.)
                    let from = piece.as_ptr() as usize - text.lines[line].as_ptr() as usize;
                    colours.runs(n, |k| rows.row(k), &text.lines, from, from + piece.len(), runs);
                    let mut at = 0;
                    for &(a, b, kind) in runs.iter() {
                        let run = &text.lines[line][a..b];
                        mono_run(r, run, x + at as f32 * cw, y, cw, area.right(), crate::highlight::color(theme, kind, color));
                        at += width(run);
                    }
                }
                None => r.text("Only the first 200 KB is shown", Rect::new(area.x, y, area.w, MONO_LINE), &TextStyle::new(theme.muted).size(11.5)),
            }
        }
        r.pop_clip();
        // A thin scrollbar when it's longer than the pane, and one along the bottom when lines
        // run past its right edge.
        if total > fits {
            let track = Rect::new(view.right() - 6.0 - 3.0, view.y + 4.0, 6.0, view.h - 8.0);
            let h = (track.h * fits as f32 / total as f32).max(24.0).min(track.h);
            let top = track.y + (track.h - h) * (scroll / max_scroll.max(1.0));
            r.fill_paint(Rect::new(track.x, top, track.w, h), 3.0, &theme.scroll_thumb_paint);
        }
        if across > 0.0 {
            let track = Rect::new(area.x, view.bottom() - 6.0 - 3.0, area.w, 6.0);
            let w = (track.w * cols as f32 / (cols as f32 + across)).max(24.0).min(track.w);
            let left = track.x + (track.w - w) * (scroll_x / across);
            r.fill_paint(Rect::new(left, track.y, w, track.h), 3.0, &theme.scroll_thumb_paint);
        }
        r.pop_clip();
    }

    /// Cuts the text into rows for a pane `cols` columns wide - again only when the text, the
    /// width or WRAP changed. The line that was at the top stays at the top.
    fn lay_out(&mut self, text: &Arc<peek::Text>, cols: usize, wrap: bool, lang: Option<&'static crate::highlight::Lang>) {
        if self.rows.as_ref().is_some_and(|rows| rows.is_for(text, cols, wrap)) {
            return;
        }
        self.colours = lang.map(crate::highlight::Painter::new);
        let top = self.rows.as_ref().filter(|rows| Arc::ptr_eq(&rows.text, text)).and_then(|rows| rows.row(self.scroll.floor() as usize));
        let rows = Rows::new(text.clone(), cols, wrap);
        if let Some((line, start, _)) = top {
            self.scroll = rows.row_at(line, start) as f32 + self.scroll.fract();
        }
        self.rows = Some(rows);
    }

    fn paint_bar(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, one: Option<&One>) {
        let bar = self.bar();
        // The bar takes the look of the file list's column titles.
        r.fill_paint(Rect::new(self.rect.x, self.rect.y, self.rect.w, TOP_H), 0.0, &theme.through(&theme.list_header));
        r.fill_rect(Rect::new(self.rect.x, self.rect.y + TOP_H, self.rect.w, 1.0), theme.line);
        let picture = self.shown.and_then(|s| s.picture);
        let button_bg = |r: &mut Renderer, rect: Rect, hit: Hit, on: bool, enabled: bool| {
            let hovered = enabled && self.hover == hit;
            if on && (theme.era.old() || hovered) {
                // An older Windows' "on" toolbar button: pressed in (its selection is too dark here).
                // Any "on" button under the mouse lights up as the others do, keeping its bar.
                let fill = if theme.era.old() || self.pressed == hit { theme.button_pressed } else { theme.button_hover };
                fill_box(r, theme, rect, 0.0, fill);
                r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 2.0), theme.accent);
            } else if on {
                fill_box_paint(r, theme, rect, 0.0, &theme.accent_tint_paint);
                r.fill_rect(Rect::new(rect.x, rect.bottom() - 2.0, rect.w, 2.0), theme.accent);
            } else if enabled && self.hover == hit {
                fill_box(r, theme, rect, 0.0, if self.pressed == hit { theme.button_pressed } else { theme.button_hover });
            }
        };
        // ×
        button_bg(r, bar.close, Hit::Close, false, true);
        let c = Point::new(bar.close.x + bar.close.w / 2.0, bar.close.y + bar.close.h / 2.0);
        let glyph = if self.hover == Hit::Close { theme.text_strong } else { theme.text };
        r.line(Point::new(c.x - 5.0, c.y - 5.0), Point::new(c.x + 5.0, c.y + 5.0), 1.3, glyph);
        r.line(Point::new(c.x + 5.0, c.y - 5.0), Point::new(c.x - 5.0, c.y + 5.0), 1.3, glyph);
        // Fit, 1:1, fill (only for pictures).
        let zoom_color = if picture.is_some() { theme.text } else { theme.disabled };
        for (rect, hit, mode) in [(bar.fit, Hit::Fit, Zoom::Fit), (bar.actual, Hit::Actual, Zoom::Actual), (bar.fill, Hit::Fill, Zoom::Fill)] {
            button_bg(r, rect, hit, picture.is_some() && self.zoom == mode, picture.is_some());
            let c = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            match mode {
                Zoom::Fit => {
                    // Two arrows pointing in.
                    r.polyline(&[Point::new(c.x + 7.0, c.y - 7.0), Point::new(c.x + 1.5, c.y - 1.5)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x + 1.5, c.y - 5.5), Point::new(c.x + 1.5, c.y - 1.5), Point::new(c.x + 5.5, c.y - 1.5)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 7.0, c.y + 7.0), Point::new(c.x - 1.5, c.y + 1.5)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 1.5, c.y + 5.5), Point::new(c.x - 1.5, c.y + 1.5), Point::new(c.x - 5.5, c.y + 1.5)], 1.3, zoom_color);
                }
                Zoom::Fill => {
                    // Two arrows pointing out.
                    r.polyline(&[Point::new(c.x + 1.5, c.y - 1.5), Point::new(c.x + 7.0, c.y - 7.0)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x + 3.0, c.y - 7.0), Point::new(c.x + 7.0, c.y - 7.0), Point::new(c.x + 7.0, c.y - 3.0)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 1.5, c.y + 1.5), Point::new(c.x - 7.0, c.y + 7.0)], 1.3, zoom_color);
                    r.polyline(&[Point::new(c.x - 3.0, c.y + 7.0), Point::new(c.x - 7.0, c.y + 7.0), Point::new(c.x - 7.0, c.y + 3.0)], 1.3, zoom_color);
                }
                _ => r.text("1:1", rect, &TextStyle::new(zoom_color).size(11.0).semibold().align(Align::Center)),
            }
        }
        if let Some((_, z)) = picture {
            r.text(&zoom_text(z), bar.zoom, &TextStyle::new(theme.muted).align(Align::Center));
        }
        // WRAP (text files): lit while long lines wrap. (Its word gets the room around the
        // button too, so it's never cut short in the narrowest pane.)
        if self.shown.is_some_and(|s| s.text.is_some()) {
            button_bg(r, bar.wrap, Hit::Wrap, app.settings.preview_wrap, true);
            r.text("WRAP", bar.wrap.inset(-8.0, 0.0), &TextStyle::new(theme.text).size(11.0).semibold().align(Align::Center));
        }
        // The "i" button: the info strip under pictures and videos.
        button_bg(r, bar.info, Hit::Info, app.settings.preview_info, true);
        let c = Point::new(bar.info.x + bar.info.w / 2.0, bar.info.y + bar.info.h / 2.0);
        r.fill_round_rect(Rect::new(c.x - 1.0, c.y - 7.0, 2.0, 2.0), 1.0, theme.text);
        r.fill_rect(Rect::new(c.x - 0.8, c.y - 3.0, 1.6, 9.0), theme.text);
        // The name, then ‹ ›.
        if let Some(o) = one {
            r.text(&o.entry.name, bar.name, &TextStyle::new(theme.text).align(Align::Right));
        }
        let tab = app.tab();
        let row = one.and_then(|o| tab.row_of(o.index));
        let can_prev = row.is_some_and(|r| r > 0);
        let can_next = row.is_some_and(|r| r + 1 < tab.view.len());
        for (rect, hit, enabled, right) in [(bar.prev, Hit::Prev, can_prev, false), (bar.next, Hit::Next, can_next, true)] {
            button_bg(r, rect, hit, false, enabled);
            let c = Point::new(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
            let color = if enabled { theme.text } else { theme.disabled };
            if right {
                chevron_right(r, c, 5.0, color);
            } else {
                r.polyline(&[Point::new(c.x + 2.75, c.y - 5.0), Point::new(c.x - 2.75, c.y), Point::new(c.x + 2.75, c.y + 5.0)], 1.2, color);
            }
        }
    }

    // ------------------------------------------------------------ zooming, panning, stepping

    /// Zooms by `factor` around `at` (a point in the window), or the view's centre.
    fn zoom_by(&mut self, factor: f32, at: Option<Point>) -> Effect {
        let Some(sh) = self.shown else { return Effect::None };
        let Some((natural, old)) = sh.picture else { return Effect::None };
        let new = (old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let centre = Point::new(sh.view.x + sh.view.w / 2.0, sh.view.y + sh.view.h / 2.0);
        let at = at.map_or(Point::default(), |p| Point::new(p.x - centre.x, p.y - centre.y));
        let s = self.scale;
        let pan = pan_for_zoom(self.pan, at, old, new);
        self.pan = clamp_pan(pan, (natural.0 * new / s, natural.1 * new / s), (sh.view.w, sh.view.h));
        self.zoom = Zoom::Custom(new);
        Effect::Redraw
    }

    fn set_zoom(&mut self, z: Zoom) -> Effect {
        self.zoom = z;
        self.pan = Point::default();
        Effect::Redraw
    }

    /// Selects the item `delta` rows away from the one shown (‹ ›, Left and Right).
    pub fn step(&mut self, app: &mut AppState, delta: isize) -> Effect {
        let one = details::focused(app);
        let tab = app.tab_mut();
        let n = tab.view.len();
        let Some(row) = one.and_then(|o| tab.row_of(o.index)).or_else(|| tab.focus_row()) else { return Effect::None };
        if n == 0 {
            return Effect::None;
        }
        let to = (row as isize + delta).clamp(0, n as isize - 1) as usize;
        if to == row {
            return Effect::None;
        }
        tab.select_only_row(to);
        tab.reveal_focus = true;
        Effect::Redraw
    }

    /// Keys while the pane is open (the window asks before the file list): Esc closes, Left and
    /// Right step through the files.
    pub fn key_down(&mut self, vk: u16, mods: crate::ui::Mods, app: &mut AppState) -> Effect {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_LEFT, VK_RIGHT};
        if mods.ctrl || mods.alt || mods.shift {
            return Effect::None;
        }
        match windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk) {
            VK_ESCAPE => Effect::Command(Command::ClosePreview),
            VK_LEFT => self.step(app, -1),
            VK_RIGHT => self.step(app, 1),
            _ => Effect::None,
        }
    }

    /// Typed characters: + and - zoom a picture. None of the others.
    pub fn char_input(&mut self, ch: char) -> Effect {
        match ch {
            '+' | '=' => self.zoom_by(ZOOM_STEP, None),
            '-' | '_' => self.zoom_by(1.0 / ZOOM_STEP, None),
            _ => Effect::None,
        }
    }

    // ------------------------------------------------------------ the mouse

    pub fn cursor(&self, p: Point) -> CursorKind {
        if self.hit(p) == Hit::View && (self.can_pan() || self.shown.is_some_and(|s| s.controls.is_some())) {
            CursorKind::Hand
        } else {
            CursorKind::Arrow
        }
    }

    /// Whether the picture is bigger than the view (so dragging moves it).
    fn can_pan(&self) -> bool {
        let Some(sh) = self.shown else { return false };
        let Some((natural, z)) = sh.picture else { return false };
        natural.0 * z / self.scale > sh.view.w + 0.5 || natural.1 * z / self.scale > sh.view.h + 0.5
    }

    pub fn mouse_move(&mut self, e: &MouseEvent) -> Effect {
        if let Some(d) = self.dragging {
            self.drag_to(d, e.pos.x);
            return Effect::Redraw;
        }
        if let Some((start, pan0)) = self.panning {
            let Some(sh) = self.shown else { return Effect::None };
            let Some((natural, z)) = sh.picture else { return Effect::None };
            let pan = Point::new(pan0.x + e.pos.x - start.x, pan0.y + e.pos.y - start.y);
            self.pan = clamp_pan(pan, (natural.0 * z / self.scale, natural.1 * z / self.scale), (sh.view.w, sh.view.h));
            return Effect::Redraw;
        }
        let h = self.hit(e.pos);
        if h != self.hover {
            self.hover = h;
            return Effect::Redraw;
        }
        Effect::None
    }

    pub fn mouse_leave(&mut self) -> Effect {
        if self.hover != Hit::None && self.panning.is_none() && self.dragging.is_none() {
            self.hover = Hit::None;
            return Effect::Redraw;
        }
        Effect::None
    }

    pub fn mouse_down(&mut self, e: &MouseEvent) -> Effect {
        if e.button != MouseButton::Left {
            return Effect::None;
        }
        let h = self.hit(e.pos);
        self.hover = h;
        let drag = match h {
            Hit::Seek => Some(Drag::Seek),
            Hit::Volume => Some(Drag::Volume),
            _ => None,
        };
        if let Some(d) = drag {
            self.dragging = Some(d);
            self.drag_to(d, e.pos.x);
            return Effect::Redraw;
        }
        if h == Hit::View && self.shown.is_some_and(|s| s.controls.is_some()) {
            // A click on the video plays or pauses it.
            self.pressed = h;
            return Effect::None;
        }
        if h == Hit::View {
            if self.can_pan() {
                self.panning = Some((e.pos, self.pan));
            }
            return Effect::None;
        }
        self.pressed = h;
        Effect::Redraw
    }

    pub fn mouse_up(&mut self, e: &MouseEvent, app: &mut AppState) -> Effect {
        if e.button != MouseButton::Left {
            return Effect::None;
        }
        if self.panning.take().is_some() {
            return Effect::Redraw;
        }
        if let Some(d) = self.dragging.take() {
            if d == Drag::Volume {
                app.settings.preview_volume = self.volume.unwrap_or(0.5);
            }
            return Effect::Redraw;
        }
        let was = std::mem::replace(&mut self.pressed, Hit::None);
        if was == Hit::None || self.hit(e.pos) != was {
            return Effect::Redraw;
        }
        match was {
            Hit::Close => Effect::Command(Command::ClosePreview),
            Hit::Fit => self.set_zoom(Zoom::Fit),
            Hit::Actual => self.set_zoom(Zoom::Actual),
            Hit::Fill => self.set_zoom(Zoom::Fill),
            Hit::Prev => self.step(app, -1),
            Hit::Next => self.step(app, 1),
            Hit::Info => {
                app.settings.preview_info = !app.settings.preview_info;
                Effect::Redraw
            }
            Hit::Wrap => {
                app.settings.preview_wrap = !app.settings.preview_wrap;
                Effect::Redraw
            }
            Hit::Play | Hit::View => self.toggle_video(app),
            Hit::Mute => {
                self.muted = !self.muted;
                // Unmuting at no volume at all: back to the middle.
                if !self.muted && self.volume.is_some_and(|v| v <= 0.0) {
                    self.volume = Some(0.5);
                    app.settings.preview_volume = 0.5;
                    if let Some(v) = self.video.as_mut() {
                        v.set_volume(0.5);
                    }
                }
                if let Some(v) = self.video.as_mut() {
                    v.set_muted(self.muted);
                }
                Effect::Redraw
            }
            _ => Effect::Redraw,
        }
    }

    /// A double-click on the picture (or text, or thumbnail) opens the file - just the one shown,
    /// whatever else is selected.
    pub fn double_click(&mut self, e: &MouseEvent, app: &AppState) -> Effect {
        if e.button == MouseButton::Left && self.hit(e.pos) == Hit::View && self.shown.is_some() {
            if let Some(one) = details::focused(app).filter(|o| o.key == self.view_key) {
                self.panning = None;
                // The button coming up after this isn't a click on the video (it would play it
                // again), and the preview's copy stops: the file opens in its own player.
                self.pressed = Hit::None;
                if let Some(v) = self.video.as_mut().filter(|_| self.video_key == one.key) {
                    v.pause();
                }
                return Effect::Command(Command::OpenEntry(one.index));
            }
        }
        Effect::None
    }

    /// Ctrl+wheel zooms a picture around the mouse; the plain wheel scrolls text, and moves a
    /// picture that is bigger than the pane (Shift: sideways). Shift also scrolls text sideways
    /// when WRAP is off and a line runs past the pane.
    pub fn wheel(&mut self, e: &MouseEvent) -> Effect {
        let Some(sh) = self.shown else { return Effect::None };
        if sh.picture.is_some() {
            if e.mods.ctrl {
                return self.zoom_by(ZOOM_STEP.powf(e.wheel), Some(e.pos));
            }
            if !self.can_pan() {
                return Effect::None;
            }
            let (natural, z) = sh.picture.unwrap_or_default();
            let step = e.wheel * 80.0;
            let pan = if e.mods.shift { Point::new(self.pan.x + step, self.pan.y) } else { Point::new(self.pan.x, self.pan.y + step) };
            self.pan = clamp_pan(pan, (natural.0 * z / self.scale, natural.1 * z / self.scale), (sh.view.w, sh.view.h));
            return Effect::Redraw;
        }
        if let Some(text) = sh.text {
            if e.mods.shift && text.across > 0.0 {
                let to = (self.scroll_x - e.wheel * WHEEL_COLS).clamp(0.0, text.across);
                if to != self.scroll_x {
                    self.scroll_x = to;
                    return Effect::Redraw;
                }
                return Effect::None;
            }
            let max = text.rows.saturating_sub(text.fits) as f32;
            let to = (self.scroll - e.wheel * WHEEL_LINES).clamp(0.0, max);
            if to != self.scroll {
                self.scroll = to;
                return Effect::Redraw;
            }
        }
        Effect::None
    }
}

// ---------------------------------------------------------------- text rows

/// A text file's lines cut into the rows the pane shows: with wrap on a long line takes several
/// rows, with it off every line is one row. It's worked out once for a text, a width and the
/// WRAP switch, and scrolling then only looks rows up. It goes when another file is shown or the
/// pane closes.
struct Rows {
    text: Arc<peek::Text>,
    /// How many columns fit across the pane, and whether long lines wrap.
    cols: usize,
    wrap: bool,
    /// With wrap on: every row's line, and the byte of that line it starts at, top to bottom.
    /// With wrap off this stays empty: row n is simply line n.
    rows: Vec<(u32, u32)>,
    /// With wrap off: how many columns the longest line takes.
    widest: usize,
}

impl Rows {
    fn new(text: Arc<peek::Text>, cols: usize, wrap: bool) -> Rows {
        let mut rows = Vec::new();
        let mut widest = 0;
        if wrap {
            rows.reserve(text.lines.len());
            let mut starts = Vec::new();
            for (i, line) in text.lines.iter().enumerate() {
                starts.clear();
                wrap_line(line, cols, &mut starts);
                rows.extend(starts.iter().map(|&start| (i as u32, start)));
            }
        } else {
            widest = text.lines.iter().map(|line| width(line)).max().unwrap_or(0);
        }
        Rows { text, cols, wrap, rows, widest }
    }

    fn is_for(&self, text: &Arc<peek::Text>, cols: usize, wrap: bool) -> bool {
        Arc::ptr_eq(&self.text, text) && self.cols == cols && self.wrap == wrap
    }

    fn len(&self) -> usize {
        if self.wrap {
            self.rows.len()
        } else {
            self.text.lines.len()
        }
    }

    /// Row `n`: its line, and the bytes of the line it runs from and to.
    fn row(&self, n: usize) -> Option<(usize, usize, usize)> {
        if !self.wrap {
            return self.text.lines.get(n).map(|line| (n, 0, line.len()));
        }
        let &(line, start) = self.rows.get(n)?;
        let end = match self.rows.get(n + 1) {
            Some(&(next, at)) if next == line => at as usize,
            _ => self.text.lines.get(line as usize)?.len(),
        };
        Some((line as usize, start as usize, end))
    }

    /// The number to show beside row `n`: the file's own line number, on a line's first row
    /// only (the rows it wraps onto have none).
    fn number(&self, n: usize) -> Option<usize> {
        match self.row(n)? {
            (line, 0, _) => Some(line + 1),
            _ => None,
        }
    }

    /// The row that holds byte `at` of line `line`.
    fn row_at(&self, line: usize, at: usize) -> usize {
        if !self.wrap {
            return line;
        }
        // (The last row that starts at or before it.)
        self.rows.partition_point(|&(l, start)| (l as usize, start as usize) <= (line, at)).saturating_sub(1)
    }

    /// How many columns the text can be scrolled sideways: none when it wraps or every line fits,
    /// else to the end of the longest line and one column of air after it.
    fn across(&self) -> f32 {
        if self.wrap || self.widest <= self.cols {
            0.0
        } else {
            (self.widest - self.cols + 1) as f32
        }
    }
}

/// How many columns of the monospaced grid a character takes, as near as can be told without
/// measuring it: one for what the monospaced fonts have themselves (Latin, Greek, Cyrillic,
/// Hebrew, Arabic, punctuation, arrows, box drawing), none for marks that sit on the letter
/// before them, two for Chinese, Japanese and Korean, and three for emoji and symbols, which
/// Windows draws in another font that is wider than two columns. Where it can't know, it says
/// more rather than less: a row then wraps a little early, and nothing is cut off at the pane's
/// edge. It never says more columns than the character has bytes in UTF-8, so a line of n bytes
/// always fits in n columns.
fn cells(c: char) -> usize {
    match c as u32 {
        0x00..=0x2FF => 1,
        // Marks on the letter before (accents, Hebrew and Arabic points).
        0x300..=0x36F | 0x483..=0x489 | 0x591..=0x5BD | 0x5BF | 0x5C1 | 0x5C2 | 0x5C4 | 0x5C5 | 0x5C7 => 0,
        0x610..=0x61A | 0x64B..=0x65F | 0x670 | 0x6D6..=0x6DC | 0x6DF..=0x6E4 | 0x6E7 | 0x6E8 | 0x6EA..=0x6ED => 0,
        0x370..=0x6FF => 1,
        0x700..=0x7FF => 2,
        // Indian and South-East Asian writing up to Georgian: about a column each on average.
        0x800..=0x10FF => 1,
        0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F => 0,
        // Zero-width spaces and joiners, and the marks that say which way text runs.
        0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF => 0,
        // More Latin and Greek; punctuation, currency signs, arrows and maths; box drawing and shapes.
        0x1E00..=0x1FFF | 0x2000..=0x22FF | 0x2500..=0x25FF => 1,
        // Symbols and dingbats (many are drawn as emoji).
        0x2600..=0x27BF | 0x2B00..=0x2BFF => 3,
        // Half-width Japanese and Korean, and the "unknown character" sign.
        0xFF61..=0xFFDC | 0xFFF9..=0xFFFD => 1,
        0x1100..=0xFFFF => 2,
        // Skin tones and tags change the emoji before them.
        0x1F3FB..=0x1F3FF | 0xE0000..=0xE0FFF => 0,
        0x1F000..=0x1FAFF => 3,
        _ => 2,
    }
}

/// A line's characters, each with the byte it starts at and the columns it takes ([`cells`]).
/// What is drawn as part of the emoji before it takes none: an emoji joined on with a zero-width
/// joiner (the people of a family), and the second letter of a flag.
struct Widths<'a> {
    chars: std::str::CharIndices<'a>,
    joined: bool,
    flag: bool,
}

impl<'a> Widths<'a> {
    fn new(line: &'a str) -> Widths<'a> {
        Widths { chars: line.char_indices(), joined: false, flag: false }
    }
}

impl Iterator for Widths<'_> {
    type Item = (usize, char, usize);

    fn next(&mut self) -> Option<(usize, char, usize)> {
        let (at, c) = self.chars.next()?;
        let joined = std::mem::replace(&mut self.joined, c == '\u{200D}');
        let flag_letter = ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
        let second = flag_letter && self.flag;
        self.flag = flag_letter && !second;
        let w = cells(c);
        Some((at, c, if second || (joined && w == 3) { 0 } else { w }))
    }
}

/// How many columns a line takes.
fn width(line: &str) -> usize {
    if line.is_ascii() {
        line.len()
    } else {
        Widths::new(line).map(|(_, _, w)| w).sum()
    }
}

/// Where each row of a wrapped line starts (bytes; the first is always 0), for rows of `cols`
/// columns. A row ends at the last space before a word that doesn't fit, as in any word wrap;
/// a word longer than a row (a long path, minified code) is cut where the row ends. Spaces at
/// the end of a row never start the next one: past the edge they just aren't seen.
fn wrap_line(line: &str, cols: usize, starts: &mut Vec<u32>) {
    starts.push(0);
    let cols = cols.max(MIN_COLS);
    // (No character takes more columns than it has bytes, so this line fits as it is.)
    if line.len() <= cols {
        return;
    }
    // The row being filled: where it starts and the columns used; and the word being read: where
    // it starts, and the columns used before it.
    let (mut row, mut used) = (0, 0);
    let mut word: Option<(usize, usize)> = None;
    for (at, c, w) in Widths::new(line) {
        if matches!(c, ' ' | '\t' | '\u{3000}') {
            used += w;
            word = None;
            continue;
        }
        let (word_at, before) = *word.get_or_insert((at, used));
        if w > 0 && used + w > cols {
            if word_at > row && used - before + w <= cols {
                // The whole word goes down to the next row.
                starts.push(word_at as u32);
                (row, used) = (word_at, used - before);
            } else {
                starts.push(at as u32);
                (row, used) = (at, 0);
            }
            word = Some((row, 0));
        }
        used += w;
    }
}

/// The part of `s` that is on screen when it's scrolled `from` columns sideways and `count`
/// columns show: that part, and the column it starts at. (A wide character cut in half by the
/// left edge is left out, so it may start a column or two after `from`.)
fn columns(s: &str, from: usize, count: usize) -> (&str, usize) {
    if s.is_ascii() {
        let a = from.min(s.len());
        return (&s[a..(from + count).min(s.len())], a);
    }
    let (mut col, mut start) = (0, None);
    for (at, _, w) in Widths::new(s) {
        match start {
            None if col >= from && w > 0 => start = Some((at, col)),
            Some((a, first)) if col + w > from + count => return (&s[a..at], first),
            _ => {}
        }
        col += w;
    }
    match start {
        Some((a, first)) => (&s[a..], first),
        None => ("", col),
    }
}

/// Draws a run of monospaced text from `x`. Plain ASCII gets a box just as wide as its columns
/// (so the same word is laid out once, wherever it's drawn); anything else may be wider than
/// its columns say, and gets the room up to `right`.
fn mono_run(r: &mut Renderer, run: &str, x: f32, y: f32, cw: f32, right: f32, color: Color) {
    let fitted = (width(run) + 1) as f32 * cw;
    let w = if run.is_ascii() { fitted } else { fitted.max(right - x) };
    r.mono_text(run, Rect::new(x, y, w, MONO_LINE), MONO_FONT, color);
}

/// The biggest rectangle of `natural`'s shape inside `area`, centred.
fn fit_rect(natural: (f32, f32), area: Rect) -> Rect {
    let k = (area.w / natural.0.max(1.0)).min(area.h / natural.1.max(1.0));
    let (w, h) = (natural.0 * k, natural.1 * k);
    Rect::new(area.x + (area.w - w) / 2.0, area.y + (area.h - h) / 2.0, w, h)
}

/// A seek bar or volume slider: a thin track, the part up to `share` in the accent colour, and a
/// round knob.
fn slider(r: &mut Renderer, theme: &Theme, area: Rect, share: f32, hot: bool) {
    if area.w < 12.0 {
        return;
    }
    let y = area.y + area.h / 2.0;
    let track = Rect::new(area.x + 6.0, y - 2.0, area.w - 12.0, 4.0);
    r.fill_round_rect(track, 2.0, theme.bar_track);
    let x = track.x + track.w * share.clamp(0.0, 1.0);
    r.fill_round_rect(Rect::new(track.x, track.y, x - track.x, track.h), 2.0, theme.accent);
    let k = if hot { 7.0 } else { 5.5 };
    r.fill_round_rect(Rect::new(x - k, y - k, k * 2.0, k * 2.0), k, theme.accent);
}

/// Where along a slider's track the point `x` is (0 to 1).
fn slider_share(area: Rect, x: f32) -> f32 {
    let w = (area.w - 12.0).max(1.0);
    ((x - area.x - 6.0) / w).clamp(0.0, 1.0)
}

/// The info strip's lines for a picture, GIF or video: the size and format first, then the camera
/// and when it was taken. `gif`: an animation's frame count and how long one loop plays (ms);
/// `duration`: a video's length (100-nanosecond steps); `taken`: the date already written out.
pub fn media_lines(dims: Option<(u32, u32)>, duration: Option<u64>, media: Option<&peek::Media>, gif: Option<(u32, u64)>, taken: Option<String>) -> Vec<String> {
    let none = peek::Media::default();
    let m = media.unwrap_or(&none);
    let mut first = Vec::new();
    if let Some((w, h)) = dims {
        first.push(format!("{} \u{d7} {}", folder::format_count(w as usize), folder::format_count(h as usize)));
    }
    if let Some(f) = &m.format {
        first.push(f.clone());
    }
    if let Some(b) = m.bits {
        first.push(format!("{b}-bit"));
    }
    if let Some(d) = m.dpi {
        first.push(format!("{} DPI", d.round() as u32));
    }
    if let Some((count, ms)) = gif {
        first.push(format!("{} frames", folder::format_count(count as usize)));
        first.push(seconds_text(ms));
    } else if let Some(n) = m.frames.filter(|&n| n > 1) {
        first.push(format!("{} frames", folder::format_count(n as usize)));
    }
    if let Some(t) = duration {
        first.push(details::duration_text(t));
    }
    if let Some(fps) = m.fps {
        first.push(fps_text(fps));
    }
    if let Some(b) = m.bitrate {
        first.push(bitrate_text(b));
    }
    let mut second = Vec::new();
    if let Some(c) = &m.camera {
        second.push(c.clone());
    }
    if let Some(t) = taken.filter(|t| !t.is_empty()) {
        second.push(format!("Taken {t}"));
    }
    [first, second].into_iter().filter(|l| !l.is_empty()).map(|l| l.join("  \u{b7}  ")).collect()
}

/// How long a GIF's loop plays: "3.2 s", or "1:05" from a minute on.
pub fn seconds_text(ms: u64) -> String {
    if ms >= 60_000 {
        return details::duration_text(ms * 10_000);
    }
    let tenths = (ms + 50) / 100;
    if tenths.is_multiple_of(10) {
        format!("{} s", tenths / 10)
    } else {
        format!("{}.{} s", tenths / 10, tenths % 10)
    }
}

/// "30 fps", "29.97 fps".
pub fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.005 {
        format!("{} fps", fps.round() as u32)
    } else {
        format!("{:.2} fps", fps)
    }
}

/// "4.2 Mb/s", "320 kb/s".
pub fn bitrate_text(bps: u32) -> String {
    if bps >= 1_000_000 {
        format!("{:.1} Mb/s", bps as f64 / 1_000_000.0)
    } else {
        format!("{} kb/s", (bps + 500) / 1000)
    }
}

/// How the zoom reads in the top bar ("93%").
pub fn zoom_text(z: f32) -> String {
    format!("{}%", folder::format_count((z * 100.0).round().max(0.0) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_never_blows_up_and_fill_fills() {
        // A 4000 x 3000 photo in an 800 x 800 pane.
        assert_eq!(zoom_value(Zoom::Fit, (4000.0, 3000.0), (800.0, 800.0)), 0.2);
        assert!((zoom_value(Zoom::Fill, (4000.0, 3000.0), (800.0, 800.0)) - 800.0 / 3000.0).abs() < 1e-6);
        assert_eq!(zoom_value(Zoom::Actual, (4000.0, 3000.0), (800.0, 800.0)), 1.0);
        // A small icon-sized picture stays its own size with fit.
        assert_eq!(zoom_value(Zoom::Fit, (64.0, 48.0), (800.0, 800.0)), 1.0);
        assert_eq!(zoom_value(Zoom::Custom(2.5), (64.0, 48.0), (800.0, 800.0)), 2.5);
    }

    #[test]
    fn panning_stays_inside_the_picture() {
        // Smaller than the view: centred.
        assert_eq!(clamp_pan(Point::new(50.0, -20.0), (100.0, 100.0), (400.0, 300.0)), Point::new(0.0, 0.0));
        // Bigger: at most half the difference either way.
        assert_eq!(clamp_pan(Point::new(500.0, -500.0), (800.0, 600.0), (400.0, 300.0)), Point::new(200.0, -150.0));
        assert_eq!(clamp_pan(Point::new(10.0, 5.0), (800.0, 600.0), (400.0, 300.0)), Point::new(10.0, 5.0));
    }

    #[test]
    fn zooming_keeps_the_point_under_the_mouse() {
        // The picture's centre is 40 DIPs left of the view's; the mouse 100 right of the view's
        // centre is 140 right of the picture's centre. Twice the zoom: 280 right, so the picture's
        // centre moves to 100 - 280 = -180.
        let pan = pan_for_zoom(Point::new(-40.0, 0.0), Point::new(100.0, 0.0), 1.0, 2.0);
        assert_eq!(pan, Point::new(-180.0, 0.0));
        // Around the centre with nothing panned, nothing moves.
        assert_eq!(pan_for_zoom(Point::default(), Point::default(), 1.0, 3.0), Point::default());
    }

    #[test]
    fn the_info_strip_says_what_is_known() {
        let photo = peek::Media { format: Some("JPEG".into()), bits: Some(24), dpi: Some(72.0), camera: Some("Canon EOS 80D".into()), ..Default::default() };
        let lines = media_lines(Some((6000, 4000)), None, Some(&photo), None, Some("08/10/2026 14:32".into()));
        assert_eq!(lines, vec!["6,000 \u{d7} 4,000  \u{b7}  JPEG  \u{b7}  24-bit  \u{b7}  72 DPI", "Canon EOS 80D  \u{b7}  Taken 08/10/2026 14:32"]);
        // A GIF: its frames and how long a loop plays.
        let gif = peek::Media { format: Some("GIF".into()), frames: Some(48), ..Default::default() };
        assert_eq!(media_lines(Some((320, 240)), None, Some(&gif), Some((48, 3200)), None), vec!["320 \u{d7} 240  \u{b7}  GIF  \u{b7}  48 frames  \u{b7}  3.2 s"]);
        // A video.
        let film = peek::Media { fps: Some(29.97), bitrate: Some(4_200_000), ..Default::default() };
        assert_eq!(media_lines(Some((1920, 1080)), Some(650_000_000), Some(&film), None, None), vec!["1,920 \u{d7} 1,080  \u{b7}  1:05  \u{b7}  29.97 fps  \u{b7}  4.2 Mb/s"]);
        // Nothing known yet: no strip.
        assert!(media_lines(None, None, None, None, None).is_empty());
    }

    #[test]
    fn small_numbers_read_well() {
        assert_eq!(seconds_text(3200), "3.2 s");
        assert_eq!(seconds_text(2000), "2 s");
        assert_eq!(seconds_text(65_000), "1:05");
        assert_eq!(fps_text(30.0), "30 fps");
        assert_eq!(fps_text(23.976), "23.98 fps");
        assert_eq!(bitrate_text(320_000), "320 kb/s");
        assert_eq!(bitrate_text(12_345_678), "12.3 Mb/s");
    }

    #[test]
    fn sliders_and_fitting() {
        let track = Rect::new(100.0, 0.0, 112.0, 40.0);
        assert_eq!(slider_share(track, 0.0), 0.0);
        assert_eq!(slider_share(track, 156.0), 0.5);
        assert_eq!(slider_share(track, 999.0), 1.0);
        // A 16:9 picture in a square: as wide as it, centred up and down.
        assert_eq!(fit_rect((1600.0, 900.0), Rect::new(0.0, 0.0, 160.0, 160.0)), Rect::new(0.0, 35.0, 160.0, 90.0));
    }

    /// An app showing one folder (in the temp folder) with `name` in it, selected.
    fn app_with_selected(dir: &std::path::Path, name: &str) -> AppState {
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        let entry = folder::Entry {
            name: name.into(),
            lower: name.to_lowercase(),
            kind: folder::EntryKind::File,
            size: std::fs::metadata(dir.join(name)).map_or(0, |m| m.len()),
            modified: 0,
            created: 0,
            attributes: 0,
            ext: folder::extension(name),
            type_name: Arc::from("File"),
            target: None,
            free: 0,
        };
        let path = dir.to_string_lossy().into_owned();
        app.tabs[0].path = path.clone();
        crate::app::apply_listing(&mut app.tabs[0], folder::Folder { path, entries: vec![entry], ..Default::default() });
        app.tabs[0].select_only_row(0);
        app
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("glint-preview-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_video_that_cant_be_played_ignores_play_and_seek() {
        let dir = temp_dir("cant-play");
        std::fs::write(dir.join("broken.mp4"), b"not a video").unwrap();
        let mut app = app_with_selected(&dir, "broken.mp4");
        let one = details::focused(&app).expect("the file is selected");
        let mut pane = PreviewPane::new();
        pane.view_key = one.key.clone();
        pane.video_key = one.key.clone();
        pane.video_failed = true;
        assert!(pane.cant_play(&one));
        // Play (the button, or a click on the picture) does nothing: nothing is opened again.
        assert!(matches!(pane.toggle_video(&mut app), Effect::None));
        assert!(pane.video.is_none() && pane.cant_play(&one));
        // Nor does the seek bar.
        pane.shown = Some(Shown { view: Rect::new(0.0, 0.0, 400.0, 300.0), picture: None, text: None, controls: Some(Controls::new(Rect::new(0.0, 300.0, 400.0, CONTROLS_H))) });
        pane.drag_to(Drag::Seek, 200.0);
        assert!(pane.video.is_none() && pane.cant_play(&one));
        // Another item (or the same file changed): it's tried again.
        pane.stop_video();
        assert!(!pane.cant_play(&one));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_video_that_wont_open_says_so() {
        // Random bytes named .mp4: Media Foundation won't take it (or, where it's missing, as
        // under Wine, there's nothing to open it with). Either way the pane knows it can't play.
        let dir = temp_dir("wont-open");
        std::fs::write(dir.join("noise.mp4"), (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect::<Vec<_>>()).unwrap();
        let mut app = app_with_selected(&dir, "noise.mp4");
        let one = details::focused(&app).expect("the file is selected");
        let mut pane = PreviewPane::new();
        pane.view_key = one.key.clone();
        let _ = pane.toggle_video(&mut app);
        if pane.video.is_none() {
            assert!(pane.cant_play(&one));
            assert!(matches!(pane.toggle_video(&mut app), Effect::None));
        }
        // (Where an engine was made, its error comes later, from its own thread: see video.rs's
        // `Watch` tests.) It lets go of the file.
        video::let_go(pane.video.take());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zoom_reads_as_a_percentage() {
        assert_eq!(zoom_text(0.93), "93%");
        assert_eq!(zoom_text(1.0), "100%");
    }

    /// Double-clicking a video opens it in its own player: the button coming up after it doesn't
    /// count as a click on the video (it would start the preview's copy again, and the sound would
    /// play twice).
    #[test]
    fn a_double_click_on_a_video_doesnt_play_it_too() {
        let dir = temp_dir("double-click");
        std::fs::write(dir.join("clip (2) & co.mp4"), b"not really a video").unwrap();
        let mut app = app_with_selected(&dir, "clip (2) & co.mp4");
        let one = details::focused(&app).expect("the file is selected");
        let mut pane = PreviewPane::new();
        pane.rect = Rect::new(0.0, 0.0, 400.0, 400.0);
        pane.view_key = one.key.clone();
        let view = Rect::new(0.0, 60.0, 400.0, 240.0);
        pane.shown = Some(Shown { view, picture: None, text: None, controls: Some(Controls::new(Rect::new(0.0, 300.0, 400.0, CONTROLS_H))) });
        let at = Point::new(200.0, 180.0);
        assert_eq!(pane.hit(at), Hit::View);
        let e = MouseEvent { pos: at, button: MouseButton::Left, mods: crate::ui::Mods::default(), wheel: 0.0 };
        // The second press of a double-click: down, then the double-click itself, then up.
        let _ = pane.mouse_down(&e);
        assert_eq!(pane.pressed, Hit::View);
        assert!(matches!(pane.double_click(&e, &app), Effect::Command(Command::OpenEntry(0))));
        assert_eq!(pane.pressed, Hit::None);
        let _ = pane.mouse_up(&e, &mut app);
        assert!(pane.video.is_none() && !pane.video_failed, "nothing was played (or even opened)");
        // Done again (a triple-click), still nothing plays.
        let _ = pane.mouse_down(&e);
        let _ = pane.double_click(&e, &app);
        let _ = pane.mouse_up(&e, &mut app);
        assert!(pane.video.is_none());
        // A plain click still plays (or tries to: there's no real video here).
        let _ = pane.mouse_down(&e);
        let _ = pane.mouse_up(&e, &mut app);
        assert!(pane.video.is_some() || pane.video_failed, "a single click still plays");
        video::let_go(pane.video.take());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_preview_keeps_a_few_big_text_files_not_32() {
        let mut c = FactsCache::with_budget(32, 2 * 1024 * 1024);
        let big = || peek::Facts { text: Some(Arc::new(peek::Text { lines: vec!["x".repeat(40); 5000], cut: true })), ..Default::default() };
        for i in 0..32 {
            c.insert(format!("log {i}"), big());
            assert!(c.text_bytes() <= 2 * 1024 * 1024, "at most the budget after {i}");
            assert!(c.get(&format!("log {i}")).is_some(), "the newest always stays");
        }
        assert!(c.get("log 0").is_none() && c.get("log 31").is_some() && c.get("log 30").is_some());
    }

    // ------------------------------------------------------------ text: wrapping

    /// The rows `line` is cut into for a pane `cols` columns wide. (Cutting it up fails if a row
    /// would start in the middle of a character.)
    fn wrapped(line: &str, cols: usize) -> Vec<&str> {
        let mut starts = Vec::new();
        wrap_line(line, cols, &mut starts);
        assert_eq!(starts[0], 0, "the first row starts the line");
        assert!(starts.windows(2).all(|w| w[0] < w[1]), "rows go forward: {starts:?}");
        let ends = starts.iter().skip(1).map(|&s| s as usize).chain([line.len()]);
        starts.iter().zip(ends).map(|(&a, b)| &line[a as usize..b]).collect()
    }

    fn text_of(lines: &[&str]) -> Arc<peek::Text> {
        Arc::new(peek::Text { lines: lines.iter().map(|l| l.to_string()).collect(), cut: false })
    }

    #[test]
    fn long_lines_wrap_at_spaces() {
        assert_eq!(wrapped("the quick brown fox", 10), ["the quick ", "brown fox"]);
        // A line that fits, an empty one, and one just as wide as the pane stay one row.
        assert_eq!(wrapped("short", 10), ["short"]);
        assert_eq!(wrapped("", 10), [""]);
        assert_eq!(wrapped("0123456789", 10), ["0123456789"]);
        assert_eq!(wrapped("0123456789a", 10), ["0123456789", "a"]);
        // Spaces at the end of a row stay on it (past the edge, unseen): the next row starts with
        // a word. So do the spaces a line starts with, when they alone fill the row.
        assert_eq!(wrapped("aaaaaaaa    bbbb", 8), ["aaaaaaaa    ", "bbbb"]);
        assert_eq!(wrapped("          x", 8), ["          ", "x"]);
        assert_eq!(wrapped(&" ".repeat(500), 8), [" ".repeat(500)]);
        // A word that fits a row of its own goes down whole; one longer than a row is cut where
        // the row ends (a long path has no spaces at all).
        assert_eq!(wrapped("hello wonderful", 8), ["hello ", "wonderfu", "l"]);
        assert_eq!(wrapped("C:\\Users\\YourName\\Documents", 10), ["C:\\Users\\Y", "ourName\\Do", "cuments"]);
        // However narrow the pane, a row holds a few characters.
        assert_eq!(wrapped("0123456789", 0), ["01234567", "89"]);
        assert_eq!(wrapped("0123456789", 3), ["01234567", "89"]);
    }

    #[test]
    fn a_huge_line_without_a_space_wraps_into_full_rows() {
        let line = "x".repeat(100_000);
        let rows = wrapped(&line, 80);
        assert_eq!(rows.len(), 1250);
        assert!(rows.iter().all(|row| row.len() == 80));
        // The same with two-byte letters, and with spaces only every 30,000 characters.
        let line = "\u{f1}".repeat(100_000);
        let rows = wrapped(&line, 80);
        assert_eq!(rows.len(), 1250);
        assert!(rows.iter().all(|row| row.chars().count() == 80));
        let line = ["y".repeat(30_000), "z".repeat(30_000), "\u{130}".repeat(40_000)].join(" ");
        let rows = wrapped(&line, 100);
        assert!(rows.iter().all(|row| width(row.trim_end()) <= 100), "no row is wider than the pane");
        assert_eq!(rows.concat(), line, "nothing is lost or shown twice");
        assert_eq!(rows.len(), 300 + 300 + 400, "full rows: the space after a word sits past the edge");
    }

    #[test]
    fn emoji_wide_and_right_to_left_text_wrap_whole() {
        // An emoji takes three columns and a Chinese or Japanese character two: neither is ever
        // cut in half, and a row never holds more columns than the pane has.
        assert_eq!(wrapped("\u{1F600}\u{1F600}\u{1F600}", 8), ["\u{1F600}\u{1F600}", "\u{1F600}"]);
        assert_eq!(wrapped("日本語のテキスト", 8), ["日本語の", "テキスト"]);
        // A family (people joined with zero-width joiners), a flag (two letters), a thumbs-up
        // with a skin tone and a heart with its "draw me as an emoji" mark are one emoji each.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!((width(family), width("\u{1F1E9}\u{1F1EA}"), width("\u{1F44D}\u{1F3FD}"), width("\u{2764}\u{FE0F}")), (3, 3, 3, 3));
        assert_eq!(wrapped(&family.repeat(3), 8), [family.repeat(2), family.to_string()]);
        // Hebrew wraps at its spaces like any other writing (each row is then drawn right to left
        // by Windows).
        assert_eq!(wrapped("שלום עולם שלום עולם", 10), ["שלום עולם ", "שלום עולם"]);
        assert_eq!(wrapped("مرحبا بالعالم", 8), ["مرحبا ", "بالعالم"]);
        // An accent typed after its letter stays on the letter's row.
        assert_eq!(wrapped("aaaaaaaa\u{301}b", 8), ["aaaaaaaa\u{301}", "b"]);
        // Letters outside ASCII are a column each.
        assert_eq!(width("\u{130}\u{f1}#\u{e9}"), 4);
        assert_eq!(wrapped("\u{130}stanbul \u{f1}and\u{fa} #1", 8), ["\u{130}stanbul ", "\u{f1}and\u{fa} #1"]);
    }

    #[test]
    fn no_row_is_wider_than_the_pane() {
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let lines = [
            "fn main() { println!(\"{}\", some_function_with_a_long_name(argument_one, argument_two)); }".to_string(),
            "    indented    with   runs   of   spaces    and a_very_long_identifier_that_never_ends_and_never_ends".to_string(),
            format!("emoji {family} mixed \u{1F600}\u{1F600} with 日本語 and עברית and \u{130}\u{f1} {}", "\u{1F1E9}\u{1F1EA}".repeat(9)),
            "日本語".repeat(40),
            "a\u{301}".repeat(60),
            "\u{1F600}".repeat(50),
            format!("{}\u{3000}{}", "全角".repeat(10), "スペース".repeat(10)),
        ];
        for line in &lines {
            for cols in [1, 8, 9, 13, 40, 80, 200] {
                let rows = wrapped(line, cols);
                assert_eq!(rows.concat(), *line);
                for row in rows {
                    let seen = row.trim_end_matches([' ', '\u{3000}']);
                    assert!(width(seen) <= cols.max(MIN_COLS), "{seen:?} is wider than {cols} columns");
                }
            }
        }
    }

    #[test]
    fn a_character_never_takes_more_columns_than_bytes() {
        // (What lets a short line skip being measured: n bytes always fit in n columns.)
        for c in (0..=0x10FFFF_u32).filter_map(char::from_u32) {
            assert!(cells(c) <= c.len_utf8(), "U+{:X}", c as u32);
        }
        assert_eq!(width(""), 0);
    }

    #[test]
    fn a_wrapped_line_keeps_its_own_number() {
        let text = text_of(&["one", "", "the quick brown fox jumps", "last"]);
        let rows = Rows::new(text.clone(), 10, true);
        // The third line takes three rows.
        let all: Vec<_> = (0..rows.len()).map(|n| rows.row(n).unwrap()).collect();
        assert_eq!(all, [(0, 0, 3), (1, 0, 0), (2, 0, 10), (2, 10, 20), (2, 20, 25), (3, 0, 4)]);
        // Its number is on its first row only, and the line after it is still line 4.
        let numbers: Vec<_> = (0..rows.len() + 1).map(|n| rows.number(n)).collect();
        assert_eq!(numbers, [Some(1), Some(2), Some(3), None, None, Some(4), None]);
        assert_eq!((rows.row(6), rows.across()), (None, 0.0));
        // Which row a place in the text is on.
        assert_eq!((rows.row_at(0, 0), rows.row_at(2, 0), rows.row_at(2, 9), rows.row_at(2, 10), rows.row_at(2, 24), rows.row_at(3, 0)), (0, 2, 2, 3, 4, 5));
        // With wrap off a line is a row, and the longest one says how far it scrolls sideways
        // (to its end and one column more).
        let rows = Rows::new(text.clone(), 10, false);
        assert_eq!(rows.len(), 4);
        assert_eq!((rows.row(2), rows.row(4)), (Some((2, 0, 25)), None));
        assert_eq!((0..5).map(|n| rows.number(n)).collect::<Vec<_>>(), [Some(1), Some(2), Some(3), Some(4), None]);
        assert_eq!((rows.row_at(2, 20), rows.across()), (2, 16.0));
        // Wide enough for every line: nothing to scroll to.
        assert_eq!(Rows::new(text, 25, false).across(), 0.0);
        // No lines at all (only the "first 200 KB" note would show).
        let none = Rows::new(text_of(&[]), 10, true);
        assert_eq!((none.len(), none.row(0), none.number(0), none.row_at(0, 0)), (0, None, None, 0));
    }

    #[test]
    fn only_the_part_of_a_row_on_screen_is_drawn() {
        assert_eq!(columns("0123456789", 0, 4), ("0123", 0));
        assert_eq!(columns("0123456789", 3, 4), ("3456", 3));
        assert_eq!(columns("0123456789", 8, 4), ("89", 8));
        assert_eq!(columns("0123456789", 50, 4), ("", 10));
        assert_eq!(columns("", 0, 4), ("", 0));
        // A wide character cut in half by either edge is left out.
        assert_eq!(columns("日本語", 0, 4), ("日本", 0));
        assert_eq!(columns("日本語", 1, 4), ("本", 2));
        assert_eq!(columns("日本語", 1, 5), ("本語", 2));
        assert_eq!(columns("日本語", 6, 4), ("", 6));
        // An accent comes with its letter.
        assert_eq!(columns("xe\u{301}y", 1, 1), ("e\u{301}", 1));
        // Of a huge line only what shows is handed over, wherever it's scrolled to.
        let long = "x".repeat(100_000);
        let (piece, col) = columns(&long, 50_000, 82);
        assert_eq!((piece.len(), col), (82, 50_000));
        let (piece, col) = columns(&long, 99_990, 82);
        assert_eq!((piece.len(), col), (10, 99_990));
        let long = "\u{1F600}\u{e9}".repeat(25_000);
        let (piece, col) = columns(&long, 50_000, 82);
        assert_eq!((width(piece), col), (80, 50_000));
        assert_eq!(width(&long), 100_000);
    }

    #[test]
    fn the_line_at_the_top_stays_there_when_the_pane_is_resized_or_wrap_is_switched() {
        let mut lines = vec!["short".to_string(); 50];
        lines.push("word ".repeat(100));
        lines.extend(vec!["after".to_string(); 50]);
        let text = Arc::new(peek::Text { lines, cut: false });
        let mut pane = PreviewPane::new();
        pane.lay_out(&text, 40, true, None);
        assert_eq!(pane.rows.as_ref().map(Rows::len), Some(50 + 13 + 50), "500 columns in rows of 40");
        // Scrolled half a row into the fifth row of the long line.
        pane.scroll = 54.5;
        let (line, start, _) = pane.rows.as_ref().unwrap().row(54).unwrap();
        assert_eq!((line, start), (50, 160));
        // The pane gets narrower: the same words are at the top, as far into their row.
        pane.lay_out(&text, 20, true, None);
        let (line, from, to) = pane.rows.as_ref().unwrap().row(pane.scroll.floor() as usize).unwrap();
        assert_eq!((line, from <= start && start < to, pane.scroll.fract()), (50, true, 0.5));
        // WRAP off: a line is a row, so that line is the 51st row. On again: its first row.
        pane.lay_out(&text, 20, false, None);
        assert_eq!(pane.scroll, 50.5);
        pane.lay_out(&text, 20, true, None);
        assert_eq!(pane.scroll, 50.5);
        // Asked again with nothing changed (every frame is): it isn't cut into rows again.
        let before = pane.rows.as_ref().unwrap().rows.as_ptr();
        pane.lay_out(&text, 20, true, None);
        assert_eq!(pane.rows.as_ref().unwrap().rows.as_ptr(), before);
        // Another file starts from its top, and closing the pane lets the rows go.
        pane.new_item("another file");
        assert!(pane.rows.is_none() && pane.scroll == 0.0 && pane.scroll_x == 0.0);
        pane.lay_out(&text, 20, true, None);
        pane.closed();
        assert!(pane.rows.is_none());
    }

    #[test]
    fn the_wheel_scrolls_rows_and_with_wrap_off_shift_scrolls_sideways() {
        let mut pane = PreviewPane::new();
        let view = Rect::new(0.0, 41.0, 400.0, 300.0);
        let wheel = |notches: f32, shift: bool| MouseEvent { pos: Point::new(10.0, 100.0), button: MouseButton::Middle, mods: crate::ui::Mods { shift, ..Default::default() }, wheel: notches };
        // Wrapped: 100 rows of which 10 fit.
        pane.shown = Some(Shown { view, picture: None, text: Some(TextShown { rows: 100, fits: 10, across: 0.0 }), controls: None });
        assert!(matches!(pane.wheel(&wheel(-1.0, false)), Effect::Redraw));
        assert_eq!(pane.scroll, WHEEL_LINES);
        let _ = pane.wheel(&wheel(-1000.0, false));
        assert_eq!(pane.scroll, 90.0, "it stops at the last row");
        assert!(matches!(pane.wheel(&wheel(-1.0, false)), Effect::None));
        // Nothing runs past the edge while it wraps, so Shift changes nothing.
        let _ = pane.wheel(&wheel(1.0, true));
        assert_eq!((pane.scroll, pane.scroll_x), (87.0, 0.0));
        // WRAP off and a line 50 columns too long: Shift goes sideways, and stops at both ends.
        pane.shown = Some(Shown { view, picture: None, text: Some(TextShown { rows: 100, fits: 10, across: 50.0 }), controls: None });
        assert!(matches!(pane.wheel(&wheel(-1.0, true)), Effect::Redraw));
        assert_eq!((pane.scroll, pane.scroll_x), (87.0, WHEEL_COLS));
        let _ = pane.wheel(&wheel(-1000.0, true));
        assert_eq!(pane.scroll_x, 50.0);
        assert!(matches!(pane.wheel(&wheel(-1.0, true)), Effect::None));
        let _ = pane.wheel(&wheel(1000.0, true));
        assert_eq!(pane.scroll_x, 0.0);
        // Without Shift it still goes up and down.
        let _ = pane.wheel(&wheel(1.0, false));
        assert_eq!((pane.scroll, pane.scroll_x), (84.0, 0.0));
        // A text that fits doesn't move at all.
        pane.scroll = 0.0;
        pane.shown = Some(Shown { view, picture: None, text: Some(TextShown { rows: 3, fits: 10, across: 0.0 }), controls: None });
        assert!(matches!(pane.wheel(&wheel(-1.0, false)), Effect::None));
        assert_eq!(pane.scroll, 0.0);
    }

    #[test]
    fn the_wrap_switch_is_there_for_text_and_turns_wrapping_off_and_on() {
        let mut app = AppState::for_tests(crate::settings::Settings::default());
        assert!(app.settings.preview_wrap, "long lines wrap unless it's turned off");
        let mut pane = PreviewPane::new();
        let click = |pane: &mut PreviewPane, app: &mut AppState, at: Point| {
            let e = MouseEvent { pos: at, button: MouseButton::Left, mods: crate::ui::Mods::default(), wheel: 0.0 };
            let _ = pane.mouse_move(&e);
            let _ = pane.mouse_down(&e);
            let pressed = pane.pressed;
            let _ = pane.mouse_up(&e, app);
            pressed
        };
        // In the widest pane and the narrowest one.
        for w in [1200.0, 300.0] {
            pane.rect = Rect::new(600.0, 0.0, w, 400.0);
            let bar = pane.bar();
            assert!(bar.wrap.w >= 30.0 && bar.wrap.right() <= bar.info.x, "it has room left of the \u{201c}i\u{201d} button at {w}");
            let at = Point::new(bar.wrap.x + bar.wrap.w / 2.0, bar.wrap.y + bar.wrap.h / 2.0);
            // A picture, a video or a file's details on screen: no switch, and a click there does nothing.
            pane.shown = Some(Shown { view: pane.view_rect(), picture: None, text: None, controls: None });
            assert_ne!(pane.hit(at), Hit::Wrap);
            let _ = click(&mut pane, &mut app, at);
            assert!(app.settings.preview_wrap);
            // A text file.
            pane.shown = Some(Shown { view: pane.view_rect(), picture: None, text: Some(TextShown { rows: 10, fits: 20, across: 0.0 }), controls: None });
            assert_eq!(pane.hit(at), Hit::Wrap);
            assert_eq!(click(&mut pane, &mut app, at), Hit::Wrap);
            assert!(!app.settings.preview_wrap, "one click turns it off");
            assert_eq!(pane.hover, Hit::Wrap, "and it shows the mouse is on it");
            // Pressed, then let go somewhere else: nothing changes.
            let e = MouseEvent { pos: at, button: MouseButton::Left, mods: crate::ui::Mods::default(), wheel: 0.0 };
            let _ = pane.mouse_down(&e);
            let _ = pane.mouse_up(&MouseEvent { pos: Point::new(at.x, 200.0), ..e }, &mut app);
            assert!(!app.settings.preview_wrap);
            // The other buttons leave it alone, and another click turns it on again.
            let _ = click(&mut pane, &mut app, Point::new(bar.info.x + 5.0, bar.info.y + 5.0));
            assert!(!app.settings.preview_wrap);
            let _ = click(&mut pane, &mut app, at);
            assert!(app.settings.preview_wrap);
        }
    }

    #[test]
    fn the_wrap_choice_is_remembered() {
        use crate::settings::Settings;
        // A settings.json from before the switch was there: long lines wrap.
        let old: Settings = serde_json::from_str("{\"PreviewInfo\": false, \"ShowHidden\": true}").unwrap();
        assert!(old.preview_wrap && !old.preview_info && old.show_hidden);
        // Turned off, it's written and read back that way; and on again.
        let mut s = Settings { preview_wrap: false, ..Default::default() };
        let text = s.to_json();
        assert!(text.contains("\"PreviewWrap\": false"), "{text}");
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert!(!back.preview_wrap);
        assert_eq!(back, s);
        s.preview_wrap = true;
        let back: Settings = serde_json::from_str(&s.to_json()).unwrap();
        assert!(back.preview_wrap);
    }
}
