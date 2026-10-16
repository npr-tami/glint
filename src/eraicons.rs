//! Icons in the style of older Windows, for a skin with `icons: classic`, `xp` or `aero` (theme.rs
//! `IconSet`): folders, the places (Home, Desktop, Documents, Downloads, Pictures, Music, Videos,
//! the Recycle Bin, This PC), drives and the common kinds of file.
//!
//! Each icon is painted here pixel by pixel at the exact size it is shown (16 pixels in lists, the
//! sidebar, tabs and the address bar; 32 to 48 on Home's tiles), kept, and drawn one to one, so
//! nothing is blurred by scaling:
//!
//! - classic (98, 2000): the limited palette of those years (the 16 colours and the folder's manila
//!   tints), hard one-pixel outlines, halftone shading, no smoothing;
//! - XP: soft ramps lit from the top left, a thin outline in a darker shade of the object's colour,
//!   the folder in perspective with its front flap, a drop shadow at the bigger sizes;
//! - aero (7): light and glossy, the folder seen from the side with papers in it, the libraries'
//!   symbols on their glass shelf.
//!
//! Every drawing is made of shapes on a grid (32 units across; 16 for classic's small icons), with
//! smaller variants where 16 pixels need simpler shapes. Pictures and videos keep their real
//! thumbnails (list.rs); only what would show Windows' icon comes here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::folder::{Entry, EntryKind};
use crate::render::Renderer;
use crate::theme::{Color, IconSet, Paint, Stop};
use crate::thumbs::Picture;
use crate::ui::{Point, Rect};

/// What an icon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Folder,
    /// A folder that is open (the one being looked at, in the address bar).
    FolderOpen,
    /// Glint's Home page.
    Home,
    /// This PC (My Computer).
    Computer,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Bin { full: bool },
    Drive(DriveKind),
    File(FileKind),
}

/// The kinds of drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DriveKind {
    Fixed,
    /// The drive Windows is on (C:).
    System,
    Removable,
    Optical,
    Network,
}

/// The kinds of file that get an icon of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Generic,
    Text,
    Image,
    Zip,
    Exe,
    Audio,
    Video,
    /// A word processor's or office document.
    Document,
    Pdf,
}

/// The kind of a file by its extension (lower case, no dot).
pub fn file_kind(ext: &str) -> FileKind {
    match ext {
        "txt" | "log" | "ini" | "md" | "csv" | "json" | "xml" | "cfg" | "conf" | "nfo" | "yaml" | "yml" | "toml" | "rs" | "cs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "bat" | "cmd" | "ps1" | "reg" => {
            if matches!(ext, "bat" | "cmd") {
                FileKind::Exe
            } else {
                FileKind::Text
            }
        }
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "heic" | "avif" | "svg" | "raw" | "psd" => FileKind::Image,
        "zip" | "7z" | "rar" | "cab" | "gz" | "tar" | "tgz" | "bz2" | "xz" | "iso" => FileKind::Zip,
        "exe" | "com" | "msi" | "scr" | "appx" | "msix" => FileKind::Exe,
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "wma" | "aac" | "opus" | "mid" | "midi" => FileKind::Audio,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "mpg" | "mpeg" | "flv" => FileKind::Video,
        "doc" | "docx" | "rtf" | "odt" | "xls" | "xlsx" | "ppt" | "pptx" | "htm" | "html" | "wri" => FileKind::Document,
        "pdf" => FileKind::Pdf,
        _ => FileKind::Generic,
    }
}

/// The user's own folders (Desktop, Documents...) by path in lower case, looked up once.
fn known_places() -> &'static [(String, Role)] {
    static KNOWN: OnceLock<Vec<(String, Role)>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        crate::shell::known_folders()
            .into_iter()
            .filter_map(|k| {
                let role = match k.name {
                    "Home" => Role::Home,
                    "Desktop" => Role::Desktop,
                    "Documents" => Role::Documents,
                    "Downloads" => Role::Downloads,
                    "Pictures" => Role::Pictures,
                    "Music" => Role::Music,
                    "Videos" => Role::Videos,
                    _ => return None,
                };
                Some((k.path.trim_end_matches('\\').to_lowercase(), role))
            })
            .collect()
    })
}

/// The drive Windows is on ("c:"), in lower case.
fn system_drive() -> &'static str {
    static DRIVE: OnceLock<String> = OnceLock::new();
    DRIVE.get_or_init(|| std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_lowercase())
}

/// The kind of a drive from what Windows calls it ("Local Disk", "Removable Disk"...) and its root.
pub fn drive_kind(kind_name: &str, root: &str) -> DriveKind {
    match kind_name {
        "Removable Disk" => DriveKind::Removable,
        "CD Drive" => DriveKind::Optical,
        "Network Drive" => DriveKind::Network,
        _ if root.trim_end_matches('\\').to_lowercase() == system_drive() => DriveKind::System,
        _ => DriveKind::Fixed,
    }
}

/// The kind of drive `root` ("E:") is, as Windows says it is now (a USB stick, a DVD drive, a
/// network drive...). Asking Windows for the type of a drive is always quick: it never wakes the
/// drive or waits on the network.
fn kind_of_drive(root: &str) -> DriveKind {
    let root_w: Vec<u16> = format!("{}\\", root.trim_end_matches('\\')).encode_utf16().chain(Some(0)).collect();
    // SAFETY: `root_w` is NUL-terminated and outlives the call.
    let t = unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(root_w.as_ptr())) };
    drive_kind(drive_type_name(t), root)
}

/// What Windows calls a drive of type `t` (GetDriveTypeW), as folder.rs names drives.
fn drive_type_name(t: u32) -> &'static str {
    match t {
        2 => "Removable Disk",
        4 => "Network Drive",
        5 => "CD Drive",
        _ => "Local Disk",
    }
}

/// What a place (a folder's path, "" for This PC, Glint's Home, the Recycle Bin) shows.
pub fn role_of_path(path: &str) -> Role {
    if path.is_empty() || path.eq_ignore_ascii_case(crate::details::THIS_PC_ID) {
        return Role::Computer;
    }
    if path.eq_ignore_ascii_case(crate::recent::LOCATION) {
        return Role::Home;
    }
    if path.eq_ignore_ascii_case(crate::recycle::LOCATION) {
        return Role::Bin { full: crate::recycle::seen_full() == Some(true) };
    }
    let trimmed = path.trim_end_matches('\\');
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return Role::Drive(kind_of_drive(trimmed));
    }
    let lower = trimmed.to_lowercase();
    known_places().iter().find(|(p, _)| *p == lower).map_or(Role::Folder, |(_, r)| *r)
}

/// What an entry of folder `dir` shows.
pub fn role_of_entry(dir: &str, e: &Entry) -> Role {
    match e.kind {
        EntryKind::Drive => Role::Drive(drive_kind(&e.type_name, e.target.as_deref().unwrap_or(""))),
        EntryKind::Folder => match role_of_path(&e.path_in(dir)) {
            Role::Drive(_) | Role::Computer => Role::Folder,
            r => r,
        },
        EntryKind::File => Role::File(file_kind(&e.ext)),
    }
}

/// Draws an entry's icon into `rect` when the skin has older Windows' icons; false when it hasn't
/// (Windows' own icon goes there then).
pub fn draw_entry(r: &mut Renderer, set: IconSet, dir: &str, e: &Entry, rect: Rect) -> bool {
    if set == IconSet::Windows {
        return false;
    }
    draw(r, set, role_of_entry(dir, e), rect);
    true
}

/// Draws a place's icon (a folder's path, see `role_of_path`) the same way.
pub fn draw_place(r: &mut Renderer, set: IconSet, path: &str, rect: Rect) -> bool {
    if set == IconSet::Windows {
        return false;
    }
    // The folder a tab is showing is an open one.
    let role = match role_of_path(path) {
        Role::Folder => Role::FolderOpen,
        other => other,
    };
    draw(r, set, role, rect);
    true
}

// ---------------------------------------------------------------- drawing helpers

/// Maps a grid of units (32 for the icons, others for the toolbar's pictures, retrotool.rs) onto
/// a square box.
#[derive(Clone, Copy)]
pub(crate) struct Pen {
    pub x: f32,
    pub y: f32,
    /// DIPs per unit.
    pub u: f32,
    /// One screen pixel, in units (for outlines that stay one pixel thick at every size).
    pub hair: f32,
}

impl Pen {
    /// A pen for a drawing `grid` units across, over the middle of `rect` (on whole pixels).
    pub fn over(r: &Renderer, rect: Rect, grid: f32) -> Pen {
        let side = rect.w.min(rect.h);
        let scale = r.scale().max(0.5);
        let x = r.snap(rect.x + (rect.w - side) / 2.0);
        let y = r.snap(rect.y + (rect.h - side) / 2.0);
        let u = side / grid;
        Pen { x, y, u, hair: 1.0 / scale / u }
    }

    pub fn p(&self, x: f32, y: f32) -> Point {
        Point::new(self.x + x * self.u, self.y + y * self.u)
    }
    pub fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(self.x + x * self.u, self.y + y * self.u, w * self.u, h * self.u)
    }
    pub fn pts(&self, pts: &[(f32, f32)]) -> Vec<Point> {
        pts.iter().map(|&(x, y)| self.p(x, y)).collect()
    }
}

pub(crate) fn c(hex: u32) -> Color {
    Color::hex(hex)
}

pub(crate) fn ca(hex: u32, a: f32) -> Color {
    Color::hex(hex).with_alpha(a)
}

/// A gradient from (position, colour) pairs.
pub(crate) fn g(stops: &[(f32, u32)]) -> Paint {
    let stops: Vec<Stop> = stops.iter().map(|&(at, hex)| Stop { at, color: Color::hex(hex) }).collect();
    Paint::from_stops(&stops)
}

pub(crate) fn fill(r: &mut Renderer, p: &Pen, x: f32, y: f32, w: f32, h: f32, color: Color) {
    r.fill_rect(p.rect(x, y, w, h), color);
}

/// A filled rectangle with corners `radius` units round, in a paint running from (x0, y0) to (x1, y1).
#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_g(r: &mut Renderer, p: &Pen, x: f32, y: f32, w: f32, h: f32, radius: f32, paint: &Paint, from: (f32, f32), to: (f32, f32)) {
    r.fill_rect_toward(p.rect(x, y, w, h), radius * p.u, paint, p.p(from.0, from.1), p.p(to.0, to.1));
}

pub(crate) fn poly(r: &mut Renderer, p: &Pen, pts: &[(f32, f32)], color: Color) {
    r.fill_polygon(&p.pts(pts), color);
}

pub(crate) fn poly_g(r: &mut Renderer, p: &Pen, pts: &[(f32, f32)], paint: &Paint, from: (f32, f32), to: (f32, f32)) {
    r.fill_polygon_toward(&p.pts(pts), paint, p.p(from.0, from.1), p.p(to.0, to.1));
}

/// The outline of a closed shape, one pixel thick.
pub(crate) fn outline(r: &mut Renderer, p: &Pen, pts: &[(f32, f32)], color: Color) {
    r.stroke_polygon(&p.pts(pts), p.hair * p.u, color);
}

/// An open line `w` units thick (at least a pixel).
pub(crate) fn stroke(r: &mut Renderer, p: &Pen, pts: &[(f32, f32)], w: f32, color: Color) {
    r.polyline(&p.pts(pts), (w * p.u).max(p.hair * p.u), color);
}

/// A one-pixel line across at `y` from `x` for `w` units (classic: hard pixels).
pub(crate) fn hline(r: &mut Renderer, p: &Pen, x: f32, y: f32, w: f32, color: Color) {
    let px = p.hair * p.u;
    r.fill_rect(Rect::new(p.x + x * p.u, p.y + y * p.u, w * p.u, px), color);
}

/// A one-pixel line down at `x` from `y` for `h` units.
pub(crate) fn vline(r: &mut Renderer, p: &Pen, x: f32, y: f32, h: f32, color: Color) {
    let px = p.hair * p.u;
    r.fill_rect(Rect::new(p.x + x * p.u, p.y + y * p.u, px, h * p.u), color);
}

/// A one-pixel frame just inside the box.
pub(crate) fn frame(r: &mut Renderer, p: &Pen, x: f32, y: f32, w: f32, h: f32, color: Color) {
    r.stroke_rect(p.rect(x, y, w, h), p.hair * p.u, color);
}

pub(crate) fn oval(r: &mut Renderer, p: &Pen, cx: f32, cy: f32, rx: f32, ry: f32, color: Color) {
    r.fill_ellipse(p.p(cx, cy), rx * p.u, ry * p.u, color);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn oval_g(r: &mut Renderer, p: &Pen, cx: f32, cy: f32, rx: f32, ry: f32, paint: &Paint, from: (f32, f32), to: (f32, f32)) {
    r.fill_ellipse_toward(p.p(cx, cy), rx * p.u, ry * p.u, paint, p.p(from.0, from.1), p.p(to.0, to.1));
}

pub(crate) fn oval_line(r: &mut Renderer, p: &Pen, cx: f32, cy: f32, rx: f32, ry: f32, color: Color) {
    r.stroke_ellipse(p.p(cx, cy), rx * p.u, ry * p.u, p.hair * p.u, color);
}


// ---------------------------------------------------------------- drawing

/// Draws `role` in `set`'s style into `rect` (a square: 16, 32 or 48 DIPs, or anything between).
/// Nothing for `IconSet::Windows` (that's Windows' own icons, icons.rs).
pub fn draw(r: &mut Renderer, set: IconSet, role: Role, rect: Rect) {
    if set == IconSet::Windows || rect.w <= 0.0 || rect.h <= 0.0 {
        return;
    }
    // The icon is made at the box's size in screen pixels and put on whole pixels, one to one.
    let scale = r.scale().max(0.5);
    let n = (rect.w.min(rect.h) * scale).round().clamp(1.0, 256.0) as u32;
    let side = n as f32 / scale;
    let at = Rect::new(r.snap(rect.x + (rect.w - side) / 2.0), r.snap(rect.y + (rect.h - side) / 2.0), side, side);
    // A picture whose pixels are gone (a new graphics device, another window) is made again, once.
    for fresh in [false, true] {
        let Some(pic) = made(r, set, role, n, fresh) else { return };
        if r.picture(&pic, at, None, 1.0, false) || !pic.is_lost() {
            return;
        }
    }
}

thread_local! {
    /// The icons painted so far, by window (renderer), set, role and size in pixels. A few hundred
    /// at most: each window shows a handful of kinds at two or three sizes.
    static MADE: RefCell<HashMap<(usize, u8, Role, u32), Arc<Picture>>> = RefCell::new(HashMap::new());
}

fn made(r: &Renderer, set: IconSet, role: Role, n: u32, fresh: bool) -> Option<Arc<Picture>> {
    let key = (r as *const Renderer as usize, set as u8, role, n);
    MADE.with(|m| {
        let mut m = m.borrow_mut();
        if let Some(pic) = m.get(&key).filter(|_| !fresh) {
            return Some(pic.clone());
        }
        if m.len() > 2000 {
            m.clear();
        }
        let pic = Picture::new(n, n, pixels(set, role, n), true)?;
        m.insert(key, pic.clone());
        Some(pic)
    })
}

/// `role` in `set`'s style, `n` pixels square: premultiplied BGRA rows, top-down.
pub(crate) fn pixels(set: IconSet, role: Role, n: u32) -> Vec<u8> {
    let n = n.clamp(1, 256) as usize;
    let mut c = match set {
        // Classic's small icons are drawn pixel for pixel on 16 units, the big ones on 32.
        IconSet::Classic => art::Canvas::new(n, if n < 32 { 16.0 } else { 32.0 }, false),
        _ => art::Canvas::new(n, 32.0, true),
    };
    match set {
        IconSet::Classic => classic::paint(&mut c, role),
        IconSet::Xp => xp::paint(&mut c, role),
        IconSet::Aero => aero::paint(&mut c, role),
        IconSet::Windows => {}
    }
    c.bgra()
}

// ---------------------------------------------------------------- the painter

/// A small painter: shapes (polygons, ovals, rounded boxes, lines) filled with flat colours, ramps
/// or classic's halftones, into a square of pixels; one-pixel outlines inside a shape's edge; a
/// soft shadow under what is there.
mod art {
    /// Red, green, blue and alpha, 0 to 1 (not premultiplied).
    pub type Rgba = [f32; 4];

    pub fn rgb(hex: u32) -> Rgba {
        [((hex >> 16) & 255) as f32 / 255.0, ((hex >> 8) & 255) as f32 / 255.0, (hex & 255) as f32 / 255.0, 1.0]
    }

    fn rgb_a(hex: u32, a: f32) -> Rgba {
        let mut c = rgb(hex);
        c[3] = a;
        c
    }

    /// What a shape is filled with.
    pub enum Fill {
        Flat(Rgba),
        /// A ramp along the line `from`..`to`: (where 0 to 1, colour) stops.
        Linear { from: (f32, f32), to: (f32, f32), stops: Vec<(f32, Rgba)> },
        /// A ramp out from `at` to the ellipse `rx`, `ry` around it.
        Radial { at: (f32, f32), rx: f32, ry: f32, stops: Vec<(f32, Rgba)> },
        /// Two colours alternating pixel by pixel (classic's 50% grey).
        Checker(Rgba, Rgba),
        /// `color` sprinkled in an ordered pattern, from none at `from` to `most` of the pixels at
        /// `to` (classic's shading).
        Dither { color: Rgba, from: (f32, f32), to: (f32, f32), most: f32 },
    }

    pub fn flat(hex: u32) -> Fill {
        Fill::Flat(rgb(hex))
    }

    pub fn flat_a(hex: u32, a: f32) -> Fill {
        Fill::Flat(rgb_a(hex, a))
    }

    pub fn lin(from: (f32, f32), to: (f32, f32), stops: &[(f32, u32)]) -> Fill {
        Fill::Linear { from, to, stops: stops.iter().map(|&(t, h)| (t, rgb(h))).collect() }
    }

    pub fn lin_a(from: (f32, f32), to: (f32, f32), stops: &[(f32, u32, f32)]) -> Fill {
        Fill::Linear { from, to, stops: stops.iter().map(|&(t, h, a)| (t, rgb_a(h, a))).collect() }
    }

    pub fn rad(at: (f32, f32), rx: f32, ry: f32, stops: &[(f32, u32)]) -> Fill {
        Fill::Radial { at, rx, ry, stops: stops.iter().map(|&(t, h)| (t, rgb(h))).collect() }
    }

    pub fn rad_a(at: (f32, f32), rx: f32, ry: f32, stops: &[(f32, u32, f32)]) -> Fill {
        Fill::Radial { at, rx, ry, stops: stops.iter().map(|&(t, h, a)| (t, rgb_a(h, a))).collect() }
    }

    pub fn checker(a: u32, b: u32) -> Fill {
        Fill::Checker(rgb(a), rgb(b))
    }

    pub fn dither(hex: u32, from: (f32, f32), to: (f32, f32), most: f32) -> Fill {
        Fill::Dither { color: rgb(hex), from, to, most }
    }

    /// The order pixels are sprinkled in (a 4 by 4 Bayer matrix).
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

    /// How far (x, y) is along `from`..`to`, 0 to 1.
    fn along(from: (f32, f32), to: (f32, f32), x: f32, y: f32) -> f32 {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let len = dx * dx + dy * dy;
        if len <= 0.0 {
            return 0.0;
        }
        (((x - from.0) * dx + (y - from.1) * dy) / len).clamp(0.0, 1.0)
    }

    fn ramp(stops: &[(f32, Rgba)], t: f32) -> Rgba {
        let Some(&(first_at, first)) = stops.first() else { return [0.0; 4] };
        if t <= first_at {
            return first;
        }
        for w in stops.windows(2) {
            let ((a_at, a), (b_at, b)) = (w[0], w[1]);
            if t <= b_at {
                let f = if b_at > a_at { (t - a_at) / (b_at - a_at) } else { 1.0 };
                return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, a[3] + (b[3] - a[3]) * f];
            }
        }
        stops[stops.len() - 1].1
    }

    impl Fill {
        /// The colour at (x, y) units, pixel (px, py).
        fn at(&self, x: f32, y: f32, px: usize, py: usize) -> Rgba {
            match self {
                Fill::Flat(c) => *c,
                Fill::Linear { from, to, stops } => ramp(stops, along(*from, *to, x, y)),
                Fill::Radial { at, rx, ry, stops } => ramp(stops, (((x - at.0) / rx).powi(2) + ((y - at.1) / ry).powi(2)).sqrt()),
                Fill::Checker(a, b) => {
                    if (px + py) % 2 == 0 {
                        *a
                    } else {
                        *b
                    }
                }
                Fill::Dither { color, from, to, most } => {
                    let level = (BAYER[py % 4][px % 4] as f32 + 0.5) / 16.0;
                    if along(*from, *to, x, y) * most > level {
                        *color
                    } else {
                        [0.0; 4]
                    }
                }
            }
        }
    }

    /// A shape, in units.
    pub enum Shape {
        /// Corners in order (filled by the nonzero rule).
        Poly(Vec<(f32, f32)>),
        /// An ellipse, turned `turn` degrees clockwise.
        Oval { cx: f32, cy: f32, rx: f32, ry: f32, turn: f32 },
        /// A box with round corners.
        Round { x: f32, y: f32, w: f32, h: f32, r: f32 },
        /// A line through points, `.1` units wide, with round ends.
        Line(Vec<(f32, f32)>, f32),
        /// Where both are.
        Both(Box<Shape>, Box<Shape>),
    }

    pub fn poly(pts: &[(f32, f32)]) -> Shape {
        Shape::Poly(pts.to_vec())
    }

    pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Shape {
        Shape::Poly(vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)])
    }

    pub fn round(x: f32, y: f32, w: f32, h: f32, r: f32) -> Shape {
        Shape::Round { x, y, w, h, r: r.min(w / 2.0).min(h / 2.0).max(0.0) }
    }

    pub fn oval(cx: f32, cy: f32, rx: f32, ry: f32) -> Shape {
        Shape::Oval { cx, cy, rx, ry, turn: 0.0 }
    }

    pub fn oval_turned(cx: f32, cy: f32, rx: f32, ry: f32, turn: f32) -> Shape {
        Shape::Oval { cx, cy, rx, ry, turn }
    }

    pub fn line(pts: &[(f32, f32)], w: f32) -> Shape {
        Shape::Line(pts.to_vec(), w)
    }

    pub fn both(a: Shape, b: Shape) -> Shape {
        Shape::Both(Box::new(a), Box::new(b))
    }

    /// Points turned `deg` degrees clockwise about `about`.
    pub fn turn(pts: &[(f32, f32)], about: (f32, f32), deg: f32) -> Vec<(f32, f32)> {
        let (s, c) = deg.to_radians().sin_cos();
        pts.iter()
            .map(|&(x, y)| {
                let (dx, dy) = (x - about.0, y - about.1);
                (about.0 + dx * c - dy * s, about.1 + dx * s + dy * c)
            })
            .collect()
    }

    /// Points moved by (dx, dy).
    pub fn moved(pts: &[(f32, f32)], dx: f32, dy: f32) -> Vec<(f32, f32)> {
        pts.iter().map(|&(x, y)| (x + dx, y + dy)).collect()
    }

    /// Points along an ellipse's edge from angle `from` to `to` (degrees, clockwise from the right).
    pub fn arc(cx: f32, cy: f32, rx: f32, ry: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
        let steps = (((to - from).abs() / 8.0).ceil() as usize).max(2);
        (0..=steps)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
                (cx + rx * a.cos(), cy + ry * a.sin())
            })
            .collect()
    }

    fn bounds_of(pts: &[(f32, f32)], grow: f32) -> (f32, f32, f32, f32) {
        let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        }
        (b.0 - grow, b.1 - grow, b.2 + grow, b.3 + grow)
    }

    /// The squared distance from (x, y) to the segment a..b.
    fn to_segment(a: (f32, f32), b: (f32, f32), x: f32, y: f32) -> f32 {
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let (wx, wy) = (x - a.0, y - a.1);
        let len = ex * ex + ey * ey;
        let t = if len > 0.0 { ((wx * ex + wy * ey) / len).clamp(0.0, 1.0) } else { 0.0 };
        let (qx, qy) = (wx - ex * t, wy - ey * t);
        qx * qx + qy * qy
    }

    fn winding(pts: &[(f32, f32)], x: f32, y: f32) -> i32 {
        let mut wn = 0;
        for i in 0..pts.len() {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            let cross = (b.0 - a.0) * (y - a.1) - (x - a.0) * (b.1 - a.1);
            if a.1 <= y {
                if b.1 > y && cross > 0.0 {
                    wn += 1;
                }
            } else if b.1 <= y && cross < 0.0 {
                wn -= 1;
            }
        }
        wn
    }

    impl Shape {
        /// The box it stays inside: left, top, right, bottom.
        fn bounds(&self) -> (f32, f32, f32, f32) {
            match self {
                Shape::Poly(p) => bounds_of(p, 0.0),
                Shape::Oval { cx, cy, rx, ry, turn } => {
                    let (a, b) = if *turn == 0.0 { (*rx, *ry) } else { (rx.max(*ry), rx.max(*ry)) };
                    (cx - a, cy - b, cx + a, cy + b)
                }
                Shape::Round { x, y, w, h, .. } => (*x, *y, x + w, y + h),
                Shape::Line(p, w) => bounds_of(p, w / 2.0),
                Shape::Both(a, b) => {
                    let (a, b) = (a.bounds(), b.bounds());
                    (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3))
                }
            }
        }

        /// How far (x, y) is from the edge, in units: below zero inside.
        fn sd(&self, x: f32, y: f32) -> f32 {
            match self {
                Shape::Poly(p) => {
                    let mut d = f32::MAX;
                    for i in 0..p.len() {
                        d = d.min(to_segment(p[i], p[(i + 1) % p.len()], x, y));
                    }
                    let d = d.sqrt();
                    if winding(p, x, y) != 0 {
                        -d
                    } else {
                        d
                    }
                }
                Shape::Oval { cx, cy, rx, ry, turn } => {
                    let (s, c) = turn.to_radians().sin_cos();
                    let (dx, dy) = (x - cx, y - cy);
                    let (u, v) = (dx * c + dy * s, -dx * s + dy * c);
                    let f = ((u / rx).powi(2) + (v / ry).powi(2)).sqrt();
                    let g = ((u / (rx * rx)).powi(2) + (v / (ry * ry)).powi(2)).sqrt();
                    if g < 1e-6 {
                        return -rx.min(*ry);
                    }
                    (f * (f - 1.0) / g).max(-rx.min(*ry))
                }
                Shape::Round { x: bx, y: by, w, h, r } => {
                    let (hw, hh) = (w / 2.0, h / 2.0);
                    let qx = (x - bx - hw).abs() - (hw - r);
                    let qy = (y - by - hh).abs() - (hh - r);
                    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
                }
                Shape::Line(p, w) => {
                    let mut d = f32::MAX;
                    if p.len() == 1 {
                        d = to_segment(p[0], p[0], x, y);
                    }
                    for s in p.windows(2) {
                        d = d.min(to_segment(s[0], s[1], x, y));
                    }
                    d.sqrt() - w / 2.0
                }
                Shape::Both(a, b) => a.sd(x, y).max(b.sd(x, y)),
            }
        }

        fn inside(&self, x: f32, y: f32) -> bool {
            match self {
                Shape::Poly(p) => winding(p, x, y) != 0,
                Shape::Both(a, b) => a.inside(x, y) && b.inside(x, y),
                _ => self.sd(x, y) < 0.0,
            }
        }
    }

    /// A square of pixels being painted.
    pub struct Canvas {
        /// Pixels across (and down).
        pub n: usize,
        /// Units across.
        pub grid: f32,
        /// Pixels per unit.
        pub k: f32,
        /// Smooth edges (XP, 7); classic keeps every pixel hard.
        aa: bool,
        /// Premultiplied red, green, blue, alpha.
        px: Vec<[f32; 4]>,
    }

    impl Canvas {
        pub fn new(n: usize, grid: f32, aa: bool) -> Canvas {
            Canvas { n, grid, k: n as f32 / grid, aa, px: vec![[0.0; 4]; n * n] }
        }

        /// One pixel, in units.
        pub fn hair(&self) -> f32 {
            1.0 / self.k
        }

        /// 24 pixels or more: room for the finer details (XP, 7).
        pub fn big(&self) -> bool {
            self.n >= 24
        }

        pub fn fill(&mut self, s: &Shape, f: &Fill) {
            self.cover(s, None, f);
        }

        /// The band `w` units wide just inside the shape's edge.
        pub fn edge(&mut self, s: &Shape, w: f32, f: &Fill) {
            self.cover(s, Some(w), f);
        }

        /// The shape's outline: its outermost pixel all round.
        pub fn rim(&mut self, s: &Shape, f: &Fill) {
            let w = self.hair();
            self.cover(s, Some(w), f);
        }

        /// Fills the shape and outlines it.
        pub fn paint(&mut self, s: &Shape, f: &Fill, rim: &Fill) {
            self.fill(s, f);
            self.rim(s, rim);
        }

        fn cover(&mut self, s: &Shape, band: Option<f32>, f: &Fill) {
            let (x0, y0, x1, y1) = s.bounds();
            let (n, k) = (self.n as i32, self.k);
            let lo = |v: f32| ((v * k).floor() as i32 - 1).clamp(0, n);
            let hi = |v: f32| ((v * k).ceil() as i32 + 1).clamp(0, n);
            for py in lo(y0)..hi(y1) {
                for px in lo(x0)..hi(x1) {
                    let cover = self.coverage(s, band, px, py);
                    if cover <= 0.0 {
                        continue;
                    }
                    let c = f.at((px as f32 + 0.5) / k, (py as f32 + 0.5) / k, px as usize, py as usize);
                    let a = (c[3] * cover).clamp(0.0, 1.0);
                    if a <= 0.0 {
                        continue;
                    }
                    let d = &mut self.px[py as usize * self.n + px as usize];
                    let keep = 1.0 - a;
                    for i in 0..3 {
                        d[i] = c[i] * a + d[i] * keep;
                    }
                    d[3] = a + d[3] * keep;
                }
            }
        }

        /// How much of pixel (px, py) the shape (or the band inside its edge) covers.
        fn coverage(&self, s: &Shape, band: Option<f32>, px: i32, py: i32) -> f32 {
            let k = self.k;
            let hit = |x: f32, y: f32| match band {
                None => s.inside(x, y),
                Some(w) => {
                    let d = s.sd(x, y);
                    d < 0.0 && d > -w
                }
            };
            let (cx, cy) = ((px as f32 + 0.5) / k, (py as f32 + 0.5) / k);
            if !self.aa {
                return if hit(cx, cy) { 1.0 } else { 0.0 };
            }
            // Well inside or well outside: no need to look closer.
            const NEAR: f32 = 1.5;
            let d = s.sd(cx, cy) * k;
            match band {
                None if d <= -NEAR => return 1.0,
                None if d >= NEAR => return 0.0,
                Some(w) if d >= NEAR || d <= -w * k - NEAR => return 0.0,
                Some(w) if d <= -NEAR && d >= -w * k + NEAR => return 1.0,
                _ => {}
            }
            let mut hits = 0;
            for j in 0..4 {
                for i in 0..4 {
                    if hit((px as f32 + (i as f32 + 0.5) / 4.0) / k, (py as f32 + (j as f32 + 0.5) / 4.0) / k) {
                        hits += 1;
                    }
                }
            }
            hits as f32 / 16.0
        }

        /// Puts a soft shadow of everything painted so far under it: moved (dx, dy) units, blurred
        /// over `blur` units, `alpha` dark.
        pub fn shadow(&mut self, dx: f32, dy: f32, blur: f32, alpha: f32) {
            let n = self.n as i32;
            let (ox, oy) = ((dx * self.k).round() as i32, (dy * self.k).round() as i32);
            let mut a = vec![0.0f32; self.n * self.n];
            for y in 0..n {
                for x in 0..n {
                    let (sx, sy) = (x - ox, y - oy);
                    if sx >= 0 && sy >= 0 && sx < n && sy < n {
                        a[(y * n + x) as usize] = self.px[(sy * n + sx) as usize][3];
                    }
                }
            }
            let r = (blur * self.k).round() as usize;
            if r > 0 {
                blur_box(&mut a, self.n, r);
                blur_box(&mut a, self.n, r);
            }
            for (d, s) in self.px.iter_mut().zip(a) {
                d[3] += s * alpha * (1.0 - d[3]);
            }
        }

        /// Premultiplied BGRA rows, top-down.
        pub fn bgra(&self) -> Vec<u8> {
            let mut out = Vec::with_capacity(self.n * self.n * 4);
            for p in &self.px {
                let a = p[3].clamp(0.0, 1.0);
                let q = |v: f32| (v.clamp(0.0, a) * 255.0 + 0.5) as u8;
                out.extend_from_slice(&[q(p[2]), q(p[1]), q(p[0]), q(a)]);
            }
            out
        }
    }

    /// Averages each value with its neighbours `r` either side, across then down.
    fn blur_box(a: &mut [f32], n: usize, r: usize) {
        let w = (2 * r + 1) as f32;
        let mut tmp = vec![0.0f32; n * n];
        for y in 0..n {
            for x in 0..n {
                let s: f32 = (x.saturating_sub(r)..=(x + r).min(n - 1)).map(|i| a[y * n + i]).sum();
                tmp[y * n + x] = s / w;
            }
        }
        for y in 0..n {
            for x in 0..n {
                let s: f32 = (y.saturating_sub(r)..=(y + r).min(n - 1)).map(|j| tmp[j * n + x]).sum();
                a[y * n + x] = s / w;
            }
        }
    }
}

// ---------------------------------------------------------------- classic (98 and 2000)

/// Windows 98 and 2000's icons, pixel for pixel: the 16 colours of those years and the manila
/// folder's tints, hard one-pixel outlines (grey or the shape's dark shade at the top and left,
/// black at the bottom and right), shading by halftone, nothing smoothed. Drawn on 16 units for
/// the small size and on 32 for the big one, so each unit is a whole pixel.
mod classic {
    use super::art::*;
    use super::{DriveKind, FileKind, Role};

    const BLACK: u32 = 0x000000;
    const WHITE: u32 = 0xffffff;
    const SILVER: u32 = 0xc0c0c0;
    const GREY: u32 = 0x808080;
    const NAVY: u32 = 0x000080;
    const BLUE: u32 = 0x0000ff;
    const TEAL: u32 = 0x008080;
    const CYAN: u32 = 0x00ffff;
    const GREEN: u32 = 0x008000;
    const LIME: u32 = 0x00ff00;
    const OLIVE: u32 = 0x808000;
    const YELLOW: u32 = 0xffff00;
    const MAROON: u32 = 0x800000;
    const RED: u32 = 0xff0000;
    // The folder's tints (2000's own palette).
    const MANILA: u32 = 0xffff99;
    const CREAM: u32 = 0xffffcc;
    const KHAKI: u32 = 0xcccc66;
    const PEACH: u32 = 0xffcc99;
    const GOLD: u32 = 0x999900;
    const SKY: u32 = 0x3399ff;

    /// A block of one colour, in units.
    fn k(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, hex: u32) {
        if w > 0.0 && h > 0.0 {
            c.fill(&rect(x, y, w, h), &flat(hex));
        }
    }

    fn pts(c: &mut Canvas, list: &[(f32, f32)], hex: u32) {
        c.fill(&poly(list), &flat(hex));
    }

    /// The 32-unit drawing (32 pixels and up).
    fn big(c: &Canvas) -> bool {
        c.grid > 16.0
    }

    pub(super) fn paint(c: &mut Canvas, role: Role) {
        match role {
            Role::Folder => folder(c),
            Role::FolderOpen => {
                open_back(c);
                open_front(c);
            }
            Role::Documents => {
                open_back(c);
                sheet(c);
                open_front(c);
            }
            Role::Pictures => {
                open_back(c);
                photo(c);
                open_front(c);
            }
            Role::Music => {
                folder(c);
                notes(c);
            }
            Role::Videos => {
                folder(c);
                film(c, if big(c) { (12.0, 15.0, 19.0, 11.0) } else { (6.0, 7.0, 10.0, 6.0) });
            }
            Role::Downloads => {
                folder(c);
                down_arrow(c);
            }
            Role::Desktop => desktop(c),
            Role::Home => house(c),
            Role::Computer => computer(c),
            Role::Bin { full } => bin(c, full),
            Role::Drive(kind) => drive(c, kind),
            Role::File(kind) => file(c, kind),
        }
    }

    // ------------------------------------------------ folders

    /// A folder's tab: `rows` rows, each a pixel wider on both sides than the one above, a light
    /// line inside the left edge; `inner` and `side` are the right edge's two pixels; `cap` the
    /// pixel right of the top row.
    #[allow(clippy::too_many_arguments)]
    fn tab(c: &mut Canvas, x: f32, y: f32, w: f32, rows: usize, light: u32, inner: u32, side: u32, cap: Option<u32>) {
        for i in 0..rows {
            let (f, row) = (i as f32, y + i as f32);
            if i == 0 {
                k(c, x, row, w, 1.0, GOLD);
                if let Some(cap) = cap {
                    k(c, x + w, row, 1.0, 1.0, cap);
                }
                continue;
            }
            k(c, x - f, row, 1.0, 1.0, GOLD);
            k(c, x - f + 1.0, row, 1.0, 1.0, light);
            k(c, x - f + 2.0, row, w + 2.0 * f - 3.0, 1.0, MANILA);
            k(c, x + w + f - 1.0, row, 1.0, 1.0, inner);
            k(c, x + w + f, row, 1.0, 1.0, side);
        }
    }

    /// The closed folder's front: gold top and left edges, a light line inside them, manila
    /// shaded with peach toward the bottom right, khaki and black down the right and along the
    /// bottom. `cut`: the top right corner is cut by a pixel (the big one).
    #[allow(clippy::too_many_arguments)]
    fn front(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32, tab_w: f32, cut: bool, light: u32) {
        let cv = if cut { 1.0 } else { 0.0 };
        let (r, b) = (x + w - 1.0, y + h - 1.0);
        k(c, x, y, w - 1.0 - cv, 1.0, GOLD);
        k(c, x + 1.0, y, tab_w, 1.0, KHAKI);
        k(c, x, y + 1.0, 1.0, h - 2.0, GOLD);
        k(c, x + 1.0, y + 1.0, w - 3.0, h - 3.0, MANILA);
        c.fill(&rect(x + 2.0, y + 2.0, w - 4.0, h - 4.0), &dither(PEACH, (x + w * 0.3, y + h * 0.15), (r, b), 0.75));
        k(c, x + 1.0, y + 1.0, w - 2.0 - cv, 1.0, light);
        k(c, x + 1.0, y + 1.0, 1.0, h - 3.0, light);
        k(c, r - 1.0, y + 1.0 + cv, 1.0, h - 3.0 - cv, KHAKI);
        k(c, x + 1.0, b - 1.0, w - 2.0, 1.0, KHAKI);
        k(c, r, y + 1.0 + cv, 1.0, h - 2.0 - cv, BLACK);
        k(c, x + 1.0, b, w - 1.0 - cv, 1.0, BLACK);
        if cut {
            k(c, r - 1.0, y + 1.0, 1.0, 1.0, BLACK);
        }
    }

    fn folder(c: &mut Canvas) {
        if big(c) {
            tab(c, 4.0, 1.0, 9.0, 4, WHITE, GOLD, BLACK, Some(BLACK));
            front(c, 0.0, 5.0, 32.0, 23.0, 15.0, true, WHITE);
        } else {
            tab(c, 2.0, 1.0, 5.0, 2, CREAM, GOLD, BLACK, Some(BLACK));
            front(c, 0.0, 3.0, 15.0, 11.0, 7.0, false, CREAM);
        }
    }

    /// The open folder's back panel, standing behind its front.
    fn open_back(c: &mut Canvas) {
        if big(c) {
            tab(c, 7.0, 2.0, 9.0, 5, WHITE, KHAKI, GOLD, None);
            k(c, 3.0, 7.0, 1.0, 21.0, GOLD);
            k(c, 4.0, 7.0, 1.0, 5.0, WHITE);
            k(c, 5.0, 7.0, 25.0, 21.0, MANILA);
            c.fill(&rect(5.0, 8.0, 25.0, 4.0), &dither(PEACH, (10.0, 6.0), (30.0, 12.0), 0.45));
            k(c, 19.0, 6.0, 11.0, 1.0, GOLD);
            k(c, 19.0, 7.0, 11.0, 1.0, WHITE);
            k(c, 30.0, 7.0, 1.0, 1.0, BLACK);
            k(c, 30.0, 8.0, 1.0, 20.0, GOLD);
            k(c, 31.0, 8.0, 1.0, 20.0, BLACK);
        } else {
            tab(c, 3.0, 1.0, 5.0, 3, WHITE, WHITE, GOLD, None);
            k(c, 1.0, 4.0, 1.0, 8.0, GOLD);
            k(c, 2.0, 4.0, 1.0, 2.0, WHITE);
            k(c, 3.0, 4.0, 11.0, 8.0, MANILA);
            k(c, 9.0, 3.0, 5.0, 1.0, GOLD);
            k(c, 10.0, 4.0, 4.0, 1.0, WHITE);
            k(c, 14.0, 4.0, 1.0, 9.0, KHAKI);
            k(c, 15.0, 4.0, 1.0, 9.0, BLACK);
        }
    }

    /// The open folder's front, leaning toward you: each row a little further right.
    fn open_front(c: &mut Canvas) {
        let (x0, top, rows, w, every) = if big(c) { (0.0, 11.0, 17, 27.0, 5) } else { (0.0, 6.0, 7, 12.0, 3) };
        let last = rows - 1;
        for j in 0..rows {
            let l = x0 + ((j + 2) / every) as f32;
            let y = top + j as f32;
            if j == 0 {
                k(c, l + 1.0, y, w - 1.0, 1.0, GOLD);
            } else if j == last {
                k(c, l, y, w, 1.0, GOLD);
                k(c, l + w, y, 1.0, 1.0, BLACK);
                k(c, l + 1.0, y + 1.0, w, 1.0, BLACK);
            } else {
                k(c, l, y, 1.0, 1.0, GOLD);
                k(c, l + 1.0, y, w - 2.0, 1.0, if j == 1 { WHITE } else { MANILA });
                if j > 1 {
                    c.fill(&rect(l + 2.0, y, w - 4.0, 1.0), &dither(PEACH, (l + w * 0.25, top), (l + w, top + rows as f32), 0.8));
                }
                k(c, l + w - 1.0, y, 1.0, 1.0, KHAKI);
                k(c, l + w, y, 1.0, 1.0, BLACK);
            }
        }
    }

    /// A white sheet standing in the open folder (My Documents).
    fn sheet(c: &mut Canvas) {
        let (x, y, w, h, step) = if big(c) { (9.0, 1.0, 14.0, 12.0, 2.0) } else { (4.0, 0.0, 7.0, 7.0, 2.0) };
        k(c, x, y, w, h, WHITE);
        k(c, x, y, w, 1.0, GREY);
        k(c, x, y, 1.0, h, GREY);
        k(c, x + w, y, 1.0, h, BLACK);
        let mut ly = y + 2.0;
        while ly < y + h - 1.0 {
            k(c, x + 2.0, ly, w - 4.0, 1.0, NAVY);
            ly += step;
        }
    }

    /// A small landscape in a white frame, standing in the open folder (My Pictures).
    fn photo(c: &mut Canvas) {
        let (x, y, w, h) = if big(c) { (7.0, 2.0, 18.0, 12.0) } else { (4.0, 0.0, 9.0, 7.0) };
        k(c, x, y, w, h, WHITE);
        k(c, x + w, y, 1.0, h, BLACK);
        k(c, x + 1.0, y + 1.0, w - 2.0, h - 2.0, SKY);
        let hill = if big(c) {
            vec![(x + 1.0, y + h - 1.0), (x + 1.0, y + 8.0), (x + 7.0, y + 4.0), (x + 13.0, y + 8.0), (x + w - 1.0, y + 6.0), (x + w - 1.0, y + h - 1.0)]
        } else {
            vec![(x + 1.0, y + h - 1.0), (x + 1.0, y + 4.0), (x + 4.0, y + 2.0), (x + w - 1.0, y + 5.0), (x + w - 1.0, y + h - 1.0)]
        };
        pts(c, &hill, GREEN);
        if big(c) {
            k(c, x + 13.0, y + 2.0, 3.0, 3.0, YELLOW);
        } else {
            k(c, x + 6.0, y + 1.0, 2.0, 1.0, YELLOW);
        }
    }

    /// Two beamed notes on the folder's front (My Music).
    fn notes(c: &mut Canvas) {
        if big(c) {
            for x in [15.0, 23.0] {
                k(c, x, 22.0, 4.0, 3.0, NAVY);
                k(c, x - 1.0, 23.0, 1.0, 1.0, NAVY);
                k(c, x + 1.0, 21.0, 2.0, 1.0, NAVY);
                k(c, x + 3.0, 11.0, 1.0, 12.0, BLACK);
            }
            for i in 0..3 {
                k(c, 18.0, 11.0 + i as f32, 9.0, 1.0, BLACK);
            }
        } else {
            k(c, 6.0, 10.0, 2.0, 2.0, NAVY);
            k(c, 10.0, 9.0, 2.0, 2.0, NAVY);
            k(c, 8.0, 5.0, 1.0, 6.0, BLACK);
            k(c, 12.0, 4.0, 1.0, 6.0, BLACK);
            k(c, 8.0, 4.0, 5.0, 1.0, BLACK);
            k(c, 8.0, 5.0, 1.0, 1.0, BLACK);
        }
    }

    /// A strip of film (My Videos, video files) in (x, y, w, h).
    fn film(c: &mut Canvas, (x, y, w, h): (f32, f32, f32, f32)) {
        k(c, x, y, w, h, BLACK);
        let big = big(c);
        let hole = if big { 2.0 } else { 1.0 };
        let mut hx = x + 1.0;
        while hx + hole <= x + w - 1.0 {
            k(c, hx, y + 1.0, hole, 1.0, WHITE);
            k(c, hx, y + h - 2.0, hole, 1.0, WHITE);
            hx += hole * 2.0;
        }
        let frame_w = ((w - 3.0) / 2.0).floor();
        k(c, x + 1.0, y + 3.0, frame_w, h - 6.0, SKY);
        k(c, x + 2.0 + frame_w, y + 3.0, frame_w, h - 6.0, CYAN);
        if big {
            k(c, x + 1.0, y + 3.0 + (h - 6.0) / 2.0, frame_w, (h - 6.0) / 2.0, BLUE);
            k(c, x + 2.0 + frame_w, y + 3.0 + (h - 6.0) / 2.0, frame_w, (h - 6.0) / 2.0, TEAL);
        }
    }

    fn down_arrow(c: &mut Canvas) {
        let arrow: &[(f32, f32)] = if big(c) {
            &[(19.0, 9.0), (26.0, 9.0), (26.0, 17.0), (31.0, 17.0), (22.5, 26.0), (14.0, 17.0), (19.0, 17.0)]
        } else {
            &[(9.0, 4.0), (13.0, 4.0), (13.0, 8.0), (15.0, 8.0), (11.0, 13.0), (7.0, 8.0), (9.0, 8.0)]
        };
        c.fill(&poly(arrow), &flat(BLACK));
        let inner: Vec<(f32, f32)> = if big(c) {
            vec![(20.0, 10.0), (25.0, 10.0), (25.0, 18.0), (29.0, 18.0), (22.5, 25.0), (16.0, 18.0), (20.0, 18.0)]
        } else {
            vec![(10.0, 5.0), (12.0, 5.0), (12.0, 9.0), (13.0, 9.0), (11.0, 12.0), (9.0, 9.0), (10.0, 9.0)]
        };
        c.fill(&poly(&inner), &flat(GREEN));
        if big(c) {
            k(c, 20.0, 10.0, 1.0, 8.0, LIME);
        } else {
            k(c, 10.0, 5.0, 1.0, 4.0, LIME);
        }
    }

    // ------------------------------------------------ places

    /// Show Desktop's picture: a blotter in a teal frame, a sheet and a pencil on it.
    fn desktop(c: &mut Canvas) {
        if big(c) {
            k(c, 2.0, 7.0, 29.0, 22.0, BLACK);
            k(c, 1.0, 6.0, 29.0, 22.0, NAVY);
            k(c, 2.0, 7.0, 27.0, 20.0, TEAL);
            for (x, y) in [(2.0, 7.0), (26.0, 7.0), (2.0, 24.0), (26.0, 24.0)] {
                k(c, x, y, 3.0, 3.0, BLUE);
            }
            k(c, 5.0, 10.0, 21.0, 14.0, WHITE);
            c.fill(&rect(5.0, 10.0, 21.0, 14.0), &dither(SILVER, (5.0, 10.0), (26.0, 24.0), 0.6));
            k(c, 8.0, 12.0, 9.0, 10.0, WHITE);
            k(c, 8.0, 12.0, 9.0, 1.0, GREY);
            k(c, 8.0, 12.0, 1.0, 10.0, GREY);
            for i in 0..4 {
                k(c, 10.0, 14.0 + i as f32 * 2.0, 5.0, 1.0, NAVY);
            }
            // The pencil, lying across to the top right.
            for i in 0..10 {
                let (x, y) = (17.0 + i as f32, 22.0 - i as f32);
                k(c, x, y - 2.0, 2.0, 1.0, YELLOW);
                k(c, x + 1.0, y - 1.0, 1.0, 1.0, OLIVE);
            }
            k(c, 16.0, 21.0, 1.0, 1.0, BLACK);
            k(c, 27.0, 10.0, 2.0, 2.0, RED);
        } else {
            k(c, 1.0, 4.0, 15.0, 10.0, BLACK);
            k(c, 0.0, 3.0, 15.0, 10.0, NAVY);
            k(c, 1.0, 4.0, 13.0, 8.0, TEAL);
            k(c, 2.0, 5.0, 11.0, 6.0, WHITE);
            k(c, 3.0, 6.0, 4.0, 4.0, CREAM);
            k(c, 4.0, 7.0, 2.0, 1.0, NAVY);
            for i in 0..5 {
                k(c, 8.0 + i as f32, 9.0 - i as f32, 1.0, 1.0, YELLOW);
                k(c, 8.0 + i as f32, 10.0 - i as f32, 1.0, 1.0, OLIVE);
            }
            k(c, 13.0, 4.0, 1.0, 1.0, RED);
        }
    }

    /// Home: a house with a red roof, white walls, a door and a window.
    fn house(c: &mut Canvas) {
        if big(c) {
            k(c, 22.0, 4.0, 3.0, 7.0, BLACK);
            k(c, 23.0, 5.0, 1.0, 6.0, MAROON);
            k(c, 6.0, 15.0, 21.0, 14.0, BLACK);
            k(c, 7.0, 15.0, 19.0, 13.0, WHITE);
            c.fill(&rect(17.0, 15.0, 9.0, 13.0), &checker(WHITE, SILVER));
            k(c, 10.0, 18.0, 6.0, 10.0, BLACK);
            k(c, 11.0, 19.0, 4.0, 9.0, OLIVE);
            k(c, 12.0, 20.0, 2.0, 7.0, YELLOW);
            k(c, 19.0, 18.0, 5.0, 5.0, BLACK);
            k(c, 20.0, 19.0, 3.0, 3.0, SKY);
            pts(c, &[(1.0, 17.0), (16.0, 2.0), (31.0, 17.0), (28.0, 17.0), (16.0, 5.0), (4.0, 17.0)], BLACK);
            pts(c, &[(4.0, 16.0), (16.0, 4.0), (16.0, 7.0), (7.0, 16.0)], RED);
            pts(c, &[(16.0, 4.0), (28.0, 16.0), (25.0, 16.0), (16.0, 7.0)], MAROON);
        } else {
            k(c, 11.0, 2.0, 2.0, 4.0, MAROON);
            k(c, 3.0, 8.0, 11.0, 7.0, BLACK);
            k(c, 4.0, 8.0, 9.0, 6.0, WHITE);
            k(c, 9.0, 8.0, 4.0, 6.0, SILVER);
            k(c, 5.0, 10.0, 3.0, 5.0, BLACK);
            k(c, 6.0, 11.0, 1.0, 3.0, YELLOW);
            k(c, 10.0, 10.0, 2.0, 2.0, SKY);
            pts(c, &[(0.0, 9.0), (8.0, 1.0), (16.0, 9.0), (14.0, 9.0), (8.0, 3.0), (2.0, 9.0)], BLACK);
            pts(c, &[(2.0, 8.0), (8.0, 2.0), (8.0, 4.0), (4.0, 8.0)], RED);
            pts(c, &[(8.0, 2.0), (14.0, 8.0), (12.0, 8.0), (8.0, 4.0)], MAROON);
        }
    }

    /// A raised grey box: white top and left edges, grey and black bottom and right ones.
    fn raised(c: &mut Canvas, x: f32, y: f32, w: f32, h: f32) {
        k(c, x, y, w, h, SILVER);
        k(c, x, y, w - 1.0, 1.0, WHITE);
        k(c, x, y, 1.0, h - 1.0, WHITE);
        k(c, x + 1.0, y + h - 2.0, w - 2.0, 1.0, GREY);
        k(c, x + w - 2.0, y + 1.0, 1.0, h - 2.0, GREY);
        k(c, x, y + h - 1.0, w, 1.0, BLACK);
        k(c, x + w - 1.0, y, 1.0, h, BLACK);
    }

    /// My Computer: a grey monitor with a teal screen on a desktop case.
    fn computer(c: &mut Canvas) {
        if big(c) {
            raised(c, 4.0, 2.0, 22.0, 18.0);
            k(c, 6.0, 4.0, 17.0, 13.0, BLACK);
            k(c, 7.0, 5.0, 15.0, 11.0, TEAL);
            k(c, 9.0, 7.0, 8.0, 5.0, WHITE);
            k(c, 9.0, 7.0, 8.0, 1.0, NAVY);
            k(c, 12.0, 20.0, 6.0, 2.0, GREY);
            k(c, 12.0, 21.0, 6.0, 1.0, BLACK);
            raised(c, 1.0, 22.0, 29.0, 7.0);
            k(c, 16.0, 24.0, 10.0, 1.0, GREY);
            k(c, 16.0, 25.0, 10.0, 1.0, WHITE);
            k(c, 4.0, 25.0, 2.0, 1.0, LIME);
        } else {
            raised(c, 2.0, 0.0, 12.0, 10.0);
            k(c, 3.0, 1.0, 9.0, 7.0, BLACK);
            k(c, 4.0, 2.0, 7.0, 5.0, TEAL);
            k(c, 6.0, 10.0, 4.0, 1.0, GREY);
            raised(c, 0.0, 11.0, 16.0, 4.0);
            k(c, 2.0, 12.0, 1.0, 1.0, LIME);
            k(c, 9.0, 12.0, 4.0, 1.0, GREY);
        }
    }

    /// The Recycle Bin: a wire basket, the green recycling arrows on it, papers when full.
    fn bin(c: &mut Canvas, full: bool) {
        let big = big(c);
        let (cx, top, rx, ry, bottom, taper) = if big { (16.0, 6.0, 11.0, 3.0, 30.0, 3.0) } else { (8.0, 3.0, 6.0, 2.0, 15.0, 2.0) };
        // The body: a mesh narrowing to the bottom, grey on the left, black on the right.
        let body = [(cx - rx, top), (cx + rx, top), (cx + rx - taper, bottom), (cx - rx + taper, bottom)];
        c.fill(&poly(&body), &flat(BLACK));
        let inner = [(cx - rx + 1.0, top), (cx + rx - 1.0, top), (cx + rx - taper - 1.0, bottom - 1.0), (cx - rx + taper + 1.0, bottom - 1.0)];
        c.fill(&poly(&inner), &checker(SILVER, WHITE));
        c.fill(&poly(&[(cx - rx, top), (cx - rx + 1.0, top), (cx - rx + taper + 1.0, bottom), (cx - rx + taper, bottom)]), &flat(GREY));
        c.fill(&poly(&[(cx + 1.0, top), (cx + rx - 1.0, top), (cx + rx - taper - 1.0, bottom - 1.0), (cx + 1.0, bottom - 1.0)]), &checker(SILVER, GREY));
        // The rim and the opening.
        c.fill(&oval(cx, top, rx, ry), &flat(BLACK));
        c.fill(&oval(cx, top, rx - 1.0, ry - 1.0 + if big { 0.0 } else { 0.5 }), &flat(GREY));
        if full {
            let paper: Vec<(f32, f32)> = if big {
                vec![(7.0, 6.0), (9.0, 1.0), (14.0, 3.0), (17.0, 0.0), (23.0, 2.0), (25.0, 6.0)]
            } else {
                vec![(3.0, 3.0), (5.0, 0.0), (8.0, 1.0), (11.0, 0.0), (13.0, 3.0)]
            };
            c.fill(&poly(&paper), &flat(WHITE));
            c.rim(&poly(&paper), &flat(GREY));
            if big {
                k(c, 12.0, 3.0, 1.0, 2.0, GREY);
                k(c, 19.0, 2.0, 1.0, 3.0, GREY);
                k(c, 8.0, 4.0, 2.0, 1.0, SILVER);
            } else {
                k(c, 7.0, 1.0, 1.0, 1.0, GREY);
            }
        }
        // The recycling sign.
        if big {
            let (x, y) = (11.0, 13.0);
            pts(c, &[(x + 5.0, y), (x + 10.0, y + 9.0), (x, y + 9.0)], GREEN);
            pts(c, &[(x + 5.0, y + 3.0), (x + 7.5, y + 7.5), (x + 2.5, y + 7.5)], SILVER);
            k(c, x + 4.0, y + 1.0, 2.0, 2.0, LIME);
        } else {
            pts(c, &[(8.0, 6.0), (11.0, 12.0), (5.0, 12.0)], GREEN);
            k(c, 7.0, 9.0, 2.0, 2.0, SILVER);
        }
    }

    // ------------------------------------------------ drives

    /// A drive: a grey slab seen a little from above, its top and right side, the front with the
    /// light (hard disk), a slot (removable), a disc above it (CD) or a cable under it (network).
    fn drive(c: &mut Canvas, kind: DriveKind) {
        let big = big(c);
        if kind == DriveKind::Optical {
            let (cx, cy, r) = if big { (16.0, 9.0, 8.0) } else { (8.0, 4.0, 4.0) };
            c.fill(&oval(cx, cy, r, r), &flat(BLACK));
            c.fill(&oval(cx, cy, r - 1.0, r - 1.0), &flat(SILVER));
            if big {
                c.fill(&oval(cx, cy, r - 2.0, r - 2.0), &flat(CYAN));
                c.fill(&oval(cx, cy, r - 3.5, r - 3.5), &flat(WHITE));
                c.fill(&oval(cx, cy, r - 5.0, r - 5.0), &flat(YELLOW));
            } else {
                c.fill(&oval(cx, cy, r - 2.0, r - 2.0), &flat(CYAN));
            }
            c.fill(&oval(cx, cy, if big { 1.5 } else { 0.8 }, if big { 1.5 } else { 0.8 }), &flat(WHITE));
        }
        let (x, y, w, h, d) = if big { (1.0, 18.0, 26.0, 9.0, 3.0) } else { (0.0, 9.0, 13.0, 5.0, 2.0) };
        // Top, right side, front.
        pts(c, &[(x + d, y - d), (x + w + d, y - d), (x + w, y), (x, y)], SILVER);
        k(c, x + d, y - d, w, 1.0, WHITE);
        pts(c, &[(x + w, y), (x + w + d, y - d), (x + w + d, y + h - d), (x + w, y + h)], GREY);
        k(c, x + w + d - 1.0, y - d, 1.0, h, BLACK);
        raised(c, x, y, w + 1.0, h);
        match kind {
            DriveKind::Fixed | DriveKind::System | DriveKind::Network | DriveKind::Optical if !big => {
                k(c, x + 2.0, y + h - 3.0, 1.0, 1.0, LIME);
                k(c, x + 6.0, y + 2.0, 5.0, 1.0, GREY);
            }
            DriveKind::Fixed | DriveKind::System | DriveKind::Network | DriveKind::Optical => {
                k(c, x + 3.0, y + h - 4.0, 2.0, 1.0, LIME);
                k(c, x + 12.0, y + 3.0, 11.0, 1.0, GREY);
                k(c, x + 12.0, y + 4.0, 11.0, 1.0, WHITE);
            }
            DriveKind::Removable => {
                if big {
                    k(c, x + 5.0, y + 3.0, 17.0, 2.0, BLACK);
                    k(c, x + 19.0, y + 6.0, 3.0, 1.0, GREY);
                } else {
                    k(c, x + 3.0, y + 2.0, 8.0, 1.0, BLACK);
                }
            }
        }
        if kind == DriveKind::Network {
            if big {
                k(c, 13.0, 27.0, 2.0, 3.0, BLACK);
                k(c, 4.0, 30.0, 22.0, 1.0, BLACK);
            } else {
                k(c, 6.0, 14.0, 1.0, 1.0, BLACK);
                k(c, 2.0, 15.0, 10.0, 1.0, BLACK);
            }
        }
    }

    // ------------------------------------------------ files

    /// A white page, its top right corner turned down; returns (left, top, right, bottom) of
    /// the room inside. `fold`: false for a pad without the corner.
    fn page(c: &mut Canvas, fold: bool) -> (f32, f32, f32, f32) {
        let (x, y, w, h, f) = if big(c) { (5.0, 1.0, 22.0, 30.0, 6.0) } else { (2.0, 0.0, 12.0, 16.0, 4.0) };
        let f = if fold { f } else { 0.0 };
        let (r, b) = (x + w - 1.0, y + h - 1.0);
        k(c, x, y, w - f, h, WHITE);
        k(c, x, y + f, w, h - f, WHITE);
        k(c, x, y, w - f, 1.0, GREY);
        k(c, x, y, 1.0, h, GREY);
        k(c, r, y + f, 1.0, h - f, BLACK);
        k(c, x, b, w, 1.0, BLACK);
        if fold {
            // The turned-down corner: a grey flap, its black fold line.
            for i in 0..f as usize {
                let i = i as f32;
                k(c, r - f + 1.0 + i, y + i, 1.0, 1.0, BLACK);
                k(c, r - f + 1.0, y + i + 1.0, i, 1.0, SILVER);
            }
            k(c, r - f + 1.0, y + f, f, 1.0, BLACK);
            k(c, r - f, y, 1.0, f + 1.0, GREY);
        }
        (x + 2.0, y + 2.0, r - 2.0, b - 2.0)
    }

    fn file(c: &mut Canvas, kind: FileKind) {
        let big = big(c);
        match kind {
            FileKind::Zip => {
                folder(c);
                zipper(c);
                return;
            }
            FileKind::Exe => {
                app_window(c);
                return;
            }
            _ => {}
        }
        let (l, t, r, b) = page(c, kind != FileKind::Text);
        let w = r - l + 1.0;
        match kind {
            FileKind::Generic => {
                // Windows' four-coloured flag, waving, the speed lines before it.
                let (x, y, s): (f32, f32, f32) = if big { (l + 6.0, t + 8.0, 8.0) } else { (l + 3.0, t + 4.0, 3.0) };
                let half = (s / 2.0).ceil();
                for (dx, dy, col) in [(0.0, 0.0, RED), (half + 1.0, 0.0, GREEN), (0.0, half + 1.0, BLUE), (half + 1.0, half + 1.0, YELLOW)] {
                    k(c, x + dx, y + dy + if dx > 0.0 { -1.0 } else { 0.0 }, half, half, col);
                }
                if big {
                    for i in 0..3 {
                        k(c, l + 2.0, y + 1.0 + i as f32 * 2.0, 4.0 - i as f32, 1.0, BLACK);
                    }
                }
            }
            FileKind::Text => {
                // A pad: black rings along the top, lines of writing.
                let n = if big { 6 } else { 4 };
                for i in 0..n {
                    k(c, l + 1.0 + i as f32 * if big { 3.0 } else { 2.0 }, t - 3.0, 1.0, 2.0, BLACK);
                }
                let mut y = t + 2.0;
                let mut i = 0;
                while y <= b - 1.0 {
                    k(c, l, y, if i % 3 == 2 { w * 0.5 } else { w - 1.0 }, 1.0, BLACK);
                    y += 2.0;
                    i += 1;
                }
            }
            FileKind::Image => {
                let (x, y, pw, ph) = if big { (l + 1.0, t + 7.0, w - 2.0, 12.0) } else { (l, t + 4.0, w, 6.0) };
                k(c, x, y, pw, ph, BLACK);
                k(c, x + 1.0, y + 1.0, pw - 2.0, ph - 2.0, SKY);
                let hill = [(x + 1.0, y + ph - 1.0), (x + 1.0, y + ph * 0.6), (x + pw * 0.4, y + ph * 0.3), (x + pw - 1.0, y + ph * 0.7), (x + pw - 1.0, y + ph - 1.0)];
                pts(c, &hill, GREEN);
                k(c, x + pw - if big { 5.0 } else { 3.0 }, y + if big { 2.0 } else { 1.0 }, if big { 2.0 } else { 1.0 }, if big { 2.0 } else { 1.0 }, YELLOW);
            }
            FileKind::Audio => {
                // A loudspeaker and its sound.
                let (x, y) = if big { (l + 2.0, t + 8.0) } else { (l, t + 3.0) };
                if big {
                    k(c, x, y + 3.0, 3.0, 6.0, BLACK);
                    k(c, x + 1.0, y + 4.0, 2.0, 4.0, YELLOW);
                    pts(c, &[(x + 3.0, y + 3.0), (x + 8.0, y - 1.0), (x + 8.0, y + 13.0), (x + 3.0, y + 9.0)], BLACK);
                    pts(c, &[(x + 3.0, y + 4.0), (x + 7.0, y + 1.0), (x + 7.0, y + 11.0), (x + 3.0, y + 8.0)], YELLOW);
                    k(c, x + 5.0, y + 4.0, 2.0, 5.0, OLIVE);
                    for (dx, hh) in [(10.0, 4.0), (13.0, 8.0)] {
                        k(c, x + dx, y + 6.0 - hh / 2.0, 1.0, hh, GREY);
                    }
                } else {
                    k(c, x, y + 2.0, 2.0, 3.0, BLACK);
                    pts(c, &[(x + 2.0, y + 2.0), (x + 4.0, y), (x + 4.0, y + 7.0), (x + 2.0, y + 5.0)], BLACK);
                    k(c, x + 2.0, y + 2.0, 1.0, 3.0, YELLOW);
                    k(c, x + 6.0, y + 2.0, 1.0, 3.0, GREY);
                }
            }
            FileKind::Video => {
                if big {
                    film(c, (l, t + 7.0, w, 12.0));
                } else {
                    film(c, (l, t + 4.0, w, 6.0));
                }
            }
            FileKind::Document | FileKind::Pdf => {
                let mut y = t + if big { 4.0 } else { 2.0 };
                while y <= b {
                    k(c, l + if big { 6.0 } else { 3.0 }, y, w - if big { 7.0 } else { 3.0 }, 1.0, NAVY);
                    y += 2.0;
                }
                // The badge: blue with lines (a word processor's), red with a loop (a PDF).
                let (x, y, s) = if big { (1.0, 16.0, 13.0) } else { (0.0, 8.0, 8.0) };
                let (fill, dark) = if kind == FileKind::Pdf { (RED, MAROON) } else { (BLUE, NAVY) };
                k(c, x, y, s, s, dark);
                k(c, x, y, s - 1.0, s - 1.0, fill);
                if kind == FileKind::Pdf {
                    if big {
                        k(c, x + 3.0, y + 3.0, 6.0, 1.0, WHITE);
                        k(c, x + 3.0, y + 3.0, 1.0, 6.0, WHITE);
                        k(c, x + 3.0, y + 8.0, 7.0, 1.0, WHITE);
                        k(c, x + 9.0, y + 4.0, 1.0, 5.0, WHITE);
                    } else {
                        k(c, x + 2.0, y + 2.0, 3.0, 1.0, WHITE);
                        k(c, x + 2.0, y + 2.0, 1.0, 4.0, WHITE);
                        k(c, x + 4.0, y + 3.0, 1.0, 2.0, WHITE);
                    }
                } else {
                    let mut ly = y + 2.0;
                    while ly < y + s - 2.0 {
                        k(c, x + 2.0, ly, s - 5.0, 1.0, WHITE);
                        ly += 2.0;
                    }
                }
            }
            _ => {}
        }
    }

    /// A zipper down the closed folder's front, its grey pull at the bottom.
    fn zipper(c: &mut Canvas) {
        let (x, top, bottom) = if big(c) { (8.0, 6.0, 24.0) } else { (4.0, 4.0, 11.0) };
        let mut y = top;
        let mut left = true;
        while y < bottom {
            k(c, if left { x } else { x + 1.0 }, y, 1.0, 1.0, BLACK);
            k(c, if left { x + 1.0 } else { x }, y, 1.0, 1.0, SILVER);
            left = !left;
            y += 1.0;
        }
        if big(c) {
            k(c, x - 1.0, bottom, 4.0, 4.0, BLACK);
            k(c, x, bottom, 2.0, 3.0, SILVER);
        } else {
            k(c, x - 1.0, bottom, 3.0, 2.0, GREY);
        }
    }

    /// A program: a plain window, its navy title bar and silver buttons.
    fn app_window(c: &mut Canvas) {
        let (x, y, w, h, t) = if big(c) { (1.0, 4.0, 30.0, 24.0, 4.0) } else { (0.0, 2.0, 16.0, 13.0, 3.0) };
        k(c, x, y, w, h, BLACK);
        k(c, x, y, w - 1.0, h - 1.0, SILVER);
        k(c, x + 1.0, y + 1.0, w - 3.0, t, NAVY);
        if big(c) {
            for i in 0..3 {
                raised(c, x + w - 13.0 + i as f32 * 4.0, y + 1.0, 4.0, t);
            }
        } else {
            k(c, x + w - 4.0, y + 1.0, 2.0, t - 1.0, SILVER);
        }
        k(c, x + 1.0, y + t + 2.0, w - 3.0, h - t - 4.0, WHITE);
        k(c, x + 1.0, y + t + 2.0, w - 3.0, 1.0, GREY);
        k(c, x + 1.0, y + t + 2.0, 1.0, h - t - 4.0, GREY);
    }
}


// ---------------------------------------------------------------- XP

/// Windows XP's icons: the yellow folder in perspective (back panel with its tab, the front flap
/// leaning toward you), the places' pictures rising out of the open folder, soft ramps, a thin
/// outline in a darker shade, a drop shadow from 24 pixels up.
mod xp {
    use super::art::*;
    use super::{DriveKind, FileKind, Role};

    pub(super) fn paint(c: &mut Canvas, role: Role) {
        match role {
            Role::Folder => folder(c, 0.0),
            Role::FolderOpen => {
                back(c, 0.0);
                front(c, true, 0.0);
            }
            Role::Documents | Role::Pictures | Role::Music | Role::Videos => special(c, role),
            Role::Downloads => {
                folder(c, 0.0);
                down_arrow(c);
            }
            Role::Home | Role::Computer => computer(c),
            Role::Desktop => desktop(c),
            Role::Bin { full } => bin(c, full),
            Role::Drive(kind) => drive(c, kind),
            Role::File(kind) => file(c, kind),
        }
        if c.big() {
            c.shadow(0.9, 0.9, 0.9, 0.3);
        }
    }

    // ------------------------------------------------ folders

    fn back_shape(c: &Canvas, dy: f32) -> Shape {
        let pts: &[(f32, f32)] = if c.big() {
            &[(1.5, 8.5), (3.3, 5.5), (10.4, 5.0), (12.8, 7.3), (26.5, 6.5), (28.3, 7.5), (28.3, 24.0), (2.7, 28.5)]
        } else {
            &[(2.0, 8.0), (4.0, 4.0), (12.0, 4.0), (14.0, 6.0), (28.0, 6.0), (28.0, 26.0), (2.0, 28.0)]
        };
        poly(&moved(pts, 0.0, dy))
    }

    fn front_shape(c: &Canvas, open: bool, dy: f32) -> Shape {
        let pts: &[(f32, f32)] = match (c.big(), open) {
            (true, false) => &[(4.5, 12.0), (13.4, 11.0), (15.2, 9.5), (29.5, 8.0), (27.1, 24.5), (3.3, 28.5)],
            (true, true) => &[(7.5, 14.5), (15.8, 13.0), (18.8, 11.5), (30.5, 10.5), (27.1, 23.8), (3.3, 28.5)],
            (false, false) => &[(4.0, 12.0), (14.0, 11.0), (16.0, 9.0), (30.0, 8.0), (27.0, 26.0), (2.0, 28.0)],
            (false, true) => &[(8.0, 14.0), (16.0, 13.0), (18.0, 11.0), (31.0, 10.0), (27.0, 26.0), (2.0, 28.0)],
        };
        poly(&moved(pts, 0.0, dy))
    }

    /// The back panel and its tab.
    fn back(c: &mut Canvas, dy: f32) {
        let s = back_shape(c, dy);
        c.paint(
            &s,
            &lin((0.0, 5.0 + dy), (0.0, 26.0 + dy), &[(0.0, 0xfef3ae), (1.0, 0xf2cf6e)]),
            &lin((2.0, 5.0 + dy), (28.0, 28.0 + dy), &[(0.0, 0xd4ae45), (1.0, 0x93701a)]),
        );
    }

    /// The front flap, lit from the top left, a light line inside its top and left edges.
    fn front(c: &mut Canvas, open: bool, dy: f32) {
        let s = front_shape(c, open, dy);
        c.fill(&s, &lin((18.0, 9.0 + dy), (6.0, 28.0 + dy), &[(0.0, 0xfffee6), (0.45, 0xfdf2b0), (1.0, 0xf8d97c)]));
        let h = c.hair();
        c.edge(&s, 2.0 * h, &lin_a((4.0, 9.0 + dy), (22.0, 28.0 + dy), &[(0.0, 0xffffff, 1.0), (0.5, 0xffffff, 0.5), (1.0, 0xffffff, 0.0)]));
        c.rim(&s, &lin((4.0, 8.0 + dy), (28.0, 28.0 + dy), &[(0.0, 0xd9b550), (1.0, 0x8a6411)]));
    }

    fn folder(c: &mut Canvas, dy: f32) {
        back(c, dy);
        front(c, false, dy);
    }

    /// My Documents, My Pictures, My Music, My Videos: the open folder, lower, with its picture
    /// rising out of it; the part inside shows faintly through the front.
    fn special(c: &mut Canvas, role: Role) {
        let dy = if c.big() { 3.0 } else { 2.0 };
        back(c, dy);
        let inside = match role {
            Role::Documents => Some((sheet(c), 0xb8cdf2)),
            Role::Pictures => Some((photo(c), 0xb8cdf2)),
            Role::Videos => Some((film(c), 0x7a7a7a)),
            _ => {
                note_stem(c);
                None
            }
        };
        front(c, true, dy);
        if let Some((s, tint)) = inside {
            c.fill(&both(s, front_shape(c, true, dy)), &flat_a(tint, 0.4));
        }
        if role == Role::Music {
            note_head(c, 15.5, 20.5);
        }
    }

    /// A sheet of paper leaning out, its corner folded; returns its shape.
    fn sheet(c: &mut Canvas) -> Shape {
        let (o, u, v) = ((6.5, 9.5), (7.5, -8.0), (10.5, 12.0));
        let p = |a: f32, b: f32| (o.0 + u.0 * a + v.0 * b, o.1 + u.1 * a + v.1 * b);
        let pts = [p(0.0, 0.0), p(0.7, 0.0), p(1.0, 0.24), p(1.0, 1.0), p(0.0, 1.0)];
        c.paint(&poly(&pts), &lin(p(1.0, 0.0), p(0.0, 1.0), &[(0.0, 0xffffff), (1.0, 0xdbe6fa)]), &flat(0x7a8db8));
        if c.big() {
            for j in 0..5 {
                let b = 0.3 + j as f32 * 0.12;
                c.fill(&line(&[p(0.2, b), p(if j == 4 { 0.55 } else { 0.82 }, b)], 0.9), &flat(0x9db1dd));
            }
        }
        let flap = [p(0.7, 0.0), p(1.0, 0.24), p(0.7, 0.24)];
        c.paint(&poly(&flap), &lin(p(0.7, 0.0), p(0.85, 0.2), &[(0.0, 0xbfe3ff), (1.0, 0x4f8ce9)]), &flat(0x485c8e));
        poly(&pts)
    }

    /// A framed photo leaning out; returns its shape.
    fn photo(c: &mut Canvas) -> Shape {
        let at = (17.0, 9.5);
        let frame = turn(&[(9.5, 3.0), (24.5, 3.0), (24.5, 16.0), (9.5, 16.0)], at, -24.0);
        c.paint(&poly(&frame), &lin((10.0, 3.0), (24.0, 16.0), &[(0.0, 0xffffff), (1.0, 0xdde6f5)]), &flat(0x6f82b0));
        let pic = turn(&[(11.5, 5.0), (22.5, 5.0), (22.5, 14.0), (11.5, 14.0)], at, -24.0);
        c.fill(&poly(&pic), &lin((17.0, 4.0), (17.0, 15.0), &[(0.0, 0x4f8fe6), (1.0, 0xc9e3ff)]));
        // A red and white ring in the picture.
        c.fill(&oval(17.0, 9.5, 3.6, 3.6), &flat(0xffffff));
        c.fill(&oval(17.0, 9.5, 3.0, 3.0), &lin((15.0, 7.0), (19.0, 12.0), &[(0.0, 0xff7a52), (1.0, 0xc8321a)]));
        c.fill(&oval(17.0, 9.5, 1.4, 1.4), &flat(0xffffff));
        if c.big() {
            for a in [45.0f32, 135.0, 225.0, 315.0] {
                let (s, co) = a.to_radians().sin_cos();
                c.fill(&line(&[(17.0 + co * 1.4, 9.5 + s * 1.4), (17.0 + co * 3.0, 9.5 + s * 3.0)], 1.1), &flat(0xffffff));
            }
        }
        poly(&frame)
    }

    /// A strip of film leaning out; returns its shape.
    fn film(c: &mut Canvas) -> Shape {
        let at = (18.0, 8.0);
        let strip = turn(&[(7.5, 4.5), (29.0, 4.5), (29.0, 11.5), (7.5, 11.5)], at, -30.0);
        c.paint(&poly(&strip), &flat(0x262626), &flat(0x000000));
        let cells: [(f32, u32, u32); 3] = [(10.0, 0x6fb4f5, 0x2c6fd6), (16.5, 0x8ee07a, 0x2f9a3a), (23.0, 0xf7c66a, 0xd9792c)];
        for (x, top, bottom) in cells {
            let cell = turn(&[(x, 6.4), (x + 5.0, 6.4), (x + 5.0, 9.6), (x, 9.6)], at, -30.0);
            c.fill(&poly(&cell), &lin(cell[0], cell[3], &[(0.0, top), (1.0, bottom)]));
        }
        if c.big() {
            for i in 0..10 {
                let x = 8.5 + i as f32 * 2.0;
                for y in [5.1, 10.3] {
                    c.fill(&poly(&turn(&[(x, y), (x + 1.0, y), (x + 1.0, y + 0.7), (x, y + 0.7)], at, -30.0)), &flat(0xffffff));
                }
            }
        }
        poly(&strip)
    }

    fn note_stem(c: &mut Canvas) {
        let ink = flat(0x2f2580);
        c.paint(&rect(18.4, 1.5, 2.4, 19.0), &lin((18.4, 0.0), (20.8, 0.0), &[(0.0, 0x9a92ea), (1.0, 0x4b3fae)]), &ink);
        let flag = [(20.8, 1.5), (26.5, 5.5), (26.0, 11.5), (20.8, 7.5)];
        c.paint(&poly(&flag), &lin((21.0, 1.5), (26.0, 11.0), &[(0.0, 0x8279dc), (1.0, 0x3b2f9e)]), &ink);
    }

    fn note_head(c: &mut Canvas, x: f32, y: f32) {
        let head = oval_turned(x, y, 4.6, 3.3, -22.0);
        c.paint(&head, &rad((x - 1.5, y - 1.5), 6.5, 5.5, &[(0.0, 0xdcd8fb), (0.45, 0x8f86e2), (1.0, 0x463aa8)]), &flat(0x2f2580));
        if c.big() {
            c.fill(&oval_turned(x - 1.6, y - 1.2, 1.6, 0.8, -22.0), &flat_a(0xffffff, 0.85));
        }
    }

    fn down_arrow(c: &mut Canvas) {
        let arrow = [(18.0, 5.0), (25.0, 5.0), (25.0, 14.0), (30.0, 14.0), (21.5, 24.5), (13.0, 14.0), (18.0, 14.0)];
        c.paint(&poly(&arrow), &lin((18.0, 5.0), (25.0, 24.0), &[(0.0, 0xa6e88a), (0.5, 0x3db43b), (1.0, 0x178317)]), &flat(0x1e6b12));
        if c.big() {
            c.fill(&line(&[(19.5, 6.5), (19.5, 14.5)], 1.0), &flat_a(0xffffff, 0.6));
        }
    }

    // ------------------------------------------------ places

    /// My Computer: the monitor in front, the tower behind it on the right.
    fn computer(c: &mut Canvas) {
        let ink = flat(0x5c5c8c);
        if c.big() {
            let side = [(23.5, 5.0), (30.0, 7.0), (30.0, 26.5), (23.5, 28.5)];
            c.paint(&poly(&side), &lin((23.5, 0.0), (30.0, 0.0), &[(0.0, 0xa8a8d8), (1.0, 0x6c6cac)]), &ink);
            let face = [(18.5, 6.5), (23.5, 5.0), (23.5, 28.5), (18.5, 27.0)];
            c.paint(&poly(&face), &lin((18.5, 5.0), (23.5, 28.0), &[(0.0, 0xf0f0fa), (1.0, 0xb4b4d4)]), &ink);
            for y in [10.0, 12.5] {
                c.fill(&line(&[(19.6, y + 0.6), (22.6, y - 0.3)], 0.9), &flat(0x7a7aa8));
            }
            c.fill(&oval(21.0, 24.0, 0.8, 0.8), &flat(0x3ce03c));
            // The monitor: its side, the bezel, the screen with a sheen, the stand.
            c.fill(&oval(12.0, 27.0, 7.5, 2.4), &lin((5.0, 25.0), (19.0, 29.0), &[(0.0, 0xececf6), (1.0, 0x9a9ac0)]));
            c.rim(&oval(12.0, 27.0, 7.5, 2.4), &ink);
            c.paint(&poly(&[(9.5, 21.5), (14.0, 20.5), (15.0, 25.5), (9.0, 26.5)]), &lin((9.0, 21.0), (15.0, 26.0), &[(0.0, 0xd0d0e4), (1.0, 0x8c8cb0)]), &ink);
            let thick = [(18.4, 3.2), (20.2, 4.4), (22.4, 18.0), (20.8, 18.6)];
            c.paint(&poly(&thick), &flat(0x8e8eb8), &ink);
            let bezel = [(1.2, 5.8), (18.4, 3.2), (20.8, 18.6), (4.6, 23.2)];
            c.paint(&poly(&bezel), &lin((2.0, 4.0), (20.0, 22.0), &[(0.0, 0xf8f8fc), (1.0, 0xc0c0d6)]), &lin((2.0, 4.0), (20.0, 22.0), &[(0.0, 0x9898b8), (1.0, 0x55558a)]));
            let screen = [(3.6, 7.6), (16.6, 5.6), (18.5, 16.9), (6.0, 20.6)];
            c.paint(&poly(&screen), &lin((5.0, 6.0), (17.0, 20.0), &[(0.0, 0xd2edff), (0.5, 0x6ca8f8), (1.0, 0x2e6fe0)]), &flat(0x3a5fb0));
            c.fill(&both(poly(&screen), poly(&[(3.0, 7.0), (17.0, 5.0), (17.4, 9.6), (4.0, 13.4)])), &flat_a(0xffffff, 0.35));
        } else {
            c.paint(&rect(20.0, 6.0, 10.0, 22.0), &lin((20.0, 0.0), (30.0, 0.0), &[(0.0, 0xd8d8f0), (1.0, 0x8080c0)]), &ink);
            c.fill(&rect(22.0, 10.0, 6.0, 2.0), &flat(0x7a7aa8));
            c.fill(&rect(26.0, 22.0, 2.0, 2.0), &flat(0x3ce03c));
            c.paint(&rect(6.0, 22.0, 12.0, 4.0), &lin((6.0, 22.0), (6.0, 26.0), &[(0.0, 0xe0e0f0), (1.0, 0x9a9ac0)]), &ink);
            c.paint(&rect(10.0, 20.0, 4.0, 3.0), &flat(0xb0b0cc), &ink);
            c.paint(&round(2.0, 4.0, 20.0, 17.0, 1.0), &lin((2.0, 4.0), (22.0, 21.0), &[(0.0, 0xf6f6fc), (1.0, 0xb8b8d2)]), &flat(0x55558a));
            c.paint(&rect(4.0, 6.0, 16.0, 12.0), &lin((4.0, 6.0), (20.0, 18.0), &[(0.0, 0xc4e6ff), (0.5, 0x6ca8f8), (1.0, 0x2e6fe0)]), &flat(0x3a5fb0));
        }
    }

    /// Show Desktop's picture: a blue frame on the desk, a sheet and a pencil.
    fn desktop(c: &mut Canvas) {
        if c.big() {
            let frame = [(3.5, 6.5), (23.5, 2.5), (28.5, 25.5), (8.5, 29.5)];
            c.paint(&poly(&frame), &lin((4.0, 4.0), (28.0, 28.0), &[(0.0, 0x76a6f0), (0.5, 0x3f6fd0), (1.0, 0x1c3c9e)]), &flat(0x13255c));
            let desk = [(6.6, 8.9), (21.8, 5.8), (25.6, 23.3), (10.4, 26.4)];
            c.paint(&poly(&desk), &lin((7.0, 7.0), (25.0, 26.0), &[(0.0, 0xfffcee), (1.0, 0xeadcb4)]), &flat(0x2b4c9c));
            let page = [(9.5, 10.4), (17.6, 8.8), (20.2, 20.6), (12.1, 22.2)];
            c.paint(&poly(&page), &lin((10.0, 9.0), (20.0, 22.0), &[(0.0, 0xffffff), (1.0, 0xc6d8f8)]), &flat(0x7a8db8));
            pencil(c, (14.5, 24.0), (29.0, 9.0), 3.0);
        } else {
            c.paint(&round(2.0, 4.0, 28.0, 24.0, 3.0), &lin((2.0, 4.0), (30.0, 28.0), &[(0.0, 0x76a6f0), (0.5, 0x3f6fd0), (1.0, 0x1c3c9e)]), &flat(0x13255c));
            c.paint(&rect(6.0, 8.0, 20.0, 16.0), &lin((6.0, 8.0), (26.0, 24.0), &[(0.0, 0xfffcee), (1.0, 0xeadcb4)]), &flat(0x2b4c9c));
            c.paint(&rect(9.0, 10.0, 9.0, 12.0), &lin((9.0, 10.0), (18.0, 22.0), &[(0.0, 0xffffff), (1.0, 0xc6d8f8)]), &flat(0x7a8db8));
            pencil(c, (15.0, 23.0), (29.0, 9.0), 3.4);
        }
    }

    fn pencil(c: &mut Canvas, tip: (f32, f32), end: (f32, f32), w: f32) {
        let (dx, dy) = (end.0 - tip.0, end.1 - tip.1);
        let len = (dx * dx + dy * dy).sqrt();
        let (ux, uy) = (dx / len, dy / len);
        let (nx, ny) = (-uy * w / 2.0, ux * w / 2.0);
        let wood = (tip.0 + ux * w * 1.2, tip.1 + uy * w * 1.2);
        let body = [(wood.0 + nx, wood.1 + ny), (end.0 + nx, end.1 + ny), (end.0 - nx, end.1 - ny), (wood.0 - nx, wood.1 - ny)];
        c.paint(&poly(&body), &lin((wood.0 + nx, wood.1 + ny), (wood.0 - nx, wood.1 - ny), &[(0.0, 0xffe27a), (1.0, 0xd99a1e)]), &flat(0x8a5a10));
        c.paint(&poly(&[tip, (wood.0 + nx, wood.1 + ny), (wood.0 - nx, wood.1 - ny)]), &flat(0xf6d7a8), &flat(0x8a5a10));
        c.fill(&oval(tip.0 + ux * 0.6, tip.1 + uy * 0.6, w * 0.22, w * 0.22), &flat(0x202020));
        c.paint(&oval(end.0, end.1, w * 0.55, w * 0.55), &rad((end.0 - 0.4, end.1 - 0.4), w * 0.6, w * 0.6, &[(0.0, 0xff9a8a), (1.0, 0xc8281e)]), &flat(0x7a1410));
    }

    /// The Recycle Bin: a bin of pale blue glass, two green arrows chasing round on it.
    fn bin(c: &mut Canvas, full: bool) {
        let rim_ink = flat(0x5b88bd);
        let glass = lin((3.0, 0.0), (29.0, 0.0), &[(0.0, 0xbfe4fa), (0.3, 0xe6f8ff), (0.7, 0xb9def7), (1.0, 0x7db5ea)]);
        let (top, rx) = if c.big() { (8.0, 12.5) } else { (8.0, 13.0) };
        // The far inside wall, what's in it, then the body and the near rim.
        c.fill(&oval(16.0, top, rx, 3.4), &lin((0.0, top - 3.0), (0.0, top + 3.0), &[(0.0, 0x7db5ea), (1.0, 0xd8efff)]));
        if full {
            let paper = [(8.0, 8.0), (10.5, 2.5), (15.0, 0.8), (19.0, 2.0), (23.5, 1.0), (25.0, 7.5), (16.0, 10.0)];
            c.paint(&poly(&paper), &lin((10.0, 1.0), (22.0, 9.0), &[(0.0, 0xffffff), (1.0, 0xfff0c0)]), &flat(0xb4a97a));
            c.fill(&line(&[(12.5, 3.5), (15.0, 6.5), (17.5, 4.0)], 0.8), &flat(0xc9bd8c));
            let sheet = [(17.0, 3.0), (22.0, 0.5), (26.0, 6.0), (21.5, 8.5)];
            c.paint(&poly(&sheet), &lin((18.0, 1.0), (25.0, 8.0), &[(0.0, 0xffffff), (1.0, 0xe2eaf8)]), &flat(0x8a9bc6));
        }
        let body = [(16.0 - rx, top), (16.0 + rx, top), (16.0 + rx - 2.6, 27.5), (16.0 - rx + 2.6, 27.5)];
        let s = poly(&body);
        c.fill(&s, &glass);
        c.fill(&oval(16.0, 27.5, rx - 2.6, 2.6), &glass);
        c.rim(&oval(16.0, 27.5, rx - 2.6, 2.6), &rim_ink);
        c.rim(&s, &rim_ink);
        c.fill(&oval(16.0, 27.5, rx - 2.6 - c.hair(), 2.6 - c.hair()), &glass);
        c.fill(&rect(16.0 - rx + 2.6 + c.hair(), 24.0, 2.0 * (rx - 2.6 - c.hair()), 3.5), &glass);
        c.fill(&line(&[(7.5, 11.5), (9.0, 25.5)], 1.6), &flat_a(0xffffff, 0.7));
        // The rim: a band round the opening.
        c.edge(&oval(16.0, top, rx, 3.4), 1.2, &lin((0.0, top - 3.0), (0.0, top + 3.0), &[(0.0, 0xa9d4f4), (1.0, 0xffffff)]));
        c.rim(&oval(16.0, top, rx, 3.4), &rim_ink);
        recycle(c, 16.5, 18.5, if c.big() { 5.2 } else { 6.0 }, if c.big() { 2.6 } else { 3.2 });
    }

    /// Two green arrows chasing each other round (cx, cy).
    pub(super) fn recycle(c: &mut Canvas, cx: f32, cy: f32, r: f32, w: f32) {
        for start in [195.0f32, 15.0] {
            let end = start + 110.0;
            let body = line(&arc(cx, cy, r, r, start, end), w);
            c.paint(&body, &lin((cx - r, cy - r), (cx + r, cy + r), &[(0.0, 0x4fd34f), (1.0, 0x0b880b)]), &flat(0x0b5e0b));
            let (b, t) = (end.to_radians(), (end + 32.0).to_radians());
            let base = (cx + r * b.cos(), cy + r * b.sin());
            let (nx, ny) = (b.cos() * w * 1.05, b.sin() * w * 1.05);
            let tip = (cx + r * t.cos(), cy + r * t.sin());
            c.paint(&poly(&[(base.0 + nx, base.1 + ny), tip, (base.0 - nx, base.1 - ny)]), &flat(0x1fa01f), &flat(0x0b5e0b));
        }
    }

    // ------------------------------------------------ drives

    fn drive(c: &mut Canvas, kind: DriveKind) {
        let ink = flat(0x4e4e5c);
        if !c.big() {
            small_drive(c, kind);
            return;
        }
        match kind {
            DriveKind::Fixed | DriveKind::System => {
                let top = [(0.8, 13.0), (15.5, 8.5), (31.2, 13.5), (16.5, 18.5)];
                let left = [(0.8, 13.0), (16.5, 18.5), (16.5, 25.0), (0.8, 19.5)];
                let right = [(16.5, 18.5), (31.2, 13.5), (31.2, 20.0), (16.5, 25.0)];
                c.paint(&poly(&left), &lin((0.0, 14.0), (0.0, 25.0), &[(0.0, 0xececf2), (1.0, 0xa0a0ac)]), &ink);
                c.paint(&poly(&right), &lin((16.5, 0.0), (31.0, 0.0), &[(0.0, 0x9a9aa6), (1.0, 0x74747e)]), &ink);
                c.paint(&poly(&top), &rad((13.0, 12.0), 17.0, 8.0, &[(0.0, 0xffffff), (1.0, 0xc4c4ce)]), &ink);
                c.fill(&poly(&[(19.0, 19.8), (29.0, 16.4), (29.0, 19.4), (19.0, 22.8)]), &lin((19.0, 17.0), (29.0, 22.0), &[(0.0, 0x404048), (1.0, 0x18181c)]));
                c.fill(&poly(&[(8.5, 12.8), (14.0, 11.0), (16.5, 11.8), (11.0, 13.6)]), &flat(0x1c1c22));
                if c.big() {
                    for x in [3.0, 13.8] {
                        c.fill(&line(&[(x, 16.5 + (x - 3.0) * 0.35), (x + 1.4, 17.0 + (x - 3.0) * 0.35)], 0.8), &flat(0x8a8a96));
                    }
                }
            }
            DriveKind::Removable | DriveKind::Optical | DriveKind::Network => {
                if kind == DriveKind::Optical {
                    disc(c, 15.0, 10.0, 8.5);
                }
                let top = [(1.0, 16.0), (16.0, 12.0), (31.0, 16.0), (16.0, 20.0)];
                let left = [(1.0, 16.0), (16.0, 20.0), (16.0, 24.0), (1.0, 20.0)];
                let right = [(16.0, 20.0), (31.0, 16.0), (31.0, 20.0), (16.0, 24.0)];
                c.paint(&poly(&left), &lin((0.0, 16.0), (0.0, 24.0), &[(0.0, 0xf0f0f4), (1.0, 0xa8a8b4)]), &ink);
                c.paint(&poly(&right), &lin((16.0, 18.0), (31.0, 22.0), &[(0.0, 0x5a5a62), (1.0, 0x202024)]), &flat(0x202024));
                c.paint(&poly(&top), &rad((14.0, 15.0), 17.0, 6.0, &[(0.0, 0xffffff), (1.0, 0xcacad4)]), &ink);
                c.fill(&line(&[(19.0, 21.4), (28.5, 18.8)], 0.9), &flat(0x9a9aa4));
                if kind == DriveKind::Network {
                    let pipe = flat(0x2e9a2e);
                    c.fill(&line(&[(16.0, 24.0), (16.0, 29.5)], 2.2), &pipe);
                    c.fill(&line(&[(5.0, 29.5), (27.0, 29.5)], 2.2), &pipe);
                }
            }
        }
    }

    /// The 16-pixel drives: a flat box on whole pixels, its light top, the front with its dark
    /// window (a hard disk) or all dark (removable, CD), a slot on top.
    fn small_drive(c: &mut Canvas, kind: DriveKind) {
        let ink = flat(0x4e4e5c);
        let hard = matches!(kind, DriveKind::Fixed | DriveKind::System);
        if kind == DriveKind::Optical {
            disc(c, 14.0, 9.0, 8.0);
        }
        let top = if hard { 10.0 } else { 14.0 };
        c.paint(&poly(&[(6.0, top), (28.0, top), (30.0, 18.0), (2.0, 18.0)]), &lin((0.0, top), (0.0, 18.0), &[(0.0, 0xffffff), (1.0, 0xcfcfd8)]), &ink);
        c.paint(&rect(2.0, 18.0, 28.0, 8.0), &lin((0.0, 18.0), (0.0, 26.0), &[(0.0, 0xc8c8d0), (1.0, 0x8c8c98)]), &ink);
        if hard {
            c.fill(&rect(10.0, 12.0, 10.0, 2.0), &flat(0x1c1c22));
            c.fill(&rect(18.0, 20.0, 10.0, 4.0), &flat(0x26262c));
        } else if kind == DriveKind::Removable {
            // A USB stick or card reader: a long dark slot across the front and a blue tag on top,
            // so it reads apart from a hard disk even at 16 pixels.
            c.fill(&rect(5.0, 20.0, 22.0, 4.0), &flat(0x1c1c22));
            c.paint(&rect(9.0, 14.5, 10.0, 3.0), &lin((9.0, 0.0), (19.0, 0.0), &[(0.0, 0x8fb4e0), (1.0, 0x2f6fc0)]), &flat(0x2a5a9a));
        } else {
            c.fill(&rect(16.0, 20.0, 12.0, 4.0), &lin((16.0, 0.0), (28.0, 0.0), &[(0.0, 0x5a5a62), (1.0, 0x202024)]));
        }
        if kind == DriveKind::Network {
            let pipe = flat(0x2e9a2e);
            c.fill(&rect(14.0, 26.0, 4.0, 4.0), &pipe);
            c.fill(&rect(4.0, 28.0, 24.0, 4.0), &pipe);
        }
    }

    pub(super) fn disc(c: &mut Canvas, cx: f32, cy: f32, r: f32) {
        c.paint(&oval(cx, cy, r, r), &rad((cx - r * 0.3, cy - r * 0.3), r * 1.3, r * 1.3, &[(0.0, 0xffffff), (0.35, 0xd8f0ff), (0.55, 0xf8d8f0), (0.75, 0xd0e8c8), (1.0, 0xa8b0c8)]), &flat(0x7a7a8a));
        c.paint(&oval(cx, cy, r * 0.32, r * 0.32), &flat(0xe8e8ee), &flat(0x8a8a9a));
        c.fill(&oval(cx, cy, r * 0.14, r * 0.14), &flat(0xffffff));
    }

    // ------------------------------------------------ files

    /// A white page, its top right corner folded over in blue; returns its shape.
    fn page(c: &mut Canvas) -> Shape {
        let (pts, fold): (&[(f32, f32)], [(f32, f32); 3]) = if c.big() {
            (&[(5.0, 1.0), (21.0, 1.0), (27.0, 7.0), (27.0, 31.0), (5.0, 31.0)], [(21.0, 1.0), (21.0, 7.0), (27.0, 7.0)])
        } else {
            (&[(4.0, 0.0), (18.0, 0.0), (26.0, 8.0), (26.0, 32.0), (4.0, 32.0)], [(18.0, 0.0), (18.0, 8.0), (26.0, 8.0)])
        };
        let s = poly(pts);
        c.paint(
            &s,
            &lin((6.0, 2.0), (26.0, 30.0), &[(0.0, 0xffffff), (0.55, 0xeef4fe), (1.0, 0xc4d6f6)]),
            &lin((5.0, 1.0), (27.0, 31.0), &[(0.0, 0x9fb0d8), (1.0, 0x4a5688)]),
        );
        c.paint(&poly(&fold), &lin(fold[0], (fold[2].0 - 1.0, fold[2].1 - 1.0), &[(0.0, 0xd8ecff), (1.0, 0x5b93e6)]), &flat(0x485c8e));
        s
    }

    fn file(c: &mut Canvas, kind: FileKind) {
        let big = c.big();
        match kind {
            FileKind::Zip => {
                folder(c, 0.0);
                zipper(c, 9.0, 12.0, 26.0);
                return;
            }
            FileKind::Exe => {
                app_window(c);
                return;
            }
            FileKind::Text => {
                notepad(c);
                return;
            }
            _ => {}
        }
        page(c);
        match kind {
            FileKind::Generic => {
                // The program-less file: a little window of coloured tiles.
                let (x, y, w, h) = if big { (9.0, 12.0, 14.0, 12.0) } else { (7.0, 12.0, 14.0, 12.0) };
                c.paint(&rect(x, y, w, h), &flat(0xffffff), &flat(0x5a74b8));
                c.fill(&rect(x, y, w, 3.0), &lin((x, y), (x + w, y), &[(0.0, 0x3d95ff), (1.0, 0x0a5fe8)]));
                let tiles = [0x4fa0f0, 0xf08a3c, 0x5cbf4c, 0xf2c94c, 0xd65ab0, 0x5a74b8];
                for (i, t) in tiles.iter().enumerate() {
                    let (tx, ty) = (x + 1.5 + (i % 3) as f32 * 4.2, y + 4.5 + (i / 3) as f32 * 3.6);
                    c.fill(&rect(tx, ty, 2.8, 2.4), &flat(*t));
                }
            }
            FileKind::Image => {
                let (x, y, w, h) = if big { (8.0, 10.0, 16.0, 15.0) } else { (6.0, 10.0, 18.0, 16.0) };
                c.paint(&rect(x, y, w, h), &lin((x, y), (x, y + h * 0.6), &[(0.0, 0xf9d769), (1.0, 0xe8743a)]), &flat(0x7a5a3a));
                c.fill(&rect(x + c.hair(), y + h * 0.6, w - 2.0 * c.hair(), h * 0.4 - c.hair()), &lin((x, y + h * 0.6), (x, y + h), &[(0.0, 0x3a4f9a), (1.0, 0x1c2a5c)]));
                c.fill(&oval(x + w * 0.62, y + h * 0.6, w * 0.16, w * 0.16), &flat_a(0xfff2b0, 0.95));
                c.fill(&both(rect(x, y + h * 0.6, w, h * 0.4), rect(x + w * 0.5, y + h * 0.6, w * 0.24, h * 0.38)), &flat_a(0xffc860, 0.5));
                c.fill(&poly(&[(x + w * 0.28, y + h * 0.25), (x + w * 0.28, y + h * 0.66), (x + w * 0.12, y + h * 0.66)]), &flat(0x2a1a10));
            }
            FileKind::Audio => {
                if big {
                    c.paint(&rect(15.5, 8.0, 2.0, 14.0), &flat(0x5a4fb8), &flat(0x2f2580));
                    c.paint(&poly(&[(17.5, 8.0), (22.0, 11.0), (21.6, 15.5), (17.5, 12.5)]), &flat(0x4b3fae), &flat(0x2f2580));
                    note_head(c, 13.2, 22.5);
                } else {
                    c.fill(&rect(14.0, 8.0, 2.0, 14.0), &flat(0x4b3fae));
                    c.fill(&poly(&[(16.0, 8.0), (21.0, 11.0), (21.0, 15.0), (16.0, 12.0)]), &flat(0x4b3fae));
                    c.fill(&oval(11.5, 22.5, 4.4, 3.4), &lin((8.0, 20.0), (15.0, 25.0), &[(0.0, 0x9a92ea), (1.0, 0x3b2f9e)]));
                }
            }
            FileKind::Video => {
                let (x, y, w, h) = if big { (8.0, 10.0, 16.0, 16.0) } else { (6.0, 10.0, 18.0, 16.0) };
                c.paint(&rect(x, y, w, h), &flat(0x262626), &flat(0x000000));
                let m = w * 0.18;
                c.fill(&rect(x + m, y + 1.5, w - 2.0 * m, h / 2.0 - 2.0), &lin((x, y), (x, y + h / 2.0), &[(0.0, 0x8ec8ff), (1.0, 0x2c6fd6)]));
                c.fill(&rect(x + m, y + h / 2.0 + 0.5, w - 2.0 * m, h / 2.0 - 2.0), &lin((x, y + h / 2.0), (x, y + h), &[(0.0, 0x8ee07a), (1.0, 0x2f8a3a)]));
                if big {
                    for i in 0..5 {
                        let yy = y + 1.0 + i as f32 * 3.0;
                        c.fill(&rect(x + 0.8, yy, 1.4, 1.4), &flat(0xffffff));
                        c.fill(&rect(x + w - 2.2, yy, 1.4, 1.4), &flat(0xffffff));
                    }
                }
            }
            FileKind::Document | FileKind::Pdf => {
                if big {
                    for i in 0..6 {
                        c.fill(&rect(13.0, 10.0 + i as f32 * 2.6, if i == 5 { 7.0 } else { 11.0 }, 1.0), &flat(0xa8b6d8));
                    }
                }
                badge(c, kind == FileKind::Pdf);
            }
            _ => {}
        }
    }

    /// The square badge of an office document (blue) or a PDF (red), at the page's bottom left.
    fn badge(c: &mut Canvas, pdf: bool) {
        let (x, y, s) = if c.big() { (2.0, 15.0, 14.0) } else { (1.0, 14.0, 16.0) };
        let (hi, lo, ink) = if pdf { (0xf2584a, 0xb3160e, 0x8a0f0a) } else { (0x5a8ee6, 0x1c4fb8, 0x173e91) };
        c.paint(&round(x, y, s, s, 1.5), &lin((x, y), (x + s, y + s), &[(0.0, hi), (1.0, lo)]), &flat(ink));
        c.fill(&both(round(x, y, s, s, 1.5), rect(x, y, s, s * 0.45)), &flat_a(0xffffff, 0.25));
        let white = flat(0xffffff);
        if pdf {
            c.fill(&line(&[(x + s * 0.22, y + s * 0.75), (x + s * 0.42, y + s * 0.3), (x + s * 0.55, y + s * 0.62), (x + s * 0.8, y + s * 0.7)], s * 0.12), &white);
        } else {
            for i in 0..3 {
                c.fill(&rect(x + s * 0.2, y + s * (0.25 + i as f32 * 0.2), s * 0.6, s * 0.11), &white);
            }
        }
    }

    /// The zip folder's zipper down the front.
    fn zipper(c: &mut Canvas, x: f32, top: f32, bottom: f32) {
        let teeth = if c.big() { 1.6 } else { 2.0 };
        let mut y = top;
        let mut left = true;
        while y < bottom {
            c.fill(&rect(if left { x } else { x + teeth }, y, teeth, teeth * 0.75), &flat(0x7a7a84));
            left = !left;
            y += teeth * 0.75;
        }
        c.paint(&round(x - 0.6, bottom - 1.0, teeth * 2.0 + 1.2, 5.0, 1.0), &lin((x, bottom), (x + 3.0, bottom), &[(0.0, 0xf2f2f6), (1.0, 0x9a9aa6)]), &flat(0x4e4e5c));
    }

    /// Notepad's document: a pad with spiral rings along the top and lines of writing.
    fn notepad(c: &mut Canvas) {
        let big = c.big();
        let (x, y, w, h) = if big { (5.0, 3.0, 21.0, 28.0) } else { (4.0, 2.0, 22.0, 30.0) };
        let pad = rect(x, y, w, h);
        c.paint(&pad, &lin((x, y), (x + w, y + h), &[(0.0, 0xffffff), (0.6, 0xf2f5fd), (1.0, 0xccd8f2)]), &lin((x, y), (x + w, y + h), &[(0.0, 0x8e9cc8), (1.0, 0x3c4878)]));
        c.fill(&rect(x + c.hair(), y + c.hair(), w * 0.14, h - 2.0 * c.hair()), &flat(0xd8dcf4));
        let rings = if big { 5 } else { 4 };
        for i in 0..rings {
            let rx = x + 3.0 + i as f32 * (w - 6.0) / (rings - 1) as f32;
            c.fill(&line(&[(rx, y + 2.5), (rx, y - 1.5)], if big { 1.4 } else { 2.0 }), &lin((rx, y - 2.0), (rx, y + 3.0), &[(0.0, 0xffe48a), (1.0, 0xc8860c)]));
        }
        let lines = if big { 6 } else { 4 };
        for i in 0..lines {
            let ly = y + 7.0 + i as f32 * if big { 3.4 } else { 5.0 };
            let lw = if i % 3 == 2 { w * 0.4 } else { w * 0.65 };
            c.fill(&rect(x + w * 0.24, ly, lw, if big { 1.0 } else { 2.0 }), &flat(0x1c1c28));
        }
    }

    /// A program: its window, the title bar in XP's blue.
    fn app_window(c: &mut Canvas) {
        let (x, y, w, h) = if c.big() { (3.0, 5.0, 26.0, 22.0) } else { (2.0, 4.0, 28.0, 24.0) };
        let t = if c.big() { 5.0 } else { 6.0 };
        c.paint(&round(x, y, w, h, 1.5), &flat(0xffffff), &flat(0x1c3c9e));
        c.fill(&both(round(x, y, w, h, 1.5), rect(x, y, w, t)), &lin((x, y), (x, y + t), &[(0.0, 0x5aa8ff), (0.5, 0x0a5fe8), (1.0, 0x0848c0)]));
        c.fill(&rect(x + c.hair(), y + t, w * 0.28, h - t - c.hair()), &flat(0xece9d8));
        if c.big() {
            c.paint(&rect(x + w - 5.0, y + 1.2, 3.4, 2.8), &lin((0.0, y + 1.0), (0.0, y + 4.0), &[(0.0, 0xf08060), (1.0, 0xc8341c)]), &flat(0xffffff));
        }
    }
}

// ---------------------------------------------------------------- 7 (aero)

/// Windows 7's icons: the manila folder seen from the side, its front cover swung open on the
/// left, papers standing in it, a grey label tab with a blue sticker; the libraries' symbols on a
/// glass shelf; grey drives with a green light; light pages with a grey outline.
mod aero {
    use super::art::*;
    use super::{DriveKind, FileKind, Role};

    pub(super) fn paint(c: &mut Canvas, role: Role) {
        match role {
            Role::Folder | Role::FolderOpen => folder(c, true),
            Role::Documents | Role::Pictures | Role::Music | Role::Videos => library(c, role),
            Role::Downloads => {
                folder(c, false);
                down_arrow(c);
            }
            Role::Home | Role::Computer => computer(c),
            Role::Desktop => desktop(c),
            Role::Bin { full } => bin(c, full),
            Role::Drive(kind) => drive(c, kind),
            Role::File(kind) => file(c, kind),
        }
        if c.big() {
            c.shadow(0.6, 0.8, 1.0, 0.22);
        }
    }

    // ------------------------------------------------ folders

    /// The folder; `papers`: with papers standing in it (the plain folder).
    fn folder(c: &mut Canvas, papers: bool) {
        if !c.big() {
            small_folder(c);
            return;
        }
        // The soft shadow it casts to the right.
        c.fill(&oval_turned(22.0, 25.5, 6.5, 3.5, -35.0), &rad_a((22.0, 25.5), 6.5, 6.5, &[(0.0, 0x000000, 0.18), (1.0, 0x000000, 0.0)]));
        let back = [(5.0, 1.6), (9.4, 0.8), (10.8, 2.2), (21.4, 3.2), (21.4, 28.0), (5.0, 30.0)];
        c.paint(&poly(&back), &lin((0.0, 1.0), (0.0, 30.0), &[(0.0, 0xfbe9a0), (1.0, 0xe5bd52)]), &flat(0xcfa743));
        if papers {
            let sheets: [(f32, f32, u32); 3] = [(8.0, 4.4, 0xe9e9e9), (9.6, 5.6, 0xf2f2f2), (11.2, 6.8, 0xfdfdfd)];
            for (x, top, color) in sheets {
                let s = [(x, top), (20.4, top + 0.8), (20.4, 28.0), (x + 0.6, 28.8)];
                c.paint(&poly(&s), &lin((x, 0.0), (20.4, 0.0), &[(0.0, color), (1.0, 0xffffff)]), &flat(0xb4b4b4));
            }
            // What is written on the front sheet.
            for i in 0..6 {
                let y = 10.0 + i as f32 * 2.2;
                c.fill(&rect(13.0, y, if i % 3 == 2 { 4.0 } else { 6.0 }, 0.8), &flat(0xa8b4c4));
            }
            c.fill(&rect(13.0, 23.0, 3.0, 3.0), &lin((13.0, 23.0), (16.0, 26.0), &[(0.0, 0x7cc4f4), (1.0, 0x2a7ab8)]));
        }
        // The label tab on the right edge, its blue sticker.
        c.paint(&round(20.2, 16.5, 2.6, 11.0, 1.0), &lin((20.2, 0.0), (22.8, 0.0), &[(0.0, 0xfafafa), (1.0, 0xd2d2d2)]), &flat(0xa0a0a0));
        c.paint(&round(20.8, 23.4, 1.6, 3.0, 0.7), &lin((0.0, 23.4), (0.0, 26.4), &[(0.0, 0x8fd0f5), (1.0, 0x3c94d4)]), &flat(0x2f7fc0));
        let front = [(3.0, 1.0), (8.6, 4.6), (9.8, 30.6), (3.6, 27.6)];
        c.paint(&poly(&front), &lin((3.0, 0.0), (9.8, 0.0), &[(0.0, 0xfdf3bc), (0.55, 0xf3d673), (1.0, 0xe6bc4c)]), &flat(0xc69b33));
        c.fill(&line(&[(4.5, 3.2), (5.0, 26.5)], 0.9), &flat_a(0xffffff, 0.75));
    }

    /// The 16-pixel folder: front cover on the left, the fold, the back with its tab and sticker.
    fn small_folder(c: &mut Canvas) {
        c.fill(&rect(2.0, 0.0, 24.0, 30.0), &lin((0.0, 0.0), (0.0, 30.0), &[(0.0, 0xfaf3b4), (1.0, 0xdec14d)]));
        c.fill(&rect(26.0, 12.0, 2.0, 17.0), &lin((0.0, 12.0), (0.0, 29.0), &[(0.0, 0xe5daa5), (1.0, 0xd8c05d)]));
        c.fill(&rect(2.0, 0.0, 10.0, 30.0), &lin((0.0, 0.0), (0.0, 30.0), &[(0.0, 0xfbf7b4), (1.0, 0xe0ce76)]));
        c.fill(&rect(2.0, 0.0, 2.0, 30.0), &lin((0.0, 0.0), (0.0, 30.0), &[(0.0, 0xf5e78d), (1.0, 0xe1ce6b)]));
        c.fill(&rect(8.0, 4.0, 4.0, 10.0), &lin_a((0.0, 4.0), (0.0, 14.0), &[(0.0, 0xffffff, 1.0), (1.0, 0xffffff, 0.2)]));
        c.fill(&rect(12.0, 2.0, 2.0, 28.0), &lin((0.0, 2.0), (0.0, 30.0), &[(0.0, 0xe0cb68), (1.0, 0xcaaf43)]));
        c.fill(&rect(14.0, 2.0, 2.0, 28.0), &lin((0.0, 2.0), (0.0, 30.0), &[(0.0, 0xd5c67e), (1.0, 0xc4a844)]));
        c.fill(&rect(2.0, 0.0, 22.0, 2.0), &lin((2.0, 0.0), (24.0, 0.0), &[(0.0, 0xf4ebaf), (1.0, 0xe9d78a)]));
        c.fill(&rect(14.0, 2.0, 10.0, 2.0), &flat(0xfcf8c0));
        c.fill(&rect(24.0, 2.0, 2.0, 28.0), &lin((0.0, 2.0), (0.0, 30.0), &[(0.0, 0xe6d486), (1.0, 0xd8c05d)]));
        c.fill(&rect(24.0, 16.0, 2.0, 6.0), &flat(0xececec));
        c.fill(&rect(24.0, 22.0, 2.0, 4.0), &lin((0.0, 22.0), (0.0, 26.0), &[(0.0, 0x5cb9dd), (1.0, 0x48a5d3)]));
        c.fill(&rect(2.0, 28.0, 24.0, 2.0), &lin((2.0, 0.0), (26.0, 0.0), &[(0.0, 0xe3d58e), (0.2, 0xd8c25e), (1.0, 0xd6be5b)]));
        c.fill(&rect(4.0, 30.0, 12.0, 2.0), &flat_a(0x9a8a50, 0.18));
    }

    fn down_arrow(c: &mut Canvas) {
        let arrow: &[(f32, f32)] = if c.big() {
            &[(12.0, 9.0), (19.0, 9.0), (19.0, 18.0), (24.0, 18.0), (15.5, 29.0), (7.0, 18.0), (12.0, 18.0)]
        } else {
            &[(14.0, 8.0), (22.0, 8.0), (22.0, 18.0), (28.0, 18.0), (18.0, 30.0), (8.0, 18.0), (14.0, 18.0)]
        };
        c.paint(&poly(arrow), &lin((12.0, 8.0), (20.0, 30.0), &[(0.0, 0x6cc4f2), (0.55, 0x1696e4), (1.0, 0x0874c8)]), &flat(0x0b6cb8));
        if c.big() {
            c.fill(&line(&[(13.5, 10.5), (13.5, 18.5)], 0.9), &flat_a(0xffffff, 0.55));
        }
    }

    // ------------------------------------------------ libraries

    /// A library: its symbol (on the glass shelf from 24 pixels up).
    fn library(c: &mut Canvas, role: Role) {
        let big = c.big();
        if big {
            let ink = flat(0x3e8ac4);
            c.paint(&poly(&[(2.0, 25.5), (24.0, 25.5), (24.0, 28.5), (2.0, 28.5)]), &lin((0.0, 25.5), (0.0, 28.5), &[(0.0, 0x8ccaf0), (1.0, 0x5faee0)]), &ink);
            c.paint(&poly(&[(24.0, 25.5), (30.5, 21.0), (30.5, 24.0), (24.0, 28.5)]), &flat(0x4c9ad0), &ink);
            c.paint(&poly(&[(2.0, 25.5), (8.5, 21.0), (30.5, 21.0), (24.0, 25.5)]), &lin((8.0, 21.0), (24.0, 25.5), &[(0.0, 0xeef8fe), (1.0, 0xb4dcf4)]), &ink);
        }
        match role {
            Role::Documents => {
                let (pts, fold): (&[(f32, f32)], [(f32, f32); 3]) = if big {
                    (&[(8.0, 2.0), (19.0, 2.0), (24.0, 7.0), (24.0, 24.0), (8.0, 24.0)], [(19.0, 2.0), (19.0, 7.0), (24.0, 7.0)])
                } else {
                    (&[(4.0, 0.0), (20.0, 0.0), (28.0, 8.0), (28.0, 32.0), (4.0, 32.0)], [(20.0, 0.0), (20.0, 8.0), (28.0, 8.0)])
                };
                c.paint(&poly(pts), &lin((8.0, 2.0), (24.0, 24.0), &[(0.0, 0xffffff), (1.0, 0xeeeeee)]), &flat(0x8d8d8d));
                c.paint(&poly(&fold), &lin(fold[0], fold[2], &[(0.0, 0xffffff), (1.0, 0xd8e0e1)]), &flat(0x8d8d8d));
                let (x, w, y, step) = if big { (10.0, 10.0, 5.0, 2.2) } else { (8.0, 14.0, 4.0, 4.0) };
                c.fill(&rect(x, y, w * 0.8, step * 0.8), &lin((x, 0.0), (x + w, 0.0), &[(0.0, 0x68959c), (1.0, 0xa0c2c7)]));
                for i in 1..6 {
                    let ly = y + i as f32 * step + 1.0;
                    let lw = if i >= 3 { w * 0.45 } else { w };
                    c.fill(&rect(x, ly, lw, step * 0.4), &flat(0xb8b8b8));
                }
                let (px, py, ps) = if big { (15.5, 12.8, 6.0) } else { (17.0, 16.0, 9.0) };
                c.paint(&rect(px, py, ps, ps * 0.8), &lin((px, py), (px, py + ps), &[(0.0, 0x6dacfc), (0.6, 0x4a7aa8), (1.0, 0x2d4a61)]), &flat(0x8d8d8d));
            }
            Role::Music => {
                let (sx, top, bottom, hx, hy) = if big { (17.0, 2.0, 19.0, 13.0, 19.5) } else { (17.0, 1.0, 24.0, 12.0, 25.0) };
                let blue = lin((sx - 6.0, top), (sx + 6.0, bottom), &[(0.0, 0x5cc8fa), (1.0, 0x107bbc)]);
                c.fill(&rect(sx, top, 3.0, bottom - top), &blue);
                let flag = [(sx + 3.0, top), (sx + 9.0, top + 6.0), (sx + 8.0, top + 12.0), (sx + 3.0, top + 7.0)];
                c.fill(&poly(&flag), &blue);
                let head = oval_turned(hx, hy, 6.0, 4.6, -24.0);
                c.fill(&head, &rad((hx - 2.0, hy - 2.0), 8.0, 7.0, &[(0.0, 0x7ad4fc), (0.5, 0x2a9ae0), (1.0, 0x0670c0)]));
                c.fill(&both(oval_turned(hx, hy, 6.0, 4.6, -24.0), rect(0.0, 0.0, 32.0, hy - 1.0)), &flat_a(0xffffff, 0.35));
            }
            Role::Pictures => {
                let (x, y, w, h) = if big { (5.0, 5.0, 21.0, 17.0) } else { (2.0, 2.0, 28.0, 26.0) };
                let b = if big { 1.6 } else { 2.0 };
                c.paint(&rect(x, y, w, h), &lin((x, y), (x + w, y + h), &[(0.0, 0xfafafa), (1.0, 0xdcdcdc)]), &flat(0x989898));
                let (ix, iy, iw, ih) = (x + b, y + b, w - 2.0 * b, h - 2.0 * b);
                c.fill(&rect(ix, iy, iw, ih), &lin((ix, iy), (ix, iy + ih * 0.6), &[(0.0, 0x3485d1), (1.0, 0xc2fbff)]));
                c.fill(&poly(&[(ix, iy + ih * 0.55), (ix + iw * 0.35, iy + ih * 0.3), (ix + iw * 0.7, iy + ih * 0.6), (ix, iy + ih * 0.7)]), &flat(0x22522e));
                c.fill(&poly(&[(ix + iw * 0.3, iy + ih * 0.62), (ix + iw * 0.75, iy + ih * 0.4), (ix + iw, iy + ih * 0.55), (ix + iw, iy + ih * 0.7)]), &flat(0x3d7e53));
                c.fill(&rect(ix, iy + ih * 0.68, iw, ih * 0.32), &lin((ix, iy + ih * 0.68), (ix, iy + ih), &[(0.0, 0x256199), (1.0, 0xafcef5)]));
            }
            _ => {
                let (x, y, w, h) = if big { (8.0, 3.0, 16.0, 20.0) } else { (6.0, 0.0, 20.0, 32.0) };
                c.paint(&rect(x, y, w, h), &flat(0x434343), &flat(0x909090));
                let m = w * 0.2;
                let cells: [(f32, u32, u32); 2] = [(0.0, 0x9fd4f5, 0x3991c5), (0.5, 0xc0d890, 0x5a8a3a)];
                for (t, hi, lo) in cells {
                    let cy = y + 1.5 + t * (h - 1.0);
                    c.fill(&rect(x + m, cy, w - 2.0 * m, h / 2.0 - 2.5), &lin((x, cy), (x, cy + h / 2.0), &[(0.0, hi), (1.0, lo)]));
                }
                let holes = if big { 6 } else { 5 };
                for i in 0..holes {
                    let hy = y + 1.0 + i as f32 * (h - 2.0) / holes as f32;
                    c.fill(&rect(x + m * 0.25, hy, m * 0.5, m * 0.5), &flat(0xaaaaaa));
                    c.fill(&rect(x + w - m * 0.75, hy, m * 0.5, m * 0.5), &flat(0xaaaaaa));
                }
            }
        }
    }

    // ------------------------------------------------ places

    /// Computer: a black monitor with a blue screen, the tower behind it.
    fn computer(c: &mut Canvas) {
        let big = c.big();
        let (tx, tw) = if big { (2.0, 9.0) } else { (2.0, 10.0) };
        c.paint(&round(tx, 3.0, tw, 24.0, 1.0), &lin((tx, 0.0), (tx + tw, 0.0), &[(0.0, 0xe8ecf0), (1.0, 0xa0a8b0)]), &flat(0x5a6470));
        if big {
            c.fill(&rect(tx + 1.5, 7.0, tw - 3.0, 1.0), &flat(0x6e7884));
            c.fill(&oval(tx + tw / 2.0, 23.0, 0.9, 0.9), &flat(0x3bd23b));
        }
        let (mx, my, mw, mh) = if big { (8.0, 5.0, 23.0, 17.0) } else { (8.0, 4.0, 22.0, 18.0) };
        c.paint(&rect(mx + mw / 2.0 - 3.0, my + mh, 6.0, 4.0), &flat(0x2c3540), &flat(0x1c232a));
        c.paint(&rect(mx + mw / 2.0 - 7.0, my + mh + 3.0, 14.0, 2.5), &lin((0.0, my + mh + 3.0), (0.0, my + mh + 5.5), &[(0.0, 0x56606c), (1.0, 0x1e252e)]), &flat(0x1c232a));
        c.paint(&round(mx, my, mw, mh, 1.5), &lin((mx, my), (mx, my + mh), &[(0.0, 0x4a5866), (1.0, 0x1e252e)]), &flat(0x14191f));
        let b = if big { 1.6 } else { 2.0 };
        let screen = rect(mx + b, my + b, mw - 2.0 * b, mh - 2.0 * b);
        c.fill(&screen, &lin((mx, my), (mx + mw * 0.6, my + mh), &[(0.0, 0x6ec2fa), (0.5, 0x1e6fd8), (1.0, 0x0b4cb4)]));
        c.fill(&both(screen, poly(&[(mx, my), (mx + mw, my), (mx + mw, my + mh * 0.25), (mx, my + mh * 0.55)])), &flat_a(0xffffff, 0.25));
    }

    /// Desktop: the screen with its blue background and a black taskbar with the start button.
    fn desktop(c: &mut Canvas) {
        let big = c.big();
        let (x, y, w, h) = if big { (1.0, 4.0, 30.0, 24.0) } else { (0.0, 4.0, 32.0, 26.0) };
        c.paint(&rect(x, y, w, h), &lin((x, y), (x + w, y), &[(0.0, 0xa3b3c4), (0.5, 0xe3e5e8), (1.0, 0xb0c1d6)]), &flat(0x8a9aac));
        let b = if big { 1.5 } else { 2.0 };
        let (sx, sy, sw, sh) = (x + b, y + b, w - 2.0 * b, h - 2.0 * b);
        c.fill(&rect(sx, sy, sw, sh), &lin((sx, sy), (sx + sw, sy + sh * 0.8), &[(0.0, 0x2450a1), (0.5, 0x3472d6), (1.0, 0x67a8f6)]));
        c.fill(&rect(sx, sy, sw, c.hair()), &flat(0x083387));
        c.fill(&rect(sx, sy, c.hair(), sh), &flat(0x083387));
        let tb = sh * 0.24;
        c.fill(&rect(sx, sy + sh - tb, sw, tb), &lin((0.0, sy + sh - tb), (0.0, sy + sh), &[(0.0, 0x848585), (0.3, 0x353535), (1.0, 0x373737)]));
        let (ox, oy, r) = (sx + tb * 0.9, sy + sh - tb * 0.55, tb * 0.75);
        c.fill(&oval(ox, oy, r, r), &flat(0x216ec2));
        let q = r * 0.45;
        for (dx, dy, col) in [(-1.0, -1.0, 0xf73e0e), (1.0, -1.0, 0x76c631), (-1.0, 1.0, 0x18b2ff), (1.0, 1.0, 0xffda59)] {
            c.fill(&rect(ox + if dx < 0.0 { -q } else { 0.15 }, oy + if dy < 0.0 { -q } else { 0.15 }, q * 0.85, q * 0.85), &flat(col));
        }
    }

    /// The Recycle Bin: a bin of clear glass, the blue recycling triangle on it.
    fn bin(c: &mut Canvas, full: bool) {
        let ink = flat(0x7d93a8);
        if full {
            for (x, y, r) in [(11.0, 5.0, 4.5), (18.5, 3.5, 5.0), (23.0, 6.0, 3.5)] {
                c.paint(&oval(x, y, r, r * 0.85), &rad((x - 1.0, y - 1.0), r * 1.2, r, &[(0.0, 0xffffff), (1.0, 0xdedede)]), &flat(0xa8a8a8));
                c.fill(&line(&[(x - r * 0.5, y), (x, y + r * 0.3), (x + r * 0.4, y - r * 0.2)], 0.6), &flat(0xc8c8c8));
            }
        }
        let body = [(5.0, 7.0), (27.0, 7.0), (24.5, 29.5), (7.5, 29.5)];
        c.paint(&poly(&body), &lin((5.0, 0.0), (27.0, 0.0), &[(0.0, 0xbcd2e4), (0.35, 0xf2f8fc), (0.7, 0xdde8f2), (1.0, 0xa8bdd0)]), &ink);
        c.fill(&line(&[(9.0, 10.0), (10.2, 27.0)], 1.4), &flat_a(0xffffff, 0.8));
        c.paint(&oval(16.0, 7.0, 11.0, 2.6), &flat(0xe8f0f7), &ink);
        c.fill(&oval(16.0, 7.2, 9.4, 1.6), &lin((0.0, 5.5), (0.0, 9.0), &[(0.0, 0x8aa2b8), (1.0, 0xbfd0de)]));
        triangle(c, 16.0, 19.0, if c.big() { 6.0 } else { 7.0 });
    }

    /// The recycling triangle: three arrows chasing round (cx, cy).
    fn triangle(c: &mut Canvas, cx: f32, cy: f32, r: f32) {
        let w = r * 0.38;
        let corner = |i: usize| {
            let a = (-90.0 + 120.0 * i as f32).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        };
        for i in 0..3 {
            let (a, b) = (corner(i), corner(i + 1));
            let at = |t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            let fill = lin(a, b, &[(0.0, 0x5bc0f5), (1.0, 0x2c8dd8)]);
            c.fill(&line(&[at(0.12), at(0.6)], w), &fill);
            let (dx, dy) = ((b.0 - a.0) / r, (b.1 - a.1) / r);
            let base = at(0.6);
            let (nx, ny) = (-dy * w * 0.9, dx * w * 0.9);
            c.fill(&poly(&[(base.0 + nx, base.1 + ny), at(0.88), (base.0 - nx, base.1 - ny)]), &fill);
        }
    }

    // ------------------------------------------------ drives

    fn drive(c: &mut Canvas, kind: DriveKind) {
        let big = c.big();
        match kind {
            DriveKind::Optical => xp_disc(c, if big { 14.0 } else { 13.0 }, 10.0, if big { 8.5 } else { 9.0 }),
            DriveKind::System => {
                let (x, y, s) = if big { (3.0, 2.0, 6.0) } else { (2.0, 2.0, 7.0) };
                for (i, col) in [0xf65314u32, 0x7cbb00, 0x00a1f1, 0xffbb00].iter().enumerate() {
                    let (dx, dy) = ((i % 2) as f32 * (s + 1.0), (i / 2) as f32 * (s + 1.0));
                    let wave = if big { (i % 2) as f32 * -0.8 } else { 0.0 };
                    c.fill(&rect(x + dx, y + dy + wave, s, s), &lin((x + dx, y + dy), (x + dx + s, y + dy + s), &[(0.0, *col), (1.0, *col)]));
                }
            }
            _ => {}
        }
        // The drive: a grey box, its light front, a slot, the green light.
        let (x, y, w, h) = if big { (2.0, 16.0, 28.0, 12.0) } else { (2.0, 16.0, 28.0, 12.0) };
        c.paint(&round(x, y, w, h, 2.0), &lin((0.0, y), (0.0, y + h), &[(0.0, 0xf4f4f4), (0.45, 0xd9d9d9), (0.5, 0xc8c8c8), (1.0, 0xa9a9a9)]), &flat(0x6e6e6e));
        c.fill(&rect(x + 3.0, y + h * 0.62, w - 10.0, c.hair().max(1.0)), &flat(0x5a5a5a));
        c.fill(&oval(x + w - 4.0, y + h * 0.62, 2.4, 2.0), &flat_a(0x9bf29b, 0.7));
        c.fill(&oval(x + w - 4.0, y + h * 0.62, 1.3, 1.2), &flat(0x3bd23b));
        if kind == DriveKind::Removable {
            if big {
                c.paint(&rect(x + 3.0, y + 2.5, 7.0, 3.0), &lin((x, y), (x + 10.0, y), &[(0.0, 0x8fb4e0), (1.0, 0x2f6fc0)]), &flat(0x2a5a9a));
            } else {
                // At 16 pixels a wide blue band, so a USB stick reads apart from a hard disk.
                c.paint(&rect(x + 2.0, y + 2.0, w - 9.0, 5.0), &lin((x, y), (x + w, y), &[(0.0, 0x8fb4e0), (1.0, 0x2f6fc0)]), &flat(0x2a5a9a));
            }
        }
        if kind == DriveKind::Network {
            let pipe = lin((0.0, 28.0), (0.0, 31.0), &[(0.0, 0x3baa3b), (1.0, 0x167a16)]);
            c.fill(&rect(15.0, 28.0, 2.0, 2.5), &pipe);
            c.fill(&rect(4.0, 29.5, 24.0, 2.5), &pipe);
        }
    }

    fn xp_disc(c: &mut Canvas, cx: f32, cy: f32, r: f32) {
        super::xp::disc(c, cx, cy, r);
    }

    // ------------------------------------------------ files

    /// A white page with a grey outline, its corner folded.
    fn page(c: &mut Canvas) {
        let (pts, fold): (&[(f32, f32)], [(f32, f32); 3]) = if c.big() {
            (&[(6.0, 1.0), (20.0, 1.0), (26.0, 7.0), (26.0, 31.0), (6.0, 31.0)], [(20.0, 1.0), (20.0, 7.0), (26.0, 7.0)])
        } else {
            (&[(4.0, 0.0), (20.0, 0.0), (28.0, 8.0), (28.0, 32.0), (4.0, 32.0)], [(20.0, 0.0), (20.0, 8.0), (28.0, 8.0)])
        };
        let ink = flat(if c.big() { 0xa0a0a0 } else { 0x8d8d8d });
        c.paint(&poly(pts), &lin((6.0, 1.0), (26.0, 31.0), &[(0.0, 0xffffff), (1.0, 0xf0f0f0)]), &ink);
        c.paint(&poly(&fold), &lin(fold[0], fold[2], &[(0.0, 0xffffff), (1.0, 0xd8e0e1)]), &ink);
    }

    fn file(c: &mut Canvas, kind: FileKind) {
        let big = c.big();
        match kind {
            FileKind::Zip => {
                folder(c, false);
                zipper(c);
                return;
            }
            FileKind::Exe => {
                let (x, y, w, h) = if big { (3.0, 5.0, 26.0, 22.0) } else { (2.0, 4.0, 28.0, 24.0) };
                let t = if big { 5.0 } else { 6.0 };
                c.paint(&rect(x, y, w, h), &lin((x, y), (x, y + h), &[(0.0, 0xffffff), (1.0, 0xeef2f6)]), &flat(0x5a6e86));
                c.fill(&rect(x + c.hair(), y + c.hair(), w - 2.0 * c.hair(), t), &lin((x, y), (x, y + t), &[(0.0, 0x7aaee8), (0.5, 0x4b8fde), (1.0, 0x1e5db8)]));
                return;
            }
            _ => {}
        }
        page(c);
        let (x, w) = if big { (9.0, 14.0) } else { (8.0, 16.0) };
        match kind {
            FileKind::Text => {
                let (top, step) = if big { (9.0, 2.6) } else { (10.0, 4.0) };
                for i in 0..if big { 7 } else { 5 } {
                    let lw = if i % 4 == 3 { w * 0.55 } else { w };
                    c.fill(&rect(x, top + i as f32 * step, lw, if big { 1.0 } else { 2.0 }), &flat(0x9aa8b8));
                }
            }
            FileKind::Image => {
                let (y, h) = if big { (11.0, 13.0) } else { (12.0, 14.0) };
                c.paint(&rect(x, y, w, h), &lin((x, y), (x, y + h * 0.6), &[(0.0, 0x3485d1), (1.0, 0xc2fbff)]), &flat(0x989898));
                c.fill(&poly(&[(x, y + h * 0.75), (x + w * 0.4, y + h * 0.4), (x + w, y + h * 0.7), (x + w, y + h), (x, y + h)]), &flat(0x3d7e53));
                c.rim(&rect(x, y, w, h), &flat(0x989898));
            }
            FileKind::Audio => {
                let blue = lin((10.0, 8.0), (20.0, 26.0), &[(0.0, 0x5cc8fa), (1.0, 0x107bbc)]);
                c.fill(&rect(17.0, 9.0, 2.2, 14.0), &blue);
                c.fill(&poly(&[(19.2, 9.0), (23.5, 13.0), (23.0, 17.0), (19.2, 13.5)]), &blue);
                c.fill(&oval_turned(14.5, 23.5, 4.4, 3.3, -24.0), &rad((13.0, 22.0), 6.0, 5.0, &[(0.0, 0x7ad4fc), (1.0, 0x0670c0)]));
            }
            FileKind::Video => {
                let (y, h) = if big { (10.0, 16.0) } else { (10.0, 18.0) };
                c.paint(&rect(x, y, w, h), &flat(0x434343), &flat(0x909090));
                c.fill(&rect(x + 2.5, y + 1.5, w - 5.0, h / 2.0 - 2.0), &lin((x, y), (x, y + h / 2.0), &[(0.0, 0x9fd4f5), (1.0, 0x3991c5)]));
                c.fill(&rect(x + 2.5, y + h / 2.0 + 0.5, w - 5.0, h / 2.0 - 2.0), &lin((x, y + h / 2.0), (x, y + h), &[(0.0, 0xf5d79f), (1.0, 0xc5793a)]));
            }
            FileKind::Document | FileKind::Pdf => {
                if big {
                    for i in 0..6 {
                        c.fill(&rect(13.0, 10.0 + i as f32 * 2.6, if i == 5 { 6.0 } else { 10.0 }, 1.0), &flat(0xb8c4d0));
                    }
                }
                let (bx, by, s) = if big { (3.0, 15.0, 13.0) } else { (1.0, 14.0, 16.0) };
                let (hi, lo, ink) = if kind == FileKind::Pdf { (0xf0604e, 0xb81c10, 0x8a1208) } else { (0x6a9ef0, 0x1f5cc4, 0x1a4a9a) };
                c.paint(&round(bx, by, s, s, 1.5), &lin((bx, by), (bx, by + s), &[(0.0, hi), (1.0, lo)]), &flat(ink));
                c.fill(&both(round(bx, by, s, s, 1.5), rect(bx, by, s, s * 0.45)), &flat_a(0xffffff, 0.3));
                if kind == FileKind::Pdf {
                    c.fill(&line(&[(bx + s * 0.22, by + s * 0.75), (bx + s * 0.42, by + s * 0.3), (bx + s * 0.55, by + s * 0.62), (bx + s * 0.8, by + s * 0.7)], s * 0.12), &flat(0xffffff));
                } else {
                    for i in 0..3 {
                        c.fill(&rect(bx + s * 0.2, by + s * (0.25 + i as f32 * 0.2), s * 0.6, s * 0.11), &flat(0xffffff));
                    }
                }
            }
            _ => {}
        }
    }

    /// The zip folder's zipper, down the front cover.
    fn zipper(c: &mut Canvas) {
        let (x, top, bottom, t) = if c.big() { (5.2, 6.0, 26.0, 1.4) } else { (6.0, 2.0, 26.0, 2.0) };
        let mut y = top;
        let mut left = true;
        while y < bottom {
            c.fill(&rect(if left { x } else { x + t }, y, t, t * 0.75), &flat(if left { 0x5a5a5a } else { 0x8a8a8a }));
            left = !left;
            y += t * 0.75;
        }
        c.paint(&round(x - 0.5, bottom, t * 2.0 + 1.0, 4.0, 0.8), &lin((x, bottom), (x + 3.0, bottom), &[(0.0, 0xf0f0f0), (1.0, 0xa8a8a8)]), &flat(0x6e6e6e));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_get_the_icon_of_their_kind() {
        assert_eq!(file_kind("txt"), FileKind::Text);
        assert_eq!(file_kind("png"), FileKind::Image);
        assert_eq!(file_kind("zip"), FileKind::Zip);
        assert_eq!(file_kind("exe"), FileKind::Exe);
        assert_eq!(file_kind("bat"), FileKind::Exe);
        assert_eq!(file_kind("mp3"), FileKind::Audio);
        assert_eq!(file_kind("mkv"), FileKind::Video);
        assert_eq!(file_kind("docx"), FileKind::Document);
        assert_eq!(file_kind("pdf"), FileKind::Pdf);
        assert_eq!(file_kind("bin"), FileKind::Generic);
        assert_eq!(file_kind(""), FileKind::Generic);
    }

    #[test]
    fn places_and_drives_are_known() {
        assert_eq!(role_of_path(""), Role::Computer);
        assert_eq!(role_of_path(crate::recent::LOCATION), Role::Home);
        assert!(matches!(role_of_path(crate::recycle::LOCATION), Role::Bin { .. }));
        assert!(matches!(role_of_path("Q:\\"), Role::Drive(DriveKind::Fixed)));
        assert_eq!(role_of_path("Q:\\Some folder"), Role::Folder);
        assert_eq!(drive_kind("Removable Disk", "E:\\"), DriveKind::Removable);
        assert_eq!(drive_kind("CD Drive", "D:\\"), DriveKind::Optical);
        assert_eq!(drive_kind("Network Drive", "N:\\"), DriveKind::Network);
    }

    #[test]
    fn a_usb_stick_looks_apart_from_a_hard_disk_at_16_pixels() {
        for set in [IconSet::Xp, IconSet::Aero] {
            let (usb, disk) = (pixels(set, Role::Drive(DriveKind::Removable), 16), pixels(set, Role::Drive(DriveKind::Fixed), 16));
            let differ = usb.chunks(4).zip(disk.chunks(4)).filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 40)).count();
            assert!(differ >= 12, "{set:?}: only {differ} pixels differ");
        }
    }

    #[test]
    fn a_drive_root_shows_the_drive_it_is() {
        // Windows' drive types map to the same kinds as the sidebar's (folder.rs names).
        assert_eq!(drive_kind(drive_type_name(2), "E:"), DriveKind::Removable);
        assert_eq!(drive_kind(drive_type_name(4), "Z:"), DriveKind::Network);
        assert_eq!(drive_kind(drive_type_name(5), "D:"), DriveKind::Optical);
        assert_eq!(drive_kind(drive_type_name(3), "Q:"), DriveKind::Fixed);
        assert_eq!(drive_kind(drive_type_name(1), "Q:"), DriveKind::Fixed); // no such drive
        // The system drive keeps its own picture, with or without the backslash, any case.
        let sys = system_drive().to_uppercase();
        for p in [sys.clone(), format!("{sys}\\"), sys.to_lowercase()] {
            assert_eq!(role_of_path(&p), Role::Drive(DriveKind::System), "{p}");
        }
        // Not a drive root: no drive picture.
        assert_eq!(role_of_path("E:\\Photos 📷"), Role::Folder);
        assert_eq!(role_of_path("\\\\server\\share"), Role::Folder);
    }
}
