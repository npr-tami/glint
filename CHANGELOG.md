# What changed in Glint

The newest version is at the top.

**Version numbers** are MAJOR.MINOR.PATCH (semantic versioning):
- **PATCH** (0.8.1): bug fixes only, nothing new.
- **MINOR** (0.9.0): new features; what worked before keeps working.
- **MAJOR** (1.0.0): changes that break something that worked before.

Glint stays at 0.x while it's in beta.

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
