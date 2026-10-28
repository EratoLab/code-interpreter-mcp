"""brand_colors – eine Markenfarbe → zugängliche pitchkit-Tokens (reines Python, ohne Abhängigkeiten).

Regel (vgl. Adobe Leonardo, Material Color Utilities, Radix Custom Palette):
- Flächen (band, Hub-Zentrum, Badges) behalten die Markenfarbe exakt, sobald dunkle oder weiße Schrift ≥ 4,5:1 erreicht.
- Linien/Umrisse (focus) brauchen ≥ 3:1 zu Hintergrund und Karte. Reicht die Marke nicht, bleiben Farbton und
  Buntheit (OKLCH H, C) erhalten und nur die Helligkeit wandert zum nächstgelegenen Wert, der besteht.
- Nur wenn die Farbe aus sRGB fällt, wird die Buntheit reduziert.

    from brand_colors import derive
    tokens, notes = derive("#FFD500", theme="light")
    render({**SPEC, "theme": "light", "tokens": tokens}, "name")   # pitchkit prüft Kontraste trotzdem
"""

import math

# Muss zu pitchkit.THEMES passen (bg, card).
BASES = {"dark": ("#0F172A", "#1E293B"), "light": ("#F8FAFC", "#FFFFFF")}
INKS = ("#0B1220", "#FFFFFF")


def _lin(c):
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _gam(c):
    return 12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055


def _rgb(h):
    return [int(h[i:i + 2], 16) / 255 for i in (1, 3, 5)]


def _hex(rgb):
    return "#" + "".join(f"{round(max(0.0, min(1.0, v)) * 255):02X}" for v in rgb)


def oklch(h):
    r, g, b = [_lin(v) for v in _rgb(h)]
    l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b) ** (1 / 3)
    m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b) ** (1 / 3)
    s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b) ** (1 / 3)
    L = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s
    A = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s
    B = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s
    return L, math.hypot(A, B), math.degrees(math.atan2(B, A)) % 360


def from_oklch(L, C, H):
    """OKLCH → Hex; reduziert C, bis die Farbe in sRGB liegt (vgl. CSS Color 4 Gamut Mapping)."""
    while True:
        a, b = C * math.cos(math.radians(H)), C * math.sin(math.radians(H))
        l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3
        m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3
        s = (L - 0.0894841775 * a - 1.2914855480 * b) ** 3
        lin = [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
               -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
               -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s]
        if C <= 0 or all(-1e-4 <= v <= 1 + 1e-4 for v in lin):
            return _hex([_gam(max(0.0, min(1.0, v))) for v in lin])
        C = max(0.0, C - 0.002)


def contrast(a, b):
    la, lb = sorted((sum(w * _lin(v) for w, v in zip((0.2126, 0.7152, 0.0722), _rgb(c))) for c in (a, b)), reverse=True)
    return (la + 0.05) / (lb + 0.05)


def on(fill):
    return max(INKS, key=lambda c: contrast(c, fill))


def delta_e(a, b):
    """Abstand in OKLab × 100 (≈ 2 kaum, > 15 deutlich sichtbar)."""
    pts = []
    for h in (a, b):
        L, C, H = oklch(h)
        pts.append((L, C * math.cos(math.radians(H)), C * math.sin(math.radians(H))))
    return 100 * math.dist(*pts)


def nearest(brand, ok):
    """Nächstgelegene OKLCH-Helligkeit zur Marke, deren Farbe ok() erfüllt."""
    L0, C, H = oklch(brand)
    for step in range(101):
        for L in (L0 + step / 100, L0 - step / 100):
            if 0 <= L <= 1 and ok(from_oklch(L, C, H)):
                return from_oklch(L, C, H)
    raise ValueError(f"Keine passende Variante für {brand}")


def derive(brand, theme="dark"):
    """→ (tokens, notes): focus (Umrisse, ≥ 3:1), band + band_ink (Fläche mit Text, ≥ 4,5:1)."""
    bg, card = BASES[theme]
    brand = brand.upper()
    focus = nearest(brand, lambda h: contrast(h, bg) >= 3 and contrast(h, card) >= 3 and contrast(on(h), h) >= 4.5)
    band = brand if contrast(on(brand), brand) >= 4.5 else nearest(brand, lambda h: contrast(on(h), h) >= 4.5)
    notes = []
    for role, value in (("focus", focus), ("band", band)):
        d = delta_e(brand, value)
        if d > 15:
            notes.append(f"{role}: Markenfarbe für Kontrast deutlich angepasst ({brand} → {value}, ΔE {d:.0f}) – "
                         "Marke wirkt über Flächen; bei Gelb-/Pastelltönen auf Hell ggf. neutrale Umrisse wählen")
    return {"focus": focus, "band": band, "band_ink": on(band)}, notes
