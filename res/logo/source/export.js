// Export the chosen Glint logo: path-only SVG masters, icon PNGs at every Windows size, title bar PNGs.
// usage: node export.js <this folder> <output folder>   (needs Playwright; reads wm-path.json from outline.py)
const { chromium } = require('playwright');
const fs = require('fs');
const dir = process.argv[2], out = process.argv[3];
(async () => {
  const b = await chromium.launch();
  const p = await b.newPage({ viewport: { width: 800, height: 600 } });
  await p.goto('file://' + dir + '/blank.html');
  const wm = JSON.parse(fs.readFileSync(dir + '/wm-path.json'));
  const svgs = await p.evaluate((wm) => {
    const common = { pathD: wm.d, dotX: wm.centres[2], r: 0.24, dotY: 0.82, vb: '8 0 660 322' };
    const make = (html) => { const d = document.createElement('div'); d.innerHTML = html; document.body.append(d); return new XMLSerializer().serializeToString(d.firstElementChild); };
    return {
      'glint-wordmark-dark.svg': make(wordmark({ ...common })),
      'glint-wordmark-light.svg': make(wordmark({ ...common, edge: '#1d3a78', tint: '#4f8cff' })),
      'glint-star.svg': make(iconTile('bare', 256)),
      'glint-star-small.svg': make(iconTile('bare', 24).replace('width="24" height="24"', 'width="256" height="256"')),
    };
  }, wm);
  for (const [name, s] of Object.entries(svgs)) fs.writeFileSync(out + '/' + name, s.replace(/ class="[^"]*"/g, '').replace(/ data-[a-z]+="[^"]*"/g, ''));
  // icon sizes: the small star (fatter, no extra sparkle) up to 24 px
  for (const px of [16, 20, 24, 32, 40, 48, 64, 128, 256]) {
    const file = out + '/glint-star-' + (px <= 24 ? 'small' : '') + '.svg';
    const svg = fs.readFileSync(px <= 24 ? out + '/glint-star-small.svg' : out + '/glint-star.svg', 'utf8');
    await p.setContent(`<body style="margin:0;background:transparent">${svg.replace(/width="256" height="256"/, `width="${px}" height="${px}"`)}</body>`);
    await p.locator('svg').first().screenshot({ path: `${out}/icon-${px}.png`, omitBackground: true });
  }
  // wordmarks as PNG for each common display scale (title bar logo is 39 px tall at 100 %)
  for (const theme of ['dark', 'light']) for (const sc of [1, 1.25, 1.5, 1.75, 2, 2.5, 3]) {
    const svg = fs.readFileSync(`${out}/glint-wordmark-${theme}.svg`, 'utf8');
    const h = Math.round(39 * sc), w = Math.round(h * 660 / 322);
    await p.setContent(`<body style="margin:0;background:transparent">${svg.replace(/width="660" height="322"/, `width="${w}" height="${h}"`)}</body>`);
    await p.locator('svg').first().screenshot({ path: `${out}/wordmark-${theme}-${Math.round(sc * 100)}.png`, omitBackground: true });
  }
  await b.close();
})();
