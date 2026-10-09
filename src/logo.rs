//! The logo at the left of every title bar (the main window's, and those of Settings, the skin
//! customiser and the update prompt): the word "glint" in chrome, with a sparkle as the dot on the
//! i (res/logo). It is a picture, not text: one PNG per display scale, 100 % to 300 %, each in a
//! version for dark skins and one for light skins (the sparkle is blue there, so it doesn't vanish
//! on white), all baked into glint.exe.
//!
//! - **Which picture.** The smallest one made for the window's scale or more (`pick`). At its own
//!   scale it is copied pixel for pixel onto whole pixels, so it is crisp at 100, 125, 150, 175,
//!   200, 250 and 300 %; at any other scale the next bigger one is shrunk smoothly (high-quality
//!   cubic). It is 39 DIPs high in the 40-DIP title bar, centred, a little in from the left (`place`).
//! - **Decoded once.** WIC turns a PNG into pixels the first time it's drawn (`pixels`), and those
//!   are kept for the rest of the run. Each window's renderer makes its Direct2D copy from them
//!   (`Renderer::wordmark`); after a lost graphics device it makes the copy again from the same
//!   pixels, without decoding again. Drawing a frame never decodes anything.
//! - **The box logo.** With the "Logo" setting on the slanted box, the skin's `logo-text` word
//!   (GLINT unless the skin names another) goes in a box in its `logo` colour, in `logo-letters`,
//!   its right edge cut as the skin's shape says (`draw`).

use std::sync::OnceLock;

use windows::Win32::Graphics::Imaging::{
    GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};

use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{size, Theme};
use crate::thumbs::{self, Pixels};
use crate::ui::{Point, Rect};

/// One picture of the wordmark: the display scale it is made for (in percent), its size in pixels,
/// and its PNG for dark skins and for light skins.
struct Wordmark {
    percent: u32,
    width: u32,
    height: u32,
    dark: &'static [u8],
    light: &'static [u8],
}

/// Every picture, smallest first.
const WORDMARKS: [Wordmark; 7] = [
    Wordmark {
        percent: 100,
        width: 80,
        height: 39,
        dark: include_bytes!("../res/logo/wordmark-dark-100.png"),
        light: include_bytes!("../res/logo/wordmark-light-100.png"),
    },
    Wordmark {
        percent: 125,
        width: 100,
        height: 49,
        dark: include_bytes!("../res/logo/wordmark-dark-125.png"),
        light: include_bytes!("../res/logo/wordmark-light-125.png"),
    },
    Wordmark {
        percent: 150,
        width: 121,
        height: 59,
        dark: include_bytes!("../res/logo/wordmark-dark-150.png"),
        light: include_bytes!("../res/logo/wordmark-light-150.png"),
    },
    Wordmark {
        percent: 175,
        width: 139,
        height: 68,
        dark: include_bytes!("../res/logo/wordmark-dark-175.png"),
        light: include_bytes!("../res/logo/wordmark-light-175.png"),
    },
    Wordmark {
        percent: 200,
        width: 160,
        height: 78,
        dark: include_bytes!("../res/logo/wordmark-dark-200.png"),
        light: include_bytes!("../res/logo/wordmark-light-200.png"),
    },
    Wordmark {
        percent: 250,
        width: 201,
        height: 98,
        dark: include_bytes!("../res/logo/wordmark-dark-250.png"),
        light: include_bytes!("../res/logo/wordmark-light-250.png"),
    },
    Wordmark {
        percent: 300,
        width: 240,
        height: 117,
        dark: include_bytes!("../res/logo/wordmark-dark-300.png"),
        light: include_bytes!("../res/logo/wordmark-light-300.png"),
    },
];

/// How many pictures there are of each version (dark, light).
pub const COUNT: usize = WORDMARKS.len();

/// How high the wordmark is in a title bar of the standard height (`size::TITLE_H`), in DIPs; a
/// lower bar gets it smaller in proportion.
const HEIGHT: f32 = 39.0;
/// Room on its left, in DIPs.
const PAD: f32 = 8.0;

/// Where the wordmark goes in a title bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// Which picture (into the list of pictures, smallest first).
    pub index: usize,
    /// Where it goes, in DIPs.
    pub rect: Rect,
    /// `rect` is the picture's own size on whole pixels: copy it pixel for pixel.
    pub exact: bool,
}

/// The picture for a window at `scale` (pixels per DIP: 1.5 at 150 %): the smallest one made for
/// that scale or more, or the biggest there is.
pub fn pick(scale: f32) -> usize {
    WORDMARKS.iter().position(|w| w.percent as f32 / 100.0 >= scale - 0.005).unwrap_or(COUNT - 1)
}

/// Where the wordmark goes in the title bar `bar` at `scale`: the picture, its place (vertically
/// centred, `PAD` in from the left, on whole pixels) and whether it is drawn at its own size.
pub fn place(bar: Rect, scale: f32) -> Placed {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let index = pick(scale);
    let w = &WORDMARKS[index];
    let wanted = (HEIGHT * bar.h / size::TITLE_H).min(bar.h).max(1.0);
    // Its own size at this scale, in DIPs (39.3 at 150 %, 38.9 at 175 %: the pixels were rounded).
    let own = w.height as f32 / scale;
    let exact = (w.percent as f32 / 100.0 - scale).abs() < 0.005 && (own - wanted).abs() <= 0.5;
    let (pw, ph) = if exact { (w.width as f32 / scale, own) } else { (wanted * w.width as f32 / w.height as f32, wanted) };
    let snap = |v: f32| (v * scale).round() / scale;
    let x = snap(bar.x + PAD);
    let y = snap(bar.y + (bar.h - ph) / 2.0);
    Placed { index, rect: Rect::new(x, y, pw, ph), exact }
}

/// The pixels of picture `index` (dark or light version), decoded the first time and then kept.
/// None when WIC couldn't decode it (it isn't tried again: the title bar then shows no logo).
pub fn pixels(dark: bool, index: usize) -> Option<&'static Pixels> {
    static DECODED: [[OnceLock<Option<Pixels>>; COUNT]; 2] = [const { [const { OnceLock::new() }; COUNT] }; 2];
    let w = WORDMARKS.get(index)?;
    DECODED[usize::from(!dark)][index]
        .get_or_init(|| {
            let decoded = decode(if dark { w.dark } else { w.light });
            if decoded.is_none() {
                crate::log::error(&format!("The logo's picture for {} % could not be decoded", w.percent));
            }
            decoded
        })
        .as_ref()
}

/// A PNG baked into glint.exe as premultiplied BGRA pixels, through WIC (needs COM on this thread,
/// which every window's thread has).
fn decode(png: &'static [u8]) -> Option<Pixels> {
    let wic = thumbs::wic_factory()?;
    // SAFETY: every WIC object is an owned wrapper, released when dropped; the PNG is static, so it
    // outlives the stream reading it; the buffer holds exactly `h` rows of `w * 4` bytes.
    unsafe {
        let stream = wic.CreateStream().ok()?;
        stream.InitializeFromMemory(png).ok()?;
        let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = decoder.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            return None;
        }
        let conv = wic.CreateFormatConverter().ok()?;
        conv.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut buf = vec![0u8; w as usize * h as usize * 4];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut buf).ok()?;
        Some(Pixels { width: w, height: h, pixels: buf })
    }
}

/// Draws the skin's logo at the left of the title bar `bar`: the chrome wordmark (dark or light, as
/// the skin is), or the skin's own word in a box. Returns where the logo's room ends (`size::LOGO_W`
/// from the bar's left): the tabs, or the window's title, start after it.
/// How wide the logo is in a title bar (0 when the "Logo" setting is None: the tabs start at the
/// left edge then).
pub fn width(theme: &Theme) -> f32 {
    if theme.logo_style == crate::settings::LogoStyle::None {
        0.0
    } else {
        size::LOGO_W
    }
}

pub fn draw(r: &mut Renderer, theme: &Theme, bar: Rect) -> f32 {
    if theme.logo_style == crate::settings::LogoStyle::None {
        return bar.x;
    }
    if theme.chrome_logo() {
        let p = place(bar, r.scale());
        r.wordmark(theme.dark, p.index, p.rect, p.exact);
    } else {
        // The box's right edge is cut at a slant (straight in a skin without slants).
        let (x, y, h, w) = (bar.x, bar.y, bar.h, size::LOGO_W);
        let lean = theme.logo_lean(h);
        let corner = theme.corner(h);
        // In the skin's `logo` paint: a gradient runs down the whole box, whichever piece of it
        // is being filled.
        let paint = &theme.logo_paint;
        if corner > 0.0 {
            // Rounded skins round the one corner that isn't against the window's edges.
            r.fill_paint_down(Rect::new(x, y, w, h), corner, paint, y, y + h);
            r.fill_paint_down(Rect::new(x, y, w, corner), 0.0, paint, y, y + h);
            r.fill_paint_down(Rect::new(x, y, corner, h), 0.0, paint, y, y + h);
        } else {
            r.fill_polygon_paint(&[Point::new(x, y), Point::new(x + w, y), Point::new(x + w - lean, y + h), Point::new(x, y + h)], paint);
        }
        let style = TextStyle::new(theme.logo_ink).size(size::FONT_LOGO).bold().align(Align::Center);
        r.text(&theme.logo_text, Rect::new(x, y, w - lean * 0.85, h), &style);
    }
    bar.x + size::LOGO_W
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG's size, from its header (the IHDR chunk comes first).
    fn png_size(png: &[u8]) -> (u32, u32) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        let n = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
        (n(16), n(20))
    }

    #[test]
    fn the_pictures_are_the_sizes_the_list_says() {
        let mut last = 0;
        for w in &WORDMARKS {
            assert_eq!(png_size(w.dark), (w.width, w.height), "dark {} %", w.percent);
            assert_eq!(png_size(w.light), (w.width, w.height), "light {} %", w.percent);
            // 39 pixels high at 100 %, in proportion at the others, the shape of the wordmark (660:322).
            let own = w.height as f32 * 100.0 / w.percent as f32;
            assert!((own - HEIGHT).abs() <= 0.5, "{} %: {own} DIPs high", w.percent);
            assert!((w.width as f32 / w.height as f32 - 660.0 / 322.0).abs() < 0.03, "{} %", w.percent);
            assert!(w.percent > last, "smallest first");
            last = w.percent;
        }
    }

    #[test]
    fn each_scale_gets_its_own_picture_or_the_next_bigger_one() {
        assert_eq!(pick(1.0), 0);
        assert_eq!(pick(1.25), 1);
        assert_eq!(pick(1.5), 2);
        assert_eq!(pick(1.75), 3);
        assert_eq!(pick(2.0), 4);
        assert_eq!(pick(2.5), 5);
        assert_eq!(pick(3.0), 6);
        // In between: the next bigger one, shrunk.
        assert_eq!(pick(1.1), 1);
        assert_eq!(pick(1.375), 2);
        assert_eq!(pick(2.25), 5);
        // Smaller than 100 % or bigger than 300 %: the nearest there is.
        assert_eq!(pick(0.75), 0);
        assert_eq!(pick(4.0), 6);
    }

    #[test]
    fn the_wordmark_fits_the_title_bar_and_is_crisp_at_its_own_scales() {
        let bar = Rect::new(0.0, 0.0, 1000.0, size::TITLE_H);
        // 100 %: the 80 x 39 picture, pixel for pixel, 8 DIPs in.
        let p = place(bar, 1.0);
        assert!(p.exact);
        assert_eq!((p.rect.x, p.rect.w, p.rect.h), (8.0, 80.0, 39.0));
        assert!(p.rect.y == 0.0 || p.rect.y == 1.0);
        for percent in [100, 125, 150, 175, 200, 250, 300, 110, 120, 133, 225, 350] {
            let scale = percent as f32 / 100.0;
            let p = place(bar, scale);
            let w = &WORDMARKS[p.index];
            assert_eq!(p.exact, w.percent == percent, "{percent} %");
            if p.exact {
                // Its own pixels, on whole pixels.
                assert!(((p.rect.w * scale) - w.width as f32).abs() < 0.01 && ((p.rect.h * scale) - w.height as f32).abs() < 0.01);
                for v in [p.rect.x, p.rect.y] {
                    assert!(((v * scale) - (v * scale).round()).abs() < 0.01, "{percent} %: {v} isn't on a whole pixel");
                }
            } else {
                assert_eq!(p.rect.h, HEIGHT, "{percent} %");
            }
            // Inside the bar, and the tabs start after it.
            assert!(p.rect.y >= 0.0 && p.rect.bottom() <= bar.h + 0.01, "{percent} %: {:?}", p.rect);
            assert!(p.rect.right() <= size::LOGO_W, "{percent} %: {:?}", p.rect);
            assert!(p.rect.x >= 7.0, "{percent} %");
        }
        // A lower bar gets a smaller wordmark, shrunk.
        let low = place(Rect::new(0.0, 0.0, 500.0, 30.0), 1.0);
        assert!(!low.exact && low.rect.h < 30.0);
    }

    #[test]
    fn every_picture_decodes() {
        let _com = crate::shell::ComGuard::init();
        for w in &WORDMARKS {
            for (png, which) in [(w.dark, "dark"), (w.light, "light")] {
                let p = decode(png).unwrap_or_else(|| panic!("{which} {} %", w.percent));
                assert_eq!((p.width, p.height), (w.width, w.height));
                assert_eq!(p.pixels.len(), (w.width * w.height * 4) as usize);
                // See-through round the letters, solid in them, and premultiplied.
                assert_eq!(p.pixels[3], 0, "{which} {} %: the top left corner", w.percent);
                assert!(p.pixels.chunks_exact(4).any(|px| px[3] == 255));
                assert!(p.pixels.chunks_exact(4).all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]));
            }
        }
    }
}
