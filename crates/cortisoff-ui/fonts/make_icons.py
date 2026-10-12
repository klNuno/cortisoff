"""Cut Phosphor Regular down to the glyphs cortisoff draws, each moved to the
Segoe Fluent code point the UI already names, so one char works with either font.

    pip install fonttools
    npm pack @phosphor-icons/web && tar xzf phosphor-icons-web-*.tgz
    cd package/src/regular && python make_icons.py Phosphor.ttf style.css icons.ttf

A new icon is a line in SEGOE_TO_PHOSPHOR and a constant in theme.rs.
"""
import re
import sys

from fontTools.ttLib import TTFont
from fontTools.subset import Subsetter, Options
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable

src, css, out = sys.argv[1:4]

SEGOE_TO_PHOSPHOR = {
    0xE80F: "house",
    0xE713: "gear-six",
    0xE710: "plus",
    0xE708: "moon",
    0xE7F4: "monitor",
    0xE8B9: "image",
    0xE945: "lightning",
    0xE769: "pause",
    0xE76C: "caret-right",
    0xE70D: "caret-down",
    0xE711: "x",
    0xE823: "clock",
    0xE7FC: "game-controller",
    0xE767: "speaker-high",
    0xE72E: "lock",
    0xE740: "arrows-out",
    0xE72A: "arrow-right",
    0xEA86: "puzzle-piece",
    0xE73E: "check",
    0xE765: "keyboard",
    0xE768: "play",
    0xE74D: "trash",
    0xE70F: "pencil-simple",
    0xE721: "magnifying-glass",
    0xE7BA: "warning",
    0xE72C: "arrow-clockwise",
    0xE706: "sun",
    0xE7E8: "power",
    0xE83F: "battery-full",
}

text = open(css, encoding="utf-8").read()
cp_of = {}
for name, cp in re.findall(r'\.ph\.ph-([a-z0-9-]+):before\s*\{\s*content:\s*"\\([0-9a-f]+)"', text):
    cp_of.setdefault(name, int(cp, 16))

font = TTFont(src)
cmap = font.getBestCmap()
mapping = {}
for segoe, name in SEGOE_TO_PHOSPHOR.items():
    mapping[segoe] = cmap[cp_of[name]]

opts = Options()
opts.layout_features = []
opts.name_IDs = ["*"]
opts.notdef_outline = True
opts.hinting = False
sub = Subsetter(opts)
sub.populate(glyphs=sorted(set(mapping.values())))
sub.subset(font)

tables = []
for pid, eid in ((3, 1), (0, 3)):
    t = CmapSubtable.newSubtable(4)
    t.platformID, t.platEncID, t.language = pid, eid, 0
    t.cmap = dict(mapping)
    tables.append(t)
font["cmap"].tables = tables
font.save(out)
print(len(mapping), "glyphs")
