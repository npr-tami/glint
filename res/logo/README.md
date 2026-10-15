# Glint logo

Glint's "liquid chrome" logo. `glint.ico` is the exe's icon (`../glint.rc`), and the 14 wordmark PNGs are baked into
the exe for the title bars (`../../src/logo.rs`; a skin with its own `logo-text` word gets a box with that word
instead).

| File | What it is |
|---|---|
| `glint-wordmark-dark.svg` | The chrome "glint" wordmark for dark skins. The dot on the i is a white sparkle (the glint). |
| `glint-wordmark-light.svg` | The same for light skins: the sparkle turns blue with a dark edge, so it doesn't vanish on white. |
| `glint-wordmark-caps-{dark,light}.svg` | The same in capitals, "GLiNT" (the Logo setting's GLiNT), with the sparkle over the small i. |
| `glint-star.svg` | The app icon: a chrome four-point star with a small sparkle, with no tile behind it. |
| `glint-star-small.svg` | The icon for 24 px and below: a fatter star, no small sparkle, so it stays readable. |
| `glint.ico` | Every Windows icon size (16, 20, 24, 32, 40, 48, 64, 128, 256), each drawn at its own size. |
| `wordmark-{dark,light}-{100..300}.png` | The wordmark for the title bar (39 px tall at 100 %), one per display scale. |
| `wordmark-caps-{dark,light}-{100..300}.png` | The same for GLiNT (wider: 103 x 39 at 100 %). |

The SVGs are plain outlines, so no font is needed to open them. The letters are Unbounded Black (SIL Open Font
Licence), turned into outlines; a logo made from an OFL font is fine to use anywhere.

**To change the logo:** edit `source/glint.js` (gradients, outline widths, sparkle), then rebuild:

```
python source/outline.py Unbounded[wght].ttf glınt 200 40 230 > wm-path.json   # pip install fonttools uharfbuzz
python source/outline.py Unbounded[wght].ttf GLıNT 200 40 230 > wm-caps.json   # GLiNT (leave out what you don't remake)
copy the .json files, source/glint.js and source/blank.html into one folder, then:
node source/export.js <that folder> <output folder>                                # needs Playwright (Chromium)
magick icon-16.png icon-20.png icon-24.png icon-32.png icon-40.png icon-48.png icon-64.png icon-128.png icon-256.png glint.ico
```

(`glınt` is written with a dotless ı on purpose: the sparkle takes the dot's place.)
