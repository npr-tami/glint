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
