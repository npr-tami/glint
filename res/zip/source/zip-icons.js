// The icons zip and 7z files show when they open in Glint (Settings > Appearance > Zip icon): the red
// label (a white page with a red ZIP or 7Z label), and a zipped chrome folder and page. Each is drawn
// in 256 x 256 design units at any pixel size; 24 px and below get a simpler drawing that stays
// readable. Needs ../../logo/source/glint.js (grad, bevel, shadow, sparkle, starD, CHROME, uid) and,
// for the label, WORDS: words.json, the logo font's outlines of "ZIP" and "7Z".
const RIM = [[0, '#ffffff'], [.5, '#8d9ab0'], [1, '#e9eef6']];
const INK = '#0a0d15';
const TEETH = [[0, '#ffffff'], [.5, '#b8c3d4'], [1, '#6c7890']];
const RED = [[0, '#ff5a5f'], [.45, '#e3262d'], [1, '#a3141a']];

// Where a path's ink sits (needs a page to measure in).
function bbox(d) {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  s.innerHTML = `<path d="${d}"/>`;
  document.body.append(s);
  const b = s.firstChild.getBBox();
  s.remove();
  return b;
}

// kind: 'label', 'folder' or 'page'; ext: 'zip' or '7z' (only the label differs).
function zipIcon(kind, px, ext = 'zip') {
  return kind === 'label' ? labelIcon(px, ext) : zippedIcon(kind, px);
}

// ---------------------------------------------------------------- the red label

// A vertical zipper on x from y0 to y1: the tape, and teeth from both sides meeting in the middle.
function zipperV(n, x, y0, y1, o = {}) {
  const w = o.w || 30, tape = o.tape || '#11141c';
  let s = `<rect x="${x - w / 2}" y="${y0}" width="${w}" height="${y1 - y0}" fill="${tape}"/>`;
  if (o.tiny) {
    for (let y = y0 + 16; y < y1 - 10; y += 30) s += `<rect x="${x - 9}" y="${y}" width="18" height="11" fill="#e9eef6"/>`;
    return s;
  }
  for (let y = y0 + 3, i = 0; y < y1 - 4; y += 7, i++)
    s += `<rect x="${i % 2 ? x - 4 : x - 11}" y="${y}" width="15" height="5" rx="2" fill="url(#zt${n})"/>`;
  return s;
}

// A zipper's slider at (x, y) with its tab hanging down, k times the standard size.
function slider(n, x, y, k = 1) {
  const fill = `url(#zp${n})`;
  const body = `M${x - 17 * k},${y} H${x + 17 * k} Q${x + 20 * k},${y} ${x + 19 * k},${y + 4 * k} L${x + 13 * k},${y + 24 * k} Q${x + 12 * k},${y + 28 * k} ${x + 8 * k},${y + 28 * k} H${x - 8 * k} Q${x - 12 * k},${y + 28 * k} ${x - 13 * k},${y + 24 * k} L${x - 19 * k},${y + 4 * k} Q${x - 20 * k},${y} ${x - 17 * k},${y} Z`;
  const tab = `<rect x="${x - 12 * k}" y="${y + 18 * k}" width="${24 * k}" height="${52 * k}" rx="${11 * k}" fill="${fill}" stroke="${INK}" stroke-width="${3.5 * k}"/>
      <rect x="${x - 5 * k}" y="${y + 46 * k}" width="${10 * k}" height="${16 * k}" rx="${5 * k}" fill="${INK}"/>`;
  return `<g filter="url(#zd${n})">${tab}<path d="${body}" fill="${fill}" stroke="${INK}" stroke-width="${3.5 * k}" stroke-linejoin="round"/>
    <rect x="${x - 9 * k}" y="${y + 6 * k}" width="${18 * k}" height="${4 * k}" rx="${2 * k}" fill="#fff" opacity=".7"/></g>`;
}

function labelIcon(px, ext) {
  const n = ++uid, tiny = px <= 24;
  const page = 'M58,14 H160 L214,68 V230 Q214,242 202,242 H58 Q46,242 46,230 V26 Q46,14 58,14 Z';
  const fold = 'M160,14 V56 Q160,68 172,68 H214 Z';
  const defs = `${grad('zt' + n, TEETH, 1, 0)}${grad('zp' + n, CHROME.cool)}
    ${shadow('zd' + n, tiny ? 0 : 3, tiny ? 0.01 : 3, '#000', tiny ? 0 : .45)}${shadow('zs' + n, tiny ? 0 : 5, tiny ? 0.01 : 6, '#000', tiny ? 0 : .5)}
    ${grad('zf' + n, [[0, '#ffffff'], [1, '#e2e6ee']])}${grad('zo' + n, [[0, '#f4f6fa'], [1, '#b9c1cf']], 1, 1)}
    ${grad('zl' + n, RED)}<clipPath id="zc${n}"><path d="${page}"/></clipPath>`;
  const edge = tiny ? 10 : 4;
  const label = tiny ? `<rect x="30" y="132" width="172" height="80" rx="10" fill="url(#zl${n})"/>`
    : `<path d="M30,206 H46 V220 Z" fill="#6b0c10"/>
      <rect x="30" y="140" width="164" height="66" rx="10" fill="url(#zl${n})"/>
      <rect x="38" y="145" width="148" height="3" rx="1.5" fill="#fff" opacity=".45"/>`;
  let text = '';
  if (!tiny) {
    const w = WORDS[ext === '7z' ? '7Z' : 'ZIP'], b = bbox(w.d), k = (ext === '7z' ? 82 : 112) / b.width;
    const tx = 112 - (b.x + b.width / 2) * k, ty = 173 - (b.y + b.height / 2) * k;
    text = `<path d="${w.d}" transform="translate(${tx},${ty}) scale(${k})" fill="#fff"/>`;
  }
  const body = `<g filter="url(#zs${n})"><path d="${page}" fill="url(#zf${n})" stroke="#3a4152" stroke-width="${edge}" stroke-linejoin="round"/></g>
    <g clip-path="url(#zc${n})">${zipperV(n, 112, 14, 242, { tiny, w: tiny ? 34 : 26 })}</g>
    <path d="${fold}" fill="url(#zo${n})" stroke="#3a4152" stroke-width="${edge}" stroke-linejoin="round"/>
    ${tiny ? '' : slider(n, 112, 28, 0.8)}
    ${label}${text}
    ${tiny ? '' : sparkle(214, 40, 22, '#cfe9ff', false, '#1d3a78')}`;
  return `<svg width="${px}" height="${px}" viewBox="0 0 256 256"><defs>${defs}</defs>${body}</svg>`;
}

// ---------------------------------------------------------------- the zipped folder and page

// A chrome folder or page in the logo's style, with a zipper down it.
function zippedIcon(kind, px) {
  const n = ++uid, tiny = px <= 24;
  const rimW = tiny ? 18 : 11, inkW = tiny ? 9 : 5.5;
  const defs = `<defs>${grad('zf' + n, CHROME.cool)}${grad('zr' + n, RIM)}
    ${grad('zb' + n, [[0, '#dfe6f1'], [.18, '#8f9cb3'], [.4, '#4d596f'], [1, '#2a3244']])}
    ${grad('zt' + n, TEETH, 1, 0)}
    ${bevel('zv' + n, 3.2, 7, 0.85)}${shadow('zs' + n, tiny ? 0 : 5, tiny ? 0.01 : 6, '#000', tiny ? 0 : .55)}</defs>`;
  const chrome = (d, fill) => `<path d="${d}" fill="none" stroke="url(#zr${n})" stroke-width="${rimW}" stroke-linejoin="round"/>
    <path d="${d}" fill="none" stroke="${INK}" stroke-width="${inkW}" stroke-linejoin="round"/>
    <path d="${d}" fill="${fill}" ${tiny ? '' : `filter="url(#zv${n})"`}/>`;
  // The zipper: a dark track with silver teeth from y0 to y1 round x, and its pull near the top.
  const zipper = (x, y0, y1, pullY) => {
    const w = tiny ? 44 : 24;
    let z = `<rect x="${x - w / 2}" y="${y0}" width="${w}" height="${y1 - y0}" fill="${INK}"/>`;
    if (tiny) {
      for (let y = y0 + 22; y < y1 - 14; y += 36) z += `<rect x="${x - 14}" y="${y}" width="28" height="14" fill="#e9eef6"/>`;
      return z;
    }
    for (let y = y0 + 6, i = 0; y < y1 - 8; y += 9, i++)
      z += `<rect x="${i % 2 ? x : x - 9}" y="${y}" width="9" height="5" rx="1" fill="url(#zt${n})"/>`;
    z += `<g><rect x="${x - 13}" y="${pullY}" width="26" height="44" rx="8" fill="url(#zf${n})" stroke="${INK}" stroke-width="4"/>
      <rect x="${x - 4.5}" y="${pullY + 9}" width="9" height="22" rx="4" fill="${INK}"/></g>`;
    return z;
  };
  let body;
  if (kind === 'folder') {
    const back = 'M30,62 Q30,48 44,48 H96 Q105,48 111,55 L121,68 H212 Q226,68 226,82 V200 H30 Z';
    const front = 'M20,104 Q20,92 32,92 H224 Q236,92 236,104 L227,208 Q226,220 214,220 H42 Q30,220 29,208 Z';
    body = `<g filter="url(#zs${n})">${chrome(back, `url(#zb${n})`)}${chrome(front, `url(#zf${n})`)}</g>
      <clipPath id="zc${n}"><path d="${front}"/></clipPath>
      <g clip-path="url(#zc${n})">${zipper(128, 92, 220, 104)}</g>`;
  } else {
    const page = 'M58,18 H158 L214,74 V226 Q214,238 202,238 H58 Q46,238 46,226 V30 Q46,18 58,18 Z';
    const fold = 'M158,18 V62 Q158,74 170,74 H214 Z';
    body = `<g filter="url(#zs${n})">${chrome(page, `url(#zf${n})`)}${chrome(fold, `url(#zb${n})`)}</g>
      <clipPath id="zc${n}"><path d="${page}"/></clipPath>
      <g clip-path="url(#zc${n})">${zipper(112, 18, 238, 34)}</g>`;
  }
  // (A dark edge, so the sparkle shows on a light background too.)
  const spark = tiny ? '' : sparkle(kind === 'folder' ? 210 : 218, 40, 24, '#cfe9ff', false, '#1d3a78');
  return `<svg width="${px}" height="${px}" viewBox="0 0 256 256">${defs}${body}${spark}</svg>`;
}
