//! Animated GIFs for the Space preview: every frame decoded with WIC (on peek.rs's thread, never
//! the UI thread), put together the way GIFs say (each frame is a patch drawn over what was there,
//! then cleared or put back as its "disposal" says), and handed over as whole pictures with how
//! long each stays on screen. The preview (preview.rs) shows them in turn, looping.
//!
//! Memory: all the frames together take at most [`MAX_BYTES`]; a long or big GIF is made smaller
//! until they fit, rather than played in part.

use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory, IWICMetadataQueryReader, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::core::Interface;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToUInt32, PROPVARIANT};

use crate::folder::wide_path;
use crate::thumbs::{self, Picture, Pixels};

/// All the frames of one GIF take at most this much memory (premultiplied BGRA).
pub const MAX_BYTES: u64 = 200 * 1024 * 1024;
/// At most this many frames are kept (a GIF with more plays its first ones, then loops).
pub const MAX_FRAMES: usize = 3000;

/// What happens to a frame's patch before the next frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposal {
    /// It stays (GIF's 0 "unspecified" and 1 "do not dispose").
    Keep,
    /// Its rectangle is cleared to transparent (2 "restore to background"; browsers clear rather
    /// than paint the background colour, and so does this).
    Clear,
    /// The picture goes back to what it was before the frame (3 "restore to previous").
    Previous,
}

/// GIF's disposal number as a [`Disposal`] (unknown numbers keep the frame, as browsers do).
pub fn disposal_of(code: u32) -> Disposal {
    match code {
        2 => Disposal::Clear,
        3 => Disposal::Previous,
        _ => Disposal::Keep,
    }
}

/// How long a frame stays, in milliseconds, from GIF's delay in hundredths of a second. Like
/// browsers, a delay of 0 or 1 (made for "as fast as possible") plays at 100 ms; anything else as
/// written, but never quicker than 20 ms.
pub fn delay_ms(hundredths: u32) -> u32 {
    if hundredths <= 1 {
        100
    } else {
        (hundredths * 10).max(20)
    }
}

/// A frame's patch: where it goes on the GIF's canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Puts frames together on a canvas the size of the GIF.
pub struct Composer {
    w: u32,
    h: u32,
    canvas: Vec<u8>,
    /// The last frame's patch and disposal, carried out before the next frame.
    pending: Option<(Patch, Disposal)>,
    /// The canvas before a "restore to previous" frame.
    saved: Option<Vec<u8>>,
}

impl Composer {
    pub fn new(w: u32, h: u32) -> Composer {
        Composer { w, h, canvas: vec![0; w as usize * h as usize * 4], pending: None, saved: None }
    }

    /// Draws the next frame (`pixels`: premultiplied BGRA of the patch's size) and returns the
    /// whole picture as it is to be shown.
    pub fn next(&mut self, pixels: &[u8], patch: Patch, disposal: Disposal) -> &[u8] {
        match self.pending.take() {
            Some((p, Disposal::Clear)) => self.clear(p),
            Some((_, Disposal::Previous)) => {
                if let Some(s) = self.saved.take() {
                    self.canvas = s;
                }
            }
            _ => {}
        }
        if disposal == Disposal::Previous {
            self.saved = Some(self.canvas.clone());
        }
        self.draw(pixels, patch);
        self.pending = Some((patch, disposal));
        &self.canvas
    }

    /// The part of `patch` that is on the canvas.
    fn clip(&self, p: Patch) -> (usize, usize, usize, usize) {
        let x1 = (p.x.saturating_add(p.w)).min(self.w) as usize;
        let y1 = (p.y.saturating_add(p.h)).min(self.h) as usize;
        (p.x.min(self.w) as usize, p.y.min(self.h) as usize, x1, y1)
    }

    fn clear(&mut self, p: Patch) {
        let (x0, y0, x1, y1) = self.clip(p);
        let stride = self.w as usize * 4;
        for y in y0..y1 {
            self.canvas[y * stride + x0 * 4..y * stride + x1 * 4].fill(0);
        }
    }

    /// Draws the patch over the canvas (premultiplied "over": transparent pixels let what's
    /// underneath show).
    fn draw(&mut self, pixels: &[u8], p: Patch) {
        if pixels.len() < p.w as usize * p.h as usize * 4 {
            return;
        }
        let (x0, y0, x1, y1) = self.clip(p);
        let stride = self.w as usize * 4;
        for y in y0..y1 {
            let src_row = (y - p.y as usize) * p.w as usize * 4;
            for x in x0..x1 {
                let s = src_row + (x - p.x as usize) * 4;
                let d = y * stride + x * 4;
                let a = pixels[s + 3] as u32;
                if a == 255 {
                    self.canvas[d..d + 4].copy_from_slice(&pixels[s..s + 4]);
                } else if a > 0 {
                    for i in 0..4 {
                        let under = self.canvas[d + i] as u32;
                        self.canvas[d + i] = (pixels[s + i] as u32 + under * (255 - a) / 255).min(255) as u8;
                    }
                }
            }
        }
    }
}

/// A GIF's frames, ready to show.
pub struct Frames {
    pub pictures: Vec<Arc<Picture>>,
    /// How long each stays (milliseconds).
    pub delays: Vec<u32>,
    /// The GIF's own size (the pictures may be smaller).
    pub size: (u32, u32),
    /// How many frames the file has (it may be more than were kept).
    pub count: u32,
}

impl Frames {
    /// How long one loop plays, in milliseconds.
    pub fn total_ms(&self) -> u64 {
        self.delays.iter().map(|&d| d as u64).sum()
    }
}

/// The size each frame is kept at: fitting `max_side`, and all `count` frames within MAX_BYTES.
pub fn frame_size(w: u32, h: u32, count: usize, max_side: u32) -> (u32, u32) {
    let per_frame = MAX_BYTES / 4 / count.max(1) as u64;
    thumbs::fit_size(w, h, max_side, per_frame.max(64 * 64))
}

/// Reads a whole number from a WIC metadata reader (None when it isn't there).
fn number(reader: &IWICMetadataQueryReader, name: PCWSTR) -> Option<u32> {
    let mut v = PROPVARIANT::default();
    // SAFETY: `v` is a valid PROPVARIANT, cleared after use; the name is a static string.
    unsafe {
        reader.GetMetadataByName(name, &mut v).ok()?;
        let n = PropVariantToUInt32(&v).ok();
        let _ = PropVariantClear(&mut v);
        n
    }
}

/// Decodes every frame of an animated GIF, each fitting a `max_side` square. None when it isn't a
/// GIF WIC can read, or has just one frame (then it's a still picture).
pub fn decode_gif(wic: &IWICImagingFactory, path: &str, max_side: u32) -> Option<Frames> {
    let wide = wide_path(path);
    // SAFETY: the name buffer outlives the call; WIC objects are owned wrappers; sizes are checked.
    unsafe {
        let decoder = wic.CreateDecoderFromFilename(PCWSTR(wide.as_ptr()), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand).ok()?;
        let count = decoder.GetFrameCount().ok()?;
        if count < 2 {
            return None;
        }
        let first = decoder.GetFrame(0).ok()?;
        let (mut fw, mut fh) = (0u32, 0u32);
        first.GetSize(&mut fw, &mut fh).ok()?;
        let screen = decoder.GetMetadataQueryReader().ok();
        let sw = screen.as_ref().and_then(|r| number(r, w!("/logscrdesc/Width"))).filter(|&v| v > 0).unwrap_or(fw);
        let sh = screen.as_ref().and_then(|r| number(r, w!("/logscrdesc/Height"))).filter(|&v| v > 0).unwrap_or(fh);
        if sw == 0 || sh == 0 || sw > 16384 || sh > 16384 {
            return None;
        }
        let kept = (count as usize).min(MAX_FRAMES);
        let (tw, th) = frame_size(sw, sh, kept, max_side);
        let mut composer = Composer::new(sw, sh);
        let mut out = Frames { pictures: Vec::with_capacity(kept), delays: Vec::with_capacity(kept), size: (sw, sh), count };
        for i in 0..kept as u32 {
            let Ok(frame) = decoder.GetFrame(i) else { break };
            let meta = frame.GetMetadataQueryReader().ok();
            let get = |name: PCWSTR| meta.as_ref().and_then(|m| number(m, name));
            let (mut w, mut h) = (0u32, 0u32);
            if frame.GetSize(&mut w, &mut h).is_err() || w == 0 || h == 0 {
                break;
            }
            let patch = Patch { x: get(w!("/imgdesc/Left")).unwrap_or(0), y: get(w!("/imgdesc/Top")).unwrap_or(0), w, h };
            let disposal = disposal_of(get(w!("/grctlext/Disposal")).unwrap_or(0));
            let delay = delay_ms(get(w!("/grctlext/Delay")).unwrap_or(0));
            let Ok(conv) = wic.CreateFormatConverter() else { break };
            let Ok(source) = frame.cast::<IWICBitmapSource>() else { break };
            if conv.Initialize(&source, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).is_err() {
                break;
            }
            let mut px = vec![0u8; w as usize * h as usize * 4];
            if conv.CopyPixels(std::ptr::null(), w * 4, &mut px).is_err() {
                break;
            }
            let whole = composer.next(&px, patch, disposal);
            let shown = if (tw, th) == (sw, sh) {
                Pixels { width: sw, height: sh, pixels: whole.to_vec() }
            } else {
                let full = Pixels { width: sw, height: sh, pixels: whole.to_vec() };
                match thumbs::shrink(wic, &full, tw.max(th)) {
                    Some(p) => p,
                    None => break,
                }
            };
            let Some(pic) = Picture::new(shown.width, shown.height, shown.pixels, false) else { break };
            out.pictures.push(pic);
            out.delays.push(delay);
        }
        (out.pictures.len() >= 2).then_some(out)
    }
}

/// Which frame shows `elapsed_ms` into the animation (looping), and how many milliseconds until
/// the next one.
pub fn frame_at(delays: &[u32], elapsed_ms: u64) -> (usize, u64) {
    let total: u64 = delays.iter().map(|&d| d as u64).sum();
    if delays.is_empty() || total == 0 {
        return (0, 100);
    }
    let mut t = elapsed_ms % total;
    for (i, &d) in delays.iter().enumerate() {
        if t < d as u64 {
            return (i, d as u64 - t);
        }
        t -= d as u64;
    }
    (0, delays[0] as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_follow_what_browsers_do() {
        assert_eq!(delay_ms(0), 100);
        assert_eq!(delay_ms(1), 100);
        assert_eq!(delay_ms(2), 20);
        assert_eq!(delay_ms(7), 70);
        assert_eq!(delay_ms(150), 1500);
    }

    #[test]
    fn disposal_numbers() {
        assert_eq!(disposal_of(0), Disposal::Keep);
        assert_eq!(disposal_of(1), Disposal::Keep);
        assert_eq!(disposal_of(2), Disposal::Clear);
        assert_eq!(disposal_of(3), Disposal::Previous);
        assert_eq!(disposal_of(7), Disposal::Keep);
    }

    fn solid(w: u32, h: u32, v: u8) -> Vec<u8> {
        vec![v; w as usize * h as usize * 4]
    }

    /// The first byte of pixel (x, y) of a 4-wide canvas.
    fn at(c: &[u8], x: usize, y: usize) -> u8 {
        c[(y * 4 + x) * 4]
    }

    #[test]
    fn frames_are_put_together_with_their_disposal() {
        let mut c = Composer::new(4, 4);
        // A full background that stays.
        let out = c.next(&solid(4, 4, 255), Patch { x: 0, y: 0, w: 4, h: 4 }, Disposal::Keep).to_vec();
        assert_eq!(at(&out, 3, 3), 255);
        // A small patch, cleared after it shows.
        let patch = Patch { x: 1, y: 1, w: 2, h: 2 };
        let out = c.next(&solid(2, 2, 255), patch, Disposal::Clear).to_vec();
        assert_eq!(at(&out, 1, 1), 255);
        // The next frame is a transparent patch elsewhere: the cleared part is transparent now,
        // the rest of the background is still there.
        let out = c.next(&solid(1, 1, 0), Patch { x: 0, y: 0, w: 1, h: 1 }, Disposal::Keep).to_vec();
        assert_eq!((at(&out, 1, 1), at(&out, 2, 2), at(&out, 0, 0), at(&out, 3, 3)), (0, 0, 255, 255));
    }

    #[test]
    fn restore_to_previous_puts_the_picture_back() {
        let mut c = Composer::new(4, 4);
        c.next(&solid(4, 4, 100), Patch { x: 0, y: 0, w: 4, h: 4 }, Disposal::Keep);
        // A "restore to previous" frame paints the corner white...
        let out = c.next(&solid(2, 2, 255), Patch { x: 0, y: 0, w: 2, h: 2 }, Disposal::Previous).to_vec();
        assert_eq!(at(&out, 0, 0), 255);
        // ...and before the next frame the corner is as it was.
        let out = c.next(&solid(1, 1, 0), Patch { x: 3, y: 3, w: 1, h: 1 }, Disposal::Keep).to_vec();
        assert_eq!((at(&out, 0, 0), at(&out, 1, 1)), (100, 100));
    }

    #[test]
    fn patches_past_the_edge_are_cut() {
        let mut c = Composer::new(4, 4);
        let out = c.next(&solid(3, 3, 255), Patch { x: 2, y: 2, w: 3, h: 3 }, Disposal::Keep).to_vec();
        assert_eq!((at(&out, 3, 3), at(&out, 1, 1)), (255, 0));
    }

    #[test]
    fn the_frame_on_screen_loops() {
        let d = [100, 50, 200];
        assert_eq!(frame_at(&d, 0), (0, 100));
        assert_eq!(frame_at(&d, 120), (1, 30));
        assert_eq!(frame_at(&d, 349), (2, 1));
        assert_eq!(frame_at(&d, 350), (0, 100));
        assert_eq!(frame_at(&[], 5), (0, 100));
    }

    #[test]
    fn many_frames_are_made_smaller_to_fit_the_memory() {
        // 500 frames of 1920 x 1080 would be about 4 GB: shrunk to fit 200 MB.
        let (w, h) = frame_size(1920, 1080, 500, 4096);
        assert!(w as u64 * h as u64 * 4 * 500 <= MAX_BYTES + 500 * 4 * 2048);
        assert!(w < 1920 && w > 300);
        // A small GIF stays as it is.
        assert_eq!(frame_size(320, 240, 40, 4096), (320, 240));
    }
}
