// Export the chosen Glint logo: path-only SVG masters, icon PNGs at every Windows size, title bar PNGs.
// usage: node export.js <this folder> <output folder>   (needs Playwright; reads wm-path.json for "glint"
// and wm-caps.json for "GLiNT" from outline.py, whichever are there)
const { chromium } = require('playwright');
const fs = require('fs');
const dir = process.argv[2], out = process.argv[3];
const read = (f) => (fs.existsSync(dir + '/' + f) ? JSON.parse(fs.readFileSync(dir + '/' + f)) : null);
// Each wordmark: its outlines, its file names and its view box (the capitals are wider).
const sets = [
  { wm: read('wm-path.json'), name: 'glint-wordmark', png: 'wordmark', w: 660 },
  { wm: read('wm-caps.json'), name: 'glint-wordmark-caps', png: 'wordmark-caps', w: 852 },
].filter((s) => s.wm);
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 1000, height: 600 } });
  await p.goto('file://' + dir + '/blank.html');
  const svgs = await p.evaluate((sets) => {
    const make = (html) => { const d = document.createElement('div'); d.innerHTML = html; document.body.append(d); return new XMLSerializer().serializeToString(d.firstElementChild); };
    const all = {
      'glint-star.svg': make(iconTile('bare', 256)),
      'glint-star-small.svg': make(iconTile('bare', 24).replace('width="24" height="24"', 'width="256" height="256"')),
    };
    for (const s of sets) {
      const common = { pathD: s.wm.d, dotX: s.wm.centres[2], r: 0.24, dotY: 0.82, vb: `8 0 ${s.w} 322` };
      all[s.name + '-dark.svg'] = make(wordmark({ ...common }));
      all[s.name + '-light.svg'] = make(wordmark({ ...common, edge: '#1d3a78', tint: '#4f8cff' }));
    }
    return all;
  }, sets);
  for (const [name, s] of Object.entries(svgs)) fs.writeFileSync(out + '/' + name, s.replace(/ class="[^"]*"/g, '').replace(/ data-[a-z]+="[^"]*"/g, ''));
  // icon sizes: the small star (fatter, no extra sparkle) up to 24 px
  for (const px of [16, 20, 24, 32, 40, 48, 64, 128, 256]) {
    const svg = fs.readFileSync(px <= 24 ? out + '/glint-star-small.svg' : out + '/glint-star.svg', 'utf8');
    await p.setContent(`<body style="margin:0;background:transparent">${svg.replace(/width="256" height="256"/, `width="${px}" height="${px}"`)}</body>`);
    await p.locator('svg').first().screenshot({ path: `${out}/icon-${px}.png`, omitBackground: true });
  }
  // wordmarks as PNG for each common display scale (title bar logo is 39 px tall at 100 %)
  for (const s of sets) for (const theme of ['dark', 'light']) for (const sc of [1, 1.25, 1.5, 1.75, 2, 2.5, 3]) {
    const svg = fs.readFileSync(`${out}/${s.name}-${theme}.svg`, 'utf8');
    const h = Math.round(39 * sc), w = Math.round(h * s.w / 322);
    await p.setContent(`<body style="margin:0;background:transparent">${svg.replace(new RegExp(`width="${s.w}" height="322"`), `width="${w}" height="${h}"`)}</body>`);
    await p.locator('svg').first().screenshot({ path: `${out}/${s.png}-${theme}-${Math.round(sc * 100)}.png`, omitBackground: true });
  }
  await b.close();
})();
