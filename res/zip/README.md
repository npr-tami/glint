# Zip and 7z icons

What zip and 7z files look like in Windows while they open in Glint (Settings > Appearance > Zip icon).
The four `.ico` files are baked into glint.exe (`../glint.rc`, icons 2 to 5), and Glint points Windows
at the chosen one (`../../src/registry.rs`).

| File | What it is |
|---|---|
| `zip-label.ico` | Red label (the standard choice): a white page with a zipper and a red ZIP label. |
| `zip-label-7z.ico` | The same saying 7Z, for when 7z files are the only ones that open in Glint. |
| `zip-folder.ico` | Folder: a chrome folder with a zipper, in the logo's style. |
| `zip-page.ico` | Page: a chrome page with a zipper. |

Each holds every Windows icon size (16, 20, 24, 32, 40, 48, 64, 128, 256), each drawn at its own size
(24 px and below are simpler, so they stay readable), stored as PNGs to keep glint.exe small.

**To change them:** edit `source/zip-icons.js` (it uses the logo's `../logo/source/glint.js`), then:

```
node source/export-zip.js <this folder>    # needs Playwright (Chromium)
```

`source/words.json` holds the label's letters, ZIP and 7Z in the logo's font (Unbounded Black),
made with `../logo/source/outline.py`.
