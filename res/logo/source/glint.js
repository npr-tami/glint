// Glint logo ideas: SVG builders shared by the mockup pages.
let uid = 0;

// A four-point sparkle centred on (cx, cy): tall arms rv, side arms rh, waist w (0..1 of the arms).
function starD(cx, cy, rv, rh, w) {
  const a = rv * w, b = rh * w;
  return `M${cx},${cy - rv} Q${cx + b},${cy - a} ${cx + rh},${cy} Q${cx + b},${cy + a} ${cx},${cy + rv} ` +
         `Q${cx - b},${cy + a} ${cx - rh},${cy} Q${cx - b},${cy - a} ${cx},${cy - rv}Z`;
}

const CHROME = {
  // Cool silver with a blue sky and a dark horizon (classic late-90s chrome).
  cool: [[0, '#ffffff'], [.2, '#e4edf9'], [.4, '#9cb0cc'], [.5, '#44526e'], [.535, '#121827'], [.57, '#3f4d68'],
         [.7, '#aebfd6'], [.84, '#f5f9ff'], [1, '#97a7bf']],
  // Silver with a Radeon-red horizon.
  red: [[0, '#ffffff'], [.22, '#eceef2'], [.42, '#a9adb7'], [.5, '#5b1015'], [.53, '#2a0507'], [.57, '#b3161d'],
        [.66, '#ff6a52'], [.8, '#f6f0ee'], [1, '#a4a4ab']],
  // Glassy sky blue for the Aero idea.
  aero: [[0, '#f4fdff'], [.35, '#9fe3ff'], [.5, '#33a9f0'], [.75, '#0f6fd0'], [1, '#62c8ff']],
};

function grad(id, stops, x2 = 0, y2 = 1) {
  return `<linearGradient id="${id}" x1="0" y1="0" x2="${x2}" y2="${y2}">` +
    stops.map(([o, c]) => `<stop offset="${o}" stop-color="${c}"/>`).join('') + `</linearGradient>`;
}

function bevel(id, blur, scale, k = 0.75) {
  return `<filter id="${id}" x="-20%" y="-20%" width="140%" height="140%">
    <feGaussianBlur in="SourceAlpha" stdDeviation="${blur}" result="b"/>
    <feSpecularLighting in="b" surfaceScale="${scale}" specularConstant="1.15" specularExponent="24" lighting-color="#fff" result="s">
      <feDistantLight azimuth="225" elevation="42"/></feSpecularLighting>
    <feComposite in="s" in2="SourceAlpha" operator="in" result="s2"/>
    <feComposite in="SourceGraphic" in2="s2" operator="arithmetic" k1="0" k2="1" k3="${k}" k4="0"/></filter>`;
}

function shadow(id, dy, blur, color, op) {
  return `<filter id="${id}" x="-20%" y="-30%" width="140%" height="160%">
    <feDropShadow dx="0" dy="${dy}" stdDeviation="${blur}" flood-color="${color}" flood-opacity="${op}"/></filter>`;
}

function glow(id, blur) {
  return `<filter id="${id}" x="-100%" y="-100%" width="300%" height="300%">
    <feGaussianBlur stdDeviation="${blur}" result="g"/><feMerge><feMergeNode in="g"/><feMergeNode in="SourceGraphic"/></feMerge></filter>`;
}

// The bright sparkle that is the "glint": white core, soft halo, a faint flare line.
function sparkle(cx, cy, r, tint = '#bfe4ff', flare = true, edge = '') {
  const n = ++uid;
  return `<g class="sparkle">
    <defs><radialGradient id="sp${n}"><stop offset="0" stop-color="#fff"/><stop offset="${edge ? .1 : .55}" stop-color="#fff"/>${edge ? `<stop offset=".45" stop-color="${tint}"/>` : ''}<stop offset="1" stop-color="${tint}"/></radialGradient>
    ${glow('spg' + n, r * 0.22)}
    <linearGradient id="fl${n}"><stop offset="0" stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity=".85"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient></defs>
    ${flare ? `<rect x="${cx - r * 2.4}" y="${cy - r * 0.035}" width="${r * 4.8}" height="${r * 0.07}" fill="url(#fl${n})"/>` : ''}
    <circle cx="${cx}" cy="${cy}" r="${r * 0.55}" fill="${tint}" opacity=".35" filter="url(#spg${n})"/>
    <path d="${starD(cx, cy, r, r * 0.78, 0.16)}" fill="url(#sp${n})" filter="url(#spg${n})" ${edge ? `stroke="${edge}" stroke-width="${r * 0.07}" stroke-linejoin="round"` : ''}/>
  </g>`;
}

// A chrome four-point star with a dark rim (used for the app icon).
function chromeStar(cx, cy, r, palette = 'cool', waist = 0.2) {
  const n = ++uid, s = r / 100;
  const d = starD(cx, cy, r, r * 0.8, waist);
  return `<defs>${grad('cs' + n, CHROME[palette])}${grad('csr' + n, [[0, '#ffffff'], [.5, '#8d9ab0'], [1, '#e9eef6']])}
    ${bevel('csb' + n, 3.2 * s, 7 * s, 0.85)}${shadow('css' + n, 5 * s, 6 * s, '#000', .55)}</defs>
    <g filter="url(#css${n})"><path d="${d}" fill="none" stroke="url(#csr${n})" stroke-width="${11 * s}" stroke-linejoin="round"/>
    <path d="${d}" fill="none" stroke="#0a0d15" stroke-width="${5.5 * s}" stroke-linejoin="round"/>
    <path d="${d}" fill="url(#cs${n})" filter="url(#csb${n})"/></g>`;
}

// A chrome wordmark. The text uses a dotless i; layout() places the sparkle where its dot would be.
function wordmark({ text = 'glınt', family = 'U', weight = 900, size = 200, palette = 'cool', skew = 0, rim = true,
                    dotIndex = 2, dot = true, w = 900, h = 320, x = 40, y = 230, tint = '#bfe4ff', shine = true, edge = '', r = 0.2, dotY = 0.8, rimW = 0, inkW = 0, ink = '#090c14', fill = '', gloss = false, spark = null, pathD = '', dotX = 0, vb = '' }) {
  const n = ++uid, k = size / 200;
  const t = pathD ? (extra) => `<path d="${pathD}" ${extra}/>`
                  : (extra) => `<text x="${x}" y="${y}" font-family="${family}" font-weight="${weight}" font-size="${size}" ${extra}>${text}</text>`;
  if (pathD && dot) spark = [dotX, y - size * dotY, size * r];
  return `<svg class="wm" data-dot="${dot && !pathD ? dotIndex : -1}" data-size="${size}" data-tint="${tint}" data-edge="${edge}" data-r="${r}" data-doty="${dotY}" viewBox="${vb || `0 0 ${w} ${h}`}" width="${vb ? vb.split(' ')[2] : w}" height="${vb ? vb.split(' ')[3] : h}">
  <defs>${grad('ch' + n, CHROME[palette])}${palette === 'aero' ? '' : ''}
    ${grad('rim' + n, [[0, '#ffffff'], [.45, '#c6d0de'], [.55, '#6f7d93'], [1, '#f2f6fb']])}
    ${bevel('bv' + n, 4 * k, 7 * k)}${shadow('sh' + n, 8 * k, 9 * k, '#000', .45)}
    <clipPath id="cl${n}">${t('')}</clipPath>
    <linearGradient id="sn${n}" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0"/>
      <stop offset=".45" stop-color="#fff" stop-opacity="0"/><stop offset=".5" stop-color="#fff" stop-opacity=".55"/>
      <stop offset=".56" stop-color="#fff" stop-opacity="0"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient></defs>
  <g transform="skewX(${skew})" transform-origin="${x} ${y}">
    <g filter="url(#sh${n})">
      ${rim ? t(`fill="none" stroke="url(#rim${n})" stroke-width="${(rimW || 26) * k}" stroke-linejoin="round"`) : ''}
      ${t(`fill="none" stroke="${ink}" stroke-width="${(inkW || 12) * k}" stroke-linejoin="round"`)}
    </g>
    ${t(`class="main" fill="${fill || `url(#ch${n})`}" ${gloss ? '' : `filter="url(#bv${n})"`}`)}
    ${gloss ? `<defs><linearGradient id="gl${n}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".95"/><stop offset="1" stop-color="#fff" stop-opacity=".15"/></linearGradient></defs>
      <rect x="0" y="${y - size * 0.86}" width="${w}" height="${size * 0.36}" rx="${size * 0.1}" fill="url(#gl${n})" clip-path="url(#cl${n})"/>` : ''}
    ${spark ? sparkle(spark[0], spark[1], spark[2], tint, true, edge) : ''}
    ${shine ? `<rect x="0" y="0" width="${w}" height="${h}" fill="url(#sn${n})" clip-path="url(#cl${n})"/>` : ''}
    <g class="dot"></g>
  </g></svg>`;
}

// Put each wordmark's sparkle over its dotless i (needs the font loaded).
window.layout = function () {
  for (const svg of document.querySelectorAll('svg.wm')) {
    const i = +svg.dataset.dot; if (i < 0) continue;
    const text = svg.querySelector('text.main'), size = +svg.dataset.size;
    const e = text.getExtentOfChar(i);
    const cx = e.x + e.width / 2, base = +text.getAttribute('y');
    const r = size * (svg.dataset.r ? +svg.dataset.r : 0.2);
    svg.querySelector('g.dot').innerHTML = sparkle(cx, base - size * +svg.dataset.doty, r, svg.dataset.tint, true, svg.dataset.edge);
  }
};

// An app icon tile (256 x 256 design units, drawn at any pixel size).
function iconTile(kind, px, opts = {}) {
  const n = ++uid;
  const tiny = px <= 24;
  let body = '';
  const bg = `<defs><radialGradient id="tb${n}" cx=".5" cy=".35" r=".8"><stop offset="0" stop-color="${opts.bg1 || '#26314d'}"/><stop offset="1" stop-color="${opts.bg2 || '#06080e'}"/></radialGradient>
    <linearGradient id="te${n}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".35"/><stop offset=".5" stop-color="#fff" stop-opacity=".04"/><stop offset="1" stop-color="#fff" stop-opacity=".12"/></linearGradient></defs>
    <rect x="8" y="8" width="240" height="240" rx="56" fill="url(#tb${n})"/>`;
  const grid = tiny ? '' : (() => {
    let g = `<g opacity=".35" stroke="${opts.grid || '#5fb6ff'}" stroke-width="1.6" fill="none" clip-path="url(#tc${n})">`;
    for (let i = 0; i <= 12; i++) { const x = 8 + i * 20; g += `<line x1="128" y1="150" x2="${x * 2.2 - 154}" y2="248"/>`; }
    for (const yy of [168, 184, 204, 228]) g += `<line x1="8" y1="${yy}" x2="248" y2="${yy}"/>`;
    return `<defs><clipPath id="tc${n}"><rect x="8" y="150" width="240" height="98" rx="0"/></clipPath>
      <linearGradient id="tf${n}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#000" stop-opacity="1"/><stop offset="1" stop-color="#000" stop-opacity="0"/></linearGradient></defs>
      ${g}</g><rect x="8" y="148" width="240" height="40" fill="url(#tf${n})" opacity=".0"/>`;
  })();
  const edge = `<rect x="9.5" y="9.5" width="237" height="237" rx="54.5" fill="none" stroke="url(#te${n})" stroke-width="3"/>`;
  if (kind === 'star') {
    body = bg + grid + (tiny ? '' : `<defs><radialGradient id="tg${n}"><stop offset="0" stop-color="${opts.glow || '#4aa8ff'}" stop-opacity=".55"/><stop offset="1" stop-color="${opts.glow || '#4aa8ff'}" stop-opacity="0"/></radialGradient></defs><circle cx="128" cy="122" r="110" fill="url(#tg${n})"/>`) +
      chromeStar(128, tiny ? 128 : 122, tiny ? 108 : 92, opts.palette, tiny ? 0.3 : 0.2) + (tiny ? '' : sparkle(186, 64, 18, '#cfe9ff', false)) + edge;
  } else if (kind === 'bare') {
    body = chromeStar(128, 128, 118, opts.palette, tiny ? 0.3 : 0.22) + (tiny ? '' : sparkle(206, 50, 22, '#cfe9ff', false));
  } else if (kind === 'g') {
    body = bg + grid + `<svg x="0" y="0" width="256" height="256" viewBox="0 0 256 256" overflow="visible">${glyphG(opts)}</svg>` +
      (tiny ? '' : sparkle(190, 70, 26, '#cfe9ff', false)) + edge;
  } else if (kind === 'orb') {
    body = `<defs><radialGradient id="ob${n}" cx=".5" cy=".62" r=".62"><stop offset="0" stop-color="#7ee6ff"/><stop offset=".55" stop-color="#1e9be8"/><stop offset="1" stop-color="#0b4fa8"/></radialGradient>
      <linearGradient id="og${n}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity=".95"/><stop offset="1" stop-color="#fff" stop-opacity=".05"/></linearGradient>
      ${shadow('os' + n, 8, 8, '#00315f', .45)}</defs>
      <circle cx="128" cy="132" r="112" fill="url(#ob${n})" filter="url(#os${n})"/>
      <circle cx="128" cy="132" r="111" fill="none" stroke="#bff2ff" stroke-opacity=".6" stroke-width="3"/>` +
      sparkle(128, 140, tiny ? 92 : 74, '#d8f6ff', false) +
      `<ellipse cx="128" cy="70" rx="82" ry="44" fill="url(#og${n})"/>`;
  } else if (kind === 'slant') {
    body = `<defs>${grad('sl' + n, [[0, '#2a2a30'], [1, '#09090b']])}${grad('slr' + n, [[0, '#ff5a52'], [1, '#a50d14']])}</defs>
      <path d="M20,40 H236 L206,216 H-10 Z" transform="translate(10,0)" fill="url(#sl${n})" stroke="url(#slr${n})" stroke-width="6"/>` +
      `<svg x="0" y="0" width="256" height="256" viewBox="0 0 256 256" overflow="visible">${glyphG({ ...opts, slant: -12 })}</svg>`;
  }
  return `<svg width="${px}" height="${px}" viewBox="0 0 256 256">${body}</svg>`;
}

// A chrome capital/lowercase G for icons (font glyph, centred).
function glyphG(opts) {
  const n = ++uid;
  const ch = opts.letter || 'g', fam = opts.family || 'U', fs = opts.fs || 230, y = opts.y || 178;
  const t = (extra) => `<text x="128" y="${y}" text-anchor="middle" font-family="${fam}" font-weight="900" font-size="${fs}" ${extra}>${ch}</text>`;
  return `<defs>${grad('gg' + n, CHROME[opts.palette || 'cool'])}${grad('gr' + n, [[0, '#ffffff'], [.5, '#8d9ab0'], [1, '#eef2f8']])}
    ${bevel('gb' + n, 4, 8)}${shadow('gs' + n, 6, 6, '#000', .5)}</defs>
    <g transform="skewX(${opts.slant || 0})" transform-origin="128 ${y}"><g filter="url(#gs${n})">${t(`fill="none" stroke="url(#gr${n})" stroke-width="22" stroke-linejoin="round"`)}
    ${t(`fill="none" stroke="#090c14" stroke-width="11" stroke-linejoin="round"`)}</g>${t(`fill="url(#gg${n})" filter="url(#gb${n})"`)}</g>`;
}
