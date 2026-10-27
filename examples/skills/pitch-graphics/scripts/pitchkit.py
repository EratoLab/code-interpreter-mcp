"""pitchkit – Konzept-Grafiken (PNG + HTML) aus einer Inhalts-Spec.

Nur Pillow nötig. Läuft in Pyodide (code-interpreter-mcp) und CPython.
Das Modell liefert nur SPEC (Inhalt); Layout, Fitting, Rendering und Prüfung
sind deterministisch. Nutzung:

    render(SPEC, "zonen")   # -> zonen.png (3200x1800) + zonen.html, druckt Prüfbericht

Bei Layoutproblemen wirft render() LayoutError mit konkreter Kürzungsanweisung
und schreibt keine Dateien.
"""

import html
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
MAX_WORDS = 90           # Pitch-Dichte: darüber Warnung

THEMES = {
    "dark": dict(
        bg="#0F172A", card="#1E293B", inset="#0B1220", ink="#F8FAFC", txt="#E2E8F0",
        mut="#94A3B8", rule="#334155", line="#64748B", band="#4F46E5", band_ink="#FFFFFF",
        accents=["#34D399", "#60A5FA", "#FBBF24", "#A78BFA"], on_accent="#0B1220",
        quote_in_accent=True,
    ),
    "light": dict(
        bg="#F8FAFC", card="#FFFFFF", inset="#F1F5F9", ink="#0F172A", txt="#1E293B",
        mut="#475569", rule="#E2E8F0", line="#94A3B8", band="#4338CA", band_ink="#FFFFFF",
        accents=["#047857", "#2563EB", "#B45309", "#7C3AED"], on_accent="#FFFFFF",
        quote_in_accent=False,
    ),
}


class LayoutError(Exception):
    pass


# ---------------------------------------------------------------- Fonts

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


# ---------------------------------------------------------------- Szene

class _Scene:
    def __init__(self, theme):
        self.t = theme
        self.ops = []        # Zeichenoperationen in logischen Koordinaten
        self.texts = []      # (bbox, owner, fg, bg, size, style, text)
        self.marks = []      # (bbox, owner) – feste Formen, die kein Text berühren darf
        self.boxes = {}      # owner -> Innenbereich (Containment)

    def text_block(self, x, top, lines, size, style, fg, bg, owner, align="left", width=None):
        ascent, descent = _metrics(size, style)
        lh = _line_h(size)
        y = top + ascent
        for line in lines:
            w = _len(line, size, style)
            lx = x + (width - w) / 2 if align == "center" else x
            self.ops.append(("text", lx, y, line, size, style, fg))
            self.texts.append(((lx, y - ascent, lx + w, y + descent), owner, fg, bg, size, style, line))
            y += lh
        return top + lh * (len(lines) - 1) + ascent + descent  # Unterkante

    def block_height(self, n_lines, size, style="r"):
        ascent, descent = _metrics(size, style)
        return _line_h(size) * (n_lines - 1) + ascent + descent


def _layout(spec, theme):
    sc = _Scene(theme)
    t = theme
    cards = spec.get("cards") or []
    n = len(cards)
    if not 1 <= n <= 4:
        raise LayoutError(f"{n} Karten: erlaubt sind 1–4 pro Grafik. Auf zwei Grafiken aufteilen oder Elemente zusammenfassen.")
    gates = spec.get("gates") or []
    if gates and len(gates) != n - 1:
        raise LayoutError(f"gates braucht genau {n - 1} Einträge (zwischen {n} Karten), hat {len(gates)}.")
    errors = []

    # Kopf
    fit = _fit(spec.get("title", ""), 44, 34, "b", W - 2 * M, 2)
    if not fit:
        raise LayoutError("Titel zu lang: auf ≤ 60 Zeichen kürzen.")
    y = sc.text_block(M, 48, fit[1], fit[0], "b", t["ink"], t["bg"], "page")
    if spec.get("subtitle"):
        fit = _fit(spec["subtitle"], 24, 20, "r", W - 2 * M, 2)
        if not fit:
            raise LayoutError("Subtitle zu lang: auf ≤ 120 Zeichen kürzen.")
        y = sc.text_block(M, y + 14, fit[1], fit[0], "r", t["mut"], t["bg"], "page")
    cards_top = y + 40

    # Fuß, Band
    footer = spec.get("footer", "")
    f_ascent, f_descent = _metrics(18)
    footer_top = H - 30 - f_ascent
    if footer:
        fit = _fit(footer, 18, 18, "r", W - 2 * M, 1)
        if not fit:
            raise LayoutError("Footer zu lang: auf ≤ 130 Zeichen kürzen.")
        sc.text_block(M, footer_top, fit[1], 18, "r", t["mut"], t["bg"], "page")
    band = spec.get("band")
    if band:
        band_bottom = footer_top - 22
        band_top = band_bottom - 112
        cards_bottom = band_top - 40
    else:
        cards_bottom = footer_top - 30
    card_h = cards_bottom - cards_top
    if card_h < 280:
        raise LayoutError("Zu wenig Höhe für Karten: Titel/Subtitle kürzen oder Band weglassen.")

    # Kartenraster
    gap = {1: 0, 2: 220, 3: 168, 4: 132}[n] if gates else 40
    cw = (W - 2 * M - (n - 1) * gap) / n
    xs = [M + i * (cw + gap) for i in range(n)]
    pad = 28 if n <= 3 else 22
    inner = cw - 2 * pad
    accents = t["accents"]

    def accent(i):
        a = cards[i].get("accent", i)
        return accents[a % len(accents)] if isinstance(a, int) else a

    # Einheitliche Bullet-Größe über alle Karten: größte, die überall passt
    plans = None
    worst = {}
    for size in range(26 if n <= 3 else 24, 19, -1):
        plans, worst = [], {}
        for i, c in enumerate(cards):
            title_fit = _fit(c.get("title", ""), 30, 22, "b", inner - 54, 2)
            if not title_fit:
                errors.append(f"Karte {i + 1}: Titel „{c.get('title')}“ zu lang (≤ 2 kurze Zeilen).")
                continue
            head_h = max(40, sc.block_height(len(title_fit[1]), title_fit[0], "b"))
            bullets = []
            for b in c.get("bullets", []):
                lines = _wrap(b, size, "r", inner - 22)
                if lines is None:
                    errors.append(f"Karte {i + 1}: Wort in „{b}“ breiter als die Karte – umformulieren oder mit Bindestrich trennen.")
                    lines = [b]
                bullets.append(lines)
            bul_h = sum(sc.block_height(len(ls), size) + 12 for ls in bullets)
            quote = None
            q_h = 0
            if c.get("quote"):
                quote = _fit(c["quote"], max(size - 2, MIN_PX), MIN_PX, "i", inner - 36, 3)
                if not quote:
                    errors.append(f"Karte {i + 1}: Zitat zu lang (≤ 3 Zeilen, ~{int((inner - 36) / 11 * 3)} Zeichen).")
                    quote = (MIN_PX, [c["quote"]])
                q_h = sc.block_height(len(quote[1]), quote[0], "i") + 32
            need = pad + head_h + 26 + bul_h + (16 + q_h if quote else 0) + pad
            worst[i] = need - card_h
            plans.append((title_fit, head_h, bullets, quote, q_h))
        if errors:
            raise LayoutError("\n".join(dict.fromkeys(errors)))
        if max(worst.values()) <= 0:
            break
    else:
        msgs = [f"Karte {i + 1}: {int(v)} px zu hoch (≈ {max(1, int(v / 30))} Zeile(n) kürzen)" for i, v in worst.items() if v > 0]
        raise LayoutError("Inhalt passt nicht, auch nicht bei 20 px Fließtext:\n" + "\n".join(msgs))
    bullet_size = size

    # Karten auf Inhalt kürzen (nicht unter 60 %) und Komposition vertikal zentrieren
    fitted = max(min(card_h, card_h + max(worst.values()) + 24), card_h * 0.6)
    shift = (card_h - fitted) / 2
    cards_top += shift
    cards_bottom = cards_top + fitted
    if band:
        band_top -= shift
        band_bottom -= shift

    for i, (c, x) in enumerate(zip(cards, xs)):
        title_fit, head_h, bullets, quote, q_h = plans[i]
        a = accent(i)
        owner = f"card{i + 1}"
        sc.ops.append(("rrect", (x, cards_top, x + cw, cards_bottom), 22, t["card"], a, 3))
        sc.boxes[owner] = (x + 8, cards_top + 8, x + cw - 8, cards_bottom - 8)
        cx, cy = x + pad + 20, cards_top + pad + 20
        sc.ops.append(("circle", cx, cy, 20, a, None, 0))
        sc.marks.append(((cx - 20, cy - 20, cx + 20, cy + 20), owner + ":mark"))
        num = str(i + 1)
        sc.ops.append(("ctext", cx, cy, num, 20, "b", t["on_accent"]))
        sc.texts.append(((cx - 7, cy - 12, cx + 7, cy + 12), owner + ":badge", t["on_accent"], a, 20, "b", num))
        th = sc.block_height(len(title_fit[1]), title_fit[0], "b")
        y = sc.text_block(x + pad + 54, cards_top + pad + (head_h - th) / 2, title_fit[1], title_fit[0], "b", t["ink"], t["card"], owner)
        y = cards_top + pad + head_h + 9
        sc.ops.append(("line", [(x + pad, y), (x + cw - pad, y)], t["rule"], 2))
        y += 9 + 8
        for lines in bullets:
            asc, _ = _metrics(bullet_size)
            dot_y = y + asc * 0.62
            sc.ops.append(("circle", x + pad + 5, dot_y, 4, a, None, 0))
            sc.marks.append(((x + pad + 1, dot_y - 4, x + pad + 9, dot_y + 4), owner + ":mark"))
            y = sc.text_block(x + pad + 22, y, lines, bullet_size, "r", t["txt"], t["card"], owner) + 12
        if quote:
            q_top = cards_bottom - pad - q_h
            qbox = (x + pad, q_top, x + cw - pad, cards_bottom - pad)
            sc.ops.append(("rrect", qbox, 12, t["inset"], None, 0))
            sc.ops.append(("rect", (x + pad, q_top + 10, x + pad + 4, cards_bottom - pad - 10), a))
            sc.boxes[owner + ":quote"] = (qbox[0] + 8, qbox[1] + 4, qbox[2] - 8, qbox[3] - 4)
            sc.marks.append((qbox, owner + ":quote"))
            q_fg = a if t["quote_in_accent"] else t["txt"]
            sc.text_block(x + pad + 20, q_top + 16, quote[1], quote[0], "i", q_fg, t["inset"], owner + ":quote")

    # Gates
    mid = (cards_top + cards_bottom) / 2
    for i, g in enumerate(gates):
        owner = f"gate{i + 1}"
        gx0, gx1 = xs[i] + cw, xs[i + 1]
        gcx = (gx0 + gx1) / 2
        gw = gap - 20
        sc.boxes[owner] = (gx0 + 6, cards_top, gx1 - 6, cards_bottom)
        sc.ops.append(("line", [(gx0 + 12, mid), (gx1 - 22, mid)], t["line"], 4))
        sc.ops.append(("poly", [(gx1 - 10, mid), (gx1 - 26, mid - 10), (gx1 - 26, mid + 10)], t["line"]))
        sc.marks.append(((gx0 + 12, mid - 10, gx1 - 10, mid + 10), owner + ":mark"))
        if g.get("lock", False):
            sc.ops.append(("circle", gcx, mid, 24, t["bg"], t["line"], 3))
            sc.ops.append(("lock", gcx, mid, t["mut"]))
            sc.marks.append(((gcx - 24, mid - 24, gcx + 24, mid + 24), owner + ":mark"))
        off = 36 if g.get("lock", False) else 24
        label = _fit(g.get("label", ""), 20, MIN_PX, "b", gw, 2) if g.get("label") else (20, [])
        if not label:
            errors.append(f"Gate {i + 1}: Label „{g['label']}“ zu lang (Lücke {gw:.0f} px ≈ {int(gw / 12)} Zeichen/Zeile).")
        elif label[1]:
            lh = sc.block_height(len(label[1]), label[0], "b")
            sc.text_block(gx0 + 10, mid - off - lh, label[1], label[0], "b", t["ink"], t["bg"], owner, "center", gw)
        if g.get("note"):
            note = _fit(g["note"], MIN_PX, MIN_PX, "r", gw, 3)
            if not note:
                errors.append(f"Gate {i + 1}: Notiz „{g['note']}“ zu lang (≤ 3 Zeilen à ≈ {int(gw / 10)} Zeichen).")
            else:
                sc.text_block(gx0 + 10, mid + off, note[1], MIN_PX, "r", t["mut"], t["bg"], owner, "center", gw)

    # Band + Konnektoren
    if band:
        bx0, bx1 = M, W - M
        sc.ops.append(("rrect", (bx0, band_top, bx1, band_bottom), 18, t["band"], None, 0))
        sc.boxes["band"] = (bx0 + 8, band_top + 6, bx1 - 8, band_bottom - 6)
        for x in xs:
            cx = x + cw / 2
            sc.ops.append(("dash", cx, cards_bottom + 6, band_top - 6, t["line"]))
            sc.marks.append(((cx - 2, cards_bottom + 6, cx + 2, band_top - 6), "band:mark"))
        bt = _fit(band.get("title", ""), 26, 22, "b", bx1 - bx0 - 64, 1)
        btxt = _fit(band.get("text", ""), 21, MIN_PX, "r", bx1 - bx0 - 64, 2) if band.get("text") else (21, [])
        if not bt or not btxt:
            errors.append("Band: Titel ≤ 1 Zeile, Text ≤ 2 Zeilen – kürzen.")
        else:
            total = sc.block_height(1, bt[0], "b") + (10 + sc.block_height(len(btxt[1]), btxt[0]) if btxt[1] else 0)
            y = band_top + (112 - total) / 2
            y = sc.text_block(bx0 + 32, y, bt[1], bt[0], "b", t["band_ink"], t["band"], "band")
            if btxt[1]:
                sc.text_block(bx0 + 32, y + 10, btxt[1], btxt[0], "r", t["band_ink"], t["band"], "band")

    if errors:
        raise LayoutError("\n".join(dict.fromkeys(errors)))
    sc.bullet_size = bullet_size
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


def _lint(sc, spec):
    problems = []
    for bb, owner, fg, bg, size, style, text in sc.texts:
        if size < MIN_PX:
            problems.append(f"Schrift {size}px < {MIN_PX}px: „{text}“")
        big = size >= 24 or (style == "b" and size >= 19)
        need = 3.0 if big else 4.5
        cr = _contrast(fg, bg)
        if cr < need:
            problems.append(f"Kontrast {cr:.2f}:1 < {need}:1: „{text}“")
        if bb[0] < M - 1 or bb[2] > W - M + 1 or bb[1] < 16 or bb[3] > H - 16:
            problems.append(f"außerhalb Satzspiegel: „{text}“")
        box = sc.boxes.get(owner)
        if box and not (bb[0] >= box[0] - 0.5 and bb[1] >= box[1] - 0.5 and bb[2] <= box[2] + 0.5 and bb[3] <= box[3] + 0.5):
            problems.append(f"ragt aus {owner}: „{text}“")
    texts = [x for x in sc.texts if not x[1].endswith(":badge")]
    for i in range(len(texts)):
        for j in range(i + 1, len(texts)):
            if _hit(texts[i][0], texts[j][0]):
                problems.append(f"Text-Kollision: „{texts[i][6]}“ × „{texts[j][6]}“")
    for bb, owner, *_rest, text in texts:
        for mb, mowner in sc.marks:
            if mowner != owner and _hit(bb, mb):
                problems.append(f"Text berührt Form ({mowner}): „{text}“")
    words = sum(len(str(x[6]).split()) for x in sc.texts if not x[1].endswith(":badge"))
    accents = len({c.get("accent", i) for i, c in enumerate(spec.get("cards", []))})
    if accents > 4:
        problems.append(f"{accents} Akzentfarben > 4")
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
        elif kind == "rect":
            _, (x0, y0, x1, y1), fill = op
            d.rectangle((p(x0), p(y0), p(x1), p(y1)), fill=fill)
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


def _alt(spec):
    parts = [spec.get("title", ""), spec.get("subtitle", "")]
    gates = spec.get("gates") or []
    for i, c in enumerate(spec.get("cards", [])):
        parts.append(f"{i + 1}. {c.get('title', '')}: " + "; ".join(c.get("bullets", [])))
        if i < len(gates) and gates[i].get("label"):
            parts.append(f"Übergang: {gates[i]['label']}")
    if spec.get("band"):
        parts.append(f"Querschicht: {spec['band'].get('title', '')}")
    return ". ".join(p.strip(". ") for p in parts if p) + "."


def _html(spec, theme):
    t = theme
    e = html.escape
    accents = t["accents"]
    gates = spec.get("gates") or []
    lock_svg = ('<svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true"><path d="M7 11V8a5 5 0 0 1 10 0v3" '
                'fill="none" stroke="currentColor" stroke-width="2.4"/><rect x="5" y="11" width="14" height="10" rx="2.5" fill="currentColor"/></svg>')
    arrow_svg = ('<svg class="arrow h" viewBox="0 0 64 16" width="64" height="16" aria-hidden="true"><path d="M2 8h50" stroke="currentColor" '
                 'stroke-width="3"/><path d="M62 8 50 1v14z" fill="currentColor"/></svg>'
                 '<svg class="arrow v" viewBox="0 0 16 40" width="16" height="40" aria-hidden="true"><path d="M8 2v26" stroke="currentColor" '
                 'stroke-width="3"/><path d="M8 38 1 26h14z" fill="currentColor"/></svg>')
    parts = []
    for i, c in enumerate(spec.get("cards", [])):
        a = c.get("accent", i)
        a = accents[a % len(accents)] if isinstance(a, int) else a
        bullets = "".join(f"<li>{e(b)}</li>" for b in c.get("bullets", []))
        quote = f'<blockquote>{e(c["quote"])}</blockquote>' if c.get("quote") else ""
        parts.append(f'<article class="card" style="--a:{a}"><header><span class="badge" aria-hidden="true">{i + 1}</span>'
                     f'<h2>{e(c.get("title", ""))}</h2></header><ul>{bullets}</ul>{quote}</article>')
        if i < len(gates):
            g = gates[i]
            lock = f'<span class="lock">{lock_svg}</span>' if g.get("lock", False) else ""
            note = f'<span class="note">{e(g["note"])}</span>' if g.get("note") else ""
            parts.append(f'<div class="gate" role="note"><strong>{e(g.get("label", ""))}</strong>'
                         f'<span class="flow">{lock}{arrow_svg}</span>{note}</div>')
    band = spec.get("band")
    band_html = (f'<aside class="band"><h2>{e(band.get("title", ""))}</h2><p>{e(band.get("text", ""))}</p></aside>' if band else "")
    sub = f'<p class="sub">{e(spec["subtitle"])}</p>' if spec.get("subtitle") else ""
    foot = f'<footer>{e(spec["footer"])}</footer>' if spec.get("footer") else ""
    css = f"""
:root{{--bg:{t['bg']};--card:{t['card']};--inset:{t['inset']};--ink:{t['ink']};--txt:{t['txt']};--mut:{t['mut']};
--line:{t['line']};--band:{t['band']};--band-ink:{t['band_ink']};--on-a:{t['on_accent']}}}
*{{box-sizing:border-box}}
body{{margin:0;background:var(--bg);color:var(--txt);font:clamp(15px,1.15vw,19px)/1.45 system-ui,-apple-system,"Segoe UI",Roboto,"DejaVu Sans",sans-serif}}
main{{max-width:1500px;margin:0 auto;padding:clamp(16px,4vw,56px)}}
h1{{color:var(--ink);font-size:clamp(1.6rem,2.9vw,2.75rem);line-height:1.15;margin:0}}
.sub{{color:var(--mut);font-size:1.2em;margin:.5em 0 0}}
.row{{display:flex;align-items:stretch;margin-top:2.2rem}}
.card{{flex:1 1 0;min-width:0;background:var(--card);border:3px solid var(--a);border-radius:22px;padding:1.3rem 1.4rem;display:flex;flex-direction:column}}
.card header{{display:flex;align-items:center;gap:.8rem;padding-bottom:.8rem;border-bottom:2px solid color-mix(in srgb,var(--line) 45%,transparent)}}
.badge{{flex:none;width:2.2rem;height:2.2rem;border-radius:50%;background:var(--a);color:var(--on-a);font-weight:700;display:grid;place-items:center}}
h2{{color:var(--ink);font-size:1.35em;line-height:1.2;margin:0}}
.card ul{{margin:1rem 0 0;padding:0;list-style:none;display:grid;gap:.6rem}}
.card li{{position:relative;padding-left:1.3rem}}
.card li::before{{content:"";position:absolute;left:.1rem;top:.55em;width:.5rem;height:.5rem;border-radius:50%;background:var(--a)}}
blockquote{{margin:auto 0 0;padding:.8rem 1rem;background:var(--inset);border-left:4px solid var(--a);border-radius:12px;font-style:italic;
color:{'var(--a)' if t['quote_in_accent'] else 'var(--txt)'}}}
.card ul+blockquote{{margin-top:1.2rem}}
.gate{{flex:0 0 clamp(110px,9vw,170px);display:flex;flex-direction:column;align-items:center;justify-content:center;gap:.5rem;text-align:center;padding:0 .6rem}}
.gate strong{{color:var(--ink)}}
.gate .note{{color:var(--mut);font-size:.9em}}
.flow{{display:flex;align-items:center;color:var(--line)}}
.arrow.v{{display:none}}
.lock{{display:grid;place-items:center;width:2.6rem;height:2.6rem;border:3px solid var(--line);border-radius:50%;color:var(--mut);margin-right:-.4rem;background:var(--bg)}}
.band{{margin-top:2rem;background:var(--band);color:var(--band-ink);border-radius:18px;padding:1.2rem 1.8rem}}
.band h2{{color:var(--band-ink)}} .band p{{margin:.3rem 0 0}}
footer{{margin-top:1.4rem;color:var(--mut);font-size:.85em}}
@media (max-width:900px){{.row{{flex-direction:column}}.gate{{flex-basis:auto;padding:.9rem 0}}.flow{{flex-direction:column}}
.lock{{margin:0 0 -.2rem}}.arrow.h{{display:none}}.arrow.v{{display:block}}}}
"""
    return (f'<!doctype html><html lang="{e(spec.get("lang", "de"))}"><head><meta charset="utf-8">'
            f'<meta name="viewport" content="width=device-width,initial-scale=1"><title>{e(spec.get("title", ""))}</title>'
            f'<style>{css}</style></head><body><main><h1>{e(spec.get("title", ""))}</h1>{sub}'
            f'<section class="row">{"".join(parts)}</section>{band_html}{foot}</main></body></html>')


def render(spec, name="grafik", outdir="."):
    theme = THEMES[spec.get("theme", "dark")]
    sc = _layout(spec, theme)
    problems, words, min_px, min_cr = _lint(sc, spec)
    if problems:
        raise LayoutError("Prüfung fehlgeschlagen:\n" + "\n".join(dict.fromkeys(problems)))
    alt = spec.get("alt") or _alt(spec)
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
        fh.write(_html(spec, theme))
    warn = f" · WARNUNG: {words} Wörter > {MAX_WORDS} (für Pitch kürzen)" if words > MAX_WORDS else ""
    report = (f"PRÜFUNG OK · {len(spec['cards'])} Karten · {words} Wörter · Fließtext {sc.bullet_size}px · "
              f"kleinste Schrift {min_px}px · min. Kontrast {min_cr:.1f}:1 · 0 Kollisionen · "
              f"{png} {im.size[0]}×{im.size[1]} {os.path.getsize(png) // 1024} KB · {page} {os.path.getsize(page) // 1024} KB{warn}")
    print(report)
    print("ALT:", alt)
    return {"png": png, "html": page, "alt": alt, "report": report}
