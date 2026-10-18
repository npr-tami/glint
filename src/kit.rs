//! A small kit of controls drawn by Glint, for Glint's own windows: the Settings window, the skin
//! customiser, the sort order editor, the update window and the question box. Everything is in the
//! skin's look (theme.rs): switches and buttons have the shape of SORT and SETTINGS (slanted in AMD
//! Radeon, square in NVIDIA, or rounded), and there is one accent colour.
//!
//! A skin with glass buttons (`buttons: liquid`, see [`glass`]) gets glass here too: glossy
//! capsules for buttons, switches with a silver knob, drop-downs like text boxes. A skin whose
//! sidebar is in panels (`sidebar-style: panels`) gets rounded selections in the page lists and
//! cards with a soft shadow. Every other skin looks exactly as it always did.
//!
//! - [`toggle`]: an on/off switch (a track with a knob, in the skin's shape).
//! - [`checkbox`]: a small box with a tick.
//! - [`Flips`]: switches and checkboxes the person flips glide between off and on (the knob slides,
//!   the colours fade), unless Windows' "Show animations" is off.
//! - [`dropdown`]: the closed box of a drop-down list; the window shows the choices as a native
//!   popup menu under it.
//! - [`button`] and [`primary_button`]: a word button, like SORT and the question box's CANCEL;
//!   a window's main one (UPDATE, the question box's yes) in the accent.
//! - [`field`]: the frame around a text box (the text box itself is a real Windows EDIT,
//!   textbox.rs), which also draws the text while the EDIT is hidden.
//! - [`card`], [`separator`], [`heading`], [`nav_item`]: cards with rows of settings, the small
//!   headings above them and the page list on the left.
//! - [`focus_ring`]: the thin outline round the control that has the keyboard.
//! - [`wrap`]: text broken into lines that fit a width.
//! - [`Scroller`], [`Bar`] and [`scrollbar`]: a scrolling page with a smooth wheel and a thin
//!   scrollbar, as thick as the skin makes them.
//! - [`title_bar`], [`logo`] and [`close_button`]: the title bar of a window of Glint's own.
//! - [`backdrop`]: a window's background, where the skin's is a gradient.
//!
//! The kit only draws and measures. The window using it keeps the state (what's under the mouse,
//! what has the keyboard, which switches are on their way) and passes it in as a [`Look`] and a
//! position between off and on. Sizes are DIPs, like everywhere.

use windows::core::BOOL;
use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};

use crate::details::blend;
use crate::pathbar::{fill_box, outline_box};
use crate::render::{Align, Renderer, TextStyle};
use crate::theme::{size as main_size, ButtonStyle, CaptionStyle, Color, Paint, SidebarStyle, Stop, Theme};
use crate::retroctl::{Held, Hold, Repeat};
use crate::ui::{Anim, Point, Rect};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::SetCapture;

/// The kit's sizes, in DIPs.
pub mod size {
    pub const TOGGLE_W: f32 = 40.0;
    pub const TOGGLE_H: f32 = 20.0;
    /// How far a switch leans (its top edge sits this far right of its bottom edge).
    pub const TOGGLE_LEAN: f32 = 6.0;
    /// A checkbox, and how far it leans (like the switches, in a slanted skin).
    pub const CHECK: f32 = 16.0;
    pub const CHECK_LEAN: f32 = 4.0;
    /// Drop-downs and text boxes.
    pub const FIELD_H: f32 = 30.0;
    pub const BUTTON_H: f32 = 28.0;
    /// Buttons lean like SORT and SETTINGS.
    pub const BUTTON_LEAN: f32 = 8.0;
    /// Text inside a drop-down starts this far in; the arrow takes this much on the right.
    pub const FIELD_PAD: f32 = 10.0;
    pub const ARROW_W: f32 = 28.0;
    /// The explanation under a setting's title.
    pub const NOTE_FONT: f32 = 11.0;
    pub const HEADING_FONT: f32 = 12.5;
    pub const PAGE_TITLE_FONT: f32 = 20.0;
    pub const NAV_FONT: f32 = 12.5;
    pub const NAV_ITEM_H: f32 = 36.0;
    /// The close button of a window's own title bar, and its marble (`caption-buttons: marbles`).
    pub const CLOSE_W: f32 = 46.0;
    pub const MARBLE: f32 = 16.0;
}

/// How a control looks right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    /// The mouse is over it.
    pub hover: bool,
    /// A button went down on it and is still down.
    pub pressed: bool,
    /// It has the keyboard and the outline should show (after a key was used, as in Windows).
    pub focused: bool,
    /// Greyed out (can't be used) when false.
    pub enabled: bool,
}

impl Look {
    /// An enabled control, not under the mouse.
    pub fn enabled() -> Look {
        Look { enabled: true, ..Look::default() }
    }
}

/// The colour for a control's parts: as is, or faded when it's greyed out.
fn faded(c: Color, look: Look) -> Color {
    if look.enabled {
        c
    } else {
        c.with_alpha(c.a * 0.38)
    }
}

/// The same for a paint.
fn faded_paint(p: Paint, look: Look) -> Paint {
    if look.enabled {
        p
    } else {
        p.with_strength(0.38)
    }
}

// ---------------------------------------------------------------- the skin's style

/// Whether the skin's buttons are glass (`buttons: liquid`): then the buttons here are glossy
/// capsules, the switches and checkboxes glass too, and drop-downs look like text boxes.
pub fn glass(theme: &Theme) -> bool {
    theme.buttons == ButtonStyle::Liquid
}

/// Whether the skin's sidebar is in panels (`sidebar-style: panels`): then the page lists mark the
/// chosen page with a rounded selection like the sidebar's, and cards have a soft shadow.
pub fn panels(theme: &Theme) -> bool {
    theme.sidebar_style == SidebarStyle::Panels
}

/// What a switch that's on, a ticked checkbox and the main button are filled with in a glass skin,
/// and the colour of what's on it: the sidebar's selection when it's solid (the strong colour of
/// such a skin), else the accent.
fn lit(theme: &Theme) -> (Paint, Color) {
    if theme.sidebar_selection.base().a > 0.9 {
        (theme.sidebar_selection, theme.sidebar_selection_text)
    } else {
        (theme.accent_paint, theme.on_accent)
    }
}

/// The edge of text boxes (and of drop-downs in a glass skin): the skin's `input-border`, or its
/// thin lines.
fn field_edge(theme: &Theme) -> Color {
    if theme.input_border.a > 0.0 {
        theme.input_border
    } else {
        theme.line
    }
}

/// A box with corners `radius` round (0: square) in one colour.
fn fill_rounded(r: &mut Renderer, rect: Rect, radius: f32, color: Color) {
    if radius > 0.0 {
        r.fill_round_rect(rect, radius, color);
    } else {
        r.fill_rect(rect, color);
    }
}

/// A 1-DIP edge just inside `rect`, its corners `radius` round.
fn edge(r: &mut Renderer, rect: Rect, radius: f32, color: Color) {
    if radius > 0.0 {
        r.stroke_round_rect(rect.inset(0.5, 0.5), (radius - 0.5).max(0.0), 1.0, color);
    } else {
        r.stroke_rect(rect, 1.0, color);
    }
}

/// The keyboard's outline round a capsule (a glass button or switch).
fn capsule_ring(r: &mut Renderer, theme: &Theme, rect: Rect) {
    focus_ring_round(r, theme, rect, rect.h / 2.0, 0.0);
}

/// A switch's knob in a glass skin: a small silver marble.
const SILVER: [Stop; 3] = [
    Stop { at: 0.0, color: Color::hex(0xffffff) },
    Stop { at: 0.45, color: Color::hex(0xdfe6f0) },
    Stop { at: 1.0, color: Color::hex(0x8d9db8) },
];

// ---------------------------------------------------------------- switch

/// Where a switch's knob sits: inside a track that leans `lean`, with an even margin all round,
/// the same slant.
fn knob(track: Rect, on: bool, lean: f32) -> (Rect, f32) {
    let h = track.h - 8.0;
    let k_lean = lean * h / track.h;
    let w = 14.0;
    // The track's slanted sides at the knob's top and bottom, 3 DIPs inside them.
    let x = if on { track.right() - lean * 4.0 / track.h - 3.0 - w } else { track.x + lean * 4.0 / track.h + 3.0 };
    (Rect::new(x, track.y + 4.0, w, h), k_lean)
}

/// An on/off switch in `rect` (TOGGLE_W x TOGGLE_H). On: filled with the accent, the knob on the
/// right. Off: an outline with the knob on the left. `on` is how far it is from off (0.0) to on
/// (1.0): while it's flipping ([`Flips`]) the knob is on its way across and the accent fades in or
/// out; `1.0` / `0.0` draw it on or off.
pub fn toggle(r: &mut Renderer, theme: &Theme, rect: Rect, on: f32, look: Look) {
    let t = on.clamp(0.0, 1.0);
    if theme.era.old() {
        // No Windows of the time had a switch: a check box at the switch's right end.
        return crate::retroctl::checkbox(r, theme, Rect::new(rect.right() - 16.0, rect.y, 16.0, rect.h), t, look);
    }
    if glass(theme) {
        return glass_toggle(r, theme, rect, t, look);
    }
    if theme.bevels {
        return bevel_toggle(r, theme, rect, t, look);
    }
    let lit = look.enabled && (look.hover || look.pressed);
    let lean = theme.lean(size::TOGGLE_LEAN);
    let (off_knob, k_lean) = knob(rect, false, lean);
    let (on_knob, _) = knob(rect, true, lean);
    let k = Rect::new(off_knob.x + (on_knob.x - off_knob.x) * t, off_knob.y, off_knob.w, off_knob.h);
    let fill = if look.pressed && look.enabled { theme.accent.with_alpha(0.75) } else if lit { theme.accent.with_alpha(0.88) } else { theme.accent };
    let line = if lit { theme.text } else { theme.muted };
    // The off look (an outline) turns into the on look (filled): the outline takes the accent's
    // colour while the fill fades in. Fully on or off, only that one look is drawn.
    if t < 1.0 {
        outline_box(r, theme, rect.inset(0.6, 0.6), lean, 1.2, faded(blend(line, fill, t), look));
    }
    if t > 0.0 {
        fill_box(r, theme, rect, lean, faded(fill.with_alpha(fill.a * t), look));
    }
    fill_box(r, theme, k, k_lean, faded(blend(line, theme.on_accent, t), look));
    if look.focused {
        focus_ring(r, theme, rect, lean);
    }
}

/// A switch in a glass skin: a capsule, sunk in the text boxes' colour when off and filled with
/// the selection's when on, and a silver marble for its knob, gliding across with `t`.
fn glass_toggle(r: &mut Renderer, theme: &Theme, rect: Rect, t: f32, look: Look) {
    let radius = rect.h / 2.0;
    let lit_now = look.enabled && (look.hover || look.pressed);
    let (on_paint, _) = lit(theme);
    if t < 1.0 {
        // The groove: a little darker along its top, as if pressed into the surface.
        let input = theme.input_paint;
        let groove = Paint::from_stops(&[
            Stop { at: 0.0, color: input.first().darker(0.16) },
            Stop { at: 0.45, color: input.color_at(0.45) },
            Stop { at: 1.0, color: input.last() },
        ]);
        r.fill_paint(rect, radius, &faded_paint(groove, look));
        let line = if lit_now { blend(field_edge(theme), theme.accent, 0.55) } else { field_edge(theme) };
        edge(r, rect, radius, faded(line, look));
    }
    if t > 0.0 {
        let fill = if lit_now { on_paint.lighter(0.1) } else { on_paint };
        r.fill_paint(rect, radius, &faded_paint(fill, look).with_strength(t));
        let rim = on_paint.last().darker(0.3);
        edge(r, rect, radius, faded(rim.with_alpha(rim.a * t), look));
        r.gloss(Rect::new(rect.x + radius * 0.6, rect.y + 1.0, (rect.w - radius * 1.2).max(0.0), rect.h * 0.42), rect.h * 0.21, 0.45 * t);
    }
    let k = radius - 2.0;
    let (from, to) = (rect.x + 2.0 + k, rect.right() - 2.0 - k);
    let silver = Paint::from_stops(&SILVER);
    let knob = match (look.enabled, look.pressed, lit_now) {
        (true, true, _) => silver.darker(0.1),
        (true, _, true) => silver.lighter(0.25),
        _ => silver,
    };
    r.marble(Point::new(from + (to - from) * t, rect.y + radius), k, &faded_paint(knob, look));
    if look.focused {
        capsule_ring(r, theme, rect);
    }
}

/// Whether `p` is on the switch (the slanted shape, a little bigger so it's easy to hit).
pub fn toggle_contains(rect: Rect, p: Point) -> bool {
    rect.inset(-4.0, -4.0).contains(p)
}

// ---------------------------------------------------------------- checkbox

/// A checkbox in `rect` (CHECK x CHECK): a small box leaning like the switches (straight or rounded
/// in skins without slants), in the accent with a tick when on. `on` is how far it is from off
/// (0.0) to on (1.0): while it's flipping ([`Flips`]) the fill and the tick fade in or out.
pub fn checkbox(r: &mut Renderer, theme: &Theme, rect: Rect, on: f32, look: Look) {
    let t = on.clamp(0.0, 1.0);
    if theme.era.old() {
        return crate::retroctl::checkbox(r, theme, rect, t, look);
    }
    if glass(theme) {
        return glass_checkbox(r, theme, rect, t, look);
    }
    if theme.bevels || old_windows(theme) {
        return box_checkbox(r, theme, rect, t, look);
    }
    let lit = look.enabled && (look.hover || look.pressed);
    let lean = theme.lean(size::CHECK_LEAN);
    let fill = if lit { theme.accent.with_alpha(0.88) } else { theme.accent };
    let line = if lit { theme.text } else { theme.muted };
    if t < 1.0 {
        outline_box(r, theme, rect.inset(0.6, 0.6), lean, 1.2, faded(blend(line, fill, t), look));
    }
    if t > 0.0 {
        fill_box(r, theme, rect, lean, faded(fill.with_alpha(fill.a * t), look));
        tick(r, rect, faded(theme.on_accent.with_alpha(theme.on_accent.a * t), look));
    }
    if look.focused {
        focus_ring(r, theme, rect, lean);
    }
}

/// The tick in a checkbox, drawn for a 16-DIP box and scaled to this one.
fn tick(r: &mut Renderer, rect: Rect, color: Color) {
    let s = rect.h / size::CHECK;
    let (x, y) = (rect.x + s, rect.y);
    let tick = [Point::new(x + 3.5 * s, y + 8.5 * s), Point::new(x + 6.5 * s, y + 11.5 * s), Point::new(x + 12.0 * s, y + 4.5 * s)];
    r.polyline(&tick, 1.8, color);
}

/// A checkbox in a glass skin: a small box in the text boxes' colour and edge, filled with the
/// selection's colour and a white tick when ticked.
fn glass_checkbox(r: &mut Renderer, theme: &Theme, rect: Rect, t: f32, look: Look) {
    let radius = theme.corner(rect.h).min(4.0);
    let lit_now = look.enabled && (look.hover || look.pressed);
    if t < 1.0 {
        r.fill_paint(rect, radius, &faded_paint(theme.input_paint, look));
        let line = if lit_now { blend(field_edge(theme), theme.accent, 0.55) } else { field_edge(theme) };
        edge(r, rect, radius, faded(line, look));
    }
    if t > 0.0 {
        let (paint, ink) = lit(theme);
        let fill = if lit_now { paint.lighter(0.1) } else { paint };
        r.fill_paint(rect, radius, &faded_paint(fill, look).with_strength(t));
        let rim = paint.last().darker(0.3);
        edge(r, rect, radius, faded(rim.with_alpha(rim.a * t), look));
        tick(r, rect, faded(ink.with_alpha(ink.a * t), look));
    }
    if look.focused {
        focus_ring_round(r, theme, rect, radius, 0.0);
    }
}

// ---------------------------------------------------------------- switches and checkboxes flipping

/// How long a switch's knob takes to slide across (while its track changes colour), in seconds.
pub const SWITCH_TIME: f64 = 0.15;
/// How long a checkbox's fill and tick take to fade in or out, in seconds.
pub const CHECK_TIME: f64 = 0.13;

/// Whether Windows' "Show animations in Windows" is on (Settings > Accessibility > Visual effects).
/// Off: switches and checkboxes change at once. It's a quick question, asked each time one is
/// flipped, so a change in Windows counts at once; when Windows doesn't answer, it's on.
pub fn animations_on() -> bool {
    let mut on = BOOL(1);
    // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer given, which lives here.
    let asked = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut BOOL as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
    asked.is_err() || on.as_bool()
}

/// The switches and checkboxes of one window that are on their way between off and on. Each is
/// known by a name the window gives it (a setting's name, say). Only a flip the person made moves
/// ([`Flips::start`]); a value changed in some other way (the settings read again, another skin
/// picked) is simply drawn where it is now. The window paints frame after frame only while
/// [`Flips::tick`] says something is moving, so an idle window still uses no CPU.
#[derive(Debug, Default)]
pub struct Flips {
    /// The controls on their way: their name, the value they're heading for, and where they are
    /// (0.0 off to 1.0 on, eased out like the scroll glides: quick at first, settling gently, never
    /// past the end).
    moving: Vec<(String, bool, Anim)>,
}

impl Flips {
    /// The person just flipped control `id` to `on` (a click, Space): it moves there from where it
    /// is drawn now, taking `time` seconds for the whole way (less when it was flipped back half
    /// way). At once when Windows' animations are off.
    pub fn start(&mut self, id: &str, on: bool, now: f64, time: f64) {
        self.slide(id, on, now, if animations_on() { time } else { 0.0 });
    }

    /// `start`, whatever Windows says (`time` 0: at once).
    fn slide(&mut self, id: &str, on: bool, now: f64, time: f64) {
        // Where it is drawn now: part of the way, when it's still moving; otherwise at the value
        // it had before this flip.
        let from = match self.moving.iter().position(|(k, ..)| k == id) {
            Some(i) => {
                let (_, _, mut anim) = self.moving.remove(i);
                anim.value(now).unwrap_or(anim.target())
            }
            None if on => 0.0,
            None => 1.0,
        };
        let to = if on { 1.0 } else { 0.0 };
        let way = (to - from).abs();
        if time > 0.0 && way > 0.01 {
            let mut anim = Anim::default();
            anim.go(from, to, now, time * f64::from(way));
            self.moving.push((id.to_string(), on, anim));
        }
    }

    /// Where control `id`, whose value is `on`, is drawn at `now`: 0.0 (off) to 1.0 (on). A control
    /// that isn't moving - or whose value isn't the one it was heading for any more, changed from
    /// elsewhere meanwhile - is simply at its value.
    pub fn at(&mut self, id: &str, on: bool, now: f64) -> f32 {
        let end = if on { 1.0 } else { 0.0 };
        match self.moving.iter_mut().find(|(k, ..)| k == id) {
            Some((_, to, anim)) if *to == on => anim.value(now).unwrap_or(end),
            _ => end,
        }
    }

    /// Moves them on to `now` and forgets the ones that arrived. True while any is still moving:
    /// paint again.
    pub fn tick(&mut self, now: f64) -> bool {
        self.moving.retain_mut(|(_, _, anim)| {
            anim.value(now);
            anim.running()
        });
        !self.moving.is_empty()
    }
}

// ---------------------------------------------------------------- drop-down

/// A small "⌄" drawn as two lines.
fn chevron_down(r: &mut Renderer, c: Point, half: f32, color: Color) {
    r.polyline(&[Point::new(c.x - half, c.y - half * 0.5), Point::new(c.x, c.y + half * 0.5), Point::new(c.x + half, c.y - half * 0.5)], 1.2, color);
}

/// The closed box of a drop-down list showing `text` (the current choice), with an arrow.
pub fn dropdown(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, look: Look) {
    if theme.era.old() {
        return crate::retroctl::combo(r, theme, rect, text, look);
    }
    if glass(theme) {
        return glass_dropdown(r, theme, rect, text, look);
    }
    if theme.bevels || old_windows(theme) {
        return combo(r, theme, rect, text, look);
    }
    let fill = match (look.enabled, look.pressed, look.hover) {
        (true, true, _) => theme.button_pressed,
        (true, _, true) => theme.chip_hover,
        _ => theme.chip,
    };
    fill_box(r, theme, rect, 0.0, faded(fill, look));
    let text_rect = Rect::new(rect.x + size::FIELD_PAD, rect.y, (rect.w - size::FIELD_PAD - size::ARROW_W).max(0.0), rect.h);
    r.text(text, text_rect, &TextStyle::new(faded(theme.text, look)));
    let c = Point::new(rect.right() - size::ARROW_W / 2.0, rect.y + rect.h / 2.0);
    chevron_down(r, c, 4.0, faded(if look.hover && look.enabled { theme.text } else { theme.muted }, look));
    if look.focused {
        focus_ring(r, theme, rect, 0.0);
    }
}

/// A drop-down in a glass skin: a box like a text box (the input colour, its edge, rounded as the
/// skin says) with a small filled arrow.
fn glass_dropdown(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, look: Look) {
    let radius = theme.corner(rect.h);
    let lit_now = look.enabled && (look.hover || look.pressed);
    let fill = if look.enabled && look.pressed { theme.input_paint.darker(0.05) } else { theme.input_paint };
    r.fill_paint(rect, radius, &faded_paint(fill, look));
    let line = if lit_now { blend(field_edge(theme), theme.accent, 0.55) } else { field_edge(theme) };
    edge(r, rect, radius, faded(line, look));
    let text_rect = Rect::new(rect.x + size::FIELD_PAD, rect.y, (rect.w - size::FIELD_PAD - size::ARROW_W).max(0.0), rect.h);
    r.text(text, text_rect, &TextStyle::new(faded(theme.text, look)));
    let c = Point::new(r.snap(rect.right() - size::ARROW_W / 2.0), r.snap(rect.y + rect.h / 2.0));
    let arrow = [Point::new(c.x - 4.0, c.y - 2.0), Point::new(c.x + 4.0, c.y - 2.0), Point::new(c.x, c.y + 3.0)];
    r.fill_polygon(&arrow, faded(if lit_now { theme.text } else { theme.muted }, look));
    if look.focused {
        focus_ring(r, theme, rect, 0.0);
    }
}

/// How wide a drop-down should be to show the longest of `labels`, between `min` and `max`.
pub fn dropdown_width<'a>(r: &mut Renderer, labels: impl IntoIterator<Item = &'a str>, min: f32, max: f32) -> f32 {
    let style = TextStyle::new(Color::hex(0));
    let longest = labels.into_iter().map(|l| r.measure(l, &style)).fold(0.0_f32, f32::max);
    (longest + size::FIELD_PAD + size::ARROW_W + 4.0).ceil().clamp(min, max)
}

// ---------------------------------------------------------------- button

/// The text style of a button's label.
fn button_style(color: Color) -> TextStyle {
    TextStyle::new(color).semibold().align(Align::Center)
}

/// A word button in the skin's shape (write the label in capitals, like SORT and CANCEL).
pub fn button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look) {
    if theme.era.old() {
        return crate::retroctl::push_button(r, theme, rect, &sentence_case(label), look, false);
    }
    if glass(theme) {
        return glass_button(r, theme, rect, label, look, false);
    }
    if theme.bevels || old_windows(theme) {
        return push_button(r, theme, rect, label, look, false);
    }
    let fill = match (look.enabled, look.pressed, look.hover) {
        (true, true, true) => theme.button_pressed,
        (true, _, true) => theme.chip_hover,
        _ => theme.chip,
    };
    let lean = theme.lean(size::BUTTON_LEAN);
    fill_box(r, theme, rect, lean, faded(fill, look));
    r.text(label, rect, &button_style(faded(theme.text, look)));
    if look.focused {
        focus_ring(r, theme, rect, lean);
    }
}

/// A window's main button (UPDATE, the question box's yes): filled with the accent, its words in
/// the colour that goes on it, in the skin's shape - in a glass skin a capsule in the selection's
/// colour.
pub fn primary_button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look) {
    if theme.era.old() {
        return crate::retroctl::push_button(r, theme, rect, &sentence_case(label), look, true);
    }
    if glass(theme) {
        return glass_button(r, theme, rect, label, look, true);
    }
    if theme.bevels || old_windows(theme) {
        return push_button(r, theme, rect, label, look, true);
    }
    let fill = match (look.enabled, look.pressed, look.hover) {
        (false, _, _) => theme.accent.with_alpha(0.38),
        (true, true, _) => theme.accent.with_alpha(0.75),
        (true, false, true) => theme.accent.with_alpha(0.88),
        _ => theme.accent,
    };
    let lean = theme.lean(size::BUTTON_LEAN);
    fill_box(r, theme, rect, lean, fill);
    let text = if look.enabled { theme.on_accent } else { theme.on_accent.with_alpha(0.7) };
    r.text(label, rect, &button_style(text));
    if look.focused {
        focus_ring(r, theme, rect, lean);
    }
}

/// A button in a glass skin: a glossy capsule (the skin's `button` glass, or the selection's for
/// the main button) with bold white words over a dark shadow. Lighter under the mouse, darker
/// while pressed, grey when it can't be used.
fn glass_button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look, primary: bool) {
    let (base, hover) = if primary {
        let p = lit(theme).0;
        (p, p.lighter(0.14))
    } else {
        (theme.chip_paint, theme.chip_hover_paint)
    };
    let paint = match (look.enabled, look.pressed, look.hover) {
        (false, _, _) => base.map(Color::grey).with_strength(0.55),
        (true, true, true) => base.darker(0.18),
        (true, _, true) => hover,
        _ => base,
    };
    r.liquid_capsule(rect, &paint);
    let style = TextStyle::new(theme.button_text).bold().align(Align::Center);
    if look.enabled {
        let shadow = paint.last().darker(0.6);
        r.text(label, Rect::new(rect.x, rect.y + 1.0, rect.w, rect.h), &style.color(shadow.with_alpha(0.8)));
        r.text(label, rect, &style);
    } else {
        r.text(label, rect, &style.color(theme.button_text.with_alpha(theme.button_text.a * 0.75)));
    }
    if look.focused {
        capsule_ring(r, theme, rect);
    }
}

/// How wide a button with `label` is.
pub fn button_width(r: &mut Renderer, label: &str) -> f32 {
    (r.measure(label, &button_style(Color::hex(0))) + 2.0 * size::BUTTON_LEAN + 20.0).ceil().max(72.0)
}

/// Whether `p` is on the button (its box: the slanted corners count too, so a button of any
/// shape is as easy to hit).
pub fn button_contains(rect: Rect, p: Point) -> bool {
    rect.contains(p)
}

// ---------------------------------------------------------------- text box frame

/// The frame of a text box: the input colour with a thin line round it (the skin's edge for text
/// boxes, or its thin lines), the accent while typing.
/// `text` is drawn when given: while the real text box (a Windows EDIT on top) is hidden -
/// scrolled partly out of view, say - the frame still shows what's in it.
pub fn field(r: &mut Renderer, theme: &Theme, rect: Rect, text: Option<&str>, typing: bool, look: Look) {
    r.fill_paint(rect, theme.corner(rect.h), &faded_paint(theme.input_paint, look));
    if theme.era.old() {
        crate::retrotool::field(r, theme, rect, typing);
    } else if theme.bevels {
        crate::pathbar::bevel(r, theme, rect, false, theme.window);
    } else {
        frame(r, theme, rect, faded(if typing { theme.accent } else { field_edge(theme) }, look));
    }
    if let Some(t) = text {
        r.text(t, Rect::new(rect.x + 8.0, rect.y, (rect.w - 16.0).max(0.0), rect.h), &TextStyle::new(faded(theme.text, look)));
    }
    if look.focused && !typing {
        focus_ring(r, theme, rect, 0.0);
    }
}

// ---------------------------------------------------------------- cards, headings, the page list

/// A card: a panel with a hairline frame, holding rows of settings (with a soft shadow under it
/// when the skin's sidebar is in panels).
pub fn card(r: &mut Renderer, theme: &Theme, rect: Rect) {
    if theme.era.old() {
        return crate::retroctl::group_box(r, theme, rect);
    }
    let radius = theme.corner(rect.h);
    if panels(theme) {
        fill_rounded(r, Rect::new(rect.x, rect.y + 1.0, rect.w, rect.h + 1.0), radius, Color::rgba(0, 0, 0, if theme.dark { 0.35 } else { 0.12 }));
    }
    r.fill_paint(rect, radius, &theme.panel_paint);
    if theme.bevels {
        r.stroke_rect(Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w - 1.0, rect.h - 1.0), 1.0, Color::hex(0xffffff));
        r.stroke_rect(Rect::new(rect.x, rect.y, rect.w - 1.0, rect.h - 1.0), 1.0, theme.window.darker(0.4));
        return;
    }
    frame(r, theme, rect, theme.line);
}

/// A hairline inside `rect`'s edges (round-cornered for a rounded skin).
fn frame(r: &mut Renderer, theme: &Theme, rect: Rect, color: Color) {
    let radius = theme.corner(rect.h);
    if radius > 0.0 {
        r.stroke_round_rect(rect.inset(0.5, 0.5), radius, 1.0, color);
    } else {
        r.stroke_rect(rect, 1.0, color);
    }
}

/// The thin line between two rows of a card: the skin's lighter line between column titles where
/// it has one, its thin lines otherwise.
pub fn separator(r: &mut Renderer, theme: &Theme, x: f32, y: f32, w: f32) {
    if theme.era.old() {
        // (A group box of the time had no lines between its rows.)
        return;
    }
    let color = if theme.column_line.a > 0.0 { theme.column_line } else { theme.line };
    r.fill_rect(Rect::new(x, y, w, 1.0), color);
}

/// 98 / 2000: a group box with its title sitting on its top line, the line broken behind the words
/// (`heading` is the title's row above the box `card`, as laid out for the other skins).
pub fn titled_group(r: &mut Renderer, theme: &Theme, card: Rect, heading: Rect, text: &str) {
    let top = r.snap(heading.y + heading.h / 2.0);
    let group = Rect::new(card.x, top, card.w, (card.bottom() - top).max(0.0));
    crate::retroctl::group_box(r, theme, group);
    let style = TextStyle::new(theme.text);
    let w = r.measure(text, &style).ceil();
    let x = r.snap(group.x + 8.0);
    r.fill_rect(Rect::new(x - 2.0, top - 7.0, w + 4.0, 14.0), theme.toolbar);
    r.text(text, Rect::new(x, top - 10.0, w + 2.0, 20.0), &style);
}

/// A small heading above a card ("Starting", "Layout"), in the colour of the sidebar's headings
/// when the sidebar is in panels (98 / 2000 put theirs on the box's line: `titled_group`).
pub fn heading(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str) {
    if theme.era.old() {
        let style = TextStyle::new(crate::retroctl::group_title(theme)).size(12.0);
        let style = if theme.era == crate::theme::Era::Win7 { style } else { style.bold() };
        return r.text(text, rect, &style);
    }
    let color = if panels(theme) { theme.panel_header_text } else { theme.text };
    r.text(text, rect, &TextStyle::new(color).size(size::HEADING_FONT).semibold());
}

/// One page in a window's page list: the chosen one is marked like a selected row in the file
/// list (the skin's edge and tint) - or, when the skin's sidebar is in panels, like the place
/// you're in there (a rounded selection inside the list).
pub fn nav_item(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, selected: bool, look: Look) {
    nav_entry(r, theme, rect, text, selected, look, false);
}

/// A page's name in a row of tabs along the top (a narrow Settings window): the name sits in the
/// middle of its tab, so every tab has the same space on both sides.
pub fn nav_tab(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, selected: bool, look: Look) {
    nav_entry(r, theme, rect, text, selected, look, true);
}

fn nav_entry(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, selected: bool, look: Look, middle: bool) {
    // 98 and 2000: a list box's line, inside the sunken well (`page_list_well`).
    let rect = if theme.era.classic() { Rect::new(rect.x + 9.0, rect.y, (rect.w - 18.0).max(0.0), rect.h) } else { rect };
    if panels(theme) {
        let pill = Rect::new(rect.x + 8.0, rect.y + 2.0, (rect.w - 16.0).max(0.0), (rect.h - 4.0).max(0.0));
        let radius = theme.corner(pill.h);
        if selected {
            r.fill_paint(pill, radius, &theme.sidebar_selection);
        } else if look.hover {
            r.fill_paint(pill, radius, &theme.hover_paint);
        }
        let color = if selected { theme.sidebar_selection_text } else { theme.sidebar_text };
        let style = TextStyle::new(color).size(size::NAV_FONT);
        let style = if selected { style.semibold() } else { style };
        if middle {
            r.text(text, pill, &style.align(Align::Center));
        } else {
            r.text(text, Rect::new(rect.x + 18.0, rect.y, (rect.w - 26.0).max(0.0), rect.h), &style);
        }
        if look.focused {
            focus_ring(r, theme, pill, 0.0);
        }
        return;
    }
    if selected {
        crate::pathbar::fill_row(r, theme, rect, &theme.accent_tint_paint, false);
        r.fill_rect(Rect::new(rect.x, rect.y, main_size::SELECT_EDGE, rect.h), theme.select_edge);
    } else if look.hover {
        crate::pathbar::fill_row(r, theme, rect, &theme.hover_paint, true);
    }
    let color = if selected { theme.selection_text } else { theme.sidebar_text };
    let style = TextStyle::new(color).size(size::NAV_FONT);
    let style = if selected { style.semibold() } else { style };
    if middle {
        r.text(text, rect, &style.align(Align::Center));
    } else {
        r.text(text, Rect::new(rect.x + 18.0, rect.y, (rect.w - 26.0).max(0.0), rect.h), &style);
    }
    if look.focused {
        focus_ring(r, theme, rect.inset(2.0, 2.0), 0.0);
    }
}

/// A page's title ("Appearance") at the top of a window's page: big and light in Glint's own
/// skins; an older Windows' bold heading in the dialog font, or 7's dark blue Control Panel title.
pub fn page_title(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str) {
    let style = match theme.era {
        crate::theme::Era::None => TextStyle::new(theme.text_strong).size(size::PAGE_TITLE_FONT).semibold(),
        crate::theme::Era::Win7 => TextStyle::new(Color::hex(0x1e395b)).size(17.0),
        _ => TextStyle::new(theme.text_strong).size(14.0).bold(),
    };
    r.text(text, rect, &style);
}

/// Behind a window's page list (the column `column`, already in the sidebar's paint): an older
/// Windows' list box (2000: a sunken white well on the grey) or task pane panel (XP: the pale panel
/// on the blue, so its blue words read).
pub fn page_list_well(r: &mut Renderer, theme: &Theme, column: Rect) {
    match theme.era {
        crate::theme::Era::Win98 | crate::theme::Era::Win2000 => {
            r.fill_rect(column, theme.toolbar);
            let well = column.inset(7.0, 7.0);
            r.fill_rect(well, theme.panel);
            crate::retro::edge(r, theme, well, crate::retro::Edge::Sunken, theme.toolbar);
        }
        crate::theme::Era::Xp if !theme.sidebar_panel.is_clear() => {
            let panel = column.inset(6.0, 8.0);
            r.fill_paint(panel, 3.0, &theme.sidebar_panel);
            r.stroke_round_rect(panel.inset(0.5, 0.5), 3.0, 1.0, Color::hex(0xffffff));
        }
        _ => {}
    }
}

/// "CANCEL" -> "Cancel" (an older Windows never wrote its buttons in capitals); a label already in
/// mixed case stays as it is.
pub fn sentence_case(label: &str) -> String {
    if label.chars().any(|ch| ch.is_lowercase()) {
        return label.to_string();
    }
    let lower = label.to_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |f| f.to_uppercase().chain(chars).collect())
}

/// How thick the outline round the control that has the keyboard is.
const FOCUS_W: f32 = 1.5;

/// The thin outline round the control that has the keyboard, right against it and in its shape
/// (`lean`: how far it leans in this skin, `Theme::lean`; else the skin's round corners).
pub fn focus_ring(r: &mut Renderer, theme: &Theme, rect: Rect, lean: f32) {
    focus_ring_round(r, theme, rect, if lean > 0.0 { 0.0 } else { theme.corner(rect.h) }, lean);
}

/// The same round a control with its own corners (`radius`).
pub fn focus_ring_round(r: &mut Renderer, theme: &Theme, rect: Rect, radius: f32, lean: f32) {
    crate::pathbar::ring_around(r, rect, lean, radius, FOCUS_W, theme.text.with_alpha(0.65));
}

// ---------------------------------------------------------------- text

/// `text` broken into lines no wider than `width`, at spaces (and at line breaks in it). A word
/// longer than a whole line (a long path) is broken too, preferably after a "\".
pub fn wrap(r: &mut Renderer, text: &str, style: &TextStyle, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split(' ').filter(|w| !w.is_empty()) {
            let longer = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if r.measure(&longer, style) <= width {
                line = longer;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            // The word alone: if even that is too wide, cut it into pieces that fit.
            let mut rest = word.to_string();
            while r.measure(&rest, style) > width && rest.chars().count() > 1 {
                let cut = longest_fitting(r, &rest, style, width);
                let piece: String = rest.chars().take(cut).collect();
                rest = rest.chars().skip(cut).collect();
                lines.push(piece);
            }
            line = rest;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(String::is_empty) && lines.len() > 1 {
        lines.pop();
    }
    lines
}

/// How many characters of `word` fit in `width` (at least one), ending after a "\" or "/" when one
/// is in the second half of what fits.
fn longest_fitting(r: &mut Renderer, word: &str, style: &TextStyle, width: f32) -> usize {
    let chars: Vec<char> = word.chars().collect();
    let (mut lo, mut hi) = (1, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let piece: String = chars[..mid].iter().collect();
        if r.measure(&piece, style) <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    match chars[..lo].iter().rposition(|&c| c == '\\' || c == '/') {
        Some(i) if i + 1 >= lo / 2 && i + 1 < chars.len() => i + 1,
        _ => lo,
    }
}

// ---------------------------------------------------------------- scrolling

/// How long a wheel scroll glides, in seconds (like the file list).
const SCROLL_TIME: f64 = 0.14;

/// How far a click on a scroll bar's arrow button moves a page: one line.
pub const ARROW_STEP: f32 = 24.0;

/// A page that scrolls: where it is, where it's gliding to, and how long it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scroller {
    /// How far down the page is scrolled now, in DIPs.
    pub offset: f32,
    anim: Anim,
    content: f32,
    view: f32,
}

impl Scroller {
    /// The page is `content` DIPs long and `view` DIPs of it show. Keeps the offset in range.
    pub fn set_extent(&mut self, content: f32, view: f32) {
        self.content = content.max(0.0);
        self.view = view.max(0.0);
        let max = self.max();
        if self.offset > max || self.anim.target() > max {
            self.anim.stop();
            self.offset = self.offset.min(max);
        }
    }

    /// The farthest it can scroll.
    pub fn max(&self) -> f32 {
        (self.content - self.view).max(0.0)
    }

    /// Where it is heading (where it is, when not gliding).
    pub fn target(&self) -> f32 {
        if self.anim.running() {
            self.anim.target()
        } else {
            self.offset
        }
    }

    /// Glide to `to` (kept in range). Returns whether anything moves.
    pub fn scroll_to(&mut self, to: f32, now: f64) -> bool {
        let to = to.clamp(0.0, self.max());
        if (to - self.target()).abs() < 0.01 {
            return false;
        }
        self.anim.go(self.offset, to, now, SCROLL_TIME);
        true
    }

    /// Glide by `delta` from where it is heading.
    pub fn scroll_by(&mut self, delta: f32, now: f64) -> bool {
        self.scroll_to(self.target() + delta, now)
    }

    /// Go to `to` at once (dragging the scrollbar, a new page).
    pub fn jump_to(&mut self, to: f32) {
        self.anim.stop();
        self.offset = to.clamp(0.0, self.max());
    }

    /// Glide so that `top..bottom` (page coordinates) shows, with `margin` round it.
    pub fn reveal(&mut self, top: f32, bottom: f32, margin: f32, now: f64) -> bool {
        let target = self.target();
        if top - margin < target {
            self.scroll_to(top - margin, now)
        } else if bottom + margin > target + self.view {
            self.scroll_to((bottom + margin - self.view).min(top - margin), now)
        } else {
            false
        }
    }

    /// Moves the glide on to `now`. Returns true while it's still moving.
    pub fn tick(&mut self, now: f64) -> bool {
        if let Some(v) = self.anim.value(now) {
            self.offset = v;
        }
        self.anim.running()
    }

    /// The scrollbar's thumb in `track`, if the page is longer than the view.
    pub fn thumb(&self, track: Rect) -> Option<Rect> {
        self.thumb_at(track, self.offset)
    }

    /// Where the thumb is once the page is at `offset`.
    pub fn thumb_at(&self, track: Rect, offset: f32) -> Option<Rect> {
        let max = self.max();
        if max <= 0.0 || self.content <= 0.0 || track.h <= 0.0 {
            return None;
        }
        let h = (track.h * self.view / self.content).clamp(24.0_f32.min(track.h), track.h);
        let y = track.y + (track.h - h) * (offset / max).clamp(0.0, 1.0);
        Some(Rect::new(track.x, y, track.w, h))
    }

    /// How far a click on the track moves the page: what shows, less a line.
    pub fn page(&self) -> f32 {
        (self.view - ARROW_STEP).max(ARROW_STEP)
    }

    /// Which ways it can still scroll (from where it is: the scroll bar's arrows grey out at the
    /// ends).
    pub fn room(&self) -> crate::retroctl::Room {
        crate::retroctl::Room::of(self.offset, self.max())
    }

    /// The offset that puts the thumb's top at `thumb_top` (dragging it).
    pub fn offset_for_thumb(&self, track: Rect, thumb_h: f32, thumb_top: f32) -> f32 {
        let room = (track.h - thumb_h).max(1.0);
        ((thumb_top - track.y) / room).clamp(0.0, 1.0) * self.max()
    }
}

/// How thick a scrollbar's thumb is and the room on each side of it, in the skin
/// (`Theme::scrollbar_w`, `Theme::scrollbar_margin`). A window keeps it from its last skin, for
/// finding the thumb under the mouse.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub w: f32,
    pub margin: f32,
    /// How long the arrow buttons at each end are (an older Windows' scroll bars; 0 otherwise).
    pub arrows: f32,
}

impl Default for Bar {
    fn default() -> Bar {
        Bar { w: main_size::SCROLLBAR_W, margin: main_size::SCROLLBAR_MARGIN, arrows: 0.0 }
    }
}

impl Bar {
    pub fn of(theme: &Theme) -> Bar {
        Bar { w: theme.scrollbar_w, margin: theme.scrollbar_margin(), arrows: crate::retroctl::arrows(theme) }
    }

    /// Where the thumb runs: along the right edge of `area`, inside the margin (and between the
    /// arrow buttons).
    pub fn track(self, area: Rect) -> Rect {
        let along = self.margin + self.arrows;
        Rect::new(area.right() - self.w - self.margin, area.y + along, self.w, (area.h - 2.0 * along).max(0.0))
    }

    /// The scroll bar along `area`'s right edge.
    pub fn place(self, area: Rect) -> BarPlace {
        let lane_w = self.w + 2.0 * self.margin;
        BarPlace { lane: Rect::new(area.right() - lane_w, area.y, lane_w, area.h), track: self.track(area), arrows: self.arrows }
    }
}

/// Where a scroll bar is: its whole lane (an older Windows' arrow buttons at the two ends), the
/// track its thumb runs in, and how long the arrow buttons are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarPlace {
    pub lane: Rect,
    pub track: Rect,
    pub arrows: f32,
}

impl BarPlace {
    /// What a press at `p` holds on an older Windows' bar (while `s` scrolls): an arrow button, or
    /// the track before or after the thumb.
    pub fn hold_at(self, s: &Scroller, p: Point) -> Hold {
        if self.arrows <= 0.0 || s.max() <= 0.0 || !self.lane.contains(p) {
            return Hold::None;
        }
        if let Some(held) = crate::retroctl::arrow_at(self.lane, self.arrows, p) {
            return Hold::Arrow(held);
        }
        match s.thumb(self.track) {
            Some(t) if p.y < t.y => Hold::Page(Held::Start),
            Some(t) if p.y >= t.bottom() => Hold::Page(Held::End),
            _ => Hold::None,
        }
    }

    /// One step of `hold` on `s`: a line for an arrow; a page for the track, while the thumb hasn't
    /// reached the mouse (`at`). Whether it moved.
    pub fn step(self, s: &mut Scroller, hold: Hold, at: Point, now: f64) -> bool {
        match hold {
            Hold::Arrow(way) => s.scroll_by(way.sign() * ARROW_STEP, now),
            Hold::Page(way) => {
                let on = s.thumb_at(self.track, s.target()).is_some_and(|t| crate::retroctl::page_on(self.lane, t, way, at));
                on && s.scroll_by(way.sign() * s.page(), now)
            }
            Hold::None => false,
        }
    }
}

/// The timer that repeats a held scroll bar arrow or track in the Settings, skin and update
/// windows (none of them uses 9 for anything else).
pub const REPEAT_TIMER: usize = 9;

/// A window's held scroll bar arrow or track (an older Windows' bars), repeating on REPEAT_TIMER.
#[derive(Clone, Copy, Debug, Default)]
pub struct Holding {
    pub repeat: Repeat,
    /// Which of the window's bars (the skin window has two).
    pub bar: usize,
    /// Where the mouse is: a held track pages until the thumb gets there.
    pub at: Point,
}

impl Holding {
    /// A press at `p`: when it is on bar `which` (at `place`, scrolling `s`), the first step is
    /// taken, the mouse captured and the timer set. Whether it was.
    #[allow(clippy::too_many_arguments)]
    pub fn press(&mut self, hwnd: HWND, place: BarPlace, s: &mut Scroller, which: usize, p: Point, now: f64) -> bool {
        let hold = place.hold_at(s, p);
        if hold == Hold::None {
            return false;
        }
        self.bar = which;
        self.at = p;
        crate::retroctl::repeat_timer(hwnd, REPEAT_TIMER, Some(self.repeat.press(hold, now)));
        place.step(s, hold, p, now);
        // SAFETY: capture on our own window, so the button coming up anywhere ends the hold.
        unsafe {
            SetCapture(hwnd);
        }
        true
    }

    /// The timer fired: the next step on the held bar (at `place`, scrolling `s`). Whether it moved.
    pub fn fire(&mut self, hwnd: HWND, place: BarPlace, s: &mut Scroller, now: f64) -> bool {
        let room = crate::retroctl::Room::of(s.target(), s.max()).toward(self.repeat.hold.way());
        let (hold, steps, next) = self.repeat.fire(room, now);
        crate::retroctl::repeat_timer(hwnd, REPEAT_TIMER, next);
        let mut moved = false;
        for _ in 0..steps {
            moved |= place.step(s, hold, self.at, now);
        }
        moved
    }

    /// The button came up: whether something was held (it shows released; the timer stops).
    pub fn release(&mut self, hwnd: HWND) -> bool {
        let was = self.repeat.release();
        if was {
            crate::retroctl::repeat_timer(hwnd, REPEAT_TIMER, None);
        }
        was
    }

    /// Whether something is held.
    pub fn active(&self) -> bool {
        self.repeat.hold != Hold::None
    }

    /// What bar `which` holds.
    pub fn hold_of(&self, which: usize) -> Hold {
        if self.bar == which {
            self.repeat.hold
        } else {
            Hold::None
        }
    }
}

/// The thin scrollbar thumb in `track` (nothing when the page fits), in the skin's paint: with its
/// track behind it, an edge and grip lines where the skin has them. An older Windows' bar shows
/// what is held (`hold`) pressed.
pub fn scrollbar(r: &mut Renderer, theme: &Theme, track: Rect, s: &Scroller, hover: bool, hold: Hold) {
    let Some(thumb) = s.thumb(track) else { return };
    if theme.era.old() {
        let (m, a) = (theme.scrollbar_margin(), crate::retroctl::arrows(theme));
        let lane = track.inset(-m, -m - a);
        let look = crate::retroctl::BarLook { hot: hover || hold != Hold::None, dragged: false, hold, room: s.room() };
        return crate::retroctl::scrollbar(r, theme, lane, thumb, look);
    }
    if !theme.scrollbar_track.is_clear() {
        let m = theme.scrollbar_margin();
        let lane = track.inset(-m, -m);
        r.fill_paint(lane, lane.w / 2.0, &theme.scrollbar_track);
    }
    let radius = if theme.bevels { 0.0 } else { thumb.w.min(thumb.h) / 2.0 };
    r.fill_paint(thumb, radius, if hover { &theme.scroll_thumb_hover_paint } else { &theme.scroll_thumb_paint });
    if theme.bevels {
        crate::pathbar::bevel(r, theme, thumb, true, theme.scroll_thumb);
    } else if theme.scrollbar_border.a > 0.0 {
        edge(r, thumb, radius, theme.scrollbar_border);
    }
    if theme.scrollbar_grip && thumb.h >= 24.0 {
        let w = (thumb.w * 0.5).round().max(2.0);
        let x = r.snap(thumb.x + (thumb.w - w) / 2.0);
        let mid = r.snap(thumb.y + thumb.h / 2.0);
        for dy in [-3.0, 0.0, 3.0] {
            r.fill_rect(Rect::new(x, mid + dy, w, 1.0), theme.scrollbar_grip_lines);
        }
    }
}

// ---------------------------------------------------------------- old Windows looks

/// Whether the skin is one of the XP / Windows 7 looks: flat buttons with an edge round them under
/// the mouse (`tool-border`). Their controls are drawn as Windows drew them then.
pub fn old_windows(theme: &Theme) -> bool {
    theme.tool_border.a > 0.0 && !glass(theme)
}

/// A push button as Windows drew it: in Windows 2000 (`bevels`) a grey raised box that sinks
/// while pressed, the main one with a black frame; in XP and 7 a rounded box of the skin's
/// `button` paint with a dark edge, an orange (or blue) glow inside it under the mouse.
pub fn push_button(r: &mut Renderer, theme: &Theme, rect: Rect, label: &str, look: Look, primary: bool) {
    let down = look.enabled && look.pressed && look.hover;
    let ink = faded(theme.button_text, look);
    let text_rect = if down { Rect::new(rect.x + 1.0, rect.y + 1.0, rect.w, rect.h) } else { rect };
    let style = TextStyle::new(ink).align(Align::Center);
    if theme.bevels {
        r.fill_paint(rect, 0.0, &theme.chip_paint);
        let face = if primary {
            r.stroke_rect(rect, 1.0, Color::hex(0x000000));
            rect.inset(1.0, 1.0)
        } else {
            rect
        };
        crate::pathbar::bevel(r, theme, face, !down, theme.chip);
        r.text(label, text_rect, &style);
        if look.focused {
            dotted_focus(r, rect.inset(4.0, 4.0), theme.button_text);
        }
        return;
    }
    let paint = if down {
        theme.chip_paint.darker(0.08)
    } else if look.hover && look.enabled {
        theme.chip_hover_paint
    } else {
        theme.chip_paint
    };
    r.fill_paint(rect, 3.0, &faded_paint(paint, look));
    if look.enabled && (look.hover || primary) {
        let glow = if look.hover && theme.tab_line.a > 0.0 { theme.tab_line } else { theme.accent.with_alpha(0.6) };
        r.stroke_round_rect(rect.inset(1.5, 1.5), 2.0, 2.0, glow);
    }
    r.stroke_round_rect(rect.inset(0.5, 0.5), 3.0, 1.0, faded(theme.input_border.darker(0.35), look));
    r.text(label, text_rect, &style);
    if look.focused {
        dotted_focus(r, rect.inset(4.0, 4.0), theme.button_text);
    }
}

/// The keyboard's dotted outline inside a classic button.
fn dotted_focus(r: &mut Renderer, rect: Rect, color: Color) {
    let c = color.with_alpha(0.6);
    let mut x = rect.x;
    while x < rect.right() {
        r.fill_rect(Rect::new(x, rect.y, 1.0, 1.0), c);
        r.fill_rect(Rect::new(x, rect.bottom() - 1.0, 1.0, 1.0), c);
        x += 2.0;
    }
    let mut y = rect.y;
    while y < rect.bottom() {
        r.fill_rect(Rect::new(rect.x, y, 1.0, 1.0), c);
        r.fill_rect(Rect::new(rect.right() - 1.0, y, 1.0, 1.0), c);
        y += 2.0;
    }
}

/// A drop-down as Windows drew it: a white box (sunken in Windows 2000, a thin edge in XP and 7)
/// with a button for its arrow on the right - raised grey, or the skin's glossy button.
fn combo(r: &mut Renderer, theme: &Theme, rect: Rect, text: &str, look: Look) {
    let down = look.enabled && look.pressed;
    r.fill_paint(rect, 0.0, &faded_paint(theme.input_paint, look));
    let arrow_w = 17.0;
    let (inner, btn) = if theme.bevels {
        crate::pathbar::bevel(r, theme, rect, false, theme.window);
        (rect.inset(2.0, 2.0), Rect::new(rect.right() - 2.0 - arrow_w, rect.y + 2.0, arrow_w, rect.h - 4.0))
    } else {
        r.stroke_rect(rect, 1.0, faded(field_edge(theme), look));
        (rect.inset(1.0, 1.0), Rect::new(rect.right() - 1.0 - arrow_w, rect.y + 1.0, arrow_w, rect.h - 2.0))
    };
    let text_rect = Rect::new(inner.x + 5.0, inner.y, (btn.x - inner.x - 6.0).max(0.0), inner.h);
    r.text(text, text_rect, &TextStyle::new(faded(theme.text, look)));
    if theme.bevels {
        r.fill_paint(btn, 0.0, &theme.chip_paint);
        crate::pathbar::bevel(r, theme, btn, !down, theme.chip);
    } else {
        let paint = if look.hover && look.enabled { theme.chip_hover_paint } else { theme.chip_paint };
        let face = btn.inset(1.0, 1.0);
        r.fill_paint(face, 2.0, &paint);
        r.stroke_round_rect(face.inset(0.5, 0.5), 2.0, 1.0, theme.input_border);
    }
    let d = if down { 1.0 } else { 0.0 };
    let c = Point::new(r.snap(btn.x + btn.w / 2.0) + d, r.snap(btn.y + btn.h / 2.0) + d);
    let arrow = [Point::new(c.x - 4.0, c.y - 2.0), Point::new(c.x + 4.0, c.y - 2.0), Point::new(c.x, c.y + 2.0)];
    r.fill_polygon(&arrow, faded(theme.button_text, look));
    if look.focused {
        dotted_focus(r, text_rect.inset(-2.0, 3.0), theme.text);
    }
}

/// A checkbox as Windows drew it: a white box, sunken (Windows 2000) or with a thin edge (XP and
/// 7), and a tick in the text colour (green in XP, the accent in 7).
fn box_checkbox(r: &mut Renderer, theme: &Theme, rect: Rect, t: f32, look: Look) {
    let b = Rect::new(r.snap(rect.x + (rect.w - 13.0) / 2.0), r.snap(rect.y + (rect.h - 13.0) / 2.0), 13.0, 13.0);
    let lit_now = look.enabled && look.hover;
    r.fill_paint(b, 0.0, &faded_paint(theme.input_paint, look));
    let ink = if theme.bevels {
        crate::pathbar::bevel(r, theme, b, false, theme.window);
        theme.text
    } else {
        if lit_now {
            r.stroke_rect(b.inset(1.0, 1.0), 1.0, theme.tab_line.with_alpha(0.8));
        }
        r.stroke_rect(b, 1.0, faded(theme.input_border.darker(0.3), look));
        theme.accent
    };
    if t > 0.0 {
        let p = [Point::new(b.x + 3.0, b.y + 6.5), Point::new(b.x + 5.5, b.y + 9.0), Point::new(b.x + 10.0, b.y + 4.0)];
        r.polyline(&p, 2.0, faded(ink.with_alpha(ink.a * t), look));
    }
    if look.focused {
        dotted_focus(r, b.inset(-2.0, -2.0), theme.text);
    }
}

/// An on/off switch in Windows 2000: a sunken slot (white when off, the accent when on) with a
/// raised grey knob sliding across.
fn bevel_toggle(r: &mut Renderer, theme: &Theme, rect: Rect, t: f32, look: Look) {
    let slot = Rect::new(r.snap(rect.x), r.snap(rect.y), r.snap(rect.w), r.snap(rect.h));
    r.fill_rect(slot, faded(blend(theme.input, theme.accent, t), look));
    crate::pathbar::bevel(r, theme, slot, false, theme.window);
    let kw = (slot.h - 2.0).round();
    let x = slot.x + 2.0 + (slot.w - 4.0 - kw) * t;
    let knob = Rect::new(r.snap(x), slot.y + 2.0, kw - 2.0, slot.h - 4.0);
    r.fill_paint(knob, 0.0, &theme.chip_paint);
    crate::pathbar::bevel(r, theme, knob, true, theme.chip);
    if look.focused {
        dotted_focus(r, slot.inset(-3.0, -3.0), theme.text);
    }
}

/// The sign on a caption button.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Sign {
    Close,
    Maximize,
    Minimize,
}

/// Minimise, maximise or close in the old Windows looks, drawn in `b` (the button's whole spot):
/// XP and 7's rounded glass tiles (`caption-buttons: tiles`) or Windows 2000's raised grey boxes
/// (`bevels`). Returns false, drawing nothing, for the other skins.
pub fn caption_face(r: &mut Renderer, theme: &Theme, b: Rect, sign: Sign, hot: bool, down: bool, active: bool) -> bool {
    if theme.era.old() {
        // As the era drew it, where it put it (a window's lone close button here).
        let state = crate::retro::State { hot, down, active };
        let (slot, sign) = match sign {
            Sign::Close => (crate::retro::close_slot(theme.era, b), crate::retro::Sign::Close),
            Sign::Maximize => (b.center(25.0, 22.0), crate::retro::Sign::Maximize),
            Sign::Minimize => (b.center(25.0, 22.0), crate::retro::Sign::Minimize),
        };
        crate::retro::caption_button(r, theme, slot, sign, state);
        return true;
    }
    if theme.caption_buttons == CaptionStyle::Tiles {
        let (w, h): (f32, f32) = if sign == Sign::Close { (25.0, 21.0) } else { (21.0, 21.0) };
        let (w, h) = (w.min(b.w - 2.0), h.min(b.h - 4.0));
        let face = Rect::new(r.snap(b.x + (b.w - w) / 2.0), r.snap(b.y + (b.h - h) / 2.0), w, h);
        let glass = if sign == Sign::Close { theme.marble_close } else { theme.marble };
        let glass = if down {
            glass.darker(0.15)
        } else if hot {
            glass.lighter(0.22)
        } else if active {
            glass
        } else {
            glass.map(|c| c.mix(c.grey(), 0.4))
        };
        r.fill_paint(face, 3.0, &glass);
        let shine = Rect::new(face.x + 1.0, face.y + 1.0, face.w - 2.0, (face.h * 0.45).round());
        r.fill_paint(shine, 2.5, &Paint::even(&[Color::rgba(255, 255, 255, 0.55), Color::rgba(255, 255, 255, 0.12)]));
        r.stroke_round_rect(face.inset(0.5, 0.5), 2.5, 1.0, Color::rgba(255, 255, 255, 0.9));
        let ink = Color::hex(0xffffff);
        let c = Point::new(face.x + face.w / 2.0, face.y + face.h / 2.0);
        match sign {
            Sign::Close => cross(r, c, 4.0, 2.0, ink),
            Sign::Maximize => {
                let g = Rect::new(c.x - 5.0, c.y - 5.0, 10.0, 10.0);
                r.stroke_rect(g.inset(0.5, 0.5), 1.0, ink);
                r.fill_rect(Rect::new(g.x, g.y, g.w, 3.0), ink);
            }
            Sign::Minimize => r.fill_rect(Rect::new(c.x - 5.0, c.y + 2.0, 7.0, 3.0), ink),
        }
        return true;
    }
    if theme.bevels {
        let face = Rect::new(b.x + (b.w - 16.0) / 2.0, b.y + (b.h - 14.0) / 2.0, 16.0, 14.0);
        let face = if sign == Sign::Minimize { Rect::new(face.x + 2.0, face.y, face.w, face.h) } else { face };
        r.fill_paint(face, 0.0, &theme.chip_paint);
        crate::pathbar::bevel(r, theme, face, !down, theme.chip);
        let glyph = if active { theme.button_text } else { theme.button_text.with_alpha(0.6) };
        let d = if down { 1.0 } else { 0.0 };
        let c = Point::new(r.snap(face.x + face.w / 2.0) + d, r.snap(face.y + face.h / 2.0) + d);
        match sign {
            Sign::Close => cross(r, c, 3.0, 1.5, glyph),
            Sign::Maximize => {
                let g = Rect::new(c.x - 4.0, c.y - 4.0, 8.0, 7.0);
                r.stroke_rect(g, 1.0, glyph);
                r.fill_rect(Rect::new(g.x, g.y, g.w, 2.0), glyph);
            }
            Sign::Minimize => r.fill_rect(Rect::new(c.x - 3.0, c.y + 2.0, 6.0, 2.0), glyph),
        }
        return true;
    }
    false
}

/// An "×" centred on `c`.
fn cross(r: &mut Renderer, c: Point, half: f32, width: f32, color: Color) {
    r.line(Point::new(c.x - half, c.y - half), Point::new(c.x + half, c.y + half), width, color);
    r.line(Point::new(c.x + half, c.y - half), Point::new(c.x - half, c.y + half), width, color);
}

// ---------------------------------------------------------------- a window's own title bar

/// The band of a window's own title bar `bar`: the skin's title bar paint - and where the skin
/// has a shine along the top of its bars, that thin light line and an edge along the bottom. With
/// an era, as that Windows drew it (`active`: whether the window is the active one).
pub fn title_bar(r: &mut Renderer, theme: &Theme, bar: Rect, active: bool) {
    if theme.era.old() {
        crate::retro::title_band(r, theme, bar, active);
        return;
    }
    if theme.title_across {
        r.fill_paint_across(bar, &theme.titlebar_paint);
    } else {
        r.fill_paint(bar, 0.0, &theme.titlebar_paint);
    }
    if theme.title_gloss.a > 0.0 {
        let top = Rect::new(bar.x, bar.y, bar.w, (bar.h * 0.48).round());
        r.fill_paint(top, 0.0, &Paint::even(&[theme.title_gloss, theme.title_gloss.with_alpha(theme.title_gloss.a * 0.45)]));
    }
    if theme.highlight.a > 0.0 {
        r.fill_rect(Rect::new(bar.x, bar.y, bar.w, 1.0), theme.highlight);
        r.fill_rect(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), theme.toolbar_line);
    }
}

/// A window's background over `area`, where the skin's is a gradient (a flat one is what the
/// window was cleared with).
pub fn backdrop(r: &mut Renderer, theme: &Theme, area: Rect) {
    if theme.window_paint.gradient().is_some() {
        r.fill_paint(area, 0.0, &theme.window_paint);
    }
}

/// The logo at the left of a title bar `rect`, as in the main window (logo.rs): the chrome
/// wordmark, or the skin's own word in a box. Returns where the logo's room ends.
pub fn logo(r: &mut Renderer, theme: &Theme, rect: Rect) -> f32 {
    crate::logo::draw(r, theme, rect)
}

/// A title bar's close button: an "×" that lights up under the mouse (red in AMD Radeon), as in
/// the main window - or, when the skin's window buttons are marbles, a glass marble in the middle
/// of the same spot: lighter under the mouse with a small "×" on it, darker while pressed, grey
/// while the window isn't the active one.
pub fn close_button(r: &mut Renderer, theme: &Theme, rect: Rect, hover: bool, pressed: bool, active: bool) {
    if caption_face(r, theme, rect, Sign::Close, hover, pressed, active) {
        return;
    }
    if theme.caption_buttons == CaptionStyle::Marbles {
        let c = Point::new(r.snap(rect.x + rect.w / 2.0), r.snap(rect.y + rect.h / 2.0));
        let paint = match (hover, pressed, active) {
            (true, true, _) => theme.marble_close.darker(0.15),
            (true, false, _) => theme.marble_close.lighter(0.12),
            (false, _, true) => theme.marble_close,
            (false, _, false) => theme.marble_close.map(Color::grey),
        };
        r.marble(c, size::MARBLE / 2.0, &paint);
        if hover {
            let half = 2.5;
            let ink = theme.marble_close.last().darker(0.45).with_alpha(0.85);
            r.line(Point::new(c.x - half, c.y - half), Point::new(c.x + half, c.y + half), 1.2, ink);
            r.line(Point::new(c.x + half, c.y - half), Point::new(c.x - half, c.y + half), 1.2, ink);
        }
        return;
    }
    let mut glyph = if active { theme.muted } else { theme.muted.with_alpha(0.6) };
    if hover {
        r.fill_rect(rect, if pressed { theme.close_hover.with_alpha(theme.close_hover.a * 0.75) } else { theme.close_hover });
        glyph = theme.on_accent;
    }
    let g = rect.center(main_size::CAPTION_GLYPH, main_size::CAPTION_GLYPH);
    let (gx, gy) = (r.snap(g.x), r.snap(g.y));
    let c = Point::new(gx + g.w / 2.0, gy + g.h / 2.0);
    let half = g.w / 2.0 - 0.5;
    r.line(Point::new(c.x - half, c.y - half), Point::new(c.x + half, c.y + half), 1.0, glyph);
    r.line(Point::new(c.x + half, c.y - half), Point::new(c.x - half, c.y + half), 1.0, glyph);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scroller_stays_in_range_and_reveals() {
        let mut s = Scroller::default();
        s.set_extent(1000.0, 400.0);
        assert_eq!(s.max(), 600.0);
        s.jump_to(5000.0);
        assert_eq!(s.offset, 600.0);
        s.jump_to(-3.0);
        assert_eq!(s.offset, 0.0);
        // Something below the view glides into it, with the margin under it.
        assert!(s.reveal(500.0, 540.0, 10.0, 0.0));
        assert_eq!(s.target(), 150.0);
        assert!(!s.tick(10.0), "the glide is over after its time");
        assert_eq!(s.offset, 150.0);
        // Something above glides into view too; something in view doesn't move it.
        assert!(s.reveal(100.0, 120.0, 10.0, 20.0));
        assert_eq!(s.target(), 90.0);
        s.tick(30.0);
        assert!(!s.reveal(200.0, 240.0, 10.0, 40.0));
        // A shorter page brings the offset back.
        s.set_extent(300.0, 400.0);
        assert_eq!((s.max(), s.offset), (0.0, 0.0));
        assert!(s.thumb(Rect::new(0.0, 0.0, 6.0, 400.0)).is_none(), "no scrollbar when it fits");
    }

    #[test]
    fn the_thumb_follows_the_offset() {
        let mut s = Scroller::default();
        s.set_extent(2000.0, 500.0);
        let track = Rect::new(0.0, 10.0, 6.0, 500.0);
        let top = s.thumb(track).unwrap();
        assert_eq!((top.y, top.h), (10.0, 125.0));
        s.jump_to(1500.0);
        let bottom = s.thumb(track).unwrap();
        assert_eq!(bottom.bottom(), track.bottom());
        assert_eq!(s.offset_for_thumb(track, bottom.h, track.y + (track.h - bottom.h) / 2.0), 750.0);
    }

    #[test]
    fn knobs_sit_inside_the_switch() {
        let track = Rect::new(10.0, 10.0, size::TOGGLE_W, size::TOGGLE_H);
        let (off, _) = knob(track, false, size::TOGGLE_LEAN);
        let (on, _) = knob(track, true, size::TOGGLE_LEAN);
        assert!(off.x > track.x && on.right() < track.right() && on.x > off.right());
        assert_eq!((off.y, off.h), (14.0, 12.0));
    }

    #[test]
    fn a_flip_eases_out_and_settles() {
        let mut f = Flips::default();
        // Never flipped: drawn at its value.
        assert_eq!((f.at("a", true, 0.0), f.at("a", false, 0.0)), (1.0, 0.0));
        f.slide("a", true, 10.0, SWITCH_TIME);
        assert_eq!(f.at("a", true, 10.0), 0.0, "it starts where it was");
        // Quick at first, then settling: always ahead of a steady pace, never back, never past on.
        let mut last = 0.0;
        for step in 1..15 {
            let part = step as f64 / 15.0;
            let at = f.at("a", true, 10.0 + SWITCH_TIME * part);
            assert!(at >= last && at <= 1.0, "no step back and no overshoot ({at} after {last})");
            assert!(at as f64 >= part, "eased out: ahead of a steady pace at {part}");
            last = at;
        }
        assert!(f.tick(10.0 + SWITCH_TIME * 0.9), "still moving: keep painting");
        assert!(!f.tick(10.0 + SWITCH_TIME + 0.001), "arrived: nothing to paint any more");
        assert_eq!(f.at("a", true, 11.0), 1.0);
        assert!(f.moving.is_empty(), "nothing is kept once it's there");
    }

    #[test]
    fn a_flip_back_turns_round_where_it_is() {
        let mut f = Flips::default();
        f.slide("a", true, 0.0, SWITCH_TIME);
        let half = SWITCH_TIME / 2.0;
        let there = f.at("a", true, half);
        assert!(there > 0.5 && there < 1.0);
        // Flipped back half way: it heads off from where it was drawn, for the part of the time
        // that's left of the way.
        f.slide("a", false, half, SWITCH_TIME);
        assert_eq!(f.at("a", false, half), there);
        assert!(f.at("a", false, half + 0.01) < there);
        assert!(!f.tick(half + SWITCH_TIME * f64::from(there) + 0.001));
        assert_eq!(f.at("a", false, 1.0), 0.0);
    }

    #[test]
    fn flips_snap_when_they_should() {
        let mut f = Flips::default();
        // A value changed elsewhere meanwhile (the settings read again) is drawn as it is.
        f.slide("a", true, 0.0, SWITCH_TIME);
        assert_eq!(f.at("a", false, 0.05), 0.0);
        // Another control isn't touched by this one's flip.
        assert_eq!(f.at("b", true, 0.05), 1.0);
        // No time (Windows' animations are off): there at once, and nothing keeps painting.
        let mut g = Flips::default();
        g.slide("c", true, 0.0, 0.0);
        assert!(!g.tick(0.0));
        assert_eq!(g.at("c", true, 0.0), 1.0);
    }
}
