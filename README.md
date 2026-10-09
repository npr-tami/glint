# Glint

Glint is a fast, lightweight file explorer for Windows 10 and 11. It draws everything itself with
Direct2D, so it opens instantly, stays out of your way and needs nothing installed: it's a single
`Glint.exe` of a few megabytes.

<!-- Screenshots: coming soon -->

## What it does

- **Tabs** that come back next time, a **sidebar** with Quick access, pinned folders and your
  drives, and a clickable path you can also type into (`~`, `%VARIABLES%` and `shell:` places work).
- **A details panel** (Alt+P) and a **Space preview** for pictures (zoom and pan), text files and
  more.
- **View modes**: extra large, large and medium icons with thumbnails, list, details and a tree.
- **Working with files**, as in File Explorer: copy, cut, paste, rename, delete and the Recycle
  Bin, all through Windows' own copy engine (so Ctrl+Z in File Explorer still works), drag and drop
  to and from other apps, and right-click menus with other apps' items.
- **A filter that searches as you type**, also through every subfolder.
- **Skins**: AMD Radeon, NVIDIA and their light versions, or your own, made in the built-in
  **skin customiser** or in Notepad (skins are small `.glint` text files).
- **Custom sort order**: write your own order, like `-folder_first -newest_first -image`, or rules for
  one folder with `in Downloads:`, with **suggestions as you type** and a live preview.
- **Your default file explorer**, if you like: folders, drives and Win+E open in Glint.
- **Background mode**: closing the window keeps Glint ready in the tray, so the next window opens
  instantly. It can start with Windows too.
- **Updates itself** from GitHub Releases: it tells you when there's a new version and puts it in
  place when you say so.

## Download

Get `Glint.exe` from the [Releases page](https://github.com/npr-tami/glint/releases). It's a single
file: put it in a folder of its own and run it. Windows may warn that it doesn't know the program
("Windows protected your PC": More info, then Run anyway).

**Portable mode:** make an empty folder called `config` next to `Glint.exe` (or switch on Settings >
About > Portable), and Glint keeps its settings, cache and logs next to itself, so the whole folder
can go on a USB stick.

## Build it yourself

1. Install Rust from [rustup.rs](https://rustup.rs) with the default (MSVC) toolchain, and the
   [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/) with
   **"Desktop development with C++"**.
2. Build:

   ```
   git clone https://github.com/npr-tami/glint
   cd glint
   cargo build --release
   ```

3. The program is `target\release\glint.exe`.

Cross-compiling from Linux works too, with mingw-w64 installed:

```
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## Where it keeps settings

| What | Normally | Portable |
|---|---|---|
| Settings (`settings.json`) and your skins | `%APPDATA%\Glint\config` | `config\` next to Glint.exe |
| Cache | `%LOCALAPPDATA%\Glint\cache` | `cache\` next to Glint.exe |
| Error log | `%LOCALAPPDATA%\Glint\logs` | `logs\` next to Glint.exe |

Settings > About shows where they are, with a button to open the folder.

## Making Glint your default file explorer

In **Settings > General**, turn on **Open folders and drives in Glint** (and, if you like,
**Open with Win+E**). Folders and drives you double-click then open in Glint; Control Panel,
phones and similar still open in File Explorer. Only your own Windows account is changed, and no
admin rights are needed. Keep Glint.exe where it is; if you move it, start it once from the new
place.

You can also use the two `.bat` files in this repository: put them next to `Glint.exe` and run
**Make Glint default.bat**.

**To undo it**, turn the settings off again, or run **Restore File Explorer.bat** (it works even
if Glint.exe is gone). Whatever was there before is put back.

## Status: beta

Glint is in beta (0.x): it's used every day, but expect rough edges. Bug reports and ideas are
welcome in [Issues](https://github.com/npr-tami/glint/issues).

The most useful keyboard shortcuts:

| Keys | What they do |
|---|---|
| Ctrl+T, Ctrl+W, Ctrl+Shift+T | New tab, close tab, reopen closed tab |
| Ctrl+Tab, Ctrl+1…9 | Next tab, go to a tab |
| Alt+Left / Alt+Right, Alt+Up | Back / forward, up a folder |
| Ctrl+L (or F4, Alt+D) | Type a path |
| Ctrl+F (or F3), Ctrl+Shift+F | Filter, search subfolders too |
| Ctrl+C, Ctrl+X, Ctrl+V | Copy, cut, paste |
| Del, Shift+Del, F2 | Delete, delete for good, rename |
| Ctrl+Shift+N | New folder |
| Ctrl+Shift+C | Copy the selected paths as text |
| Space, Alt+P | Preview, details panel |
| Ctrl+Shift+1…6 | Views: extra large, large, medium icons, tree, list, details |
| Ctrl+B, Ctrl+D, Ctrl+H | Sidebar, pin the folder, hidden files |
| Ctrl+Shift+L | Switch between the skin's light and dark versions |
| Ctrl+` | Open a terminal here |
| Ctrl+N, Ctrl+Q | New window, quit Glint |
| Ctrl+, | Settings |

## License

[MIT](LICENSE)
