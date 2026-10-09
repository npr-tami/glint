//! The sidebar on the left: "Quick access" (Home, Desktop, Documents, Downloads, Pictures, Music,
//! Videos and the Recycle Bin), "Pinned" (folders you pinned) and "This PC" (the drives, each with a
//! thin red bar showing how full it is).
//!
//! What it does: hover highlights; the place you're in is marked (or the closest place that holds
//! it: Documents while you're in Documents\Projects, like Explorer); click goes there, middle-click
//! opens it in a new tab, right-click gives a menu; clicking the "This PC" heading shows all
//! drives; smooth wheel scrolling and a thin scrollbar when it doesn't fit.
//!
//! Quick access places can be taken out (right-click > Remove from Quick access) and brought back
//! (right-click > Show in Quick access ›). Pinned folders (Ctrl+D, "Pin to sidebar", or dropped on
//! "Pinned") can be renamed - only the name shown here, in a name box over the row - moved up or
//! down and unpinned. Both are kept in the settings (app/places.rs).
//!
//! Icons: each place's real Windows icon is looked up on a background thread - asking a sleeping
//! DVD drive or a lost network drive for its icon can take seconds, and the window must never wait
//! for that. The icons go into the shared icon cache when they arrive (so This PC in the file list
//! gets the drive icons for free); until then a folder shows the plain folder icon and a drive a
//! small drawn one. Pinned folders show the plain folder icon (a pin on a sleeping server must not
//! hold up the others), and the Recycle Bin its own, full or empty.
//!
//! The drive list itself (names, sizes) comes from `app.drives`, which app.rs reads in the
//! background at start, on refresh (F5) and when a drive comes or goes (WM_DEVICECHANGE).

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};

use crate::app::{can_pin, AppState};
use crate::folder::Drive;
use crate::icons::{self, IconId, Icons, LoadedIcon};
use crate::recycle;
use crate::render::{Renderer, TextStyle};
use crate::shell::{self, ComGuard, KnownFolder};
use crate::textbox::{TextBox, TextEvent, TextEventKind};
use crate::theme::{size, Color, Theme};
use crate::ui::{Anim, Command, Component, Ctx, DropTarget, Effect, Menu, MenuItem, MouseButton, MouseEvent, Point, Rect};

/// A heading's height: the same as the file list's column titles, so "Quick access" and "Name"
/// sit on one line.
const HEADING_H: f32 = size::HEADER_H;
/// Extra room above the "Pinned" and "This PC" headings, between the groups.
const GROUP_GAP: f32 = 6.0;
/// Room after the last row, so it doesn't touch the status bar when scrolled to the end.
const BOTTOM_PAD: f32 = 6.0;
/// Where icons and headings start, from the sidebar's left edge (the red selection edge sits before it).
const PAD_LEFT: f32 = 16.0;
/// Between an icon and its name.
const ICON_GAP: f32 = 8.0;
/// Kept free on the right of names and usage bars.
const PAD_RIGHT: f32 = 12.0;
/// The usage bar under a drive's name: its thickness and its distance from the row's bottom.
const BAR_H: f32 = 3.0;
const BAR_BOTTOM: f32 = 4.0;
/// How much of a drive row the name gets (the rest holds the bar).
const DRIVE_TEXT_H: f32 = size::ROW_H - 8.0;
/// How long a wheel scroll glides, in seconds (the same as the file list).
const SCROLL_TIME: f64 = 0.12;
/// How far into the glide the first frame starts, so a wheel notch answers at once.
const FRAME_HEAD_START: f64 = 0.008;

/// The pinned folders' name box (text boxes 500-599 belong to the sidebar).
const PIN_NAME_BOX: u16 = 500;

/// The Recycle Bin's id among the Quick access places ("HiddenPlaces").
const BIN_ID: &str = "recycle";

const MENU_PLACE: u32 = 1;
const PICK_OPEN: u32 = 1;
const PICK_NEW_TAB: u32 = 2;
const PICK_COPY_PATH: u32 = 3;
const PICK_EXPLORER: u32 = 4;
const PICK_TERMINAL: u32 = 5;
const PICK_PROPERTIES: u32 = 6;
const PICK_PIN: u32 = 7;
const PICK_RENAME_PIN: u32 = 8;
const PICK_MOVE_UP: u32 = 9;
const PICK_MOVE_DOWN: u32 = 10;
const PICK_UNPIN: u32 = 11;
const PICK_REMOVE_PLACE: u32 = 12;
const PICK_EMPTY_BIN: u32 = 13;
const PICK_PIN_HERE: u32 = 14;
/// "Show in Quick access ›": this plus the place's number in `quick_places`.
const PICK_SHOW_PLACE: u32 = 100;

/// What a line of the sidebar is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// "Quick access" (just a title).
    QuickHeading,
    /// One of the user's folders: an index into `Sidebar::known`.
    Known(usize),
    /// The Recycle Bin, in Quick access.
    Bin,
    /// "Right-click to show items again", when every Quick access place was removed.
    QuickHint,
    /// "Pinned" (a title; folders dropped on it are pinned).
    PinnedHeading,
    /// A pinned folder: an index into `app.settings.pinned`.
    Pin(usize),
    /// "Ctrl+D or drop folders here", while nothing is pinned.
    PinHint,
    /// "This PC": clicking it shows all drives.
    PcHeading,
    /// A drive: an index into `app.drives`.
    Drive(usize),
}

/// A line of the sidebar and where it sits, measured from the top of the (unscrolled) content.
#[derive(Clone, Copy, Debug)]
struct Placed {
    item: Item,
    top: f32,
    h: f32,
}

/// What the open right-click menu is about.
#[derive(Clone, Debug)]
struct MenuFor {
    item: Item,
    /// Where the line goes ("" = This PC), if anywhere.
    path: Option<String>,
}

/// A pinned folder whose name is being edited.
struct Renaming {
    path: String,
}

pub struct Sidebar {
    rect: Rect,
    known: Vec<KnownFolder>,
    /// The lines, rebuilt from `known`, the settings and `app.drives` before each use (a few dozen at
    /// most, so it's free).
    items: Vec<Placed>,
    /// Height of all lines together.
    content_h: f32,
    /// Where the mouse is (None when it's elsewhere).
    mouse: Option<Point>,
    /// The line under the mouse when we last looked (to repaint only when it changes).
    hover: Option<usize>,
    /// Whether the mouse was over the scrollbar when we last looked.
    over_track: bool,
    /// Scrolled this far down, in DIPs.
    scroll: f32,
    anim: Anim,
    /// Dragging the scrollbar thumb: where on the thumb it was taken.
    drag: Option<f32>,
    wheel_lines: f32,
    /// What the open right-click menu is about.
    menu_for: Option<MenuFor>,
    icons: IconLoader,
    /// The main window and its scale, from the last layout (for the name box).
    hwnd: HWND,
    scale: f32,
    /// The pinned folder being renamed, and the name box (made the first time it's needed).
    renaming: Option<Renaming>,
    name_box: Option<TextBox>,
    /// Where the name box was put last (None: hidden), so it only moves when its row does.
    name_box_at: Option<Rect>,
    /// Where something dragged would be dropped (window/drag.rs): that place, the Recycle Bin or
    /// "Pinned" is outlined in red.
    drop_at: Option<DropTarget>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl Sidebar {
    pub fn new() -> Sidebar {
        let mut lines: u32 = 3;
        // SAFETY: writes one u32 into `lines`.
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, Some(&mut lines as *mut u32 as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        let known = shell::known_folders();
        let mut icons = IconLoader::new();
        // Start on the folders' icons right away, while the window is still being made: they're
        // usually in before the first frame.
        icons.ask(known.iter().map(|k| (k.path.as_str(), "")));
        Sidebar {
            rect: Rect::default(),
            known,
            items: Vec::new(),
            content_h: 0.0,
            mouse: None,
            hover: None,
            over_track: false,
            scroll: 0.0,
            anim: Anim::default(),
            drag: None,
            // "One screen per notch" comes back as u32::MAX; the sidebar is short, so a few rows do.
            wheel_lines: if lines == u32::MAX { 6.0 } else { lines.clamp(1, 20) as f32 },
            menu_for: None,
            icons,
            hwnd: HWND::default(),
            scale: 1.0,
            renaming: None,
            name_box: None,
            name_box_at: None,
            drop_at: None,
        }
    }

    /// The Quick access places in their order, as (id, name, path): the user's folders (by their
    /// ids: "home", "desktop"...) and the Recycle Bin.
    fn quick_places(&self) -> Vec<(String, &str, &str)> {
        let mut all: Vec<(String, &str, &str)> = self.known.iter().map(|k| (k.name.to_lowercase(), k.name, k.path.as_str())).collect();
        all.push((BIN_ID.to_string(), recycle::NAME, recycle::LOCATION));
        all
    }

    /// Lays the lines out again (the drives, pins or hidden places may have changed since last time).
    fn rebuild(&mut self, app: &AppState) {
        self.items.clear();
        let mut y = 0.0;
        let mut push = |items: &mut Vec<Placed>, item: Item, gap: f32, h: f32| {
            items.push(Placed { item, top: y + gap, h });
            y += gap + h;
        };
        push(&mut self.items, Item::QuickHeading, 0.0, HEADING_H);
        let before = self.items.len();
        for (i, k) in self.known.iter().enumerate() {
            if !app.is_place_hidden(&k.name.to_lowercase()) {
                push(&mut self.items, Item::Known(i), 0.0, size::ROW_H);
            }
        }
        if !app.is_place_hidden(BIN_ID) {
            push(&mut self.items, Item::Bin, 0.0, size::ROW_H);
        }
        if self.items.len() == before {
            push(&mut self.items, Item::QuickHint, 0.0, size::ROW_H);
        }
        push(&mut self.items, Item::PinnedHeading, GROUP_GAP, HEADING_H);
        for i in 0..app.settings.pinned.len() {
            push(&mut self.items, Item::Pin(i), 0.0, size::ROW_H);
        }
        if app.settings.pinned.is_empty() {
            push(&mut self.items, Item::PinHint, 0.0, size::ROW_H);
        }
        push(&mut self.items, Item::PcHeading, GROUP_GAP, HEADING_H);
        for i in 0..app.drives.len() {
            push(&mut self.items, Item::Drive(i), 0.0, size::ROW_H);
        }
        self.content_h = y + BOTTOM_PAD;
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// Where a line takes you: a folder or drive root, "" for This PC, the Recycle Bin's path, None
    /// for a plain title or hint.
    fn path_of<'a>(&'a self, item: Item, app: &'a AppState) -> Option<&'a str> {
        match item {
            Item::QuickHeading | Item::QuickHint | Item::PinnedHeading | Item::PinHint => None,
            Item::Known(i) => self.known.get(i).map(|k| k.path.as_str()),
            Item::Bin => Some(recycle::LOCATION),
            Item::Pin(i) => app.settings.pinned.get(i).map(String::as_str),
            Item::PcHeading => Some(""),
            Item::Drive(i) => app.drives.get(i).map(|d| d.root.as_str()),
        }
    }

    fn max_scroll(&self) -> f32 {
        (self.content_h - self.rect.h).max(0.0)
    }

    /// The line under a point (an index into `items`), None over a gap, the scrollbar or outside.
    fn item_at(&self, p: Point) -> Option<usize> {
        if !self.rect.contains(p) || self.on_track(p) {
            return None;
        }
        let y = p.y - self.rect.y + self.scroll;
        self.items.iter().position(|it| y >= it.top && y < it.top + it.h)
    }

    /// The scrollbar's lane (only when the sidebar is longer than the window).
    fn track(&self) -> Option<Rect> {
        (self.max_scroll() > 0.0).then(|| {
            Rect::new(
                self.rect.right() - size::SCROLLBAR_W - size::SCROLLBAR_MARGIN,
                self.rect.y + size::SCROLLBAR_MARGIN,
                size::SCROLLBAR_W,
                (self.rect.h - 2.0 * size::SCROLLBAR_MARGIN).max(0.0),
            )
        })
    }

    /// Whether a point is on the scrollbar (a little wider than drawn, so it's easy to hit).
    fn on_track(&self, p: Point) -> bool {
        self.track().is_some_and(|t| t.inset(-3.0, 0.0).contains(p))
    }

    fn thumb(&self) -> Option<Rect> {
        let track = self.track()?;
        let max = self.max_scroll();
        let h = (track.h * self.rect.h / self.content_h).clamp(24.0_f32.min(track.h), track.h);
        let y = track.y + (track.h - h) * (self.scroll / max).clamp(0.0, 1.0);
        Some(Rect::new(track.x, y, track.w, h))
    }

    /// Glide the scroll position to `target`.
    fn scroll_to(&mut self, target: f32, now: f64) -> Effect {
        let target = target.clamp(0.0, self.max_scroll());
        self.anim.go(self.scroll, target, now - FRAME_HEAD_START, SCROLL_TIME);
        Effect::Animate
    }

    /// The place the current tab is in: the place itself, or else the deepest place that holds it
    /// (Downloads while you're in Downloads\Setup, the drive while you're anywhere else on it).
    /// Nothing on This PC.
    fn current_place(&self, app: &AppState) -> Option<Item> {
        let path = app.tab().path.as_str();
        if path.is_empty() {
            return None;
        }
        let mut best: Option<(usize, Item)> = None;
        for placed in &self.items {
            let item = placed.item;
            let Some(place) = self.path_of(item, app).filter(|p| !p.is_empty()) else { continue };
            let score = if same_path(place, path) {
                usize::MAX
            } else if is_inside(path, place) {
                place.trim_end_matches('\\').len()
            } else {
                continue;
            };
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, item));
            }
        }
        best.map(|(_, item)| item)
    }

    /// Whether a line reacts to the mouse (titles and hints don't, except "This PC").
    fn clickable(item: Item) -> bool {
        !matches!(item, Item::QuickHeading | Item::QuickHint | Item::PinnedHeading | Item::PinHint)
    }

    /// Repaint only if what the mouse is over changed.
    fn update_hover(&mut self) -> Effect {
        let hover = if self.drag.is_some() { None } else { self.mouse.and_then(|p| self.item_at(p)) };
        let hover = hover.filter(|&i| Self::clickable(self.items[i].item));
        let over_track = self.mouse.is_some_and(|p| self.on_track(p));
        if hover != self.hover || over_track != self.over_track {
            self.hover = hover;
            self.over_track = over_track;
            return Effect::Redraw;
        }
        Effect::None
    }

    /// The row of a line on screen (None when there's no such line).
    fn row_of(&self, item: Item) -> Option<Rect> {
        let placed = self.items.iter().find(|p| p.item == item)?;
        Some(Rect::new(self.rect.x, self.rect.y + placed.top - self.scroll, self.rect.w, placed.h))
    }

    /// Where a line's name starts, and how wide it may be.
    fn name_span(&self, row: Rect) -> (f32, f32) {
        let text_x = row.x + PAD_LEFT + size::ICON + ICON_GAP;
        let right = row.right() - PAD_RIGHT - if self.track().is_some() { size::SCROLLBAR_W } else { 0.0 };
        (text_x, (right - text_x).max(0.0))
    }

    fn paint_place(&self, r: &mut Renderer, theme: &Theme, app: &AppState, item: Item, row: Rect, state: RowState) {
        match state {
            RowState::Selected => {
                r.fill_rect(row, theme.accent_tint);
                r.fill_rect(Rect::new(row.x, row.y, size::SELECT_EDGE, row.h), theme.select_edge);
            }
            RowState::Hover => r.fill_rect(row, theme.hover),
            RowState::Normal => {}
        }
        let icon = Rect::new(row.x + PAD_LEFT, row.y + (row.h - size::ICON) / 2.0, size::ICON, size::ICON);
        let (text_x, text_w) = self.name_span(row);
        let color = if state == RowState::Selected { theme.text_strong } else { theme.text };
        let path = self.path_of(item, app).unwrap_or("");
        match item {
            Item::Known(i) => {
                match self.icons.get(path) {
                    Some(id) => r.icon(&app.icons, id, icon),
                    None => r.icon(&app.icons, app.icons.folder(), icon),
                }
                let name = self.known.get(i).map_or("", |k| k.name);
                r.text(name, Rect::new(text_x, row.y, text_w, row.h), &TextStyle::new(color));
            }
            Item::Bin => {
                match self.icons.get(path) {
                    Some(id) => r.icon(&app.icons, id, icon),
                    None => draw_bin_glyph(r, icon, theme.muted),
                }
                r.text(recycle::NAME, Rect::new(text_x, row.y, text_w, row.h), &TextStyle::new(color));
            }
            Item::Pin(_) => {
                // A pinned drive gets its drive's icon (once the drive list brought it), a folder
                // the plain folder icon.
                let drive = app.drives.iter().find(|d| same_path(&d.root, path));
                match drive.and_then(|d| self.icons.get(&d.root).or_else(|| app.icons.stock_drive(d.kind_name))) {
                    Some(id) => r.icon(&app.icons, id, icon),
                    None => r.icon(&app.icons, app.icons.folder(), icon),
                }
                if self.renaming.as_ref().is_none_or(|rn| !same_path(&rn.path, path)) {
                    r.text(&app.pin_name(path), Rect::new(text_x, row.y, text_w, row.h), &TextStyle::new(color));
                }
            }
            Item::Drive(i) => {
                let Some(drive) = app.drives.get(i) else { return };
                // Its own icon once the background lookup brought it, Windows' standard drive icon
                // until then, and a drawn one if Windows has neither.
                match self.icons.get(path).or_else(|| app.icons.stock_drive(drive.kind_name)) {
                    Some(id) => r.icon(&app.icons, id, icon),
                    None => draw_drive_glyph(r, icon, theme.muted, theme.text),
                }
                match usage(drive) {
                    Some(used) => {
                        r.text(&drive.name, Rect::new(text_x, row.y, text_w, DRIVE_TEXT_H), &TextStyle::new(color));
                        let bar = Rect::new(text_x, row.bottom() - BAR_BOTTOM - BAR_H, text_w, BAR_H);
                        r.fill_rect(bar, theme.bar_track);
                        if used > 0.0 {
                            // At least a sliver, so a nearly empty drive still shows some red.
                            let w = (bar.w * used).clamp(1.0, bar.w);
                            r.fill_rect(Rect::new(bar.x, bar.y, w, bar.h), theme.accent);
                        }
                    }
                    None => r.text(&drive.name, Rect::new(text_x, row.y, text_w, row.h), &TextStyle::new(color)),
                }
            }
            Item::QuickHeading | Item::QuickHint | Item::PinnedHeading | Item::PinHint | Item::PcHeading => {}
        }
    }

    // ------------------------------------------------------------ the right-click menu

    /// "Show in Quick access ›" with the places that were taken out (None when none were).
    fn show_again_menu(&self, app: &AppState) -> Option<MenuItem> {
        let hidden: Vec<MenuItem> = self
            .quick_places()
            .iter()
            .enumerate()
            .filter(|(_, (id, _, _))| app.is_place_hidden(id))
            .map(|(k, (_, name, _))| MenuItem::new(PICK_SHOW_PLACE + k as u32, name))
            .collect();
        (!hidden.is_empty()).then(|| MenuItem::submenu("Show in Quick access", hidden))
    }

    /// The right-click menu for a line, or None when it has none.
    fn menu_items(&self, item: Item, path: Option<&str>, app: &AppState) -> Vec<MenuItem> {
        let mut items = Vec::new();
        let open = |items: &mut Vec<MenuItem>| {
            items.push(MenuItem::new(PICK_OPEN, "Open"));
            items.push(MenuItem::new(PICK_NEW_TAB, "Open in new tab"));
            items.push(MenuItem::separator());
        };
        // The things every folder or drive has, at the end.
        let folder_tail = |items: &mut Vec<MenuItem>| {
            items.push(MenuItem::separator());
            items.push(MenuItem::new(PICK_COPY_PATH, "Copy path"));
            items.push(MenuItem::new(PICK_EXPLORER, "Show in File Explorer"));
            items.push(MenuItem::new(PICK_TERMINAL, "Open in terminal"));
            items.push(MenuItem::new(PICK_PROPERTIES, "Properties"));
        };
        let pin_label = |p: &str| if app.is_pinned(p) { "Unpin from sidebar" } else { "Pin to sidebar" };
        match item {
            Item::Known(_) => {
                let p = path.unwrap_or("");
                open(&mut items);
                items.push(MenuItem::new(PICK_PIN, pin_label(p)));
                items.push(MenuItem::new(PICK_REMOVE_PLACE, "Remove from Quick access"));
                items.extend(self.show_again_menu(app));
                folder_tail(&mut items);
            }
            Item::Bin => {
                open(&mut items);
                items.push(MenuItem::new(PICK_EMPTY_BIN, "Empty Recycle Bin"));
                items.push(MenuItem::separator());
                items.push(MenuItem::new(PICK_REMOVE_PLACE, "Remove from Quick access"));
                items.extend(self.show_again_menu(app));
                items.push(MenuItem::separator());
                items.push(MenuItem::new(PICK_EXPLORER, "Open in File Explorer"));
                items.push(MenuItem::new(PICK_PROPERTIES, "Properties"));
            }
            Item::QuickHeading | Item::QuickHint => items.extend(self.show_again_menu(app)),
            Item::Pin(i) => {
                open(&mut items);
                items.push(MenuItem::new(PICK_RENAME_PIN, "Rename\u{2026}"));
                items.push(MenuItem::new(PICK_MOVE_UP, "Move up").disabled(i == 0));
                items.push(MenuItem::new(PICK_MOVE_DOWN, "Move down").disabled(i + 1 >= app.settings.pinned.len()));
                items.push(MenuItem::new(PICK_UNPIN, "Unpin from sidebar"));
                folder_tail(&mut items);
            }
            Item::PinnedHeading | Item::PinHint => {
                let here = app.tab().path.as_str();
                let label = if app.is_pinned(here) { "Unpin current folder\tCtrl+D" } else { "Pin current folder\tCtrl+D" };
                items.push(MenuItem::new(PICK_PIN_HERE, label).disabled(!can_pin(here)));
            }
            Item::PcHeading => {
                open(&mut items);
                items.push(MenuItem::new(PICK_EXPLORER, "Show in File Explorer"));
            }
            Item::Drive(_) => {
                let p = path.unwrap_or("");
                open(&mut items);
                items.push(MenuItem::new(PICK_PIN, pin_label(p)));
                folder_tail(&mut items);
            }
        }
        items
    }

    // ------------------------------------------------------------ renaming a pinned folder

    /// Opens the name box over a pinned folder's name, with the name it shows selected.
    fn begin_rename(&mut self, path: &str, app: &AppState) -> Effect {
        if self.hwnd.is_invalid() || !app.is_pinned(path) {
            return Effect::None;
        }
        self.end_rename(false, false, None);
        let hwnd = self.hwnd;
        let b = self.name_box.get_or_insert_with(|| TextBox::new(hwnd, PIN_NAME_BOX));
        b.set_text(&app.pin_name(path));
        self.renaming = Some(Renaming { path: path.to_string() });
        self.name_box_at = None;
        self.place_name_box(app);
        if let Some(b) = &self.name_box {
            b.focus();
            b.select_all();
        }
        Effect::Redraw
    }

    /// Closes the name box; with `commit`, what was typed becomes the pin's name (in `app`).
    /// `focus_back`: give the keyboard back to the window (after Enter / Esc).
    fn end_rename(&mut self, commit: bool, focus_back: bool, app: Option<&mut AppState>) {
        let Some(rn) = self.renaming.take() else { return };
        self.name_box_at = None;
        let Some(b) = self.name_box.as_mut() else { return };
        let typed = b.text();
        b.hide();
        if focus_back {
            b.give_focus_back();
        }
        if let (true, Some(app)) = (commit, app) {
            app.rename_pin(&rn.path, &typed);
        }
    }

    /// The name box's place: over the pin's name, while its row is in sight.
    fn name_box_rect(&self, app: &AppState) -> Option<Rect> {
        let rn = self.renaming.as_ref()?;
        let i = app.settings.pinned.iter().position(|p| same_path(p, &rn.path))?;
        let row = self.row_of(Item::Pin(i))?;
        if row.y < self.rect.y - 0.5 || row.bottom() > self.rect.bottom() + 0.5 {
            return None;
        }
        let (x, w) = self.name_span(row);
        Some(Rect::new(x - 3.0, row.y + 2.0, (w + 3.0).max(60.0), row.h - 4.0))
    }

    /// Moves the name box to its row (only when that moved), or hides it while the row is out of sight.
    fn place_name_box(&mut self, app: &AppState) {
        let rect = self.name_box_rect(app);
        if rect == self.name_box_at {
            return;
        }
        self.name_box_at = rect;
        let scale = self.scale;
        let Some(b) = self.name_box.as_mut() else { return };
        match rect {
            Some(r) => {
                b.place(r, 2.0, scale);
                if !b.is_visible() {
                    b.show();
                }
            }
            None => b.hide(),
        }
    }

    // ------------------------------------------------------------ drag and drop

    /// Whether `item` is where something dragged would be dropped now (it's outlined in red).
    fn is_drop_target(&self, item: Item, app: &AppState) -> bool {
        match (&self.drop_at, item) {
            (Some(DropTarget::Pin), Item::PinnedHeading | Item::PinHint) => true,
            (Some(DropTarget::RecycleBin), Item::Bin) => true,
            (Some(DropTarget::Folder(d)), Item::Known(_) | Item::Pin(_) | Item::Drive(_)) => self.path_of(item, app).is_some_and(|p| same_path(p, d)),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowState {
    Normal,
    Hover,
    Selected,
}

impl Component for Sidebar {
    fn layout(&mut self, rect: Rect, ctx: &Ctx, app: &AppState) {
        self.rect = rect;
        self.hwnd = ctx.hwnd;
        if (ctx.scale - self.scale).abs() > 0.001 {
            self.scale = ctx.scale;
            self.name_box_at = None; // placed again (and its font made again) at the new scale
        }
        self.icons.set_window(ctx.hwnd);
        self.rebuild(app);
        if rect.w <= 0.0 {
            // Hidden (Ctrl+B): a name box still open goes with it.
            self.end_rename(false, false, None);
        }
        if self.renaming.is_some() {
            self.place_name_box(app);
        }
    }

    fn rect(&self) -> Rect {
        self.rect
    }

    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx) {
        // Icons that came in since the last frame, and any drive that still needs one. The Recycle
        // Bin's is looked up again when it turns full or empty.
        self.icons.set_window(ctx.hwnd);
        self.icons.receive(&app.icons);
        self.icons.ask(app.drives.iter().map(|d: &Drive| (d.root.as_str(), d.name.as_str())));
        let bin_state = match recycle::seen_full() {
            Some(true) => "full",
            Some(false) => "empty",
            None => "",
        };
        self.icons.ask(std::iter::once((recycle::LOCATION, bin_state)));
        self.rebuild(app);
        // A pinned folder being renamed that was unpinned meanwhile (Ctrl+D): its box closes.
        if self.renaming.as_ref().is_some_and(|rn| !app.is_pinned(&rn.path)) {
            self.end_rename(false, false, None);
        }
        if self.renaming.is_some() {
            self.place_name_box(app);
        }

        r.fill_rect(self.rect, theme.sidebar);
        r.push_clip(self.rect);
        let hover = if self.drag.is_some() { None } else { self.mouse.and_then(|p| self.item_at(p)) };
        let current = self.current_place(app);
        let heading_w = self.rect.w - PAD_LEFT - PAD_RIGHT;
        for (index, placed) in self.items.iter().enumerate() {
            let y = self.rect.y + placed.top - self.scroll;
            if y + placed.h <= self.rect.y || y >= self.rect.bottom() {
                continue;
            }
            let row = Rect::new(self.rect.x, y, self.rect.w, placed.h);
            let hovered = hover == Some(index);
            match placed.item {
                Item::QuickHeading | Item::PinnedHeading => {
                    // Folders dragged onto "Pinned" are pinned: it lights up and is outlined.
                    let dropping = self.is_drop_target(placed.item, app);
                    if dropping {
                        r.fill_rect(row, theme.hover);
                        r.stroke_rect(row, 1.0, theme.accent);
                    }
                    let title = if placed.item == Item::QuickHeading { "Quick access" } else { "Pinned" };
                    r.text(title, Rect::new(row.x + PAD_LEFT, y, heading_w, row.h), &TextStyle::new(if dropping { theme.text } else { theme.muted }));
                }
                Item::QuickHint | Item::PinHint => {
                    if self.is_drop_target(placed.item, app) {
                        r.fill_rect(row, theme.hover);
                        r.stroke_rect(row, 1.0, theme.accent);
                    }
                    // Where the icons start (a hint has none), so it fits the narrow sidebar.
                    let hint = if placed.item == Item::PinHint { "Ctrl+D or drop folders here" } else { "Right-click to show items again" };
                    let (x, w) = self.name_span(row);
                    let left = row.x + PAD_LEFT;
                    r.text(hint, Rect::new(left, y, w + x - left, row.h), &TextStyle::new(theme.muted));
                }
                Item::PcHeading => {
                    // A title you can click (it shows all drives): it lights up like a place.
                    if hovered {
                        r.fill_rect(row, theme.hover);
                    }
                    let color = if hovered { theme.text } else { theme.muted };
                    r.text("This PC", Rect::new(row.x + PAD_LEFT, y, heading_w, row.h), &TextStyle::new(color));
                }
                item => {
                    // The place something dragged would go into: outlined in red.
                    let dropping = self.is_drop_target(item, app);
                    let state = if current == Some(item) {
                        RowState::Selected
                    } else if hovered || dropping {
                        RowState::Hover
                    } else {
                        RowState::Normal
                    };
                    self.paint_place(r, theme, app, item, row, state);
                    if dropping {
                        r.stroke_rect(row, 1.0, theme.accent);
                    }
                }
            }
        }
        // The name box is a Windows text box on top; this is the frame around it.
        if let Some(at) = self.name_box_at.filter(|_| self.renaming.is_some()) {
            r.fill_rect(at, theme.input);
            r.stroke_rect(at, 1.0, theme.accent);
        }
        r.pop_clip();

        if let Some(thumb) = self.thumb() {
            let color = if self.drag.is_some() {
                theme.accent
            } else if self.over_track {
                theme.scroll_thumb_hover
            } else {
                theme.scroll_thumb
            };
            r.fill_round_rect(thumb, size::SCROLLBAR_W / 2.0, color);
        }
    }

    fn mouse_move(&mut self, _ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.mouse = Some(e.pos);
        if let Some(grab) = self.drag {
            self.rebuild(app);
            if let (Some(track), Some(thumb)) = (self.track(), self.thumb()) {
                let span = (track.h - thumb.h).max(1.0);
                let max = self.max_scroll();
                self.scroll = ((e.pos.y - grab - track.y) / span * max).clamp(0.0, max);
                self.anim.stop();
            }
            return Effect::Redraw;
        }
        self.update_hover()
    }

    fn mouse_leave(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        self.mouse = None;
        self.update_hover()
    }

    fn mouse_down(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        self.rebuild(app);
        // The scrollbar: grab the thumb, or page towards the click.
        if self.on_track(e.pos) {
            if e.button != MouseButton::Left {
                return Effect::None;
            }
            let Some(thumb) = self.thumb() else { return Effect::None };
            if e.pos.y >= thumb.y && e.pos.y < thumb.bottom() {
                self.drag = Some(e.pos.y - thumb.y);
                self.anim.stop();
                return Effect::Redraw;
            }
            let from = if self.anim.running() { self.anim.target() } else { self.scroll };
            let page = (self.rect.h - size::ROW_H).max(size::ROW_H);
            return self.scroll_to(from + if e.pos.y < thumb.y { -page } else { page }, ctx.now);
        }
        let Some(item) = self.item_at(e.pos).map(|i| self.items[i].item) else { return Effect::None };
        let path = self.path_of(item, app).map(str::to_string);
        match (e.button, path) {
            (MouseButton::Left, Some(path)) => Effect::Command(Command::Navigate(path)),
            (MouseButton::Middle, Some(path)) => Effect::Command(Command::NewTabAt(path)),
            (MouseButton::Right, path) => {
                let items = self.menu_items(item, path.as_deref(), app);
                if items.is_empty() {
                    return Effect::None;
                }
                self.menu_for = Some(MenuFor { item, path });
                Effect::Menu(Menu { tag: MENU_PLACE, at: e.pos, items, right_aligned: false })
            }
            _ => Effect::None,
        }
    }

    fn mouse_up(&mut self, _ctx: &Ctx, _e: &MouseEvent, _app: &mut AppState) -> Effect {
        if self.drag.take().is_some() {
            self.hover = None;
            let _ = self.update_hover();
            return Effect::Redraw;
        }
        Effect::None
    }

    fn wheel(&mut self, ctx: &Ctx, e: &MouseEvent, app: &mut AppState) -> Effect {
        // Scrolling while renaming keeps what was typed, like clicking elsewhere.
        if self.renaming.is_some() {
            self.end_rename(true, true, Some(app));
        }
        self.rebuild(app);
        if self.max_scroll() <= 0.0 {
            return Effect::Redraw;
        }
        let from = if self.anim.running() { self.anim.target() } else { self.scroll };
        self.scroll_to(from - e.wheel * self.wheel_lines * size::ROW_H, ctx.now)
    }

    fn menu_pick(&mut self, ctx: &Ctx, tag: u32, item: u32, app: &mut AppState) -> Effect {
        if tag != MENU_PLACE {
            return Effect::None;
        }
        let Some(MenuFor { item: line, path }) = self.menu_for.take() else { return Effect::None };
        // "Show in Quick access ›": the place picked comes back.
        if let Some(k) = item.checked_sub(PICK_SHOW_PLACE) {
            if let Some((id, _, _)) = self.quick_places().get(k as usize) {
                app.set_place_hidden(id, false);
            }
            return Effect::Redraw;
        }
        match item {
            PICK_PIN_HERE => return Effect::Command(Command::TogglePin(app.tab().path.clone())),
            PICK_REMOVE_PLACE => {
                let id = match line {
                    Item::Known(i) => self.known.get(i).map(|k| k.name.to_lowercase()),
                    Item::Bin => Some(BIN_ID.to_string()),
                    _ => None,
                };
                if let Some(id) = id {
                    app.set_place_hidden(&id, true);
                }
                return Effect::Redraw;
            }
            PICK_EMPTY_BIN => {
                app.bin_empty();
                return Effect::Redraw;
            }
            _ => {}
        }
        let Some(path) = path else { return Effect::None };
        match item {
            PICK_OPEN => Effect::Command(Command::Navigate(path)),
            PICK_NEW_TAB => Effect::Command(Command::NewTabAt(path)),
            PICK_PIN => Effect::Command(Command::TogglePin(path)),
            PICK_RENAME_PIN => self.begin_rename(&path, app),
            PICK_MOVE_UP | PICK_MOVE_DOWN => {
                app.move_pin(&path, item == PICK_MOVE_UP);
                Effect::Redraw
            }
            PICK_UNPIN => {
                app.unpin(&path);
                app.say(format!("Unpinned {}", app.display_name(&path)));
                Effect::Redraw
            }
            PICK_COPY_PATH => {
                app.say(if shell::copy_text(ctx.hwnd, &path) { "Path copied" } else { "Couldn't copy (the clipboard is busy)" });
                Effect::Redraw
            }
            PICK_EXPLORER => {
                let at = if recycle::is_bin(&path) { "shell:RecycleBinFolder" } else { path.as_str() };
                shell::show_in_explorer(at, false);
                Effect::None
            }
            PICK_TERMINAL => Effect::Command(Command::OpenTerminal(path)),
            PICK_PROPERTIES => {
                // Windows' Properties window for the folder, drive or Recycle Bin (shellmenu.rs).
                if !crate::shellmenu::properties(ctx.hwnd, vec![path]) {
                    app.say("Windows couldn't show the properties right now");
                }
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    fn text_event(&mut self, _ctx: &Ctx, ev: &TextEvent, app: &mut AppState) -> Effect {
        if ev.id != PIN_NAME_BOX || self.renaming.is_none() {
            return Effect::None;
        }
        match ev.kind {
            TextEventKind::Enter => self.end_rename(true, true, Some(app)),
            TextEventKind::Escape => self.end_rename(false, true, None),
            // Clicking elsewhere keeps what was typed, as in the file list. The message was posted,
            // so the box may have the keyboard back by now.
            TextEventKind::LostFocus if !self.name_box.as_ref().is_some_and(|b| b.has_focus()) => self.end_rename(true, false, Some(app)),
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// What dropping at `p` does: "Pinned" (its heading or hint) pins the dropped folders, the
    /// Recycle Bin takes the dropped items, a place, pinned folder or drive takes them in. The
    /// other headings take nothing.
    fn drop_target(&self, p: Point, app: &AppState) -> Option<DropTarget> {
        let item = self.items.get(self.item_at(p)?)?.item;
        match item {
            Item::PinnedHeading | Item::PinHint => Some(DropTarget::Pin),
            Item::Bin => Some(DropTarget::RecycleBin),
            Item::Known(_) | Item::Pin(_) | Item::Drive(_) => Some(DropTarget::Folder(self.path_of(item, app)?.to_string())),
            Item::QuickHeading | Item::QuickHint | Item::PcHeading => None,
        }
    }

    fn drop_hover(&mut self, target: Option<&DropTarget>) -> Effect {
        if target == self.drop_at.as_ref() {
            return Effect::None;
        }
        self.drop_at = target.cloned();
        Effect::Redraw
    }

    fn tick(&mut self, now: f64, _app: &mut AppState) -> bool {
        if let Some(v) = self.anim.value(now) {
            self.scroll = v.clamp(0.0, self.max_scroll());
            // The rows moved under a still mouse: the highlight follows.
            self.hover = None;
            self.over_track = false;
            let _ = self.update_hover();
        }
        self.anim.running()
    }
}

/// How full a drive is (0.0 - 1.0), for its bar. Only for local disks and removable drives that
/// are ready: asking a network drive or an empty DVD drive can stall, and a full bar on every DVD
/// says nothing.
fn usage(d: &Drive) -> Option<f32> {
    let local = matches!(d.kind_name, "Local Disk" | "Removable Disk");
    (local && d.ready && d.total > 0).then(|| (1.0 - d.free as f64 / d.total as f64).clamp(0.0, 1.0) as f32)
}

/// A small drive drawn with lines, shown until the drive's own icon has arrived (or if Windows has
/// none): a flat box with a light on its right.
fn draw_drive_glyph(r: &mut Renderer, icon: Rect, outline: Color, light: Color) {
    let (x0, x1) = (icon.x + 1.5, icon.right() - 1.5);
    let (y0, y1) = (icon.y + 4.0, icon.bottom() - 3.5);
    let corners = [Point::new(x0, y0), Point::new(x1, y0), Point::new(x1, y1), Point::new(x0, y1), Point::new(x0, y0)];
    r.polyline(&corners, 1.2, outline);
    r.fill_rect(Rect::new(x1 - 4.0, (y0 + y1) / 2.0 - 0.75, 2.0, 1.5), light);
}

/// A small bin drawn with lines, until Windows' Recycle Bin icon has arrived: a lid and a tapered can.
fn draw_bin_glyph(r: &mut Renderer, icon: Rect, color: Color) {
    let (l, t) = (icon.x, icon.y);
    r.polyline(&[Point::new(l + 2.5, t + 4.0), Point::new(l + 13.5, t + 4.0)], 1.2, color);
    r.polyline(&[Point::new(l + 6.5, t + 4.0), Point::new(l + 6.5, t + 2.5), Point::new(l + 9.5, t + 2.5), Point::new(l + 9.5, t + 4.0)], 1.2, color);
    r.polyline(&[Point::new(l + 4.0, t + 5.5), Point::new(l + 5.0, t + 14.0), Point::new(l + 11.0, t + 14.0), Point::new(l + 12.0, t + 5.5)], 1.2, color);
}

/// Two paths are the same folder ("C:\Users\Me" and "c:\users\me\").
fn same_path(a: &str, b: &str) -> bool {
    eq_ignore_case(a.trim_end_matches('\\'), b.trim_end_matches('\\'))
}

/// `path` is somewhere inside `place` (not `place` itself).
fn is_inside(path: &str, place: &str) -> bool {
    let place = place.trim_end_matches('\\');
    let n = place.len();
    path.len() > n && path.is_char_boundary(n) && path.as_bytes()[n] == b'\\' && eq_ignore_case(&path[..n], place)
}

/// Equal apart from upper/lower case, also for letters like "Ü" (Windows paths ignore case).
fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

// ---------------------------------------------------------------- icons, off the UI thread

/// Looks places' icons up on a background thread and hands them to the icon cache as they arrive.
struct IconLoader {
    tx: Sender<(String, Option<LoadedIcon>)>,
    rx: Receiver<(String, Option<LoadedIcon>)>,
    /// The window to repaint when an icon arrives (0 until the window exists).
    hwnd: Arc<AtomicIsize>,
    /// Every path asked for, with what the place was called then: a drive is asked again when its
    /// name changes (a disc went in, a USB stick was swapped for another), the Recycle Bin when it
    /// turns full or empty.
    asked: HashMap<String, String>,
    /// Icons that arrived, by path (None: Windows has no icon for it).
    got: HashMap<String, Option<IconId>>,
}

impl IconLoader {
    fn new() -> IconLoader {
        let (tx, rx) = channel();
        IconLoader { tx, rx, hwnd: Arc::new(AtomicIsize::new(0)), asked: HashMap::new(), got: HashMap::new() }
    }

    fn set_window(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    }

    /// The icon for a path, once it's here.
    fn get(&self, path: &str) -> Option<IconId> {
        self.got.get(path).copied().flatten()
    }

    /// Starts looking up the icons of these (path, name) places that haven't been asked for yet.
    fn ask<'a>(&mut self, places: impl Iterator<Item = (&'a str, &'a str)>) {
        let mut batch = Vec::new();
        for (path, name) in places {
            if path.is_empty() || self.asked.get(path).is_some_and(|n| n == name) {
                continue;
            }
            self.asked.insert(path.to_string(), name.to_string());
            batch.push(path.to_string());
        }
        if batch.is_empty() {
            return;
        }
        let (tx, hwnd, paths) = (self.tx.clone(), self.hwnd.clone(), batch.clone());
        let started = std::thread::Builder::new().name("sidebar icons".into()).spawn(move || {
            let _com = ComGuard::init();
            for path in paths {
                let icon = icons::load_path_icon(&path);
                // Send first, then wake the window: if the window doesn't exist yet, its first
                // frame picks the icon up anyway.
                if tx.send((path, icon)).is_err() {
                    return;
                }
                repaint(&hwnd);
            }
        });
        if started.is_err() {
            // No thread to be had right now: forget these, so the next frame asks again.
            for path in batch {
                self.asked.remove(&path);
            }
        }
    }

    /// Puts icons that arrived into the shared cache.
    fn receive(&mut self, cache: &Icons) {
        while let Ok((path, icon)) = self.rx.try_recv() {
            let id = icon.map(|icon| cache.adopt(&path, Some(icon)));
            // Keep a drive's old picture if its new lookup found nothing.
            if id.is_some() || !self.got.contains_key(&path) {
                self.got.insert(path, id);
            }
        }
    }
}

/// Asks the window to paint again (safe from any thread; it's a no-op if the window is gone).
fn repaint(hwnd: &AtomicIsize) {
    let h = hwnd.load(Ordering::SeqCst);
    if h != 0 {
        // SAFETY: invalidating a window from another thread is allowed; a stale handle just fails.
        unsafe {
            let _ = InvalidateRect(Some(HWND(h as *mut _)), None, false);
        }
    }
}
