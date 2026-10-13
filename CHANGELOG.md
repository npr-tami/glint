# What changed in Glint

The newest version is at the top.

**Version numbers** look like **1.0.12-beta.2** while Glint is in beta:
- **1.0** stays the same through the betas.
- **12** is the release: it goes up by one with every new release, features and fixes alike.
- **-beta.2** is the build of that release: a rebuild of the same release (a quick fix to it)
  only bumps this number.

Each new number counts as newer than the one before it (1.0.12-beta.1, 1.0.12-beta.2,
1.0.13-beta.1, ...), so the updater offers them in that order. Versions up to 0.11.0 used
MAJOR.MINOR.PATCH, where a new feature bumped the middle number.

## 1.0.18-beta.2 (2026-10-13): Quick fix

### Fixes
- The skins and Settings windows no longer look stretched after you resize them or Glint.

## 1.0.18-beta.1 (2026-10-13): Settings beside Glint

### New
- Settings now opens beside Glint, docked like the sort window, so your files stay in view.
- Pick how it opens: Settings > Appearance > Settings opens (Docked, Small in the middle, Tab or Last position).
- The skin editor's text view now works like a code editor: line numbers, coloured settings and a colour square by each colour.
- Start typing a setting and a list shows what fits, then what it can be. Tab or Enter takes one.
- The line you're on says what it does, or what's wrong with it. Click a problem to jump to its line.
- GUIDE under the text shows a short how-to with an example.
- Skin colours are now grouped: Main colours, Title bar, Tabs, Toolbar, Sidebar, File list and more.
- Wallpaper zoom: wallpaper-zoom: 100 zooms in, -100 zooms out (-500 to 500, 0 is normal).
- wallpaper-position can now also say top, middle or bottom, like: right bottom.

### Fixes
- wallpaper-fit: whole now always shows the entire picture, whatever its size, instead of cutting off wide pictures.
- The skins window now sits right against Glint, with no gap.
- "Light or dark" is greyed out for skins that have no light or dark version.
- Moving with the arrow keys in the sort and skin editors no longer pops up suggestions.

## 1.0.17-beta.1 (2026-10-12): wallpaper support for skins

### New
- Wallpaper support: a skin can now show a picture behind your files and Home.
- Choose how strongly it shows, which side stays in view, and whether it fills the list or sits whole at one side.
- A skin can live in a folder of its own, with its pictures next to it. Drop the folder into the skins folder and the skin shows up.

### How to add a wallpaper
- Open SETTINGS > Appearance > CUSTOMISE… and pick your skin (for a built-in one, click Duplicate first).
- Click Open skins folder and put your picture in it.
- Tick Show as text and add a line with the picture's name, like: wallpaper: my picture.png
- Optional: wallpaper-opacity: 50 (0 to 100), wallpaper-position: left, middle or right, and wallpaper-fit: whole to show the whole picture at one side.

## 1.0.16-beta.1 (2026-10-12): new Home, video preview, and an easier sort window

### New
- Home looks like File Explorer's: your folders at the top, then Recent files and Favorites.
- Right-click any file or folder and pick Add to Favorites to put it on Home.
- Prefer the old Home? SETTINGS > Home opens can switch it back to your user folder.
- Space preview now plays GIFs, and videos too (press play).
- Videos start with the sound fading in, and Glint remembers your volume.
- The "i" button in the preview shows a picture's or video's details.
- Each folder remembers its view, like File Explorer. Even after a restart.
- Skins can now look like older Windows (98, 2000, XP or 7), with icons from that time.

### Sort order window
- DETACH lets it float as its own window. DOCK puts it back.
- Line numbers, with a red dot on any line that has a mistake.
- Every mistake is listed in full. Click one to jump to its line.
- Clear tips for bracket mistakes, most with a one-click Fix.
- Faint words after your line say what it does.
- Add and_subfolders to a folder's rules so folders inside it follow them too.
- Start typing a folder name and it suggests the full path.
- Clicking a column sorts only that folder, until you leave it.
- The window shows which rules sort the folder you're in.
- Ctrl+Z / Ctrl+Y undo and redo, even after closing the window.
- Right-click for cut, copy, paste and more. Ctrl+/ turns lines into notes.
- Alt+Up and Alt+Down move lines.

### Fixes
- Deleting on a USB stick or network drive now asks first (there's no Recycle Bin there).
- Typing letters to jump to a file keeps working when nothing matches.
- Renaming or moving a folder in Glint updates its sort rules.
- START FROM asks before replacing your own rules.

## 1.0.15-beta.1 (2026-10-11): the same rules for several folders

- **One set of rules for several folders**: in the custom sort order (SORT > Edit custom
  order..., or VIEW > Views for folders...), start with `in = (`, put each folder's full path on
  a line of its own, then the rules, and end with `)`. For example:

  ```
  in = (
  C:\Users\me\Pictures\Wallpapers
  ~\Documents\Photos
  large_icons
  newest_first
  )
  ```

  Every folder listed gets those rules, so they don't have to be written out again for each one.
  Paths can start with `~` (your home folder), use `/` or `\`, and have spaces. Typing a path
  suggests the folders in it, and the guide (GUIDE) has a line about it. `in Downloads (` and the
  other rules work as before.

## 1.0.14-beta.1 (2026-10-11): a simpler sort order window

- **The sort order window is simpler** (SORT > Edit custom order..., or VIEW > Views for
  folders...): the preview is gone, since the folder beside it already shows the result as you
  type, so the rules box gets the whole window. The guide stays hidden until you click GUIDE; it
  then shows at the bottom, starting with an example.

## 1.0.13-beta.1 (2026-10-11): a view for each folder

- **Folders can have a view of their own**: in the custom sort order (VIEW > Views for folders...,
  or SORT > Edit custom order...), put a view in a folder's rules, and that folder always opens
  that way, while the others keep the view you picked with VIEW. For example, a line
  `in Pictures (`, then `-large_icons` on the next line, then `)` on a line of its own shows
  Pictures with large icons.
  The views are `-details`, `-list`, `-tree`, `-medium_icons`, `-large_icons` and
  `-extra_large_icons` (spaces work too: `large icons`). A view written outside any folder's rules
  is for every folder without a view of its own. A folder's subfolders don't get its view. Views
  work even with the custom sort order switched off, since they don't sort anything.
- **Picking a view with VIEW in such a folder** is just for now: the folder's own view comes back
  the next time you open it. Elsewhere, VIEW changes the tab's view as before.

## 1.0.12-beta.1 (2026-10-11): new version numbers

- **Version numbers have a new form**: 1.0.12-beta.1 is the release after 0.11.0. Every new
  release raises the 12 by one (1.0.13-beta.1 next); a quick fix to the same release only raises
  the beta number (1.0.12-beta.2). Settings > About shows the whole number.

## 0.11.0 (2026-10-10): the sort order you write, WinRAR's submenu in Windows 11 menu mode

- **WinRAR's submenu is no longer empty** when WinRAR uses its Windows 11 context menu. The same
  goes for other apps whose Windows 11 menu items have a submenu.
- **The custom sort order keeps the order you write**: each thing goes on the first line that fits
  it. A line like `-updated` or `-png` no longer pulls things out of a line above it, so with
  `-image` above `-updated`, a picture changed today stays with the pictures (and with
  `-folder_first` above `-hidden_last`, hidden folders stay with the folders). Glint points out a
  line that can't get anything because the lines above take it all.
- **Lines stacked under a `_first` line go up with it**, in their order: with `-image`, then
  `-folder_first` and `-updated` right under it, folders come first and `-updated` second. Lines
  under a `_last` line go down with it, and Glint points them out. A blank line ends the stack (a
  line of notes doesn't): in an order written before, a blank line after the `_first` or `_last`
  line keeps the lines under it where they were.
- **A folder the custom sort order has no rules for sorts by its column**: with only `in …` rules
  (say for Downloads), other folders keep the column and direction you pick instead of A to Z, and
  clicking a column there no longer turns the custom order off for Downloads. Folders first in
  Settings can be changed while the custom order is on, since it still counts in those folders.
- **A folder's own rules go in brackets**: `in Downloads (`, its rules, then `)` on a line of its
  own. The lines after the `)` are for every folder again. `in Downloads:` still works and runs to
  the next `in` or the end. A missing, extra or misplaced bracket is marked, with a fix. A folder
  written twice gets the rules of both (before, the second was left out).

## 0.10.4 (2026-10-10): undo in the skin editor, one folder for Glint's files

- **Undo and redo in the skin editor**: UNDO and REDO buttons, or Ctrl+Z and Ctrl+Y.
- **Colours can be set to none**: each colour has a NONE button.
- **All of Glint's files are in one folder**: `%APPDATA%\Glint` (settings, skins, cache and logs),
  or `Glint data` next to Glint.exe when portable. Files from older versions move there by
  themselves.
- **Portable moves the files** from one place to the other when Glint next starts, instead of
  copying them and leaving `.old` and `config.off` copies behind (and works while Glint has its
  settings open, which it always does).
- About says Glint is a beta. The timing log in the temp folder starts again once it reaches 1 MB.

Fixes:

- Typing a letter like "ñ" or "ö" near the start of a line in the sort order window no longer
  closes Glint, and neither does a sort rule on a name whose extension has letters like "İ".
- A column clicked while the custom sort order is on sorts A to Z (or newest/biggest first), not
  backwards. The right-click menu's "Sort by" no longer checks a column then.
- Typing a file's path in the path bar keeps the Forward button working.
- The mouse's Back and Forward buttons wait while a question (like "Delete permanently?") is open.
- A row size picked in Settings keeps the same rows on screen.
- Alt+P says when the window is too narrow for the details panel.
- Right-dragging files a little within their own folder shows the menu ("Create shortcuts here").
- Copy path in This PC with nothing selected no longer empties the clipboard.
- Skin editor: undoing after a rename keeps the new name; undoing the Dark skin switch also
  switches "Light or dark" back; a new skin doesn't get a deleted skin's undo steps; UNDO is
  greyed out for a skin whose file is gone.

## 0.9.3 (2026-10-10): apps listed twice in the right-click menu

- **WinRAR (and other apps) in Windows 11 menu mode work again.** Such apps can show up twice in
  Windows' menu, and only one copy has its submenu filled; Glint now keeps that one instead of
  always the first.
- The log notes which menu items were left out and what each submenu had when it opened.

## 0.9.2 (2026-10-10): right-click submenus

- **Submenus from other apps open again on slower PCs** (WinRAR ›, Send to ›, Give access to ›…).
  Glint now waits longer for Windows to fill a submenu, and one slow submenu no longer stops all
  the others in the same menu from opening.

## 0.9.1 (2026-10-10): fixes

- **Updates**: Glint now finds the newest release by its version number, even when GitHub still
  marks an older one as "latest".
- **Right-click menus**: Windows' commands from other apps (WinRAR, Send to and the like) may
  finish their work after the click, as they can in File Explorer, and Glint keeps what they need
  a little longer.
- Glint notes in its log which of Windows' menu commands ran, to help find problems.

## 0.9.0 (2026-10-09): Chrome and Gunmetal

- **Two new skins: Chrome and Gunmetal.** Year 2000 chrome: shiny silver-blue (or dark
  gunmetal) bars, glass marbles for minimise, maximise and close, glossy liquid buttons, rounded
  tabs and sidebar panels. "Light or dark" switches between the two.
- **Skins can do more**: gradients for any colour, liquid buttons, marble window buttons, pill
  tabs, sidebar panels and a chrome scrollbar. Your own skins can use all of it.
- **Every colour is in the skin customiser** now, in one list with clear names, plus the line
  widths and a Style group.
- **Logo** can also be chosen in the skin customiser. "Slanted box" is now just "Box".
- OPEN SKINS FOLDER no longer closes the skin customiser.

## 0.8.0 (2026-10-09): first public beta

- **A fast, lightweight file explorer** that draws everything itself: a single Glint.exe with
  nothing to install, and a portable mode.
- **Tabs** that come back next time, a **sidebar** (Quick access, pinned folders, drives), a path
  you can click or type into, and a filter that can search subfolders too.
- **The details panel** (Alt+P) and the **Space preview** for pictures, text files and more.
- **Views**: extra large, large and medium icons with thumbnails, list, details and a tree.
- **Working with files** through Windows' own copy engine: copy, cut, paste, rename, delete, the
  Recycle Bin, drag and drop, and right-click menus with other apps' items.
- **Skins**: AMD Radeon, NVIDIA and their light versions, and your own, made in the skin customiser.
- **Custom sort order** with suggestions as you type and a live preview.
- **Default file explorer and Win+E**, if you want them, and a **background mode** with a tray
  icon and Start with Windows.
- **Updates** from GitHub Releases, when you say so.
