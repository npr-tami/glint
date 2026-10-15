// Draw the zip and 7z icons at every Windows icon size and pack each into an .ico (every size a PNG).
// usage: node export-zip.js <res/zip folder>   (needs Playwright, with Chromium)
const { chromium } = require('playwright');
const fs = require('fs');
const out = process.argv[2], src = out + '/source';
const SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256];
// The file each icon goes to, and what draws it (res/glint.rc names the files).
const ICONS = [['zip-label.ico', 'label', 'zip'], ['zip-label-7z.ico', 'label', '7z'], ['zip-folder.ico', 'folder', 'zip'], ['zip-page.ico', 'page', 'zip']];

// An .ico holding PNGs: the header, one directory entry per size, then the PNGs.
function ico(pngs) {
  const head = Buffer.alloc(6 + 16 * pngs.length);
  head.writeUInt16LE(1, 2);
  head.writeUInt16LE(pngs.length, 4);
  let at = head.length;
  pngs.forEach(({ px, png }, i) => {
    const e = 6 + 16 * i;
    head.writeUInt8(px >= 256 ? 0 : px, e); // 0 means 256
    head.writeUInt8(px >= 256 ? 0 : px, e + 1);
    head.writeUInt16LE(1, e + 4); // planes
    head.writeUInt16LE(32, e + 6); // bits per pixel
    head.writeUInt32LE(png.length, e + 8);
    head.writeUInt32LE(at, e + 12);
    at += png.length;
  });
  return Buffer.concat([head, ...pngs.map((p) => p.png)]);
}

(async () => {
  const words = JSON.parse(fs.readFileSync(src + '/words.json'));
  const b = await chromium.launch();
  const p = await b.newPage();
  await p.goto('file://' + src + '/blank.html');
  for (const [file, kind, ext] of ICONS) {
    const pngs = [];
    for (const px of SIZES) {
      const svg = await p.evaluate(([words, kind, px, ext]) => {
        window.WORDS = words;
        const d = document.createElement('div');
        d.innerHTML = zipIcon(kind, px, ext);
        document.body.append(d);
        const s = new XMLSerializer().serializeToString(d.firstElementChild);
        d.remove();
        return s;
      }, [words, kind, px, ext]);
      await p.setContent(`<body style="margin:0;background:transparent">${svg}</body>`);
      pngs.push({ px, png: await p.locator('svg').first().screenshot({ omitBackground: true }) });
    }
    fs.writeFileSync(out + '/' + file, ico(pngs));
    console.log(file, fs.statSync(out + '/' + file).size, 'bytes');
  }
  await b.close();
})();
