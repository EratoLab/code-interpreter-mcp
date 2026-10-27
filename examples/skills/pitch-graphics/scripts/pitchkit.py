"""pitchkit – Konzept-Grafiken (PNG + HTML/SVG) aus einer Inhalts-Spec.

Nur Pillow nötig. Läuft in Pyodide (code-interpreter-mcp) und CPython.
Das Modell liefert nur SPEC (Inhalt + Layout-Typ); Layout, Text-Fitting,
Rendering und Prüfung sind deterministisch. Layouts:

    flow    Abfolge, Stufen, Zonen mit beschrifteten Übergängen (+ Band)
    layers  Schichten: was worauf aufsetzt (+ Querschnitt rechts)
    matrix  2x2-Einordnung nach zwei Kriterien
    hub     Kern mit Bausteinen/Beteiligten, beschriftete Verbindungen
    cycle   Kreislauf/Feedback-Schleife
    nested  Enthaltensein: Zonen, Vertrauens- oder Geltungsbereiche

    render(SPEC, "name")   # -> name.png (3200x1800) + name.html (inline SVG), druckt Prüfbericht

Bei Problemen wirft render() LayoutError mit konkreter Kürzungsanweisung
und schreibt keine Dateien.
"""

import html
import math
import os
import sys

if False:  # Nur für Pyodides Import-Scanner: legt matplotlib samt DejaVu-Fonts in die Sandbox, ohne es zu importieren.
    import matplotlib  # noqa: F401

from PIL import Image, ImageDraw, ImageFont
from PIL.PngImagePlugin import PngInfo

W, H, M = 1600, 900, 64  # logische Pixel (16:9), Seitenrand
OUT = 2                  # Ausgabe-Skalierung -> 3200x1800
SS = 2                   # Supersampling-Faktor (Antialiasing für Formen)
MIN_PX = 18              # kleinste erlaubte Schrift (≈ 11 pt auf Vollbild-Folie)
MAX_WORDS = 80           # Dichte: darüber Warnung

THEMES = {
    "dark": dict(
        bg="#0F172A", card="#1E293B", inset="#0B1220", ink="#F8FAFC", txt="#E2E8F0",
        mut="#94A3B8", rule="#334155", line="#64748B", band="#4F46E5", band_ink="#FFFFFF",
        accents=["#34D399", "#60A5FA", "#FBBF24", "#A78BFA"], on_accent="#0B1220",
    ),
    "light": dict(
        bg="#F8FAFC", card="#FFFFFF", inset="#F1F5F9", ink="#0F172A", txt="#1E293B",
        mut="#475569", rule="#E2E8F0", line="#94A3B8", band="#4338CA", band_ink="#FFFFFF",
        accents=["#047857", "#2563EB", "#B45309", "#7C3AED"], on_accent="#FFFFFF",
    ),
}
LIMITS = {"flow": (1, 4), "layers": (2, 5), "matrix": (4, 4), "hub": (3, 6), "cycle": (3, 6), "nested": (2, 4)}


class LayoutError(Exception):
    pass


# ---------------------------------------------------------------- Fonts & Text

def _font_dir():
    here = os.path.dirname(globals().get("__file__", "") or ".")
    candidates = [os.environ.get("PITCHKIT_FONTS"), os.path.join(here, "fonts")]
    candidates += [os.path.join(p, "matplotlib", "mpl-data", "fonts", "ttf") for p in sys.path]
    candidates += ["/usr/share/fonts/truetype/dejavu", "/usr/share/fonts/dejavu", "/usr/share/fonts/TTF"]
    for c in candidates:
        if c and os.path.isfile(os.path.join(c, "DejaVuSans.ttf")):
            return c
    raise RuntimeError(
        "Keine DejaVu-TTF gefunden. matplotlib vorinstallieren oder Runtime-Installation "
        "aktivieren (der Import oben lädt die Fonts). Niemals den Pillow-Default-Font nutzen."
    )


_FONT_DIR = None
_FONTS = {}
_STYLE = {"r": "DejaVuSans.ttf", "b": "DejaVuSans-Bold.ttf", "i": "DejaVuSans-Oblique.ttf"}
S = OUT * SS  # Zeichen-Skalierung


def _font(size, style="r"):
    global _FONT_DIR
    key = (size, style)
    if key not in _FONTS:
        _FONT_DIR = _FONT_DIR or _font_dir()
        _FONTS[key] = ImageFont.truetype(os.path.join(_FONT_DIR, _STYLE[style]), size * S)
    return _FONTS[key]


def _len(text, size, style="r"):
    return _font(size, style).getlength(text) / S


def _metrics(size, style="r"):
    ascent, descent = _font(size, style).getmetrics()
    return ascent / S, descent / S


def _line_h(size):
    return round(size * 1.32)


def _block_h(n_lines, size, style="r"):
    if not n_lines:
        return 0
    ascent, descent = _metrics(size, style)
    return _line_h(size) * (n_lines - 1) + ascent + descent


def _wrap(text, size, style, maxw):
    """Wortumbruch; zu lange Wörter werden an '-' oder '/' getrennt, sonst None."""
    words = []  # (Wort, ohne Leerzeichen an den Vorgänger hängen)
    for w in str(text).split():
        if _len(w, size, style) <= maxw:
            words.append((w, False))
            continue
        parts, cur = [], ""
        for ch in w:
            cur += ch
            if ch in "-/":
                parts.append(cur)
                cur = ""
        if cur:
            parts.append(cur)
        if len(parts) == 1 or any(_len(p, size, style) > maxw for p in parts):
            return None
        words.extend((p, k > 0) for k, p in enumerate(parts))
    lines, cur = [], ""
    for w, glued in words:
        t = (cur + ("" if glued else " ") + w) if cur else w
        if _len(t, size, style) <= maxw:
            cur = t
        else:
            lines.append(cur)
            cur = w
    if cur:
        lines.append(cur)
    return lines


def _fit(text, size, min_size, style, maxw, max_lines):
    for s in range(size, min_size - 1, -1):
        lines = _wrap(text, s, style, maxw)
        if lines is not None and len(lines) <= max_lines:
            return s, lines
    return None


def _mix(a, b, f):
    ca = [int(a[i:i + 2], 16) for i in (1, 3, 5)]
    cb = [int(b[i:i + 2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x + (y - x) * f):02X}" for x, y in zip(ca, cb))


# ---------------------------------------------------------------- Szene

class _Scene:
    def __init__(self, theme):
        self.t = theme
        self.ops = []      # Zeichenoperationen in logischen Koordinaten
        self.texts = []    # (bbox, owner, fg, bg, size, style, text)
        self.marks = []    # (("rect", bbox) | ("seg", p, q), owner) – Formen, die fremder Text nicht berühren darf
        self.boxes = {}    # owner -> Innenbereich (Containment)

    def text(self, x, top, lines, size, style, fg, bg, owner, align="left", width=None):
        ascent, descent = _metrics(size, style)
        y = top + ascent
        for line in lines:
            w = _len(line, size, style)
            lx = x + (width - w) / 2 if align == "center" else (x + width - w if align == "right" else x)
            self.ops.append(("text", lx, y, line, size, style, fg))
            self.texts.append(((lx, y - ascent, lx + w, y + descent), owner, fg, bg, size, style, line))
            y += _line_h(size)
        return top + _block_h(len(lines), size, style)

    def rrect(self, rect, r, fill, outline=None, width=0, owner=None):
        self.ops.append(("rrect", rect, r, fill, outline, width))
        if owner:
            self.marks.append((("rect", rect), owner))

    def arrow(self, p, q, color, owner, width=4, start=False, end=True, head=16):
        ang = math.atan2(q[1] - p[1], q[0] - p[0])
        ux, uy = math.cos(ang), math.sin(ang)
        a = (p[0] + ux * head * 0.8, p[1] + uy * head * 0.8) if start else p
        b = (q[0] - ux * head * 0.8, q[1] - uy * head * 0.8) if end else q
        self.ops.append(("line", [a, b], color, width))
        for tip, sgn, on in ((q, 1, end), (p, -1, start)):
            if on:
                bx, by = tip[0] - sgn * ux * head, tip[1] - sgn * uy * head
                nx, ny = -uy * head * 0.55, ux * head * 0.55
                self.ops.append(("poly", [tip, (bx + nx, by + ny), (bx - nx, by - ny)], color))
        self.marks.append((("seg", p, q), owner))

    def pill(self, cx, cy, label, owner, maxw=230):
        fit = _fit(label, 19, MIN_PX, "r", maxw - 28, 2)
        if not fit:
            return f"Verbindungs-Label „{label}“ zu lang (≤ 2 kurze Wörter)"
        size, lines = fit
        w = max(_len(x, size) for x in lines) + 28
        h = _block_h(len(lines), size) + 14
        rect = (cx - w / 2, cy - h / 2, cx + w / 2, cy + h / 2)
        self.rrect(rect, min(h / 2, 16), self.t["bg"], self.t["line"], 2, owner)
        self.text(rect[0] + 14, rect[1] + 7, lines, size, "r", self.t["txt"], self.t["bg"], owner, "center", w - 28)
        return None


# ---------------------------------------------------------------- Knoten-Inhalt

def _plan(node, w, size, title_max=28, numbered=False, title_w=None, divider=False):
    """Zeilen für Titel/Text/Punkte/Chips; Fehlertext statt dict, wenn etwas nicht passt."""
    title = node.get("title", "")
    tw = title_w if title_w else w - (54 if numbered else 0)
    tfit = _fit(title, title_max, 22, "b", tw, 2)
    if not tfit:
        return f"Titel „{title}“ zu lang (1–3 Wörter)"
    th = _block_h(len(tfit[1]), tfit[0], "b")
    head = 0 if title_w else max(40 if numbered else 0, th)
    body = 0
    text = None
    if node.get("text"):
        text = _wrap(node["text"], size, "r", w)
        if text is None:
            return f"Wort in „{node['text']}“ zu breit – umformulieren oder mit Bindestrich trennen"
        if len(text) > 4:
            return f"Text „{node['text'][:40]}…“ zu lang (≤ 4 Zeilen; Erklärungen gehören in die Bildunterschrift)"
        body += _block_h(len(text), size)
    points = []
    for p in node.get("points", []):
        lines = _wrap(p, size, "r", w - 22)
        if lines is None:
            return f"Wort in „{p}“ zu breit – umformulieren oder mit Bindestrich trennen"
        points.append(lines)
    if points:
        body += (12 if body else 0) + sum(_block_h(len(x), size) for x in points) + 10 * (len(points) - 1)
    rows, row, roww = [], [], 0
    for item in node.get("items", []):
        cw = _len(item, MIN_PX) + 28
        if cw > w:
            return f"Chip „{item}“ zu lang"
        if row and roww + 10 + cw > w:
            rows.append(row)
            row, roww = [], 0
        roww += (10 if row else 0) + cw
        row.append((item, cw))
    if row:
        rows.append(row)
    if rows:
        body += (16 if body else 0) + len(rows) * 36 + (len(rows) - 1) * 10
    gap = (26 if divider else 14) if (body and not title_w) else 0
    h = max(th, body) if title_w else head + gap + body
    return dict(title=tfit, th=th, head=head, gap=gap, text=text, points=points, rows=rows, body=body, h=h)


def _fit_nodes(nodes, widths, heights, sizes, **kw):
    """Größte gemeinsame Textgröße, bei der alle Knoten passen."""
    errors, over = {}, {}
    for size in sizes:
        plans = [_plan(n, w, size, **kw) for n, w in zip(nodes, widths)]
        errors = {i: p for i, p in enumerate(plans) if isinstance(p, str)}
        if errors:
            continue
        over = {i: p["h"] - h for i, (p, h) in enumerate(zip(plans, heights)) if p["h"] > h}
        if not over:
            return size, plans
    if errors:
        raise LayoutError("\n".join(f"Element {i + 1} („{nodes[i].get('title', '')}“): {e}" for i, e in errors.items()))
    raise LayoutError("Inhalt passt nicht, auch nicht bei kleinster Schrift:\n" + "\n".join(
        f"Element {i + 1} („{nodes[i].get('title', '')}“): {int(v)} px zu hoch (≈ {max(1, round(v / 26))} Zeile(n) kürzen)"
        for i, v in over.items()))


def _draw_node(sc, plan, x, y, w, size, accent, owner, bg, num=None, divider=False, title_w=None, body_x=None):
    t = sc.t
    if num is not None:
        cx, cy = x + 20, y + 20
        sc.ops.append(("circle", cx, cy, 20, accent, None, 0))
        sc.marks.append((("rect", (cx - 20, cy - 20, cx + 20, cy + 20)), owner + ":badge"))
        sc.ops.append(("ctext", cx, cy, str(num), 20, "b", t["on_accent"]))
        sc.texts.append(((cx - 8, cy - 12, cx + 8, cy + 12), owner + ":badge", t["on_accent"], accent, 20, "b", str(num)))
    tsize, tlines = plan["title"]
    if title_w:  # Titel links, Inhalt rechts (layers)
        sc.text(x, y + (plan["h"] - plan["th"]) / 2, tlines, tsize, "b", t["ink"], bg, owner)
        x, w = body_x, w - (body_x - x)
        y += (plan["h"] - plan["body"]) / 2
    else:
        tx = x + (54 if num is not None else 0)
        sc.text(tx, y + (plan["head"] - plan["th"]) / 2, tlines, tsize, "b", t["ink"], bg, owner)
        y += plan["head"]
        if divider and plan["body"]:
            sc.ops.append(("line", [(x, y + 12), (x + w, y + 12)], t["rule"], 2))
        y += plan["gap"]
    if plan["text"]:
        y = sc.text(x, y, plan["text"], size, "r", t["txt"], bg, owner)
    if plan["points"]:
        y += 12 if plan["text"] else 0
        asc, _ = _metrics(size)
        for k, lines in enumerate(plan["points"]):
            dot = y + asc * 0.62
            sc.ops.append(("circle", x + 5, dot, 4, accent, None, 0))
            sc.marks.append((("rect", (x + 1, dot - 4, x + 9, dot + 4)), owner + ":dot"))
            y = sc.text(x + 22, y, lines, size, "r", t["txt"], bg, owner) + (10 if k < len(plan["points"]) - 1 else 0)
    if plan["rows"]:
        y += 16 if (plan["text"] or plan["points"]) else 0
        for row in plan["rows"]:
            cx = x
            for item, cw in row:
                sc.rrect((cx, y, cx + cw, y + 36), 18, t["inset"], None, 0, owner + ":chip")
                sc.text(cx + 14, y + (36 - _block_h(1, MIN_PX)) / 2, [item], MIN_PX, "r", t["txt"], t["inset"], owner + ":chip")
                cx += cw + 10
            y += 46


def _accent(theme, node, i, n=0):
    """Akzent in fester Reihenfolge; bei mehr Elementen als Akzenten neutral statt zyklisch (Farbe = Bedeutung)."""
    if "accent" not in node and n > len(theme["accents"]):
        return theme["mut"]
    a = node.get("accent", i)
    return theme["accents"][a % len(theme["accents"])] if isinstance(a, int) else a


def _edge(rect, toward, gap=6):
    cx, cy = (rect[0] + rect[2]) / 2, (rect[1] + rect[3]) / 2
    dx, dy = toward[0] - cx, toward[1] - cy
    hw, hh = (rect[2] - rect[0]) / 2 + gap, (rect[3] - rect[1]) / 2 + gap
    s = min(hw / abs(dx) if dx else math.inf, hh / abs(dy) if dy else math.inf)
    return (cx + dx * s, cy + dy * s)


# ---------------------------------------------------------------- Layouts

def _flow(sc, spec, nodes, top, bottom):
    t = sc.t
    n = len(nodes)
    links = spec.get("links") or spec.get("gates") or []
    if links and len(links) != n - 1:
        raise LayoutError(f"links braucht genau {n - 1} Einträge (zwischen {n} Elementen), hat {len(links)}.")
    band = spec.get("band")
    cards_bottom = bottom - (152 if band else 0)
    gap = {1: 0, 2: 220, 3: 168, 4: 132}[n] if links else 40
    cw = (W - 2 * M - (n - 1) * gap) / n
    xs = [M + i * (cw + gap) for i in range(n)]
    pad = 28 if n <= 3 else 22
    card_h = cards_bottom - top
    numbered = spec.get("numbered", True)
    size, plans = _fit_nodes(nodes, [cw - 2 * pad] * n, [card_h - 2 * pad] * n, range(26 if n <= 3 else 24, MIN_PX - 1, -1),
                             title_max=30, numbered=numbered, divider=True)
    need = max(p["h"] for p in plans) + 2 * pad
    fitted = max(min(card_h, need + 24), card_h * 0.4)
    shift = (card_h - fitted) / 2
    y0, y1 = top + shift, top + shift + fitted
    for i, x in enumerate(xs):
        a = _accent(t, nodes[i], i)
        owner = f"n{i + 1}"
        sc.rrect((x, y0, x + cw, y1), 22, t["card"], a, 3, owner)
        sc.boxes[owner] = (x + 8, y0 + 8, x + cw - 8, y1 - 8)
        _draw_node(sc, plans[i], x + pad, y0 + pad, cw - 2 * pad, size, a, owner, t["card"], i + 1 if numbered else None, True)
    mid = (y0 + y1) / 2
    errors = []
    for i, g in enumerate(links):
        owner = f"link{i + 1}"
        gx0, gx1 = xs[i] + cw, xs[i + 1]
        gcx, gw = (gx0 + gx1) / 2, gap - 20
        sc.boxes[owner] = (gx0 + 6, y0, gx1 - 6, y1)
        sc.arrow((gx0 + 12, mid), (gx1 - 10, mid), t["line"], owner + ":arrow")
        lock = g.get("lock", False)
        if lock:
            sc.ops.append(("circle", gcx, mid, 24, t["bg"], t["line"], 3))
            sc.ops.append(("lock", gcx, mid, t["mut"]))
            sc.marks.append((("rect", (gcx - 24, mid - 24, gcx + 24, mid + 24)), owner + ":lock"))
        off = 36 if lock else 24
        if g.get("label"):
            label = _fit(g["label"], 20, MIN_PX, "b", gw, 2)
            if not label:
                errors.append(f"Übergang {i + 1}: Label „{g['label']}“ zu lang (≈ {int(gw / 12)} Zeichen/Zeile).")
            else:
                lh = _block_h(len(label[1]), label[0], "b")
                sc.text(gx0 + 10, mid - off - lh, label[1], label[0], "b", t["ink"], t["bg"], owner, "center", gw)
        if g.get("note"):
            note = _fit(g["note"], MIN_PX, MIN_PX, "r", gw, 3)
            if not note:
                errors.append(f"Übergang {i + 1}: Notiz „{g['note']}“ zu lang (≤ 3 Zeilen à ≈ {int(gw / 10)} Zeichen).")
            else:
                sc.text(gx0 + 10, mid + off, note[1], MIN_PX, "r", t["mut"], t["bg"], owner, "center", gw)
    if band:
        b0 = y1 + 40
        b1 = b0 + 112
        sc.rrect((M, b0, W - M, b1), 18, t["band"], None, 0, "band")
        sc.boxes["band"] = (M + 8, b0 + 6, W - M - 8, b1 - 6)
        for x in xs:
            sc.ops.append(("dash", x + cw / 2, y1 + 6, b0 - 6, t["line"]))
            sc.marks.append((("seg", (x + cw / 2, y1 + 6), (x + cw / 2, b0 - 6)), "band:dash"))
        bt = _fit(band.get("title", ""), 26, 22, "b", W - 2 * M - 64, 1)
        bx = _fit(band.get("text", ""), 21, MIN_PX, "r", W - 2 * M - 64, 2) if band.get("text") else (21, [])
        if not bt or not bx:
            errors.append("Band: Titel ≤ 1 Zeile, Text ≤ 2 Zeilen – kürzen.")
        else:
            total = _block_h(1, bt[0], "b") + (10 + _block_h(len(bx[1]), bx[0]) if bx[1] else 0)
            y = sc.text(M + 32, b0 + (112 - total) / 2, bt[1], bt[0], "b", t["band_ink"], t["band"], "band")
            if bx[1]:
                sc.text(M + 32, y + 10, bx[1], bx[0], "r", t["band_ink"], t["band"], "band")
    return errors


def _layers(sc, spec, nodes, top, bottom):
    t = sc.t
    n = len(nodes)
    side = spec.get("side")
    x1 = W - M - (264 if side else 0)
    gapy, pad, tw = 14, 24, 300
    row_max = (bottom - top - (n - 1) * gapy) / n
    body_w = x1 - M - 10 - pad - tw - pad - pad
    size, plans = _fit_nodes(nodes, [body_w] * n, [row_max - 2 * pad] * n, range(22, MIN_PX - 1, -1), title_max=28, title_w=tw)
    row_h = max(min(row_max, max(p["h"] for p in plans) + 2 * pad + 8), row_max * 0.55)
    total = n * row_h + (n - 1) * gapy
    y = top + (bottom - top - total) / 2
    y_first = y
    for i, node in enumerate(nodes):
        a = _accent(t, node, i, n)
        owner = f"n{i + 1}"
        sc.rrect((M, y, x1, y + row_h), 16, t["card"], t["rule"], 2, owner)
        sc.ops.append(("rrect", (M, y, M + 10, y + row_h), 5, a, None, 0))
        sc.boxes[owner] = (M + 12, y + 4, x1 - 6, y + row_h - 4)
        bx = M + 10 + pad + tw + pad
        _draw_node(sc, plans[i], M + 10 + pad, y + (row_h - plans[i]["h"]) / 2, x1 - pad - (M + 10 + pad), size, a, owner,
                   t["card"], title_w=tw, body_x=bx)
        y += row_h + gapy
    if side:
        sx0, sx1, sy0, sy1 = x1 + 24, W - M, y_first, y - gapy
        sc.rrect((sx0, sy0, sx1, sy1), 16, t["band"], None, 0, "side")
        sc.boxes["side"] = (sx0 + 8, sy0 + 8, sx1 - 8, sy1 - 8)
        st = _fit(side.get("title", ""), 24, 20, "b", sx1 - sx0 - 40, 3)
        sx = _fit(side.get("text", ""), 19, MIN_PX, "r", sx1 - sx0 - 40, 8) if side.get("text") else (19, [])
        if not st or not sx:
            return ["Querschnitt (side): Titel ≤ 3 kurze Zeilen, Text ≤ 8 Zeilen à ≈ 16 Zeichen – kürzen."]
        hh = _block_h(len(st[1]), st[0], "b") + (12 + _block_h(len(sx[1]), sx[0]) if sx[1] else 0)
        yy = sc.text(sx0 + 20, sy0 + (sy1 - sy0 - hh) / 2, st[1], st[0], "b", t["band_ink"], t["band"], "side")
        if sx[1]:
            sc.text(sx0 + 20, yy + 12, sx[1], sx[0], "r", t["band_ink"], t["band"], "side")
    return []


def _matrix(sc, spec, nodes, top, bottom):
    t = sc.t
    axes = spec.get("axes") or {}
    ax, ay = axes.get("x") or {}, axes.get("y") or {}
    axis_x, axis_y = M + 170, bottom - 64
    q0x, q1x, q0y, q1y = axis_x + 24, W - M, top, axis_y - 24
    g, pad = 16, 26
    qw, qh = (q1x - q0x - g) / 2, (q1y - q0y - g) / 2
    size, plans = _fit_nodes(nodes, [qw - 2 * pad] * 4, [qh - 2 * pad] * 4, range(22, MIN_PX - 1, -1), title_max=28)
    hi = spec.get("highlight")
    off = (qh - max(p["h"] for p in plans)) / 2  # gleiche Oberkante in allen Feldern
    for i in range(4):
        x = q0x + (i % 2) * (qw + g)
        y = q0y + (i // 2) * (qh + g)
        owner = f"n{i + 1}"
        a = _accent(t, nodes[i], 1)
        sc.rrect((x, y, x + qw, y + qh), 18, t["card"], a if i == hi else t["rule"], 3 if i == hi else 2, owner)
        sc.boxes[owner] = (x + 8, y + 8, x + qw - 8, y + qh - 8)
        _draw_node(sc, plans[i], x + pad, y + off, qw - 2 * pad, size, a, owner, t["card"])
    sc.arrow((axis_x, axis_y), (axis_x, top), t["line"], "axis:y", width=3)
    sc.arrow((axis_x, axis_y), (W - M, axis_y), t["line"], "axis:x", width=3)
    errors = []

    def label(text, x, y, align, width, style="r", lines=2):
        fit = _fit(text, 19, MIN_PX, style, width, lines)
        if not fit:
            errors.append(f"Achsen-Beschriftung „{text}“ zu lang.")
            return
        sc.text(x, y, fit[1], fit[0], style, t["mut"] if style == "r" else t["ink"], t["bg"], "axis", align, width)

    lw = axis_x - M - 16
    if ay.get("high"):
        label(ay["high"], M, top + 4, "right", lw)
    if ay.get("low"):
        label(ay["low"], M, axis_y - 30, "right", lw)
    if ay.get("title"):
        label(ay["title"], M, (top + axis_y) / 2 - 24, "right", lw, "b", 3)
    if ax.get("low"):
        label(ax["low"], q0x, axis_y + 14, "left", 220)
    if ax.get("high"):
        label(ax["high"], W - M - 220, axis_y + 14, "right", 220)
    if ax.get("title"):
        label(ax["title"], (q0x + W - M) / 2 - 260, axis_y + 14, "center", 520, "b", 1)
    sc.boxes["axis"] = (M - 1, top - 1, W - M + 1, bottom + 1)
    return errors


def _ring(n, rx, ry, start):
    return [(rx * math.cos(math.radians(start + k * 360 / n)), ry * math.sin(math.radians(start + k * 360 / n))) for k in range(n)]


# Feste Satelliten-Positionen (Einheiten von rx/ry), im Uhrzeigersinn ab oben links/oben
_HUB_POS = {
    3: [(0, -1), (0.75, 1), (-0.75, 1)],
    4: [(-0.72, -1), (0.72, -1), (0.72, 1), (-0.72, 1)],
    5: [(0, -1), (0.9, -0.3), (0.6, 1), (-0.6, 1), (-0.9, -0.3)],
    6: [(-0.6, -1), (0.6, -1), (1, 0), (0.6, 1), (-0.6, 1), (-1, 0)],
}


def _hub(sc, spec, nodes, top, bottom):
    t = sc.t
    n = len(nodes)
    center = spec.get("center") or {}
    links = spec.get("links") or []
    if links and len(links) != n:
        raise LayoutError(f"hub: links braucht genau {n} Einträge (eine Verbindung je Element), hat {len(links)}.")
    sw, pad = (300 if n <= 4 else 270), 20
    cw = 360
    size, plans = _fit_nodes(nodes, [sw - 2 * pad] * n, [150 - 2 * pad] * n, range(20, MIN_PX - 1, -1), title_max=24)
    sh = max(p["h"] for p in plans) + 2 * pad
    cplan = _fit_nodes([center], [cw - 2 * pad], [200 - 2 * pad], range(21, MIN_PX - 1, -1), title_max=28)
    ch = cplan[1][0]["h"] + 2 * pad
    cx, cy = W / 2, (top + bottom) / 2
    crect = (cx - cw / 2, cy - ch / 2, cx + cw / 2, cy + ch / 2)
    sc.rrect(crect, 22, t["band"], None, 0, "center")
    sc.boxes["center"] = (crect[0] + 8, crect[1] + 8, crect[2] - 8, crect[3] - 8)
    cp = cplan[1][0]
    saved = t["ink"], t["txt"]
    t["ink"] = t["txt"] = t["band_ink"]
    _draw_node(sc, cp, crect[0] + pad, crect[1] + pad, cw - 2 * pad, cplan[0], t["band_ink"], "center", t["band"])
    t["ink"], t["txt"] = saved
    rx, ry = (W - 2 * M - sw) / 2, (bottom - top - sh) / 2
    errors = []
    for i, (dx, dy) in enumerate((ux * rx, uy * ry) for ux, uy in _HUB_POS[n]):
        a = _accent(t, nodes[i], i, n)
        owner = f"n{i + 1}"
        x, y = cx + dx - sw / 2, cy + dy - sh / 2
        rect = (x, y, x + sw, y + sh)
        sc.rrect(rect, 18, t["card"], a, 3, owner)
        sc.boxes[owner] = (x + 8, y + 8, x + sw - 8, y + sh - 8)
        _draw_node(sc, plans[i], x + pad, y + (sh - plans[i]["h"]) / 2, sw - 2 * pad, size, a, owner, t["card"])
        lk = links[i] if links else {}
        p = _edge(crect, (cx + dx, cy + dy))
        q = _edge(rect, (cx, cy))
        d = lk.get("dir", "none")
        sc.arrow(p, q, t["line"], f"link{i + 1}", width=3, start=d in ("in", "both"), end=d in ("out", "both"))
        if lk.get("label"):
            err = sc.pill((p[0] + q[0]) / 2, (p[1] + q[1]) / 2, lk["label"], f"link{i + 1}:pill")
            if err:
                errors.append(f"Verbindung {i + 1}: {err}")
    return errors


def _cycle(sc, spec, nodes, top, bottom):
    t = sc.t
    n = len(nodes)
    links = spec.get("links") or []
    if links and len(links) != n:
        raise LayoutError(f"cycle: links braucht genau {n} Einträge (Pfeil i → i+1, der letzte zurück zum ersten), hat {len(links)}.")
    sw, pad = (320 if n <= 4 else 280), 20
    size, plans = _fit_nodes(nodes, [sw - 2 * pad] * n, [150 - 2 * pad] * n, range(20, MIN_PX - 1, -1),
                             title_max=24, numbered=True)
    sh = max(p["h"] for p in plans) + 2 * pad
    cx, cy = W / 2, (top + bottom) / 2
    rx, ry = min(520, (W - 2 * M - sw) / 2), (bottom - top - sh) / 2
    pos = [(cx + dx, cy + dy) for dx, dy in _ring(n, rx, ry, -90)]
    rects = [(px - sw / 2, py - sh / 2, px + sw / 2, py + sh / 2) for px, py in pos]
    for i, rect in enumerate(rects):
        a = _accent(t, nodes[i], i, n)
        owner = f"n{i + 1}"
        sc.rrect(rect, 18, t["card"], a, 3, owner)
        sc.boxes[owner] = (rect[0] + 8, rect[1] + 8, rect[2] - 8, rect[3] - 8)
        _draw_node(sc, plans[i], rect[0] + pad, rect[1] + (sh - plans[i]["h"]) / 2, sw - 2 * pad, size, a, owner, t["card"], i + 1)
    errors = []
    for i in range(n):
        j = (i + 1) % n
        p, q = _edge(rects[i], pos[j], 10), _edge(rects[j], pos[i], 10)
        sc.arrow(p, q, t["line"], f"link{i + 1}", width=3)
        if links and links[i].get("label"):
            mx, my = (p[0] + q[0]) / 2, (p[1] + q[1]) / 2
            d = math.hypot(mx - cx, my - cy) or 1
            err = sc.pill(mx + (mx - cx) / d * 26, my + (my - cy) / d * 26, links[i]["label"], f"link{i + 1}:pill")
            if err:
                errors.append(f"Pfeil {i + 1}: {err}")
    center = spec.get("center")
    if center:
        fit = _fit(center.get("title", ""), 28, 22, "b", 360, 2)
        txt = _fit(center.get("text", ""), 20, MIN_PX, "r", 360, 3) if center.get("text") else (20, [])
        if not fit or not txt:
            errors.append("Zentrum: Titel ≤ 2 kurze Zeilen, Text ≤ 3 Zeilen – kürzen.")
        else:
            hh = _block_h(len(fit[1]), fit[0], "b") + (10 + _block_h(len(txt[1]), txt[0]) if txt[1] else 0)
            sc.boxes["center"] = (cx - 190, cy - hh / 2 - 10, cx + 190, cy + hh / 2 + 10)
            y = sc.text(cx - 180, cy - hh / 2, fit[1], fit[0], "b", t["ink"], t["bg"], "center", "center", 360)
            if txt[1]:
                sc.text(cx - 180, y + 10, txt[1], txt[0], "r", t["mut"], t["bg"], "center", "center", 360)
    return errors


def _nested(sc, spec, nodes, top, bottom):
    t = sc.t
    n = len(nodes)
    heads = []
    for i, node in enumerate(nodes[:-1]):
        p = _plan(dict(title=node.get("title", ""), text=node.get("text", "")), W - 2 * M - 56 - 80 * i, 20, title_max=26)
        if isinstance(p, str):
            raise LayoutError(f"Ebene {i + 1} („{node.get('title', '')}“): {p}")
        if p["text"] and len(p["text"]) > 1:
            raise LayoutError(f"Ebene {i + 1} („{node.get('title', '')}“): Text ≤ 1 Zeile (äußere Ebenen tragen nur Titel + Kurzsatz).")
        heads.append(p)
    shell = sum(p["h"] + 40 for p in heads) + 28 * (n - 1)
    w_in = W - 2 * M - 56 - 80 * (n - 1)
    if bottom - top - shell < 150:
        raise LayoutError("Zu viele Ebenen für die Höhe: maximal 3–4 Ebenen, Texte der äußeren Ebenen kürzen.")
    size, plans = _fit_nodes(nodes[-1:], [w_in], [bottom - top - shell - 56], range(24, MIN_PX - 1, -1), title_max=30)
    total = min(bottom - top, shell + max(plans[0]["h"] + 72, 170))
    y0 = top + (bottom - top - total) / 2
    rect = (M, y0, W - M, y0 + total)
    for i, node in enumerate(nodes):
        a = _accent(t, node, i)
        owner = f"n{i + 1}"
        fill = _mix(t["bg"], t["card"], 0.35 + 0.65 * i / (n - 1))
        sc.ops.append(("rrect", rect, 26, fill, a, 3))
        if i == n - 1:
            p = plans[0]
            sc.boxes[owner] = (rect[0] + 8, rect[1] + 8, rect[2] - 8, rect[3] - 8)
            sc.marks.append((("rect", rect), owner))
            _draw_node(sc, p, rect[0] + 28, rect[1] + (rect[3] - rect[1] - p["h"]) / 2, w_in, size, a, owner, fill)
            break
        p = heads[i]
        head = p["h"] + 40
        sc.boxes[owner] = (rect[0] + 8, rect[1] + 8, rect[2] - 8, rect[1] + head)
        _draw_node(sc, p, rect[0] + 28, rect[1] + 22, rect[2] - rect[0] - 56, 20, a, owner, fill)
        rect = (rect[0] + 40, rect[1] + head, rect[2] - 40, rect[3] - 28)
    return []


LAYOUTS = {"flow": _flow, "layers": _layers, "matrix": _matrix, "hub": _hub, "cycle": _cycle, "nested": _nested}


def _layout(spec, theme):
    sc = _Scene(dict(theme))
    t = sc.t
    kind = spec.get("layout", "flow")
    if kind not in LAYOUTS:
        raise LayoutError(f"Unbekanntes layout „{kind}“ – erlaubt: {', '.join(LAYOUTS)}.")
    nodes = spec.get("nodes") or spec.get("cards") or []
    lo, hi = LIMITS[kind]
    if not lo <= len(nodes) <= hi:
        allowed = f"genau {lo}" if lo == hi else f"{lo}–{hi}"
        raise LayoutError(f"{kind}: {len(nodes)} Elemente – erlaubt sind {allowed}. Zusammenfassen oder auf zwei Grafiken aufteilen.")
    for nd in nodes:  # Rückwärtskompatibel zu v1-Specs
        if "bullets" in nd and "points" not in nd:
            nd["points"] = nd["bullets"]
    fit = _fit(spec.get("title", ""), 44, 34, "b", W - 2 * M, 2)
    if not fit:
        raise LayoutError("Titel zu lang: auf ≤ 60 Zeichen kürzen.")
    y = sc.text(M, 48, fit[1], fit[0], "b", t["ink"], t["bg"], "page")
    if spec.get("subtitle"):
        fit = _fit(spec["subtitle"], 24, 20, "r", W - 2 * M, 2)
        if not fit:
            raise LayoutError("Subtitle zu lang: auf ≤ 120 Zeichen kürzen.")
        y = sc.text(M, y + 14, fit[1], fit[0], "r", t["mut"], t["bg"], "page")
    f_ascent, _ = _metrics(18)
    footer_top = H - 30 - f_ascent
    if spec.get("footer"):
        fit = _fit(spec["footer"], 18, 18, "r", W - 2 * M, 1)
        if not fit:
            raise LayoutError("Footer zu lang: auf ≤ 130 Zeichen kürzen.")
        sc.text(M, footer_top, fit[1], 18, "r", t["mut"], t["bg"], "page")
    top, bottom = y + 40, footer_top - 28
    if bottom - top < 420:
        raise LayoutError("Zu wenig Höhe: Titel/Subtitle kürzen.")
    errors = LAYOUTS[kind](sc, spec, nodes, top, bottom)
    if errors:
        raise LayoutError("\n".join(dict.fromkeys(errors)))
    return sc


# ---------------------------------------------------------------- Prüfung

def _lum(hexcolor):
    c = [int(hexcolor[i:i + 2], 16) / 255 for i in (1, 3, 5)]
    c = [v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4 for v in c]
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]


def _contrast(a, b):
    la, lb = sorted((_lum(a), _lum(b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def _hit(a, b):
    return not (a[2] <= b[0] or b[2] <= a[0] or a[3] <= b[1] or b[3] <= a[1])


def _seg_hits(p, q, r):
    """Liang-Barsky: schneidet Strecke p-q das Rechteck r?"""
    t0, t1 = 0.0, 1.0
    dx, dy = q[0] - p[0], q[1] - p[1]
    for pk, qk in ((-dx, p[0] - r[0]), (dx, r[2] - p[0]), (-dy, p[1] - r[1]), (dy, r[3] - p[1])):
        if pk == 0:
            if qk < 0:
                return False
        else:
            tk = qk / pk
            if pk < 0:
                t0 = max(t0, tk)
            else:
                t1 = min(t1, tk)
    return t0 <= t1


def _lint(sc, spec):
    problems = []
    for bb, owner, fg, bg, size, style, text in sc.texts:
        if size < MIN_PX:
            problems.append(f"Schrift {size}px < {MIN_PX}px: „{text}“")
        need = 3.0 if (size >= 24 or (style == "b" and size >= 19)) else 4.5
        cr = _contrast(fg, bg)
        if cr < need:
            problems.append(f"Kontrast {cr:.2f}:1 < {need}:1: „{text}“")
        if bb[0] < M - 1 or bb[2] > W - M + 1 or bb[1] < 16 or bb[3] > H - 16:
            problems.append(f"außerhalb Satzspiegel: „{text}“")
        box = sc.boxes.get(owner.split(":")[0]) if owner.endswith(":chip") else sc.boxes.get(owner)
        if box and not (bb[0] >= box[0] - 0.5 and bb[1] >= box[1] - 0.5 and bb[2] <= box[2] + 0.5 and bb[3] <= box[3] + 0.5):
            problems.append(f"ragt aus {owner}: „{text}“")
    texts = [x for x in sc.texts if not x[1].endswith(":badge")]
    for i in range(len(texts)):
        for j in range(i + 1, len(texts)):
            if _hit(texts[i][0], texts[j][0]):
                problems.append(f"Text-Kollision: „{texts[i][6]}“ × „{texts[j][6]}“")
    for bb, owner, *_rest, text in texts:
        for mark, mowner in sc.marks:
            if owner == mowner or owner.startswith(mowner + ":"):
                continue
            hit = _hit(mark[1], bb) if mark[0] == "rect" else _seg_hits(mark[1], mark[2], bb)
            if hit:
                problems.append(f"„{text}“ berührt {mowner}")
    rects = [(m[1], o) for m, o in sc.marks if m[0] == "rect" and (o == "center" or (o.startswith("n") and ":" not in o))]
    if spec.get("layout", "flow") != "nested":
        for i in range(len(rects)):
            for j in range(i + 1, len(rects)):
                if _hit(rects[i][0], rects[j][0]):
                    problems.append(f"Elemente überlappen: {rects[i][1]} × {rects[j][1]}")
    words = sum(len(str(x[6]).split()) for x in sc.texts if not x[1].endswith(":badge"))
    return problems, words, min(x[4] for x in sc.texts), min(_contrast(x[2], x[3]) for x in sc.texts)


# ---------------------------------------------------------------- Ausgabe

def _draw(sc):
    t = sc.t
    im = Image.new("RGB", (W * S, H * S), t["bg"])
    d = ImageDraw.Draw(im)

    def p(v):
        return round(v * S)

    for op in sc.ops:
        kind = op[0]
        if kind == "rrect":
            _, (x0, y0, x1, y1), r, fill, outline, width = op
            d.rounded_rectangle((p(x0), p(y0), p(x1), p(y1)), radius=p(r), fill=fill, outline=outline, width=p(width))
        elif kind == "circle":
            _, cx, cy, r, fill, outline, width = op
            d.ellipse((p(cx - r), p(cy - r), p(cx + r), p(cy + r)), fill=fill, outline=outline, width=p(width))
        elif kind == "line":
            _, pts, fill, width = op
            d.line([(p(x), p(y)) for x, y in pts], fill=fill, width=p(width))
        elif kind == "poly":
            _, pts, fill = op
            d.polygon([(p(x), p(y)) for x, y in pts], fill=fill)
        elif kind == "dash":
            _, x, y0, y1, fill = op
            y = y0
            while y < y1:
                d.line([(p(x), p(y)), (p(x), p(min(y + 8, y1)))], fill=fill, width=p(2))
                y += 14
        elif kind == "lock":
            _, cx, cy, fill = op
            d.arc((p(cx - 7), p(cy - 13), p(cx + 7), p(cy + 1)), 180, 360, fill=fill, width=p(3))
            d.line([(p(cx - 7), p(cy - 6)), (p(cx - 7), p(cy - 1))], fill=fill, width=p(3))
            d.line([(p(cx + 7), p(cy - 6)), (p(cx + 7), p(cy - 1))], fill=fill, width=p(3))
            d.rounded_rectangle((p(cx - 11), p(cy - 2), p(cx + 11), p(cy + 13)), radius=p(3), fill=fill)
        elif kind == "ctext":
            _, x, y, s, size, style, fill = op
            d.text((p(x), p(y)), s, font=_font(size, style), fill=fill, anchor="mm")
        elif kind == "text":
            _, x, y, s, size, style, fill = op
            d.text((p(x), p(y)), s, font=_font(size, style), fill=fill, anchor="ls")
    if SS > 1:
        im = im.resize((W * OUT, H * OUT), Image.Resampling.LANCZOS)
    return im


def _svg(sc, label):
    """Dieselbe Szene als Vektor. textLength fixiert die gemessene Breite, falls DejaVu Sans fehlt."""
    e = html.escape
    f = lambda v: f"{v:.1f}".rstrip("0").rstrip(".")  # noqa: E731
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" role="img" aria-label="{e(label)}" '
           f'font-family="DejaVu Sans, Verdana, sans-serif"><rect width="{W}" height="{H}" fill="{sc.t["bg"]}"/>']
    for op in sc.ops:
        kind = op[0]
        if kind == "rrect":
            _, (x0, y0, x1, y1), r, fill, outline, width = op
            i = width / 2 if outline else 0
            stroke = f' stroke="{outline}" stroke-width="{f(width)}"' if outline else ""
            out.append(f'<rect x="{f(x0 + i)}" y="{f(y0 + i)}" width="{f(x1 - x0 - 2 * i)}" height="{f(y1 - y0 - 2 * i)}" '
                       f'rx="{f(max(r - i, 0))}" fill="{fill or "none"}"{stroke}/>')
        elif kind == "circle":
            _, cx, cy, r, fill, outline, width = op
            i = width / 2 if outline else 0
            stroke = f' stroke="{outline}" stroke-width="{f(width)}"' if outline else ""
            out.append(f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(r - i)}" fill="{fill or "none"}"{stroke}/>')
        elif kind == "line":
            _, pts, color, width = op
            out.append(f'<polyline points="{" ".join(f"{f(x)},{f(y)}" for x, y in pts)}" fill="none" stroke="{color}" stroke-width="{f(width)}"/>')
        elif kind == "poly":
            _, pts, color = op
            out.append(f'<polygon points="{" ".join(f"{f(x)},{f(y)}" for x, y in pts)}" fill="{color}"/>')
        elif kind == "dash":
            _, x, y0, y1, color = op
            out.append(f'<line x1="{f(x)}" y1="{f(y0)}" x2="{f(x)}" y2="{f(y1)}" stroke="{color}" stroke-width="2" stroke-dasharray="8 6"/>')
        elif kind == "lock":
            _, cx, cy, color = op
            out.append(f'<path d="M{f(cx - 7)} {f(cy - 1)}V{f(cy - 6)}A7 7 0 0 1 {f(cx + 7)} {f(cy - 6)}V{f(cy - 1)}" fill="none" '
                       f'stroke="{color}" stroke-width="3"/><rect x="{f(cx - 11)}" y="{f(cy - 2)}" width="22" height="15" rx="3" fill="{color}"/>')
        elif kind in ("text", "ctext"):
            _, x, y, s, size, style, color = op
            wt = ' font-weight="bold"' if style == "b" else ""
            it = ' font-style="italic"' if style == "i" else ""
            anchor = ' text-anchor="middle" dominant-baseline="central"' if kind == "ctext" else ""
            out.append(f'<text x="{f(x)}" y="{f(y)}" font-size="{size}"{wt}{it}{anchor} fill="{color}" '
                       f'textLength="{f(_len(s, size, style))}" lengthAdjust="spacingAndGlyphs">{e(s)}</text>')
    out.append("</svg>")
    return "".join(out)


def _outline(spec, nodes):
    """Textfassung für Screenreader, Suche und schmale Bildschirme."""
    e = html.escape
    parts = []
    for key in ("center", "side", "band"):
        if spec.get(key):
            parts.append(f'<p><strong>{e(spec[key].get("title", ""))}</strong> {e(spec[key].get("text", ""))}</p>')
    axes = spec.get("axes") or {}
    for k, name in (("x", "Waagrecht"), ("y", "Senkrecht")):
        a = axes.get(k)
        if a:
            parts.append(f'<p>{name}: {e(a.get("title", ""))} ({e(a.get("low", ""))} → {e(a.get("high", ""))})</p>')
    links = spec.get("links") or spec.get("gates") or []
    items = []
    for i, nd in enumerate(nodes):
        body = "".join(f"<li>{e(x)}</li>" for x in nd.get("points", []) + nd.get("items", []))
        link = links[i] if i < len(links) else {}
        lk = f'<p>→ {e(link.get("label", ""))} {e(link.get("note", ""))}</p>' if link.get("label") or link.get("note") else ""
        items.append(f'<li><h2>{e(nd.get("title", ""))}</h2>{"<p>" + e(nd["text"]) + "</p>" if nd.get("text") else ""}'
                     f'{"<ul>" + body + "</ul>" if body else ""}{lk}</li>')
    return "".join(parts) + f"<ol>{''.join(items)}</ol>"


def _alt(spec, nodes):
    kind = spec.get("layout", "flow")
    names = [nd.get("title", "") for nd in nodes]
    links = spec.get("links") or spec.get("gates") or []
    lead = {
        "flow": "Abfolge: " + " → ".join(names),
        "layers": "Schichten von oben nach unten: " + ", ".join(names),
        "matrix": "Vier Felder (oben links, oben rechts, unten links, unten rechts): " + ", ".join(names),
        "hub": f"Im Zentrum {(spec.get('center') or {}).get('title', '')}, verbunden mit: " + ", ".join(
            f"{x} ({links[i]['label']})" if i < len(links) and links[i].get("label") else x for i, x in enumerate(names)),
        "cycle": "Kreislauf: " + " → ".join(names) + " → zurück zu " + (names[0] if names else ""),
        "nested": "Von außen nach innen: " + " ⊃ ".join(names),
    }[kind]
    parts = [spec.get("title", ""), spec.get("subtitle", ""), lead]
    return ". ".join(p.strip(". ") for p in parts if p) + "."


def _html(spec, nodes, sc, alt):
    e = html.escape
    t = sc.t
    css = (f"body{{margin:0;background:{t['bg']};color:{t['txt']};font:16px/1.5 system-ui,-apple-system,'Segoe UI',Roboto,sans-serif}}"
           "figure{margin:0 auto;max-width:1600px;padding:clamp(8px,2vw,24px)}svg{display:block;width:100%;height:auto}"
           ".outline{max-width:70ch;margin:0 auto;padding:0 16px 24px}"
           f".outline h2{{font-size:1.05rem;color:{t['ink']};margin:.8rem 0 .2rem}}"
           "@media (min-width:760px){.outline{position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%)}}")
    sub = f'<p>{e(spec["subtitle"])}</p>' if spec.get("subtitle") else ""
    return (f'<!doctype html><html lang="{e(spec.get("lang", "de"))}"><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1"><title>{e(spec.get("title", ""))}</title>'
            f'<style>{css}</style></head><body><figure>{_svg(sc, alt)}</figure>'
            f'<section class="outline" aria-label="Inhalt als Text">{sub}{_outline(spec, nodes)}</section></body></html>')


def render(spec, name="grafik", outdir="."):
    theme = THEMES[spec.get("theme", "dark")]
    sc = _layout(spec, theme)
    nodes = spec.get("nodes") or spec.get("cards") or []
    problems, words, min_px, min_cr = _lint(sc, spec)
    if problems:
        raise LayoutError("Prüfung fehlgeschlagen:\n" + "\n".join(dict.fromkeys(problems)))
    alt = spec.get("alt") or _alt(spec, nodes)
    im = _draw(sc)
    info = PngInfo()
    info.add_text("Title", spec.get("title", ""))
    info.add_text("Description", alt)
    png = os.path.join(outdir, f"{name}.png")
    # Flächige Grafik: 256-Farben-Palette ist visuell verlustfrei und ~4x kleiner (Base64 im Tool-Result!)
    im = im.quantize(256, method=Image.Quantize.FASTOCTREE, dither=Image.Dither.NONE)
    im.save(png, format="PNG", optimize=True, pnginfo=info)
    page = os.path.join(outdir, f"{name}.html")
    with open(page, "w", encoding="utf-8") as fh:
        fh.write(_html(spec, nodes, sc, alt))
    warn = f" · WARNUNG: {words} Wörter > {MAX_WORDS} (kürzen; Erklärungen in den Antworttext)" if words > MAX_WORDS else ""
    ignored = sorted({k for nd in nodes for k in nd} - {"title", "text", "points", "items", "accent", "bullets"})
    if ignored:
        warn += f" · WARNUNG: ignorierte Felder {', '.join(ignored)}"
    report = (f"PRÜFUNG OK · {spec.get('layout', 'flow')} · {len(nodes)} Elemente · {words} Wörter · "
              f"kleinste Schrift {min_px}px · min. Kontrast {min_cr:.1f}:1 · 0 Kollisionen · "
              f"{png} {im.size[0]}×{im.size[1]} {os.path.getsize(png) // 1024} KB · {page} {os.path.getsize(page) // 1024} KB{warn}")
    print(report)
    print("ALT:", alt)
    return {"png": png, "html": page, "alt": alt, "report": report}
