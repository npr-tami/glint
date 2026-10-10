# How Glint is built

Glint is one Windows program written in Rust. It uses plain Win32 for its windows, Direct2D for
shapes and pictures, and DirectWrite for text, and it draws everything itself: the title bar, tabs,
toolbar, sidebar, file list, status bar, and the Settings window with its switches and drop-downs.
It uses no UI framework.

A few things are Windows' own on purpose: the text boxes (real EDIT controls, so typing, the caret
and input methods behave like the rest of Windows), popup menus (with Windows' own right-click items
merged in), file icons and thumbnails, the copy engine with its progress and conflict windows, drag
and drop, and the folder, colour and file pickers.

## How the pieces talk

- `main.rs` starts up and runs the message loop. `window.rs` owns the window, the renderer, the
  state (`AppState`) and the parts: title bar, toolbar, sidebar, file list, status bar and the
  pane on the right (details panel or Space preview).
- Each part implements `ui::Component`: it lays itself out, paints, and gets mouse, key and text-box
  events. It answers with an `Effect` (redraw, animate, a `Command`, a menu, a drag), and the window
  carries it out. Parts never touch the window directly.
- Commands about folders, tabs and files go to `AppState::run` (`app.rs`).
- The disk is never touched on the UI thread. Folder reads, drives, icons, thumbnails, searches,
  file jobs, the folder watch, Windows' menu items and settings saves all run on background
  threads, and send their results back through a channel plus a `PostMessage` that wakes the
  window. A result for something already left behind is dropped.
- Nothing animates unless something moves: an idle Glint uses no CPU.

## Files

All code is in `src/`.

| File | What it does |
|---|---|
| `main.rs` | Start-up (DPI, COM, the panic hook, command-line switches, becoming the main Glint, reading the settings, the first tabs) and the message loop. |
| `window.rs` | The main window: creates it, owns the parts and the state, routes Windows' messages to the parts, carries out their effects, opens menus, saves settings, hides in the background. |
| `window/drag.rs` | The window's side of drag and drop: where a drop would go, the highlight, scrolling near the edges, the drop's plan, dragging out. |
| `window/sorting.rs` | The window's side of the sort editor: opening it, keeping it docked beside the window, handing it its messages. |
| `ui.rs` | The contract between the window and its parts: `Component`, `Effect`, `Command`, `Menu`, `Rect`/`Point`, easing. |
| `frame.rs` | The custom window frame: no Windows caption, but edge resizing, snapping, the shadow, rounded corners and dark menus. |
| `render.rs` | The renderer: Direct2D target, brushes, shapes, icons, pictures, and DirectWrite text with a layout cache. |
| `theme.rs` | `Theme`: the skin in use as plain values (colours by role, shapes, sizes, font) plus fixed sizes in DIPs. |
| `skin.rs` | Skins as text files: the format, the built-in skins (`skins/*.glint`), the person's own skins (list, save, rename, delete, export) and the skins folder's watch. |
| `skinwin.rs` | The skin customiser window (Settings > Appearance > CUSTOMISE…). |
| `logo.rs` | The title bar logo: the GLINT box, the chrome wordmark (embedded PNGs), or nothing. |
| `titlebar.rs` | The logo, tabs and their menu, "+", and the minimise / maximise / close buttons. |
| `toolbar.rs` | Sidebar switch, back / forward / up / refresh, the path, the filter box, and the SORT, VIEW and SETTINGS buttons. |
| `pathbar.rs` | The clickable path, plus the shared box shapes (slanted, straight, rounded). |
| `sidebar.rs` | Quick access, Pinned and This PC (drives with usage bars), their menus and drop targets. |
| `list.rs` | The virtualised file list in every view: header, rows, tiles, tree arrows, selection, keys, scrolling, columns, renaming, dragging. |
| `views.rs` | Where items go in each view (rows, columns, tiles) and how the arrow keys move. |
| `status.rs` | The status bar, the notes box when it's off, and the memory figure. |
| `side.rs` | The pane right of the list: details panel or Space preview, and its splitter. |
| `details.rs` | The details panel (Alt+P): preview, name, chips, dates, and its buttons. |
| `preview.rs` | The Space preview: pictures with zoom and pan, text with line numbers, other files' details. |
| `peek.rs` | Looking inside files off the UI thread: picture size, media length, folder counts, text, full-size pictures. |
| `thumbs.rs` | Thumbnails for the panel, the preview and the icon views, made on a background thread and kept in an LRU cache. |
| `icons.rs` | File, drive and Recycle Bin icons, type names, and which extensions Windows knows. |
| `app.rs` | The state: tabs, history, listings, selection, sorting, the filter and subfolder search, navigation, background reads and the folder watch. |
| `app/files.rs` | File commands: copy, cut, paste, delete, rename, new folder and text file. |
| `app/places.rs` | Pinned folders and the Quick access places shown in the sidebar. |
| `app/bin.rs` | The Recycle Bin's commands and its watch. |
| `app/dropping.rs` | Files dropped onto Glint: copy, move, shortcuts, delete, pin. |
| `app/life.rs` | Hiding in the background, coming back, and folders sent from outside. |
| `app/tree.rs` | The tree view: open folders in a tab, their items, their watches, switching views. |
| `app/skins.rs` | Using a skin: picking one, light and dark versions, applying changes. |
| `folder.rs` | The disk: reading folders, drives, sorting, the filter's rules, formatting sizes and dates, path helpers. |
| `search.rs` | Searching subfolders for the filter text on a thread of its own. |
| `sortrecipe.rs` | The custom sort order language: groups, `_first` / `_last`, `in <folder> (` … `)` (or the older `in <folder>:`), typo hints. |
| `complete.rs` | Suggestions while typing a sort order. |
| `sortwin.rs` | The sort editor window, with its live preview and presets. |
| `watch.rs` | Watching the folder on screen (ReadDirectoryChangesW), folding bursts of changes into one. |
| `recycle.rs` | The Recycle Bin: listing, restoring, deleting for good, emptying, and its watch. |
| `fileops.rs` | File jobs through Windows' copy engine, the clipboard, name checks. |
| `dragdrop.rs` | OLE drag and drop: the drop target, the drop menu, dragging items out. |
| `ask.rs` | The question box ("Delete 3 items permanently?"). |
| `textbox.rs` | The EDIT control wrapper for the path, filter, name and folder boxes. |
| `menu.rs` | The file list's right-click menus and which of Windows' items they keep. |
| `popup.rs` | Showing those menus, with Windows' items merged in. |
| `shellmenu.rs` | Windows' own right-click menu (IContextMenu) on a thread of its own. |
| `shell.rs` | Doors into Windows: open, Open with, the terminal, the clipboard, known folders, `shell:` places, light or dark. |
| `settings.rs` | settings.json and Glint's folder with config / cache / logs (and portable mode, which moves it). |
| `settingswin.rs` | The Settings window. |
| `settingswin/pages.rs` | The Settings window's pages, row by row, each bound to a setting. |
| `kit.rs` | Controls drawn by Glint for its own windows: switch, drop-down, button, card, scroller. |
| `life.rs` | How Glint runs: one main Glint, extra windows, background mode, command-line switches. |
| `registry.rs` | Default file explorer, Win+E and Start with Windows in the registry, per user. |
| `tray.rs` | The tray icon while Glint is hidden. |
| `winekey.rs` | Catching Win+E while Glint runs. |
| `update.rs` | Looking for new versions and putting them in place. |
| `updatewin.rs` | The update prompt window. |
| `log.rs` | The timing log and the error log, with the crash report. |

Outside `src/`:

- `build.rs` bakes `res/glint.rc` (icon, manifest, version info) into the exe, with rc.exe (MSVC)
  or windres (MinGW).
- `res/` holds the icon, the manifest and the logo (`res/logo/README.md` says how to rebuild it).
- `skins/` holds the built-in skins, baked into the exe.

## Testing

`cargo test` runs the unit tests (on Windows, or under Wine when cross-compiling). Useful
environment variables (most are listed at the top of `main.rs`): `GLINT_SETTINGS_DIR` keeps settings
elsewhere, `GLINT_TEST_REGISTRY` puts registry changes under a test key, `GLINT_TEST_BIN` uses a
pretend Recycle Bin, and `GLINT_FRAME_LOG`, `GLINT_WATCH_LOG` and `GLINT_THUMB_LOG` log timings.
