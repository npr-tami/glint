# Turn "glınt" in Unbounded Black into one SVG path (font size 200, pen at x=40, baseline y=230),
# and report where the dotless i's centre is, so the sparkle can sit on it.
# usage: python outline.py Unbounded[wght].ttf glınt 200 40 230 > wm-path.json   (pip install fonttools uharfbuzz)
import json, os, sys, tempfile
import uharfbuzz as hb
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen

src, text, size, x0, base = sys.argv[1], sys.argv[2], float(sys.argv[3]), float(sys.argv[4]), float(sys.argv[5])
font = TTFont(src)
if 'fvar' in font:
    font = instantiateVariableFont(font, {'wght': 900})
static = os.path.join(tempfile.gettempdir(), 'Unbounded-Black-static.ttf')
font.save(static)
blob = hb.Blob.from_file_path(static)
face = hb.Face(blob); hbfont = hb.Font(face)
buf = hb.Buffer(); buf.add_str(text); buf.guess_segment_properties()
hb.shape(hbfont, buf, {'kern': True, 'liga': True})
upem = font['head'].unitsPerEm; k = size / upem
gs = font.getGlyphSet(); order = font.getGlyphOrder()
pen = SVGPathPen(gs)
x = 0; centres = []
for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
    name = order[info.codepoint]
    # font units -> SVG: scale, flip y, move to the pen position
    tp = TransformPen(pen, (k, 0, 0, -k, x0 + (x + pos.x_offset) * k, base - pos.y_offset * k))
    gs[name].draw(tp)
    adv = pos.x_advance
    centres.append(x0 + (x + adv / 2) * k)
    x += adv
print(json.dumps({'d': pen.getCommands(), 'centres': centres, 'width': x * k}))
