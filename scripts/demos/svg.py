# /// script
# requires-python = ">=3.10"
# dependencies = ["pyte>=0.8", "fonttools>=4.40"]
# ///
"""Render the recorded demos as animated SVG terminal windows.

Reads each scripts/demos/<name>.frames.json written by record.py, emulates
every frame with pyte and writes docs/images/demo-<name>.svg. Frame 0 is drawn
plainly; each later frame is a layer that repaints only the rows that changed,
shown at its start time by a CSS animation. No JavaScript, no SMIL, so GitHub
renders it.

Text is set in JetBrains Mono, subset and embedded, read from ~/Library/Fonts
or DEMO_FONT_DIR. Box-drawing characters are drawn as paths so borders join
across the taller-than-the-font line height.

    uv run scripts/demos/svg.py [name ...]
"""

import base64
import io
import json
import os
import string
import sys
from pathlib import Path

import pyte
from fontTools import subset
from fontTools.ttLib import TTFont

ROOT = Path(__file__).resolve().parents[2]
DEMOS = ROOT / "scripts" / "demos"
OUT = ROOT / "docs" / "images"
FONT_DIR = Path(os.environ.get("DEMO_FONT_DIR", Path.home() / "Library" / "Fonts"))
FAMILY = "DemoMono"

SIZE = 14
CELL_W = 0.6 * SIZE
CELL_H = 1.45 * SIZE
PAD = 18
BAR = 34
GAP = 6  # between the title bar and the grid
FINAL_HOLD = 2.5  # seconds the last frame is held, at least

BG = "#000000"
FG = "#F3F2CC"
STROKE = "#2C2C30"
TITLE = "#707070"
DOTS = ("#FF5F57", "#FEBC2E", "#28C840")
DIM = 0.55

# The sixteen ANSI colours, black to white then the bright set, from the
# author's Ghostty theme so the demos look like the terminal they run in.
ANSI = [
    "#1C1F20", "#F05E48", "#99BE70", "#FAD566", "#B07BBF", "#CFBA8B", "#E8A87C", "#A89984",
    "#626C66", "#FF7A63", "#B3D48A", "#FCE38A", "#C490D1", "#DECA9F", "#FFC799", "#F3F2CC",
]

# Box drawing: the cell edges each character reaches (up, right, down, left).
# Rounded corners are the same as square ones, drawn with an arc.
LINES = {
    "─": "rl", "│": "ud", "┌": "rd", "┐": "dl", "└": "ur", "┘": "ul",
    "├": "urd", "┤": "udl", "┬": "rdl", "┴": "url", "┼": "urdl",
    "╭": "rd", "╮": "dl", "╰": "ur", "╯": "ul",
}
ROUND = set("╭╮╰╯")


def rgb(hexstr):
    return tuple(int(hexstr[i:i + 2], 16) for i in (1, 3, 5))


def colour(c, default):
    """A pyte colour, as stored by Screen.select_graphic_rendition, to hex."""
    if c == "default":
        return default
    if c.startswith("#"):
        return c
    n = int(c)
    if n < 16:
        return ANSI[n]
    if n >= 232:
        v = 8 + 10 * (n - 232)
        return f"#{v:02X}{v:02X}{v:02X}"
    n -= 16
    cube = [0 if x == 0 else 55 + 40 * x for x in (n // 36, n // 6 % 6, n % 6)]
    if max(cube) - min(cube) < 24:
        v = sum(cube) // 3
        return f"#{v:02X}{v:02X}{v:02X}"
    # Snap a cube colour to the nearest hue in the palette.
    return min(ANSI[1:7], key=lambda h: sum((a - b) ** 2 for a, b in zip(rgb(h), cube)))


class Screen(pyte.Screen):
    """A pyte screen that keeps SGR 2 (dim) in the unused blink attribute and
    stores colours as palette indices or #hex, which colour() resolves."""

    def select_graphic_rendition(self, *attrs, private=False):
        if private:
            return
        a = self.cursor.attrs
        new = {}
        args = list(attrs) or [0]
        while args:
            n = args.pop(0)
            if n == 0:
                new = dict(self.default_char._asdict())
            elif n == 1:
                new["bold"] = True
            elif n == 2:
                new["blink"] = True
            elif n == 3:
                new["italics"] = True
            elif n == 4:
                new["underscore"] = True
            elif n == 7:
                new["reverse"] = True
            elif n == 9:
                new["strikethrough"] = True
            elif n == 22:
                new["bold"] = new["blink"] = False
            elif n == 23:
                new["italics"] = False
            elif n == 24:
                new["underscore"] = False
            elif n == 27:
                new["reverse"] = False
            elif n == 29:
                new["strikethrough"] = False
            elif 30 <= n <= 37 or 90 <= n <= 97:
                new["fg"] = str(n - 30 if n < 90 else n - 82)
            elif 40 <= n <= 47 or 100 <= n <= 107:
                new["bg"] = str(n - 40 if n < 100 else n - 92)
            elif n in (39, 49):
                new["fg" if n == 39 else "bg"] = "default"
            elif n in (38, 48) and args:
                key = "fg" if n == 38 else "bg"
                mode = args.pop(0)
                if mode == 5 and args:
                    new[key] = str(args.pop(0) & 0xFF)
                elif mode == 2 and len(args) >= 3:
                    r, g, b = (min(args.pop(0), 255) for _ in range(3))
                    new[key] = f"#{r:02X}{g:02X}{b:02X}"
        self.cursor.attrs = a._replace(**new)


def emulate(doc):
    """Every frame as a list of rows, each a list of (char, style) cells."""
    frames = []
    for f in doc["frames"]:
        screen = Screen(doc["cols"], doc["rows"])
        pyte.Stream(screen).feed(f["ansi"])
        rows = []
        for y in range(doc["rows"]):
            line = screen.buffer[y]
            rows.append([cell(line[x]) for x in range(doc["cols"])])
        frames.append(rows)
    return frames


def cell(c):
    fg, bg = colour(c.fg, FG), colour(c.bg, BG)
    if c.reverse:
        fg, bg = bg, fg
    return c.data, (fg, bg, c.bold, c.italics, c.underscore, c.strikethrough, c.blink)


def used(row):
    return any((ch.strip() and ch != "") or st[1] != BG for ch, st in row)


def lift_status(frame):
    """Move the status line, the last row record.py writes, up to one blank
    row under this frame's view, so a view that shrinks leaves its spare rows
    below the status line rather than above it."""
    status, view = frame[-1], frame[:-1]
    if not used(status):
        return frame
    last = max((y + 1 for y, r in enumerate(view) if used(r)), default=0)
    blank = [(" ", (FG, BG, False, False, False, False, False))] * len(status)
    rows = view[:last] + [blank, status]
    return rows + [blank] * (len(frame) - len(rows))


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def num(v):
    return f"{v:.2f}".rstrip("0").rstrip(".")


class Painter:
    def __init__(self, cols, baseline):
        self.cols = cols
        self.baseline = baseline

    def row(self, y, cells, clear=False):
        """SVG for one row of cells at grid row y."""
        top = BAR + GAP + y * CELL_H
        out = []
        if clear:
            out.append(f'<rect x="{num(PAD - 1)}" y="{num(top - 0.5)}" width="{num(self.cols * CELL_W + 2)}" '
                       f'height="{num(CELL_H + 1)}" fill="{BG}"/>')
        # Backgrounds, merged across adjacent cells.
        x = 0
        while x < len(cells):
            bg = cells[x][1][1]
            end = x + 1
            while end < len(cells) and cells[end][1][1] == bg:
                end += 1
            if bg != BG:
                out.append(f'<rect x="{num(PAD + x * CELL_W)}" y="{num(top)}" '
                           f'width="{num((end - x) * CELL_W)}" height="{num(CELL_H)}" fill="{bg}"/>')
            x = end
        out += self.lines(cells, top)
        out += self.text(cells, top)
        return out

    def lines(self, cells, top):
        """Box-drawing cells as stroked paths, one path per colour."""
        paths = {}
        cy = top + CELL_H / 2
        x = 0
        while x < len(cells):
            ch, st = cells[x]
            left = PAD + x * CELL_W
            cx = left + CELL_W / 2
            d = paths.setdefault((st[0], st[2], st[6]), [])
            if ch == "─":
                end = x + 1
                while end < len(cells) and cells[end] == cells[x]:
                    end += 1
                d.append(f"M{num(left)} {num(cy)}H{num(PAD + end * CELL_W)}")
                x = end
                continue
            if ch == "█":
                d.append(f"M{num(left)} {num(top)}h{num(CELL_W)}v{num(CELL_H)}h-{num(CELL_W)}z")
            elif ch in ROUND:
                r = CELL_W / 2
                (a, b) = LINES[ch]
                ends = {"u": (cx, top), "d": (cx, top + CELL_H), "l": (left, cy), "r": (left + CELL_W, cy)}
                near = {"u": (cx, cy - r), "d": (cx, cy + r), "l": (cx - r, cy), "r": (cx + r, cy)}
                sweep = 1 if ch == "╯" else 0
                d.append(f"M{num(ends[a][0])} {num(ends[a][1])}L{num(near[a][0])} {num(near[a][1])}"
                         f"A{num(r)} {num(r)} 0 0 {sweep} {num(near[b][0])} {num(near[b][1])}"
                         f"L{num(ends[b][0])} {num(ends[b][1])}")
            elif ch in LINES:
                for e in LINES[ch]:
                    if e == "u":
                        d.append(f"M{num(cx)} {num(top)}V{num(cy)}")
                    elif e == "d":
                        d.append(f"M{num(cx)} {num(cy)}V{num(top + CELL_H)}")
                    elif e == "l":
                        d.append(f"M{num(left)} {num(cy)}H{num(cx)}")
                    else:
                        d.append(f"M{num(cx)} {num(cy)}H{num(left + CELL_W)}")
            x += 1
        out = []
        for (fg, bold, dim), d in paths.items():
            if not d:
                continue
            fill = [p for p in d if p.endswith("z")]
            stroke = [p for p in d if not p.endswith("z")]
            op = f' opacity="{DIM}"' if dim else ""
            if stroke:
                w = 1.6 if bold else 1.1
                out.append(f'<path d="{"".join(stroke)}" stroke="{fg}" stroke-width="{w}" stroke-linecap="square" fill="none"{op}/>')
            if fill:
                out.append(f'<path d="{"".join(fill)}" fill="{fg}"{op}/>')
        return out

    def text(self, cells, top):
        """Runs of cells sharing a style, each pinned to its cells' width."""
        out = []
        base = top + self.baseline
        x = 0
        while x < len(cells):
            ch, st = cells[x]
            if ch in LINES or ch == "█" or ch == "":
                x += 1
                continue
            end = x + 1
            while end < len(cells):
                c2, s2 = cells[end]
                if s2 != st or c2 in LINES or c2 == "█":
                    break
                end += 1
            run = cells[x:end]
            # A wide character leaves an empty cell after it; it stays in the
            # run's width but adds no text.
            while run and run[-1][0] in (" ", "") and not (st[4] or st[5]):
                run.pop()
            lead = 0
            while lead < len(run) and run[lead][0] == " " and not (st[4] or st[5]):
                lead += 1
            run = run[lead:]
            if run:
                fg, _, bold, italic, under, strike, dim = st
                attrs = [f'x="{num(PAD + (x + lead) * CELL_W)}"', f'y="{num(base)}"']
                if fg != FG:
                    attrs.append(f'fill="{fg}"')
                if bold:
                    attrs.append('font-weight="bold"')
                if italic:
                    attrs.append('font-style="italic"')
                deco = " ".join(n for n, on in (("underline", under), ("line-through", strike)) if on)
                if deco:
                    attrs.append(f'text-decoration="{deco}"')
                if dim:
                    attrs.append(f'opacity="{DIM}"')
                attrs.append(f'textLength="{num(len(run) * CELL_W)}" lengthAdjust="spacingAndGlyphs"')
                text = esc("".join(c for c, _ in run))
                out.append(f'<text {" ".join(attrs)}>{text}</text>')
            x = end
        return out


def font_face(path, chars, weight):
    font = TTFont(path, recalcTimestamp=False)  # stable bytes across runs
    opts = subset.Options()
    opts.hinting = False
    opts.layout_features = []
    opts.name_IDs = []
    opts.notdef_outline = True
    sub = subset.Subsetter(opts)
    sub.populate(unicodes={ord(c) for c in chars})
    sub.subset(font)
    buf = io.BytesIO()
    font.save(buf)
    data = base64.b64encode(buf.getvalue()).decode()
    return (f'@font-face{{font-family:"{FAMILY}";font-weight:{weight};'
            f'src:url(data:font/ttf;base64,{data}) format("truetype")}}')


def render(name):
    doc = json.loads((DEMOS / f"{name}.frames.json").read_text())
    frames = [lift_status(f) for f in emulate(doc)]
    cols = doc["cols"]
    rows = max((y + 1 for f in frames for y, r in enumerate(f) if used(r)), default=1)
    frames = [f[:rows] for f in frames]

    regular_path = FONT_DIR / "JetBrainsMono-Regular.ttf"
    bold_path = FONT_DIR / "JetBrainsMono-Bold.ttf"
    for p in (regular_path, bold_path):
        if not p.exists():
            sys.exit(f"{p} is missing; install JetBrains Mono or set DEMO_FONT_DIR")
    ascii_ = string.ascii_letters + string.digits
    regular, bold = set(ascii_ + doc["title"]), set(ascii_)
    for f in frames:
        for r in f:
            for ch, st in r:
                if ch and ch not in LINES and ch != "█":
                    (bold if st[2] else regular).add(ch)

    upm = TTFont(regular_path)
    ascent = upm["hhea"].ascent / upm["head"].unitsPerEm * SIZE
    descent = -upm["hhea"].descent / upm["head"].unitsPerEm * SIZE
    baseline = (CELL_H - ascent - descent) / 2 + ascent
    paint = Painter(cols, baseline)

    width = 2 * PAD + cols * CELL_W
    height = BAR + GAP + rows * CELL_H + PAD
    delays = [f["delay_ms"] / 1000 for f in doc["frames"]]
    delays[-1] = max(delays[-1], FINAL_HOLD)
    total = sum(delays)

    css = [font_face(regular_path, regular, 400), font_face(bold_path, bold, 700),
           f'text{{font-family:"{FAMILY}",monospace;font-size:{SIZE}px;white-space:pre}}']
    body = []
    for y, r in enumerate(frames[0]):
        body += paint.row(y, r)
    start = 0.0
    for k in range(1, len(frames)):
        start += delays[k - 1]
        changed = [y for y in range(rows) if frames[k][y] != frames[k - 1][y]]
        if not changed:
            continue
        p = f"{start / total * 100:.3f}".rstrip("0").rstrip(".")
        css.append(f"@keyframes s{k}{{0%,{p}%{{visibility:hidden}}{p}%,100%{{visibility:visible}}}}"
                   f".s{k}{{visibility:hidden;animation:s{k} {total:.2f}s step-end infinite}}")
        body.append(f'<g class="s{k}">')
        for y in changed:
            body += paint.row(y, frames[k][y], clear=True)
        body.append("</g>")

    dots = "".join(f'<circle cx="{PAD + 6 + 20 * i}" cy="{BAR / 2}" r="6" fill="{c}"/>'
                   for i, c in enumerate(DOTS))
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{num(width)}" height="{num(height)}" '
        f'viewBox="0 0 {num(width)} {num(height)}">'
        f'<style>{"".join(css)}</style>'
        f'<rect x="0.5" y="0.5" width="{num(width - 1)}" height="{num(height - 1)}" rx="12" '
        f'fill="{BG}" stroke="{STROKE}"/>'
        f'{dots}'
        f'<text x="{num(width / 2)}" y="{num(BAR / 2 + 4)}" text-anchor="middle" '
        f'style="font-size:12px;fill:{TITLE}">{esc(doc["title"])}</text>'
        f'<g fill="{FG}">' + "\n".join(body) + "</g>"
        "</svg>\n"
    )
    out = OUT / f"demo-{name}.svg"
    out.write_text(svg)
    print(f"{out.relative_to(ROOT)}: {len(svg) // 1024} KB, {len(frames)} frames, "
          f"{cols}x{rows}, {total:.1f}s loop")


def main():
    names = sys.argv[1:] or sorted(p.name.removesuffix(".frames.json")
                                   for p in DEMOS.glob("*.frames.json"))
    OUT.mkdir(parents=True, exist_ok=True)
    for name in names:
        render(name)


if __name__ == "__main__":
    main()
