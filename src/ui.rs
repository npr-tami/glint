//! The contract between the window and its parts (title bar, toolbar, sidebar, file list, status bar).
//!
//! Every part implements [`Component`]. The window (window.rs) owns the parts, tells each one where it
//! sits ([`Component::layout`]), asks it to draw ([`Component::paint`]) and hands it the mouse and the
//! keyboard. A part never touches the window directly: it answers with an [`Effect`] ("redraw me",
//! "keep animating", "run this command", "show this menu") and the window carries it out.
//!
//! All coordinates are DIPs (1/96 inch) measured from the top-left of the window's client area.

use crate::app::{AppState, SortKey};
use crate::render::Renderer;
use crate::settings::{ThemeMode, ViewMode};
use crate::theme::Theme;
use crate::textbox::TextEvent;
use windows::Win32::Foundation::HWND;

/// A point in DIPs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

/// A rectangle in DIPs: left, top, width, height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }
    /// Smaller by `dx` on the left and right and `dy` on the top and bottom.
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    /// A `w` x `h` rectangle centred in this one.
    pub fn center(&self, w: f32, h: f32) -> Rect {
        Rect::new(self.x + (self.w - w) / 2.0, self.y + (self.h - h) / 2.0, w, h)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// Which modifier keys were down.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// One mouse event, already converted to DIPs.
#[derive(Clone, Copy, Debug)]
pub struct MouseEvent {
    pub pos: Point,
    /// The button that went down/up (for moves: Left).
    pub button: MouseButton,
    pub mods: Mods,
    /// Wheel turns in notches: +1.0 is one notch away from you (scroll up). 0 for other events.
    pub wheel: f32,
}

/// What the window should do after a part handled something.
#[derive(Debug, Default)]
pub enum Effect {
    /// Nothing changed.
    #[default]
    None,
    /// Something changed: paint again.
    Redraw,
    /// Paint again and keep calling [`Component::tick`] every frame until it returns false.
    Animate,
    /// Run an app command (this also repaints).
    Command(Command),
    /// Show a native popup menu at `menu.at`. The pick (if any) comes back through
    /// [`Component::menu_pick`] on the part that asked.
    Menu(Menu),
    /// Show a right-click menu for files: Glint's own items, with Windows' items for the same files
    /// (Send to, 7-Zip, Open with Code...) where `SHELL_SLOT` is - or Windows' whole menu (see
    /// [`FilesMenu`] and shellmenu.rs). Glint's picks come back through `menu_pick` like `Menu`'s.
    FilesMenu(FilesMenu),
    /// Drag these items out (dragdrop.rs): Windows' drag loop takes the mouse from here on.
    DragOut(DragOut),
}

/// Items dragged out of the file list (dragdrop.rs): into File Explorer, another program, another
/// folder in Glint or another Glint window.
#[derive(Clone, Debug, PartialEq)]
pub struct DragOut {
    /// The folder they're in.
    pub folder: String,
    /// Their names (the selected items when the drag started).
    pub names: Vec<String>,
    /// Dragged with the right button: the program dropped into shows its menu.
    pub right: bool,
}

/// Where something dragged over Glint would be dropped (window/drag.rs asks each part with
/// [`Component::drop_target`]).
#[derive(Clone, Debug, PartialEq)]
pub enum DropTarget {
    /// Into this folder: a folder's row, the list's empty space (the folder on screen), a step of
    /// the path, a place in the sidebar.
    Folder(String),
    /// A tab (by its place in the tab row): into its folder; resting on it a moment switches to it.
    Tab(usize),
    /// The Recycle Bin: the items are deleted.
    RecycleBin,
    /// The sidebar's "Pinned" group: dropped folders are pinned there.
    Pin,
}

/// A right-click menu for files or a folder's empty space (the file list's).
#[derive(Clone, Debug, Default)]
pub struct FilesMenu {
    /// Glint's own items. Put one `MenuItem::new(SHELL_SLOT, "")` where Windows' items go, and
    /// give "Show more options" the id `MORE_OPTIONS`. If Windows' menu can't be had (or is slow),
    /// this menu shows without them.
    pub menu: Menu,
    /// The item shown in bold (what a double-click does), or 0.
    pub default_item: u32,
    /// Which files Windows' items are for; None shows only Glint's.
    pub windows: Option<ShellRequest>,
    /// What the menu is about, as it was when it was asked for (see [`MenuAbout`]).
    pub about: MenuAbout,
}

/// What a right-click menu for files is about, as it was when the menu was asked for: the tab, the
/// folder it showed and the selected items (lower-case names). A pick acts on these items even if
/// the selection changed while the menu waited for Windows' items or was open - a paste finishing
/// meanwhile selects what it made, and Delete must not then delete those instead
/// (`AppState::reselect_for_menu`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MenuAbout {
    pub tab_id: u64,
    pub folder: String,
    pub names: Vec<String>,
}

/// Which Windows menu to ask for (shellmenu.rs builds it on its own thread).
#[derive(Clone, Debug, PartialEq)]
pub struct ShellRequest {
    pub target: ShellTarget,
    /// Shift was held: Windows adds its extra items ("Copy as path", "Open PowerShell window here"...).
    pub extended: bool,
    /// Show Windows' whole menu instead of Glint's ("Show more options", Shift+right-click, Shift+F10).
    pub full: bool,
}

/// What a Windows menu is for.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellTarget {
    /// These items (full paths, all in the same folder).
    Items(Vec<String>),
    /// The empty space of this folder ("" = This PC): New, Paste, and apps' "Open here" items.
    Background(String),
}

/// In a [`FilesMenu`]: where Windows' items go (this item itself is never shown).
pub const SHELL_SLOT: u32 = 0x0FFF;
/// In a [`FilesMenu`]: "Show more options" - Windows' whole menu opens at the same place.
pub const MORE_OPTIONS: u32 = 0x0FFE;

/// What the title bar reports to Windows for a point (used for WM_NCHITTEST, so Windows can drag the
/// window, show the snap-layouts flyout on the maximise button, and so on).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// An ordinary point: the part handles the mouse itself (tabs, buttons, rows...).
    Client,
    /// Empty title-bar space: dragging moves the window, double-clicking maximises.
    Caption,
    Minimize,
    Maximize,
    Close,
}

/// The mouse pointer to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorKind {
    #[default]
    Arrow,
    Hand,
    IBeam,
    /// Left-right arrows (resizing a column).
    SizeWE,
}

/// Things any part may ask the app to do. The window runs them (see `Window::run_command` in
/// window.rs); the ones about folders and tabs end up in [`AppState::run`].
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Go to a folder in the current tab. "" means This PC. Accepts what a person types too
    /// ("c:", "C:/Users", "%USERPROFILE%", "This PC").
    Navigate(String),
    Back,
    Forward,
    /// Back or Forward that many places at once (XP's lists beside its Back and Forward).
    BackBy(usize),
    ForwardBy(usize),
    Up,
    Refresh,
    /// Open a new tab (Ctrl+T, "+") where the "New tabs open" setting says: the current folder,
    /// home or This PC.
    NewTab,
    /// Open a new tab showing this folder (middle-click in the sidebar, say).
    NewTabAt(String),
    CloseTab(usize),
    /// Open a copy of this tab (same folder, history and sort) right after it.
    DuplicateTab(usize),
    /// Close every tab except this one.
    CloseOtherTabs(usize),
    /// Bring back the tab closed last, where it was (Ctrl+Shift+T). The last few are kept.
    ReopenClosedTab,
    /// Show or hide hidden files and folders (Ctrl+H).
    ToggleHidden,
    SwitchTab(usize),
    NextTab,
    PrevTab,
    /// Open the selected items: a folder is entered, files open in their app (Enter).
    OpenSelected,
    /// Open one item of the current listing by its index in `tab.folder.entries`.
    OpenEntry(usize),
    /// Open the selected folders in new tabs (Ctrl+Enter, "Open in new tab"); selected files open
    /// in their apps. One folder: its tab is shown. Several: they open behind the current tab.
    OpenInNewTabs,
    /// Open this folder in a new tab behind the current one (middle-click on a folder in the list).
    NewTabBehind(String),
    /// Choose the app to open the selected file with (Windows' "Open with" window).
    OpenWith,
    /// Windows' Properties window for the selected items - or for the folder on screen when nothing
    /// is selected (Alt+Enter). The window runs it (shellmenu.rs).
    Properties,
    /// Open a terminal in this folder (Ctrl+` and "Open in terminal"): Windows Terminal if it's
    /// installed, otherwise PowerShell.
    OpenTerminal(String),
    /// Put the selected items' full paths on the clipboard.
    CopyPath,
    /// Open File Explorer with the selection (or the current folder) selected.
    ShowInExplorer,
    /// Put the selected items on the clipboard as files, to copy them (Ctrl+C) or to move them
    /// (cut, Ctrl+X) - pasting works in File Explorer too. See app/files.rs.
    CopyFiles,
    CutFiles,
    /// Paste files copied or cut (in Glint or File Explorer) into the folder the tab is in (Ctrl+V).
    Paste,
    /// Paste them into this folder instead (right-click on a folder > Paste into folder).
    PasteInto(String),
    /// Delete the selected items: to the Recycle Bin (Del), or for good with `permanent` (Shift+Del).
    Delete { permanent: bool },
    /// Open the name box on the selected item (F2). Handled by the file list's `begin_rename_selected`.
    BeginRename,
    /// Rename `from` in folder `dir` to `to` (what was typed in the name box).
    Rename { dir: String, from: String, to: String },
    /// Make "New folder" here, select it and open its name box (Ctrl+Shift+N).
    NewFolder,
    /// The same for "New Text Document.txt".
    NewTextFile,
    /// Pin this folder to the sidebar, or unpin it if it's pinned (Ctrl+D, "Pin to sidebar").
    TogglePin(String),
    /// Add the selected items to Home's Favorites, or take them out when they all are in
    /// ("Add to Favorites" / "Remove from Favorites").
    ToggleFavorites,
    /// Sort by this column; asking for the column that is already sorted reverses it.
    SortBy(SortKey),
    /// Sort by this column in this direction.
    SetSort(SortKey, bool),
    /// Turn the custom sort order on or off (SORT > Custom order).
    SetCustomSort(bool),
    /// Open the custom sort order's editor (SORT > Edit custom order…, Settings), or bring it to
    /// the front. Handled by the window.
    OpenSortEditor,
    /// Show only items whose name matches this (folder.rs `NameFilter`: several words, or a pattern
    /// like `*.pdf`). "" shows all. With Subfolders on, the subfolders are searched too.
    SetFilter(String),
    /// The "Subfolders" switch next to the filter (Ctrl+Shift+F): the filter searches subfolders.
    ToggleSubfolders,
    /// A search result's folder opens, with it selected ("Open file location").
    OpenFileLocation,
    /// Show or hide the sidebar (Ctrl+B). Handled by the window.
    ToggleSidebar,
    /// Show or hide the sidebar (the Settings window's "Show the sidebar").
    ShowSidebar(bool),
    /// Show or hide the details panel on the right of the list (Alt+P). Handled by the window.
    ToggleDetails,
    /// Show or hide the details panel (the Settings window's "Show the details panel").
    ShowDetails(bool),
    /// Open or close the Space preview on the right of the list (Space). Handled by the window.
    TogglePreview,
    /// Close the Space preview (its × button, Esc).
    ClosePreview,
    /// The splitter between the list and the details panel or preview was dragged: the pane's new
    /// width in DIPs (0: back to the standard width). Handled by the window (side.rs).
    SideWidth(f32),
    /// Show or hide the status bar (the Settings window's "Show the status bar"). Handled by the
    /// window.
    ShowStatusBar(bool),
    /// Show the tab on screen this way (the VIEW button, Ctrl+Shift+1 to 6, right-click > View).
    SetView(ViewMode),
    /// Switch between the skin's dark and light versions (Ctrl+Shift+L).
    ToggleTheme,
    /// Light, dark, or following Windows' app mode. Handled by the window.
    SetTheme(ThemeMode),
    /// Open the Settings window (Ctrl+, and SETTINGS), or bring it to the front - at this page
    /// (settingswin::ABOUT for "About Glint"). Handled by the window.
    OpenSettings(Option<usize>),
    /// Open the skin customiser (Settings > Appearance > CUSTOMISE…), or bring it to the front.
    /// Handled by the window.
    OpenSkinEditor,
    /// Close the Settings window (the × on its tab, when it opens as a tab).
    CloseSettings,
    /// Switch the path bar to typing (Ctrl+L, F4). Handled by the toolbar's `begin_path_edit`.
    EditPath,
    /// Put the cursor in the filter box (Ctrl+F). Handled by the toolbar's `focus_filter`.
    FocusFilter,
    Minimize,
    ToggleMaximize,
    /// The close button: the main window hides in the background when "Keep Glint running in the
    /// background" is on, otherwise it closes (window.rs, life.rs).
    Close,
    /// Quit Glint completely, even in background mode (Ctrl+Q). Handled by the window.
    Quit,
    /// An extra window at the folder on screen (Ctrl+N). Handled by the window.
    NewWindow,
    /// Look for a new version of Glint now (Settings > About > CHECK FOR UPDATES), and show what
    /// was found. Handled by the window (update.rs).
    CheckForUpdates,
}

/// A native popup menu a part wants shown.
#[derive(Clone, Debug, Default)]
pub struct Menu {
    /// Your own number to tell your menus apart in `menu_pick`.
    pub tag: u32,
    /// Where the menu's top-left corner goes, in DIPs (client coordinates) - or its top-right
    /// corner when `right_aligned` (for buttons at the right edge of the window).
    pub at: Point,
    pub items: Vec<MenuItem>,
    pub right_aligned: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MenuItem {
    /// Returned in `menu_pick`. Use 1 and up (0 means "nothing picked").
    pub id: u32,
    /// The text; an empty label makes a separator line.
    pub label: String,
    pub checked: bool,
    pub disabled: bool,
    /// When not empty, the item opens this submenu (and `id` is not used). Picks in it come back
    /// through `menu_pick` like any other, so give its items ids unique within the whole menu.
    pub submenu: Vec<MenuItem>,
}

impl MenuItem {
    pub fn new(id: u32, label: &str) -> MenuItem {
        MenuItem { id, label: label.to_string(), ..Default::default() }
    }
    /// An item that opens a submenu ("When Glint starts, open ›").
    pub fn submenu(label: &str, items: Vec<MenuItem>) -> MenuItem {
        MenuItem { label: label.to_string(), submenu: items, ..Default::default() }
    }
    pub fn checked(mut self, on: bool) -> MenuItem {
        self.checked = on;
        self
    }
    /// Greyed out (and can't be picked) when `off`.
    pub fn disabled(mut self, off: bool) -> MenuItem {
        self.disabled = off;
        self
    }
    pub fn separator() -> MenuItem {
        MenuItem::default()
    }
}

/// Facts about the window that parts may need while handling an event.
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    /// The main window (for creating child controls such as text boxes).
    pub hwnd: HWND,
    /// DIPs to pixels: 1.0 at 96 DPI, 1.5 at 144 DPI. You rarely need it (the renderer scales).
    pub scale: f32,
    /// Seconds since the app started; use it for animations.
    pub now: f64,
    /// Whether the window is maximised (the title bar shows "restore" instead of "maximise").
    pub maximized: bool,
    /// Whether the window is the active (foreground) window.
    pub active: bool,
}

/// A part of the window. Every method except `layout`, `rect` and `paint` has a do-nothing default,
/// so a part only writes what it needs.
pub trait Component {
    /// You sit at `rect` now (called on resize, DPI change and theme change).
    fn layout(&mut self, rect: Rect, ctx: &Ctx, app: &AppState);
    /// Where you sit (as given by the last `layout`).
    fn rect(&self) -> Rect;
    /// Draw yourself. Only draw inside `rect()`. Called often: keep it cheap (no file-system calls).
    fn paint(&mut self, r: &mut Renderer, theme: &Theme, app: &AppState, ctx: &Ctx);

    /// For the title bar: what Windows should treat this point as. Others leave the default.
    fn hit_test(&self, _p: Point, _app: &AppState) -> Hit {
        Hit::Client
    }
    /// The mouse pointer to show over this point.
    fn cursor(&self, _p: Point, _app: &AppState) -> CursorKind {
        CursorKind::Arrow
    }
    fn mouse_move(&mut self, _ctx: &Ctx, _e: &MouseEvent, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// A button went down over you. Until it comes up you get every move and the `mouse_up`,
    /// even outside your rect (the window captures the mouse for you).
    fn mouse_down(&mut self, _ctx: &Ctx, _e: &MouseEvent, _app: &mut AppState) -> Effect {
        Effect::None
    }
    fn mouse_up(&mut self, _ctx: &Ctx, _e: &MouseEvent, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// The second click of a double-click (the first came as a normal `mouse_down`/`mouse_up`).
    fn double_click(&mut self, _ctx: &Ctx, _e: &MouseEvent, _app: &mut AppState) -> Effect {
        Effect::None
    }
    fn wheel(&mut self, _ctx: &Ctx, _e: &MouseEvent, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// The mouse left you (clear hover highlights).
    fn mouse_leave(&mut self, _ctx: &Ctx, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// A key the window did not use for a shortcut. Only the file list gets these today.
    fn key_down(&mut self, _ctx: &Ctx, _vk: u16, _mods: Mods, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// A typed character (for type-to-jump). Only the file list gets these today.
    fn char_input(&mut self, _ctx: &Ctx, _ch: char, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// One of the text boxes (see textbox.rs) changed, got Enter/Esc or lost focus. Every part gets
    /// every text event; check `ev.id` against your own boxes.
    fn text_event(&mut self, _ctx: &Ctx, _ev: &TextEvent, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// The person picked `item` (an id from your [`Menu`]) in the menu you asked for with `tag`.
    fn menu_pick(&mut self, _ctx: &Ctx, _tag: u32, _item: u32, _app: &mut AppState) -> Effect {
        Effect::None
    }
    /// Move animations forward to time `now` (seconds). Return true while still animating.
    fn tick(&mut self, _now: f64, _app: &mut AppState) -> bool {
        false
    }
    /// Files are being dragged over `p` (from File Explorer, another program or Glint itself):
    /// where would they go? None: they can't be dropped here.
    fn drop_target(&self, _p: Point, _app: &AppState) -> Option<DropTarget> {
        None
    }
    /// Show that a drop would go into `target` (one of your own answers to `drop_target`), or,
    /// with None, stop showing it. Return Redraw when the look changed.
    fn drop_hover(&mut self, _target: Option<&DropTarget>) -> Effect {
        Effect::None
    }
}

/// An ease-out animation of one number (fast start, gentle stop), e.g. a scroll offset.
#[derive(Clone, Copy, Debug, Default)]
pub struct Anim {
    from: f32,
    to: f32,
    start: f64,
    duration: f64,
    running: bool,
}

impl Anim {
    /// Head for `to`, starting from `current`, over `duration` seconds.
    pub fn go(&mut self, current: f32, to: f32, now: f64, duration: f64) {
        *self = Anim { from: current, to, start: now, duration, running: (to - current).abs() > 0.01 };
    }
    /// Where it should be at `now`; `None` once finished (then use [`Anim::target`]).
    pub fn value(&mut self, now: f64) -> Option<f32> {
        if !self.running {
            return None;
        }
        let t = ((now - self.start) / self.duration).clamp(0.0, 1.0) as f32;
        if t >= 1.0 {
            self.running = false;
            return Some(self.to);
        }
        let eased = 1.0 - (1.0 - t).powi(3); // cubic ease-out
        Some(self.from + (self.to - self.from) * eased)
    }
    pub fn target(&self) -> f32 {
        self.to
    }
    pub fn running(&self) -> bool {
        self.running
    }
    pub fn stop(&mut self) {
        self.running = false;
    }
}
