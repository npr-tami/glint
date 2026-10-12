//! The renderer: everything Glint draws goes through here, on the graphics card, with Direct2D
//! (shapes, pictures) and DirectWrite (text).
//!
//! Coordinates are DIPs (see theme.rs). The render target is told the window's real DPI, so Direct2D
//! scales for us; edges of filled rectangles are additionally "snapped" to whole screen pixels so
//! lines and panel edges stay crisp at 125 % and 150 %.
//!
//! If the graphics driver resets (a driver update, the GPU going to sleep, a remote-desktop switch),
//! Direct2D reports D2DERR_RECREATE_TARGET when a frame ends. We then throw the render target and
//! everything made from it away and build a new one on the next frame - see [`Renderer::end`].

use std::collections::HashMap;

use windows::core::{Result, BOOL, HSTRING, PCWSTR};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED,
    D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1PathGeometry, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, D2D1_CAP_STYLE_ROUND,
    D2D1_DASH_STYLE_SOLID, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINE_JOIN_ROUND, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES, D2D1_BITMAP_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_ALIASED, D2D1_TEXT_ANTIALIAS_MODE_DEFAULT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1_OPACITY_MASK_CONTENT_GRAPHICS,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
// Names of several lines under the icon views' pictures (`text_wrapped`).
use windows::Win32::Graphics::DirectWrite::{DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_WORD_WRAPPING_EMERGENCY_BREAK, DWRITE_WORD_WRAPPING_WRAP};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows_numerics::Vector2;

use crate::icons::{IconId, Icons};
// Pictures (thumbs.rs): thumbnails and the Space preview's full-size pictures.
use crate::thumbs::Picture;
use std::sync::{Arc, Weak};
use windows::Win32::Graphics::Direct2D::{
    ID2D1BitmapBrush, ID2D1DeviceContext, D2D1_BITMAP_BRUSH_PROPERTIES, D2D1_EXTEND_MODE_WRAP, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
};
use windows_core::Interface;
use crate::theme::{size, Color, Gradient, Paint, Stop, MAX_STOPS};
use crate::ui::{Point, Rect};
// Gradients (`fill_paint`, `marble`, `liquid_capsule`, `gloss`).
use windows::Win32::Graphics::Direct2D::Common::D2D1_GRADIENT_STOP;
use windows::Win32::Graphics::Direct2D::{
    ID2D1GradientStopCollection, ID2D1LinearGradientBrush, ID2D1RadialGradientBrush, D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_GAMMA_2_2,
    D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES,
};

/// Fonts to try, best first. Windows 11 has the first, Windows 10 the second; the rest are for
/// Wine and odd setups. DirectWrite fills in missing letters (e.g. Chinese names) by itself.
const FONT_FAMILIES: &[&str] = &[
    "Segoe UI Variable Text",
    "Segoe UI",
    "Selawik",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Tahoma",
    "Arial",
];

/// How many laid-out texts (and measured widths) are kept. A full screen shows a few hundred.
/// Scrolling through a big folder keeps making new ones, so the cache works in two generations:
/// when the new one is full, the old one is dropped and the new one becomes the old one. Texts
/// still on screen get carried over to the new one as they're drawn, so nothing stutters.
const MAX_CACHED_TEXTS: usize = 768;

/// What a laid-out text depends on: the words, the style (apart from colour) and the box size.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct LayoutKey {
    text: String,
    size: u32,
    weight: u16,
    align: Align,
    /// Box size in quarter DIPs.
    w: i32,
    h: i32,
    /// Broken into lines that fit the width (`text_wrapped`).
    wrap: bool,
    /// In the windows' title font (`TextStyle::title_font`).
    title: bool,
}

/// Horizontal text alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// How a piece of text looks. Build one with `TextStyle::new(theme.text).bold().align(Align::Right)`.
#[derive(Clone, Copy, Debug)]
pub struct TextStyle {
    /// Font size in DIPs (12.0 = 9 pt).
    pub size: f32,
    /// 400 = normal, 600 = semibold, 700 = bold.
    pub weight: u16,
    pub align: Align,
    pub color: Color,
    /// In the font of the windows' titles (`Renderer::set_title_font`) rather than the skin's.
    pub title: bool,
}

impl TextStyle {
    pub fn new(color: Color) -> TextStyle {
        TextStyle { size: size::FONT, weight: 400, align: Align::Left, color, title: false }
    }
    /// In the skin's font for window titles (the skin's font when it has none).
    pub fn title_font(mut self) -> TextStyle {
        self.title = true;
        self
    }
    pub fn size(mut self, size: f32) -> TextStyle {
        self.size = size;
        self
    }
    pub fn bold(mut self) -> TextStyle {
        self.weight = 700;
        self
    }
    pub fn semibold(mut self) -> TextStyle {
        self.weight = 600;
        self
    }
    pub fn align(mut self, align: Align) -> TextStyle {
        self.align = align;
        self
    }
    pub fn color(mut self, color: Color) -> TextStyle {
        self.color = color;
        self
    }
}

/// What happened when a frame was finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameResult {
    /// On screen.
    Shown,
    /// The graphics device was lost; everything was reset. Paint again.
    DeviceLost,
}

/// Things made from the render target; they all die together when the device is lost.
struct Target {
    rt: ID2D1HwndRenderTarget,
    brush: ID2D1SolidColorBrush,
    /// Direct2D copies of the cached icons, by `IconId`, made the first time each is drawn.
    bitmaps: Vec<Option<ID2D1Bitmap>>,
    /// Pictures (thumbnails, the Space preview's), see `Renderer::picture`.
    pictures: Pictures,
    /// Direct2D copies of the logo's pictures (logo.rs), dark ones then light ones, made the first
    /// time each is drawn (`Renderer::wordmark`).
    wordmarks: [Option<ID2D1Bitmap>; 2 * crate::logo::COUNT],
    /// Gradients' brushes, by their colours (`Renderer::fill_paint`), made the first time each is
    /// drawn and moved into place for each shape after that.
    gradients: HashMap<GradientKey, GradientBrushes>,
}

/// What a gradient's brushes are kept by: its colours (8 bits each of red, green, blue and
/// opacity) and their positions (in ten-thousandths). A fixed-size value, so looking one up never
/// allocates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct GradientKey {
    colors: [u32; MAX_STOPS],
    at: [u16; MAX_STOPS],
    len: u8,
}

impl GradientKey {
    fn of(g: &Gradient) -> GradientKey {
        let mut k = GradientKey { colors: [0; MAX_STOPS], at: [0; MAX_STOPS], len: 0 };
        for (i, s) in g.stops().iter().enumerate() {
            let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
            k.colors[i] = (b(s.color.r) << 24) | (b(s.color.g) << 16) | (b(s.color.b) << 8) | b(s.color.a);
            k.at[i] = (s.at.clamp(0.0, 1.0) * 10_000.0).round() as u16;
            k.len += 1;
        }
        k
    }
}

/// A gradient's colours as Direct2D keeps them, and the brushes made from them (each made the
/// first time it's needed).
struct GradientBrushes {
    stops: ID2D1GradientStopCollection,
    linear: Option<ID2D1LinearGradientBrush>,
    radial: Option<ID2D1RadialGradientBrush>,
}

/// More gradients than this (a skin has a few dozen) and the kept brushes start over.
const MAX_CACHED_GRADIENTS: usize = 256;

/// What the renderer keeps for drawing pictures (thumbs.rs), made from the render target.
#[derive(Default)]
struct Pictures {
    /// Direct2D copies, by picture id. Each goes as soon as its picture does (`begin` checks).
    bitmaps: HashMap<u64, (Weak<Picture>, ID2D1Bitmap)>,
    /// The render target as a Direct2D 1.1 device context, which can shrink pictures smoothly
    /// (high-quality cubic); None where there's no such thing.
    dc: Option<ID2D1DeviceContext>,
    /// The checkerboard behind pictures (`fill_checker`) and the two colours it was made with.
    checker: Option<(ID2D1BitmapBrush, Color, Color)>,
}

pub struct Renderer {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: Option<Target>,
    /// Round-capped lines for glyphs (arrows, chevrons, the caption-button signs).
    round: ID2D1StrokeStyle,
    family: HSTRING,
    /// The font of the windows' titles, when the skin has one of its own (`set_title_font`).
    title_family: Option<HSTRING>,
    formats: HashMap<(u32, u16, Align, bool), IDWriteTextFormat>,
    /// Texts laid out once and drawn every frame after that: shaping and line-breaking a text is the
    /// expensive part of drawing it (see `text`). The current generation and the one before.
    layouts: HashMap<LayoutKey, IDWriteTextLayout>,
    old_layouts: HashMap<LayoutKey, IDWriteTextLayout>,
    /// Measured widths, by (text, size, weight, title font).
    widths: HashMap<(String, u32, u16, bool), f32>,
    dpi: f32,
    /// Reused buffer for converting text to UTF-16, so drawing text doesn't allocate.
    wide: Vec<u16>,
    clip_depth: u32,
    in_frame: bool,
    /// While set (`set_tint`), every colour drawn goes this far toward this colour, and a
    /// picture gets a veil of it over its shape: 98 / 2000's selected icon.
    tint: Option<(Color, f32)>,
    /// The skin's text size as a part of the standard one (1.0 = 9 pt): every text is drawn that
    /// much bigger or smaller (`set_font`).
    text_scale: f32,
    /// What `set_font` was last given, and how many times the font changed (parts that keep
    /// measured widths measure again when it does).
    font_asked: (String, f32),
    font_generation: u32,
    /// Monospaced text (the Space preview's text files), see `mono_text`.
    mono: Mono,
    /// 7's see-through glass at the window's top (`ask_glass`).
    glass: Glass,
}

/// How far down the window's top is see-through: as asked for while drawing this frame (DIPs), as
/// it is now (pixels; 0: not at all), and whether this window can't be (older Windows, Wine).
#[derive(Default)]
struct Glass {
    asked: f32,
    on: i32,
    cannot: bool,
    /// Where Windows keeps its own window buttons (DIPs), which the glass mustn't show.
    solid: Option<Rect>,
}

thread_local! {
    /// Whether text is drawn without smoothing (`set_text_aliased`).
    static TEXT_ALIASED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Draws every window's words without smoothing from the next frame on (98 / 2000's skins: their
/// Windows drew crisp, aliased text), or smoothed as usual.
pub fn set_text_aliased(on: bool) {
    TEXT_ALIASED.with(|t| t.set(on));
}

/// The key of a monospaced text's layout: the words, the size and the box (as in `LayoutKey`).
type MonoKey = (String, u32, i32, i32);

/// What `mono_text` keeps: the monospaced font and the texts laid out with it (two generations,
/// like `layouts`).
#[derive(Default)]
struct Mono {
    family: Option<HSTRING>,
    formats: HashMap<u32, IDWriteTextFormat>,
    layouts: HashMap<MonoKey, IDWriteTextLayout>,
    old: HashMap<MonoKey, IDWriteTextLayout>,
}

impl Renderer {
    /// Sets up Direct2D and DirectWrite for `hwnd`. The render target itself is made on the first frame.
    pub fn new(hwnd: HWND, dpi: u32) -> Result<Renderer> {
        // SAFETY: plain factory constructors; the returned COM pointers are owned by us.
        let (d2d, dwrite): (ID2D1Factory, IDWriteFactory) = unsafe {
            (D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?, DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?)
        };
        let props = D2D1_STROKE_STYLE_PROPERTIES {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
        };
        // SAFETY: `props` lives across the call.
        let round = unsafe { d2d.CreateStrokeStyle(&props, None)? };
        let family = HSTRING::from(pick_font_family(&dwrite));
        Ok(Renderer {
            hwnd,
            d2d,
            dwrite,
            target: None,
            round,
            family,
            title_family: None,
            formats: HashMap::new(),
            layouts: HashMap::new(),
            old_layouts: HashMap::new(),
            widths: HashMap::new(),
            dpi: dpi as f32,
            wide: Vec::with_capacity(256),
            clip_depth: 0,
            in_frame: false,
            tint: None,
            text_scale: 1.0,
            font_asked: (String::new(), 1.0),
            font_generation: 0,
            mono: Mono::default(),
            glass: Glass::default(),
        })
    }

    /// The font family in use (for logging, and for the text boxes).
    pub fn font_family(&self) -> String {
        self.family.to_string()
    }

    /// The skin's fonts: the first of `families` ("Segoe UI Variable Text, Segoe UI") that is
    /// installed - or Glint's own choice when none is - with every text `scale` times its standard
    /// size. Laid-out texts and measured widths are made again.
    pub fn set_font(&mut self, families: &str, scale: f32) {
        let scale = if scale.is_finite() { scale.clamp(0.5, 2.0) } else { 1.0 };
        if self.font_asked.0 == families && self.font_asked.1 == scale {
            return;
        }
        self.font_asked = (families.to_string(), scale);
        let wanted = families.split(',').map(str::trim).filter(|f| !f.is_empty()).find(|f| font_installed(&self.dwrite, f));
        let family = HSTRING::from(wanted.unwrap_or_else(|| pick_font_family(&self.dwrite)));
        if family != self.family || scale != self.text_scale {
            self.family = family;
            self.text_scale = scale;
            self.formats.clear();
            self.layouts.clear();
            self.old_layouts.clear();
            self.widths.clear();
            self.font_generation = self.font_generation.wrapping_add(1);
        }
    }

    /// The skin's font for window titles: the first of `families` that is installed, or the
    /// skin's own font when none is (or `families` is empty).
    pub fn set_title_font(&mut self, families: &str) {
        let wanted = families.split(',').map(str::trim).filter(|f| !f.is_empty()).find(|f| font_installed(&self.dwrite, f));
        let family = wanted.map(HSTRING::from);
        if family != self.title_family {
            self.title_family = family;
            self.formats.retain(|k, _| !k.3);
            self.layouts.retain(|k, _| !k.title);
            self.old_layouts.retain(|k, _| !k.title);
            self.widths.retain(|k, _| !k.3);
        }
    }

    /// Changes every time the font or the text size changes (see `set_font`).
    pub fn font_generation(&self) -> u32 {
        self.font_generation
    }

    /// DIPs to pixels (1.0 at 96 DPI).
    pub fn scale(&self) -> f32 {
        self.dpi / 96.0
    }

    /// The window moved to a screen with another DPI.
    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi as f32;
        if let Some(t) = &self.target {
            // SAFETY: plain setter on a live render target.
            unsafe { t.rt.SetDpi(self.dpi, self.dpi) };
        }
    }

    /// The window's client area changed size (in pixels).
    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(t) = &self.target {
            let size = D2D_SIZE_U { width: width.max(1), height: height.max(1) };
            // SAFETY: `size` lives across the call. A failure here shows up again at EndDraw, which recreates.
            if unsafe { t.rt.Resize(&size) }.is_err() {
                self.target = None;
            }
        }
    }

    fn create_target(&mut self) -> Result<()> {
        let mut rc = RECT::default();
        // SAFETY: `hwnd` is our live window and `rc` a valid out-pointer.
        unsafe { GetClientRect(self.hwnd, &mut rc)? };
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            // See-through pixels only matter (and only cost) while the glass is on.
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: if self.glass.on > 0 { D2D1_ALPHA_MODE_PREMULTIPLIED } else { D2D1_ALPHA_MODE_IGNORE },
            },
            dpiX: self.dpi,
            dpiY: self.dpi,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: self.hwnd,
            pixelSize: D2D_SIZE_U {
                width: (rc.right - rc.left).max(1) as u32,
                height: (rc.bottom - rc.top).max(1) as u32,
            },
            // Waits for the screen's refresh when presenting, so animations are smooth and tear-free.
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        // SAFETY: both property structs live across the calls; the results are owned COM pointers.
        unsafe {
            // The graphics card first; if that fails (broken driver, remote session), Windows'
            // software renderer. GLINT_RENDER=software forces the latter, for testing.
            let forced = std::env::var("GLINT_RENDER").is_ok_and(|v| v.eq_ignore_ascii_case("software"));
            let hardware = if forced { Err(windows::core::Error::empty()) } else { self.d2d.CreateHwndRenderTarget(&props, &hwnd_props) };
            let rt = match hardware {
                Ok(rt) => rt,
                Err(_) => {
                    let soft = D2D1_RENDER_TARGET_PROPERTIES { r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE, ..props };
                    self.d2d.CreateHwndRenderTarget(&soft, &hwnd_props)?
                }
            };
            let brush = rt.CreateSolidColorBrush(&color_f(Color::hex(0)), None)?;
            let pictures = Pictures { dc: rt.cast::<ID2D1DeviceContext>().ok(), ..Pictures::default() };
            self.target = Some(Target { rt, brush, bitmaps: Vec::new(), pictures, wordmarks: Default::default(), gradients: HashMap::new() });
        }
        Ok(())
    }

    /// Starts a frame. Returns false if there is nothing to draw on (the device can't be made right now).
    pub fn begin(&mut self) -> bool {
        if self.target.is_none() && self.create_target().is_err() {
            return false;
        }
        let Some(t) = &self.target else { return false };
        // SAFETY: BeginDraw/EndDraw are always paired: `end` is called by the window after painting.
        unsafe {
            t.rt.BeginDraw();
            t.rt.SetTransform(&windows_numerics::Matrix3x2::identity());
            // 98 / 2000 drew their words crisp, without smoothing (`set_text_aliased`). On a
            // see-through window, plain smoothing: ClearType's coloured edges need a solid
            // background behind them.
            let crisp = TEXT_ALIASED.with(std::cell::Cell::get);
            t.rt.SetTextAntialiasMode(if crisp {
                D2D1_TEXT_ANTIALIAS_MODE_ALIASED
            } else if self.glass.on > 0 {
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE
            } else {
                D2D1_TEXT_ANTIALIAS_MODE_DEFAULT
            });
        }
        if self.glass.on > 0 {
            let s = self.scale();
            self.glass.solid = crate::frame::caption_buttons(self.hwnd)
                .map(|b| Rect::new(b.left as f32 / s, b.top as f32 / s, (b.right - b.left) as f32 / s, (b.bottom - b.top) as f32 / s));
        }
        // Pictures nobody has any more (thumbs.rs dropped them) let go of their copies.
        self.drop_unused_pictures();
        self.in_frame = true;
        self.clip_depth = 0;
        true
    }

    /// Lets go of the Direct2D copies of pictures nobody has any more (every frame, and when the
    /// window hides: thumbs.rs forgot its pictures then, and nothing is drawn while hidden).
    pub fn drop_unused_pictures(&mut self) {
        if let Some(t) = self.target.as_mut() {
            t.pictures.bitmaps.retain(|_, (pic, _)| pic.strong_count() > 0);
        }
    }

    /// Finishes the frame and puts it on screen.
    pub fn end(&mut self) -> FrameResult {
        if !self.in_frame {
            return FrameResult::Shown;
        }
        self.in_frame = false;
        while self.clip_depth > 0 {
            self.pop_clip();
        }
        let Some(t) = &self.target else { return FrameResult::DeviceLost };
        // SAFETY: matches the BeginDraw in `begin`.
        match unsafe { t.rt.EndDraw(None, None) } {
            Ok(()) => {
                self.settle_glass();
                FrameResult::Shown
            }
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                self.target = None;
                FrameResult::DeviceLost
            }
            Err(_) => {
                // Anything else: also start over with a fresh target, it is cheap.
                self.target = None;
                FrameResult::DeviceLost
            }
        }
    }

    /// Asks for the window's top, down to `bottom` (DIPs), to be see-through: 7's glass, with the
    /// skin's `see-through`. Asked for in every frame it should stay so; from the next frame on it
    /// is (Windows 11 only, see frame.rs `set_glass`), and `glass_bottom` says how far.
    pub fn ask_glass(&mut self, bottom: f32) {
        self.glass.asked = self.glass.asked.max(bottom);
    }

    /// How far down from the window's top it is see-through now, in DIPs (0: not at all). There,
    /// whatever is drawn see-through lets what is behind the window show.
    pub fn glass_bottom(&self) -> f32 {
        self.glass.on as f32 / self.scale()
    }

    /// Where the see-through top must be painted solid (Windows' own window buttons are there,
    /// unseen: see frame.rs `caption_buttons`), if anywhere.
    pub fn glass_solid(&self) -> Option<Rect> {
        self.glass.solid.filter(|_| self.glass.on > 0)
    }

    /// Makes `r` fully see-through, to be painted over (only within `glass_bottom`).
    pub fn clear_glass(&mut self, r: Rect) {
        let bottom = self.glass_bottom();
        if r.y >= bottom {
            return;
        }
        self.push_clip(Rect::new(r.x, r.y, r.w, r.h.min(bottom - r.y)));
        if let Some(t) = self.target.as_ref().filter(|_| self.in_frame) {
            // SAFETY: the colour lives across the call; Clear keeps to the clip just pushed.
            unsafe { t.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 })) };
        }
        self.pop_clip();
    }

    /// After a frame: turns the glass on, off, or to a new height when the frame asked for other
    /// than what is on, and draws again (a target with see-through pixels is made for it).
    fn settle_glass(&mut self) {
        let asked = std::mem::take(&mut self.glass.asked);
        let want = if self.glass.cannot { 0 } else { (asked * self.scale()).floor() as i32 };
        if want == self.glass.on {
            return;
        }
        let now = if crate::frame::set_glass(self.hwnd, want) { want } else { 0 };
        if want > 0 && now == 0 {
            // This Windows (or this window) can't: drawn solid from now on, without asking again.
            self.glass.cannot = true;
        }
        if now == self.glass.on {
            return;
        }
        if (now > 0) != (self.glass.on > 0) {
            self.target = None;
        }
        self.glass.on = now;
        // SAFETY: our own window; it draws again with the glass as it now is.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Moves everything drawn from now on by (`x`, `y`) DIPs, until the next call or frame (a
    /// window drawing its parts inside a frame of its own: frame.rs `paint_classic_border`).
    pub fn set_origin(&mut self, x: f32, y: f32) {
        if let Some(t) = self.target.as_ref().filter(|_| self.in_frame) {
            // SAFETY: a plain setter on a live render target.
            unsafe { t.rt.SetTransform(&windows_numerics::Matrix3x2::translation(x, y)) };
        }
    }

    /// Pixel-snaps a DIP value (so edges fall on whole screen pixels).
    pub fn snap(&self, v: f32) -> f32 {
        let s = self.scale();
        (v * s).round() / s
    }

    fn snapped(&self, r: Rect) -> D2D_RECT_F {
        D2D_RECT_F { left: self.snap(r.x), top: self.snap(r.y), right: self.snap(r.right()), bottom: self.snap(r.bottom()) }
    }

    /// Sets the one brush to `color` and returns the target (None outside a frame).
    fn paint_with(&self, color: Color) -> Option<&Target> {
        let t = self.target.as_ref().filter(|_| self.in_frame)?;
        // SAFETY: plain setter; `color_f` returns a value that lives across the call.
        unsafe { t.brush.SetColor(&color_f(self.tinted(color))) };
        Some(t)
    }

    /// Until `set_tint(None)`, everything drawn goes `amount` of the way toward `color` (its
    /// opacity kept): 98 / 2000 blend a selected icon half way into the selection's colour.
    pub fn set_tint(&mut self, tint: Option<(Color, f32)>) {
        self.tint = tint;
    }

    /// `c` as drawn now (see `set_tint`).
    fn tinted(&self, c: Color) -> Color {
        match self.tint {
            Some((to, amount)) => c.mix(Color { a: c.a, ..to }, amount),
            None => c,
        }
    }

    /// `g` as drawn now (see `set_tint`).
    fn tinted_gradient(&self, g: &Gradient) -> Gradient {
        if self.tint.is_none() {
            return *g;
        }
        let mut stops = [Stop { at: 0.0, color: Color::rgba(0, 0, 0, 0.0) }; MAX_STOPS];
        let n = g.stops().len();
        for (d, s) in stops.iter_mut().zip(g.stops()) {
            *d = Stop { at: s.at, color: self.tinted(s.color) };
        }
        Gradient::new(&stops[..n]).unwrap_or(*g)
    }

    /// Fills the whole window.
    pub fn clear(&mut self, color: Color) {
        if let Some(t) = self.target.as_ref().filter(|_| self.in_frame) {
            // SAFETY: the colour lives across the call.
            unsafe { t.rt.Clear(Some(&color_f(color))) };
        }
    }

    /// A filled rectangle with pixel-sharp edges.
    pub fn fill_rect(&mut self, r: Rect, color: Color) {
        if r.is_empty() || color.a <= 0.0 {
            return;
        }
        let rect = self.snapped(r);
        if let Some(t) = self.paint_with(color) {
            // SAFETY: `rect` lives across the call; the brush belongs to this target.
            unsafe { t.rt.FillRectangle(&rect, &t.brush) };
        }
    }

    /// A filled rectangle with rounded corners (e.g. the scrollbar thumb).
    pub fn fill_round_rect(&mut self, r: Rect, radius: f32, color: Color) {
        if r.is_empty() || color.a <= 0.0 {
            return;
        }
        let rr = D2D1_ROUNDED_RECT { rect: self.snapped(r), radiusX: radius, radiusY: radius };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: `rr` lives across the call.
            unsafe { t.rt.FillRoundedRectangle(&rr, &t.brush) };
        }
    }

    /// The outline of a rounded rectangle, `width` DIPs thick, centred on `r`'s edges (a rounded
    /// skin's focus rings and frames).
    pub fn stroke_round_rect(&mut self, r: Rect, radius: f32, width: f32, color: Color) {
        if r.is_empty() || color.a <= 0.0 {
            return;
        }
        let rr = D2D1_ROUNDED_RECT { rect: D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }, radiusX: radius, radiusY: radius };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: `rr` lives across the call; brush and stroke style are live.
            unsafe { t.rt.DrawRoundedRectangle(&rr, &t.brush, width, &self.round) };
        }
    }

    /// A rectangle outline `width` DIPs thick, drawn inside `r`.
    pub fn stroke_rect(&mut self, r: Rect, width: f32, color: Color) {
        self.fill_rect(Rect::new(r.x, r.y, r.w, width), color);
        self.fill_rect(Rect::new(r.x, r.bottom() - width, r.w, width), color);
        self.fill_rect(Rect::new(r.x, r.y + width, width, r.h - 2.0 * width), color);
        self.fill_rect(Rect::new(r.right() - width, r.y + width, width, r.h - 2.0 * width), color);
    }

    /// A smooth (anti-aliased) line with round ends, for drawing glyphs.
    pub fn line(&mut self, a: Point, b: Point, width: f32, color: Color) {
        if let Some(t) = self.paint_with(color) {
            // SAFETY: plain values; brush and stroke style are live.
            unsafe { t.rt.DrawLine(vec2(a), vec2(b), &t.brush, width, &self.round) };
        }
    }

    /// Connected lines through `points` (open shape), round joins - chevrons, arrows, ticks.
    pub fn polyline(&mut self, points: &[Point], width: f32, color: Color) {
        if points.len() < 2 {
            return;
        }
        let Some(geo) = self.path(points, false) else { return };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: the geometry, brush and stroke style are live.
            unsafe { t.rt.DrawGeometry(&geo, &t.brush, width, &self.round) };
        }
    }

    /// A filled shape through `points` (closed automatically) - the slanted logo, path-step boxes.
    pub fn fill_polygon(&mut self, points: &[Point], color: Color) {
        if points.len() < 3 {
            return;
        }
        let Some(geo) = self.path(points, true) else { return };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: the geometry and brush are live; no opacity brush.
            unsafe { t.rt.FillGeometry(&geo, &t.brush, None) };
        }
    }

    fn path(&self, points: &[Point], filled: bool) -> Option<ID2D1PathGeometry> {
        // SAFETY: the sink is opened, filled and closed in order; all points are plain values.
        unsafe {
            let geo = self.d2d.CreatePathGeometry().ok()?;
            let sink = geo.Open().ok()?;
            sink.BeginFigure(vec2(points[0]), if filled { D2D1_FIGURE_BEGIN_FILLED } else { D2D1_FIGURE_BEGIN_HOLLOW });
            let rest: Vec<Vector2> = points[1..].iter().map(|p| vec2(*p)).collect();
            sink.AddLines(&rest);
            sink.EndFigure(if filled { D2D1_FIGURE_END_CLOSED } else { D2D1_FIGURE_END_OPEN });
            sink.Close().ok()?;
            Some(geo)
        }
    }

    fn format(&mut self, style: &TextStyle) -> Option<IDWriteTextFormat> {
        let key = ((style.size * 100.0) as u32, style.weight, style.align, style.title);
        if let Some(f) = self.formats.get(&key) {
            return Some(f.clone());
        }
        let family = match (&self.title_family, style.title) {
            (Some(t), true) => t.clone(),
            _ => self.family.clone(),
        };
        // SAFETY: plain DirectWrite calls; the family string and locale outlive them.
        let format = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    &family,
                    None::<&IDWriteFontCollection>,
                    DWRITE_FONT_WEIGHT(style.weight as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    style.size * self.text_scale,
                    &HSTRING::from("en-us"),
                )
                .ok()?;
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetTextAlignment(match style.align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            // Too long for its box: cut at a letter and end with "…".
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&f) {
                let _ = f.SetTrimming(&trimming, &sign);
            }
            f
        };
        self.formats.insert(key, format.clone());
        Some(format)
    }

    /// One line of text in `r`: vertically centred, aligned as the style says, cut with "…" when too long.
    ///
    /// Each text is laid out (shaped, measured, trimmed) once by DirectWrite and the layout kept, so
    /// the next frames only draw it: a list row costs the same whether it's new or old, and
    /// scrolling or hovering doesn't lay out the whole screen again. (Direct2D's own DrawText would
    /// redo the layout on every call.)
    pub fn text(&mut self, text: &str, r: Rect, style: &TextStyle) {
        if text.is_empty() || r.w <= 0.0 || !self.in_frame {
            return;
        }
        let Some(layout) = self.layout(text, r.w, r.h, style, false) else { return };
        if let Some(t) = self.paint_with(style.color) {
            // SAFETY: the layout and brush live across the call.
            unsafe { t.rt.DrawTextLayout(vec2(Point::new(r.x, r.y)), &layout, &t.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        }
    }

    /// Text broken into as many lines as fit in `r` (at spaces, or inside a long word), from the
    /// top; the last line that fits ends with "…" when there's more. For names under pictures.
    pub fn text_wrapped(&mut self, text: &str, r: Rect, style: &TextStyle) {
        if text.is_empty() || r.w <= 0.0 || !self.in_frame {
            return;
        }
        let Some(layout) = self.layout(text, r.w, r.h, style, true) else { return };
        if let Some(t) = self.paint_with(style.color) {
            // SAFETY: the layout and brush live across the call.
            unsafe { t.rt.DrawTextLayout(vec2(Point::new(r.x, r.y)), &layout, &t.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        }
    }

    /// The kept layout of `text` in a `w` x `h` box, made if needed; `wrap`: in several lines.
    fn layout(&mut self, text: &str, w: f32, h: f32, style: &TextStyle, wrap: bool) -> Option<IDWriteTextLayout> {
        let key = LayoutKey {
            text: text.to_string(),
            size: (style.size * 100.0) as u32,
            weight: style.weight,
            align: style.align,
            w: (w * 4.0).round() as i32,
            h: (h * 4.0).round() as i32,
            wrap,
            title: style.title,
        };
        if let Some(layout) = self.layouts.get(&key) {
            return Some(layout.clone());
        }
        let layout = match self.old_layouts.remove(&key) {
            Some(layout) => layout,
            None => {
                let format = self.format(style)?;
                self.wide.clear();
                self.wide.extend(text.encode_utf16());
                // SAFETY: the UTF-16 buffer and the format live across the call.
                let layout = unsafe { self.dwrite.CreateTextLayout(&self.wide, &format, w, h) }.ok()?;
                if wrap {
                    // SAFETY: plain setters on the layout just made (a layout is also a text format).
                    unsafe {
                        // Break inside a word too when it's longer than a line (names without spaces).
                        if layout.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK).is_err() {
                            let _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
                        }
                        let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                    }
                }
                layout
            }
        };
        if self.layouts.len() >= MAX_CACHED_TEXTS {
            self.old_layouts = std::mem::take(&mut self.layouts);
        }
        self.layouts.insert(key, layout.clone());
        Some(layout)
    }

    /// How wide `text` is in this style, in DIPs.
    pub fn measure(&mut self, text: &str, style: &TextStyle) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let key = (text.to_string(), (style.size * 100.0) as u32, style.weight, style.title);
        if let Some(&w) = self.widths.get(&key) {
            return w;
        }
        let left = TextStyle { align: Align::Left, ..*style };
        let Some(format) = self.format(&left) else { return 0.0 };
        self.wide.clear();
        self.wide.extend(text.encode_utf16());
        // SAFETY: the buffer and format live across the calls; `m` is a valid out-pointer.
        let width = unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&self.wide, &format, 100_000.0, 1_000.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return 0.0;
            }
            m.widthIncludingTrailingWhitespace
        };
        if self.widths.len() >= MAX_CACHED_TEXTS {
            self.widths.clear();
        }
        self.widths.insert(key, width);
        width
    }

    /// Draws a cached Windows icon (see icons.rs) into `r` (normally 16 x 16 DIPs).
    pub fn icon(&mut self, icons: &Icons, id: IconId, r: Rect) {
        if !self.in_frame {
            return;
        }
        let rect = self.snapped(r);
        let Some(t) = self.target.as_mut() else { return };
        let i = id.index();
        if t.bitmaps.len() <= i {
            t.bitmaps.resize(i + 1, None);
        }
        if t.bitmaps[i].is_none() {
            let Some(wic) = icons.bitmap(id) else { return };
            // SAFETY: `wic` is a live WIC bitmap in 32bppPBGRA, which Direct2D accepts as-is.
            t.bitmaps[i] = unsafe { t.rt.CreateBitmapFromWicBitmap(&wic, None) }.ok();
        }
        if let Some(bmp) = &t.bitmaps[i] {
            // SAFETY: bitmap and rect live across the call.
            unsafe { t.rt.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None) };
            if let Some((to, amount)) = self.tint {
                // The veil (see `set_tint`): the colour over the icon's own shape. Direct2D draws
                // an opacity mask only without antialiasing.
                // SAFETY: plain calls on our own target; the brush, bitmap and rect live across them.
                unsafe {
                    let mode = t.rt.GetAntialiasMode();
                    t.rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
                    t.brush.SetColor(&color_f(to.with_alpha(amount)));
                    t.rt.FillOpacityMask(bmp, &t.brush, D2D1_OPACITY_MASK_CONTENT_GRAPHICS, Some(&rect), None);
                    t.rt.SetAntialiasMode(mode);
                }
            }
        }
    }

    /// Only draw inside `r` until the matching [`Renderer::pop_clip`].
    pub fn push_clip(&mut self, r: Rect) {
        let rect = self.snapped(r);
        if let Some(t) = self.target.as_ref().filter(|_| self.in_frame) {
            // SAFETY: balanced by pop_clip (and `end` pops any left over).
            unsafe { t.rt.PushAxisAlignedClip(&rect, D2D1_ANTIALIAS_MODE_ALIASED) };
            self.clip_depth += 1;
        }
    }

    pub fn pop_clip(&mut self) {
        if self.clip_depth == 0 {
            return;
        }
        if let Some(t) = &self.target {
            // SAFETY: there is a matching PushAxisAlignedClip (clip_depth > 0).
            unsafe { t.rt.PopAxisAlignedClip() };
        }
        self.clip_depth -= 1;
    }
}

// ---------------------------------------------------------------- paints: gradients and glass

/// The soft shadow under a marble: dark in the middle, fading out at its edge.
const MARBLE_SHADOW: [Stop; 3] = [
    Stop { at: 0.0, color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.32 } },
    Stop { at: 0.72, color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.26 } },
    Stop { at: 1.0, color: Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 } },
];

// (Not every part draws with all of these yet.)
#[allow(dead_code)]
impl Renderer {
    /// Fills `r` with a paint (theme.rs `Paint`), with corners `radius` DIPs round (0: square).
    /// A flat paint draws exactly as `fill_rect` / `fill_round_rect`; a gradient runs across the
    /// short side of `r`: top to bottom when it's wider than tall, left to right otherwise.
    pub fn fill_paint(&mut self, r: Rect, radius: f32, paint: &Paint) {
        let g = match paint {
            Paint::Flat(c) if radius > 0.0 => return self.fill_round_rect(r, radius, *c),
            Paint::Flat(c) => return self.fill_rect(r, *c),
            Paint::Gradient(g) => g,
        };
        if r.is_empty() || paint.is_clear() || !self.in_frame {
            return;
        }
        let rect = self.snapped(r);
        let (a, b) = across(rect);
        let Some(brush) = self.linear_brush(g, a, b) else { return };
        let Some(t) = &self.target else { return };
        // SAFETY: the rectangle lives across the calls; the brush belongs to this target.
        unsafe {
            if radius > 0.0 {
                t.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &brush);
            } else {
                t.rt.FillRectangle(&rect, &brush);
            }
        }
    }

    /// A filled shape through `points` (closed automatically) in a paint - a slanted box. A
    /// gradient runs across the short side of the shape's bounding box.
    pub fn fill_polygon_paint(&mut self, points: &[Point], paint: &Paint) {
        let g = match paint {
            Paint::Flat(c) => return self.fill_polygon(points, *c),
            Paint::Gradient(g) => g,
        };
        if points.len() < 3 || paint.is_clear() || !self.in_frame {
            return;
        }
        let (mut lo, mut hi) = (points[0], points[0]);
        for p in points {
            (lo.x, lo.y, hi.x, hi.y) = (lo.x.min(p.x), lo.y.min(p.y), hi.x.max(p.x), hi.y.max(p.y));
        }
        let (a, b) = across(D2D_RECT_F { left: lo.x, top: lo.y, right: hi.x, bottom: hi.y });
        let Some(geo) = self.path(points, true) else { return };
        let Some(brush) = self.linear_brush(g, a, b) else { return };
        let Some(t) = &self.target else { return };
        // SAFETY: the geometry and brush are live; no opacity brush.
        unsafe { t.rt.FillGeometry(&geo, &brush, None) };
    }

    /// A glass marble `radius` DIPs round centred on `c` (caption buttons, the toolbar's liquid
    /// buttons): the paint as a round gradient lit from above (its first colour where the light
    /// falls, its last at the rim), a soft shadow under it and a white shine on top. A flat paint
    /// gets a lighter top and a darker rim of its own colour.
    pub fn marble(&mut self, c: Point, radius: f32, paint: &Paint) {
        if radius <= 0.0 || paint.is_clear() || !self.in_frame {
            return;
        }
        let shadow = Gradient::new(&MARBLE_SHADOW);
        if let Some(shadow) = shadow {
            let at = Point::new(c.x, c.y + radius * 0.12 + 0.5);
            self.fill_ellipse_radial(at, radius * 1.12 + 0.5, &shadow, at, radius * 1.12 + 0.5);
        }
        // Lit from a point a third of the way down (as CSS's "circle at 50% 32%"), the rim
        // reaching to the far corners.
        if let Paint::Gradient(glass) = marble_glass(paint) {
            let light = Point::new(c.x, c.y - radius * 0.36);
            self.fill_ellipse_radial(c, radius, &glass, light, radius * 1.7);
        }
        let shine = Rect::new(c.x - radius * 0.58, c.y - radius * 0.86, radius * 1.16, radius * 0.72);
        self.gloss_ellipse(shine, 0.85);
    }

    /// A glossy pill filling `r` (the liquid SORT, VIEW and SETTINGS): the paint laid out like lit
    /// glass (light at the top, darkest across the middle, a glow of light at the bottom), a white
    /// shine over the top ~40 %, a thin darker edge and a slight shadow under it.
    pub fn liquid_capsule(&mut self, r: Rect, paint: &Paint) {
        if r.is_empty() || paint.is_clear() || !self.in_frame {
            return;
        }
        let radius = (r.h.min(r.w) / 2.0).max(0.0);
        self.fill_round_rect(Rect::new(r.x, r.y + 1.0, r.w, r.h), radius, Color::rgba(0, 0, 0, 0.22));
        self.fill_paint(r, radius, &capsule_glass(paint));
        let inset = radius * 0.6;
        let band = Rect::new(r.x + inset, r.y + 1.0, (r.w - 2.0 * inset).max(0.0), r.h * 0.4);
        self.gloss(band, band.h / 2.0, 0.72);
        let edge = paint.last().darker(0.3);
        self.stroke_round_rect(r.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, edge.with_alpha(edge.a * 0.45));
    }

    /// A soft white shine over `r` (corners `radius` round): white at `strength` opacity (0 to 1)
    /// at the top, fading to almost nothing at the bottom. Put it over the top part of a shape.
    pub fn gloss(&mut self, r: Rect, radius: f32, strength: f32) {
        if r.is_empty() || strength <= 0.0 {
            return;
        }
        self.fill_paint(r, radius, &shine(strength));
    }

    /// Fills `r` (corners `radius` round) with a paint whose gradient runs down from `top` to
    /// `bottom`, whatever `r`'s shape: a tall sidebar shaded top to bottom like the bars, or a
    /// part of a bar (a header with only its top corners round, drawn clipped) shaded as the
    /// whole bar is. A flat paint draws as `fill_paint` does.
    pub fn fill_paint_down(&mut self, r: Rect, radius: f32, paint: &Paint, top: f32, bottom: f32) {
        let g = match paint {
            Paint::Flat(c) if radius > 0.0 => return self.fill_round_rect(r, radius, *c),
            Paint::Flat(c) => return self.fill_rect(r, *c),
            Paint::Gradient(g) => g,
        };
        if r.is_empty() || paint.is_clear() || !self.in_frame {
            return;
        }
        let rect = self.snapped(r);
        let (a, b) = (Vector2 { X: rect.left, Y: self.snap(top) }, Vector2 { X: rect.left, Y: self.snap(bottom) });
        let Some(brush) = self.linear_brush(g, a, b) else { return };
        let Some(t) = &self.target else { return };
        // SAFETY: the rectangle lives across the calls; the brush belongs to this target.
        unsafe {
            if radius > 0.0 {
                t.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &brush);
            } else {
                t.rt.FillRectangle(&rect, &brush);
            }
        }
    }

    /// Fills `r` with `paint` running left to right, whatever the shape (a title bar's
    /// `title-bar-across`).
    pub fn fill_paint_across(&mut self, r: Rect, paint: &Paint) {
        let g = match paint {
            Paint::Flat(c) => return self.fill_rect(r, *c),
            Paint::Gradient(g) => g,
        };
        if r.is_empty() || paint.is_clear() || !self.in_frame {
            return;
        }
        let rect = self.snapped(r);
        let (a, b) = (Vector2 { X: rect.left, Y: rect.top }, Vector2 { X: rect.right, Y: rect.top });
        let Some(brush) = self.linear_brush(g, a, b) else { return };
        let Some(t) = &self.target else { return };
        // SAFETY: the rectangle lives across the calls; the brush belongs to this target.
        unsafe { t.rt.FillRectangle(&rect, &brush) };
    }

    /// The linear brush of `g`, made the first time, set to run from `a` to `b`.
    fn linear_brush(&mut self, g: &Gradient, a: Vector2, b: Vector2) -> Option<ID2D1LinearGradientBrush> {
        let g = &self.tinted_gradient(g);
        let t = self.target.as_mut().filter(|_| self.in_frame)?;
        let kept = gradient_brushes(&t.rt, &mut t.gradients, g)?;
        if kept.linear.is_none() {
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: a, endPoint: b };
            // SAFETY: the properties live across the call; the stop collection belongs to this target.
            kept.linear = unsafe { t.rt.CreateLinearGradientBrush(&props, None, &kept.stops) }.ok();
        }
        let brush = kept.linear.clone()?;
        // SAFETY: plain setters on a live brush.
        unsafe {
            brush.SetStartPoint(a);
            brush.SetEndPoint(b);
        }
        Some(brush)
    }

    /// Fills the circle of `radius` round `c` with `g` as a round gradient: its first colour at
    /// `light`, its last `reach` DIPs away (and beyond).
    fn fill_ellipse_radial(&mut self, c: Point, radius: f32, g: &Gradient, light: Point, reach: f32) {
        let g = &self.tinted_gradient(g);
        let Some(t) = self.target.as_mut().filter(|_| self.in_frame) else { return };
        let Some(kept) = gradient_brushes(&t.rt, &mut t.gradients, g) else { return };
        if kept.radial.is_none() {
            let props = D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: vec2(light), gradientOriginOffset: Vector2 { X: 0.0, Y: 0.0 }, radiusX: reach, radiusY: reach };
            // SAFETY: the properties live across the call; the stop collection belongs to this target.
            kept.radial = unsafe { t.rt.CreateRadialGradientBrush(&props, None, &kept.stops) }.ok();
        }
        let Some(brush) = kept.radial.clone() else { return };
        let ellipse = D2D1_ELLIPSE { point: vec2(c), radiusX: radius, radiusY: radius };
        // SAFETY: plain setters on a live brush; the ellipse lives across the call.
        unsafe {
            brush.SetCenter(vec2(light));
            brush.SetGradientOriginOffset(Vector2 { X: 0.0, Y: 0.0 });
            brush.SetRadiusX(reach);
            brush.SetRadiusY(reach);
            t.rt.FillEllipse(&ellipse, &brush);
        }
    }

    /// A white shine in the ellipse filling `r`, fading from `strength` at its top to nothing.
    fn gloss_ellipse(&mut self, r: Rect, strength: f32) {
        let Paint::Gradient(g) = shine(strength) else { return };
        let Some(brush) = self.linear_brush(&g, Vector2 { X: r.x, Y: r.y }, Vector2 { X: r.x, Y: r.bottom() }) else { return };
        let Some(t) = &self.target else { return };
        let ellipse = D2D1_ELLIPSE { point: vec2(Point::new(r.x + r.w / 2.0, r.y + r.h / 2.0)), radiusX: r.w / 2.0, radiusY: r.h / 2.0 };
        // SAFETY: the ellipse lives across the call; the brush belongs to this target.
        unsafe { t.rt.FillEllipse(&ellipse, &brush) };
    }
}

impl Renderer {
    /// Shapes drawn from here on get hard edges (`true`: every pixel all or nothing, as the icons
    /// of Windows 2000 were) or smooth ones again (`false`, the normal).
    pub fn set_aliased(&mut self, on: bool) {
        if let Some(t) = self.target.as_ref().filter(|_| self.in_frame) {
            let mode = if on { D2D1_ANTIALIAS_MODE_ALIASED } else { D2D1_ANTIALIAS_MODE_PER_PRIMITIVE };
            // SAFETY: a plain setter on a live render target.
            unsafe { t.rt.SetAntialiasMode(mode) };
        }
    }

    /// A filled ellipse with radii `rx` and `ry` round `c`.
    pub fn fill_ellipse(&mut self, c: Point, rx: f32, ry: f32, color: Color) {
        if rx <= 0.0 || ry <= 0.0 || color.a <= 0.0 {
            return;
        }
        let e = D2D1_ELLIPSE { point: vec2(c), radiusX: rx, radiusY: ry };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: the ellipse lives across the call; the brush belongs to this target.
            unsafe { t.rt.FillEllipse(&e, &t.brush) };
        }
    }

    /// An ellipse's outline, `width` DIPs thick, centred on its edge.
    pub fn stroke_ellipse(&mut self, c: Point, rx: f32, ry: f32, width: f32, color: Color) {
        if rx <= 0.0 || ry <= 0.0 || color.a <= 0.0 {
            return;
        }
        let e = D2D1_ELLIPSE { point: vec2(c), radiusX: rx, radiusY: ry };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: the ellipse lives across the call; brush and stroke style are live.
            unsafe { t.rt.DrawEllipse(&e, &t.brush, width, &self.round) };
        }
    }

    /// A filled ellipse in a paint whose gradient runs from `a` to `b`.
    pub fn fill_ellipse_toward(&mut self, c: Point, rx: f32, ry: f32, paint: &Paint, a: Point, b: Point) {
        let g = match paint {
            Paint::Flat(col) => return self.fill_ellipse(c, rx, ry, *col),
            Paint::Gradient(g) => g,
        };
        if rx <= 0.0 || ry <= 0.0 || !self.in_frame {
            return;
        }
        let Some(brush) = self.linear_brush(g, vec2(a), vec2(b)) else { return };
        let Some(t) = &self.target else { return };
        let e = D2D1_ELLIPSE { point: vec2(c), radiusX: rx, radiusY: ry };
        // SAFETY: the ellipse lives across the call; the brush belongs to this target.
        unsafe { t.rt.FillEllipse(&e, &brush) };
    }

    /// A filled shape through `points` in a paint whose gradient runs from `a` to `b` (any
    /// direction: an icon lit from the top left).
    pub fn fill_polygon_toward(&mut self, points: &[Point], paint: &Paint, a: Point, b: Point) {
        let g = match paint {
            Paint::Flat(c) => return self.fill_polygon(points, *c),
            Paint::Gradient(g) => g,
        };
        if points.len() < 3 || paint.is_clear() || !self.in_frame {
            return;
        }
        let Some(geo) = self.path(points, true) else { return };
        let Some(brush) = self.linear_brush(g, vec2(a), vec2(b)) else { return };
        let Some(t) = &self.target else { return };
        // SAFETY: the geometry and brush are live; no opacity brush.
        unsafe { t.rt.FillGeometry(&geo, &brush, None) };
    }

    /// A rectangle (corners `radius` round) in a paint whose gradient runs from `a` to `b`.
    pub fn fill_rect_toward(&mut self, r: Rect, radius: f32, paint: &Paint, a: Point, b: Point) {
        let g = match paint {
            Paint::Flat(c) if radius > 0.0 => return self.fill_round_rect(r, radius, *c),
            Paint::Flat(c) => return self.fill_rect(r, *c),
            Paint::Gradient(g) => g,
        };
        if r.is_empty() || paint.is_clear() || !self.in_frame {
            return;
        }
        let rect = self.snapped(r);
        let Some(brush) = self.linear_brush(g, vec2(a), vec2(b)) else { return };
        let Some(t) = &self.target else { return };
        // SAFETY: the rectangle lives across the calls; the brush belongs to this target.
        unsafe {
            if radius > 0.0 {
                t.rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius }, &brush);
            } else {
                t.rt.FillRectangle(&rect, &brush);
            }
        }
    }

    /// The outline of the closed shape through `points`, `width` DIPs thick (joins round).
    pub fn stroke_polygon(&mut self, points: &[Point], width: f32, color: Color) {
        if points.len() < 2 || color.a <= 0.0 {
            return;
        }
        let Some(geo) = self.path(points, true) else { return };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: the geometry, brush and stroke style are live.
            unsafe { t.rt.DrawGeometry(&geo, &t.brush, width, &self.round) };
        }
    }
}

/// The brushes kept for `g` (made with `rt`), its stop collection made the first time.
fn gradient_brushes<'a>(rt: &ID2D1HwndRenderTarget, kept: &'a mut HashMap<GradientKey, GradientBrushes>, g: &Gradient) -> Option<&'a mut GradientBrushes> {
    let key = GradientKey::of(g);
    if !kept.contains_key(&key) {
        if kept.len() >= MAX_CACHED_GRADIENTS {
            kept.clear();
        }
        // Direct2D takes a gradient's colours with straight opacity; Wine's takes them premultiplied
        // (a see-through white would come out solid white there).
        let premultiply = crate::frame::is_wine();
        let mut stops = [D2D1_GRADIENT_STOP::default(); MAX_STOPS];
        for (d, s) in stops.iter_mut().zip(g.stops()) {
            let c = if premultiply { Color { r: s.color.r * s.color.a, g: s.color.g * s.color.a, b: s.color.b * s.color.a, a: s.color.a } } else { s.color };
            *d = D2D1_GRADIENT_STOP { position: s.at, color: color_f(c) };
        }
        // SAFETY: the stops live across the call, which copies them.
        let collection = unsafe { rt.CreateGradientStopCollection(&stops[..g.stops().len()], D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP) }.ok()?;
        kept.insert(key, GradientBrushes { stops: collection, linear: None, radial: None });
    }
    kept.get_mut(&key)
}

/// Where a gradient starts and ends across a box: top to bottom on a wide box, left to right on a
/// tall one.
fn across(r: D2D_RECT_F) -> (Vector2, Vector2) {
    if r.right - r.left >= r.bottom - r.top {
        (Vector2 { X: r.left, Y: r.top }, Vector2 { X: r.left, Y: r.bottom })
    } else {
        (Vector2 { X: r.left, Y: r.top }, Vector2 { X: r.right, Y: r.top })
    }
}

/// White from `strength` opacity to almost nothing: a shine.
fn shine(strength: f32) -> Paint {
    let s = strength.clamp(0.0, 1.0);
    let white = Color::hex(0xffffff);
    Paint::from_stops(&[Stop { at: 0.0, color: white.with_alpha(s) }, Stop { at: 1.0, color: white.with_alpha(s * 0.07) }])
}

/// A paint as a marble's glass: a gradient as it is (light point first, rim last); one colour
/// gets a light point and a rim of its own.
fn marble_glass(paint: &Paint) -> Paint {
    match paint {
        Paint::Gradient(_) => *paint,
        Paint::Flat(c) => Paint::from_stops(&[Stop { at: 0.0, color: c.lighter(0.7) }, Stop { at: 0.45, color: *c }, Stop { at: 1.0, color: c.darker(0.35) }]),
    }
}

/// A paint as a capsule's glass, from its light, middle and dark colours (as for a marble): light
/// at the top, the middle colour going to the darkest across the middle, and a glow at the bottom
/// where light comes through the glass.
fn capsule_glass(paint: &Paint) -> Paint {
    let g = marble_glass(paint);
    let (light, mid, dark) = (g.color_at(0.0), g.color_at(0.45), g.color_at(1.0));
    Paint::from_stops(&[
        Stop { at: 0.0, color: light.mix(mid, 0.55) },
        Stop { at: 0.35, color: mid.mix(dark, 0.45) },
        Stop { at: 0.55, color: dark },
        Stop { at: 0.78, color: dark.darker(0.12) },
        Stop { at: 1.0, color: mid.mix(dark, 0.6) },
    ])
}

// ---------------------------------------------------------------- pictures and monospaced text

/// Fonts to try for monospaced text, best first.
const MONO_FAMILIES: &[&str] = &["Cascadia Mono", "Consolas", "DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono", "Courier New"];

/// The pixel format of pictures: premultiplied BGRA, at 96 DPI (one pixel is one DIP before scaling).
fn picture_props() -> D2D1_BITMAP_PROPERTIES {
    D2D1_BITMAP_PROPERTIES {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
        dpiX: 96.0,
        dpiY: 96.0,
    }
}

impl Renderer {
    /// Draws a picture (thumbs.rs) stretched into `dest`, or the part `src` of it (in the picture's
    /// pixels). `smooth`: shrinking or growing blends pixels (high-quality cubic where Direct2D
    /// 1.1 is there, else linear); otherwise each pixel stays a sharp square (zooming in a lot).
    ///
    /// The Direct2D copy is made the first time (here, on the UI thread) and the picture's own
    /// pixels are let go then. Returns false when it couldn't be drawn: after a lost graphics
    /// device the pixels are gone, so the picture is marked lost (`Picture::is_lost`) and whoever
    /// made it makes it again.
    pub fn picture(&mut self, pic: &Arc<Picture>, dest: Rect, src: Option<Rect>, opacity: f32, smooth: bool) -> bool {
        if !self.in_frame || dest.is_empty() {
            return false;
        }
        let rect = self.snapped(dest);
        let Some(t) = self.target.as_mut() else { return false };
        let id = pic.id();
        if !t.pictures.bitmaps.contains_key(&id) {
            let Some(pixels) = pic.take_pixels() else {
                pic.mark_lost();
                return false;
            };
            let size = D2D_SIZE_U { width: pic.width(), height: pic.height() };
            // SAFETY: `pixels` holds exactly height rows of width * 4 bytes (Picture::new checks) and
            // lives across the call, which copies them.
            match unsafe { t.rt.CreateBitmap(size, Some(pixels.as_ptr() as *const _), pic.width() * 4, &picture_props()) } {
                Ok(bmp) => {
                    t.pictures.bitmaps.insert(id, (Arc::downgrade(pic), bmp));
                }
                // Too big for this graphics card (pictures are made under `max_bitmap_size`), or no
                // memory there right now: the picture keeps its pixels for the next try, rather
                // than looking lost and being made again on every frame.
                Err(_) => {
                    pic.give_back(pixels);
                    return false;
                }
            }
        }
        let Some((_, bmp)) = t.pictures.bitmaps.get(&id) else { return false };
        let src = src.map(|s| D2D_RECT_F { left: s.x, top: s.y, right: s.right(), bottom: s.bottom() });
        let src_ptr = src.as_ref().map(|s| s as *const D2D_RECT_F);
        // SAFETY: the bitmap belongs to this target; the rectangles live across the calls.
        unsafe {
            match (&t.pictures.dc, smooth) {
                (Some(dc), true) => dc.DrawBitmap(bmp, Some(&rect), opacity, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, src_ptr, None),
                _ => {
                    let mode = if smooth { D2D1_BITMAP_INTERPOLATION_MODE_LINEAR } else { D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR };
                    t.rt.DrawBitmap(bmp, Some(&rect), opacity, mode, src_ptr)
                }
            }
        }
        true
    }

    /// Draws picture `index` of the chrome logo (logo.rs), for a dark skin or a light one, into
    /// `dest`. `exact`: `dest` is the picture's own size on whole pixels, so it is copied pixel for
    /// pixel; otherwise it is shrunk smoothly (high-quality cubic where Direct2D 1.1 is there).
    ///
    /// The Direct2D copy is made the first time and kept with the render target. After a lost
    /// graphics device it is made again from the pixels logo.rs decoded once, never decoded again.
    pub fn wordmark(&mut self, dark: bool, index: usize, dest: Rect, exact: bool) {
        if !self.in_frame || dest.is_empty() || index >= crate::logo::COUNT {
            return;
        }
        let Some(t) = self.target.as_mut() else { return };
        let slot = if dark { index } else { crate::logo::COUNT + index };
        if t.wordmarks[slot].is_none() {
            let Some(px) = crate::logo::pixels(dark, index) else { return };
            let size = D2D_SIZE_U { width: px.width, height: px.height };
            // SAFETY: `pixels` holds exactly height rows of width * 4 bytes (logo.rs decodes them so)
            // and lives across the call, which copies them.
            t.wordmarks[slot] = unsafe { t.rt.CreateBitmap(size, Some(px.pixels.as_ptr() as *const _), px.width * 4, &picture_props()) }.ok();
        }
        let Some(bmp) = &t.wordmarks[slot] else { return };
        let rect = D2D_RECT_F { left: dest.x, top: dest.y, right: dest.right(), bottom: dest.bottom() };
        // SAFETY: the bitmap belongs to this target; the rectangle lives across the calls.
        unsafe {
            match (&t.pictures.dc, exact) {
                (Some(dc), false) => dc.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None),
                (_, true) => t.rt.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None),
                (None, false) => t.rt.DrawBitmap(bmp, Some(&rect), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
            }
        }
    }

    /// The biggest picture the graphics card takes, in pixels per side.
    pub fn max_bitmap_size(&self) -> u32 {
        // SAFETY: a plain query on a live render target.
        self.target.as_ref().map_or(4096, |t| unsafe { t.rt.GetMaximumBitmapSize() }).max(1024)
    }

    /// A checkerboard of `cell`-DIP squares in colours `a` and `b` (behind see-through pictures):
    /// one small tiled bitmap brush, so a big area costs a single fill.
    pub fn fill_checker(&mut self, r: Rect, cell: f32, a: Color, b: Color) {
        if r.is_empty() || !self.in_frame {
            return;
        }
        let rect = self.snapped(r);
        let Some(t) = self.target.as_mut() else { return };
        if !t.pictures.checker.as_ref().is_some_and(|(_, ca, cb)| *ca == a && *cb == b) {
            let px = |c: Color| {
                let v = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                [v(c.b), v(c.g), v(c.r), 255]
            };
            let (pa, pb) = (px(a), px(b));
            let pixels: Vec<u8> = [pa, pb, pb, pa].concat();
            let tile = D2D1_BITMAP_BRUSH_PROPERTIES {
                extendModeX: D2D1_EXTEND_MODE_WRAP,
                extendModeY: D2D1_EXTEND_MODE_WRAP,
                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            };
            // SAFETY: a 2 x 2 bitmap from the 16 bytes above (the call copies them), and a brush of it.
            let brush = unsafe {
                t.rt.CreateBitmap(D2D_SIZE_U { width: 2, height: 2 }, Some(pixels.as_ptr() as *const _), 8, &picture_props())
                    .and_then(|bmp| t.rt.CreateBitmapBrush(&bmp, Some(&tile), None))
            };
            t.pictures.checker = brush.ok().map(|br| (br, a, b));
        }
        let Some((brush, _, _)) = &t.pictures.checker else { return };
        // Each pixel of the 2 x 2 bitmap becomes a `cell` square, starting at the area's corner.
        let m = windows_numerics::Matrix3x2 { M11: cell, M12: 0.0, M21: 0.0, M22: cell, M31: rect.left, M32: rect.top };
        // SAFETY: the brush belongs to this target; `m` and `rect` live across the calls.
        unsafe {
            brush.SetTransform(&m);
            t.rt.FillRectangle(&rect, brush);
        }
    }

    /// One line of monospaced text in `r` (left, vertically centred, cut off at the right edge
    /// without "…"). Laid out once and kept, like `text`.
    pub fn mono_text(&mut self, text: &str, r: Rect, size: f32, color: Color) {
        if text.is_empty() || r.w <= 0.0 || !self.in_frame {
            return;
        }
        let key = (text.to_string(), (size * 100.0) as u32, (r.w * 4.0).round() as i32, (r.h * 4.0).round() as i32);
        let layout = match self.mono.layouts.get(&key) {
            Some(l) => l.clone(),
            None => {
                let made = match self.mono.old.remove(&key) {
                    Some(l) => Some(l),
                    None => self.mono_format(size).and_then(|format| {
                        self.wide.clear();
                        self.wide.extend(text.encode_utf16());
                        // SAFETY: the UTF-16 buffer and the format live across the call.
                        unsafe { self.dwrite.CreateTextLayout(&self.wide, &format, r.w, r.h) }.ok()
                    }),
                };
                let Some(l) = made else { return };
                if self.mono.layouts.len() >= MAX_CACHED_TEXTS {
                    self.mono.old = std::mem::take(&mut self.mono.layouts);
                }
                self.mono.layouts.insert(key, l.clone());
                l
            }
        };
        if let Some(t) = self.paint_with(color) {
            // SAFETY: the layout and brush live across the call.
            unsafe { t.rt.DrawTextLayout(vec2(Point::new(r.x, r.y)), &layout, &t.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP) };
        }
    }

    /// How wide one character of monospaced text is at `size` (they're all the same).
    pub fn mono_char_width(&mut self, size: f32) -> f32 {
        let Some(format) = self.mono_format(size) else { return size * 0.6 };
        let wide: Vec<u16> = "0000000000".encode_utf16().collect();
        // SAFETY: the buffer and format live across the calls; `m` is a valid out-pointer.
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide, &format, 10_000.0, 1_000.0) else { return size * 0.6 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_err() {
                return size * 0.6;
            }
            m.widthIncludingTrailingWhitespace / 10.0
        }
    }

    fn mono_format(&mut self, size: f32) -> Option<IDWriteTextFormat> {
        let key = (size * 100.0) as u32;
        if let Some(f) = self.mono.formats.get(&key) {
            return Some(f.clone());
        }
        let dwrite = &self.dwrite;
        let family = self.mono.family.get_or_insert_with(|| HSTRING::from(pick_family(dwrite, MONO_FAMILIES))).clone();
        // SAFETY: plain DirectWrite calls; the family string and locale outlive them.
        let format = unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(&family, None::<&IDWriteFontCollection>, DWRITE_FONT_WEIGHT(400), DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, &HSTRING::from("en-us"))
                .ok()?;
            let _ = f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            let _ = f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            f
        };
        self.mono.formats.insert(key, format.clone());
        Some(format)
    }
}

/// The first of `names` that is installed (the last one if none is: DirectWrite then picks a font
/// by itself).
fn pick_family(dwrite: &IDWriteFactory, names: &[&'static str]) -> &'static str {
    let fallback = names.last().copied().unwrap_or("Courier New");
    let mut collection: Option<IDWriteFontCollection> = None;
    // SAFETY: valid out-pointer; the collection is released when dropped.
    if unsafe { dwrite.GetSystemFontCollection(&mut collection, false) }.is_err() {
        return fallback;
    }
    let Some(collection) = collection else { return fallback };
    names.iter().find(|name| family_in(&collection, name)).copied().unwrap_or(fallback)
}

fn color_f(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a }
}

fn vec2(p: Point) -> Vector2 {
    Vector2 { X: p.x, Y: p.y }
}

/// The first font family from FONT_FAMILIES that is installed.
fn pick_font_family(dwrite: &IDWriteFactory) -> &'static str {
    let mut collection: Option<IDWriteFontCollection> = None;
    // SAFETY: valid out-pointer; the collection is released when dropped.
    if unsafe { dwrite.GetSystemFontCollection(&mut collection, false) }.is_err() {
        return FONT_FAMILIES[0];
    }
    let Some(collection) = collection else { return FONT_FAMILIES[1] };
    FONT_FAMILIES.iter().find(|name| family_in(&collection, name)).copied().unwrap_or(FONT_FAMILIES[1])
}

/// Whether the font family `name` is installed (a skin's font).
fn font_installed(dwrite: &IDWriteFactory, name: &str) -> bool {
    let mut collection: Option<IDWriteFontCollection> = None;
    // SAFETY: valid out-pointer; the collection is released when dropped.
    if unsafe { dwrite.GetSystemFontCollection(&mut collection, false) }.is_err() {
        return false;
    }
    collection.is_some_and(|c| family_in(&c, name))
}

fn family_in(collection: &IDWriteFontCollection, name: &str) -> bool {
    let (mut index, mut exists) = (0u32, BOOL(0));
    let wide = HSTRING::from(name);
    // SAFETY: `wide` lives across the call; index/exists are valid out-pointers.
    let ok = unsafe { collection.FindFamilyName(PCWSTR(wide.as_ptr()), &mut index, &mut exists) };
    ok.is_ok() && exists.as_bool()
}
