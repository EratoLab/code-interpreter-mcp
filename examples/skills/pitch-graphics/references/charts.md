# Datencharts (matplotlib)

Für zahlengetriebene Grafiken. Gleiche Tokens, gleiche Ausgabegröße (16:9, 3200×1800) wie pitchkit.

## Form wählen

| Aufgabe der Daten | Form |
|---|---|
| Kategorien vergleichen | Balken, **sortiert**; horizontal, wenn Labels lang sind |
| Entwicklung über Zeit | Linie (≤ 4 Serien) |
| Anteil am Ganzen, ≤ 3 Teile | gestapelter Balken; Torte nur, wenn wirklich 2–3 Teile |
| Eine Kennzahl | keine Grafik – große Zahl in einer pitchkit-Karte |

## Regeln

- **Titel = Erkenntnis** („Recherche spart am meisten Zeit“), Subtitle = Messgröße, Basis, Einheit.
- **Fokus + Kontext:** die eine Serie/Kategorie, um die es geht, in `focus`, den Rest in `context`-Grau. Mehrere gleichwertige Serien: `series` in fester Reihenfolge (validiert für Farbsehschwäche und Kontrast auf `#0F172A`); nie Farben zyklisch wiederverwenden, maximal 4 Serien.
- **Direkt beschriften** statt Legende (Wert am Balken, Serienname am Linienende); Werte nur dort, wo sie die Aussage tragen.
- Achsen zurücknehmen: keine Box, nur dezente horizontale Gitterlinien (bei direkt beschrifteten Balken gar keine); Balken starten bei 0.
- Nie: zwei y-Achsen, 3D, Regenbogen-Farbskalen, Text in Serienfarbe.
- Schrift ≥ 13 pt bei `dpi=200` (≈ 18 px logisch); Quelle + Stand unten links.

## Vorlage (getestet mit matplotlib 3.10 / Pillow 12.2)

Nur den Inhalts-Block anpassen. Ein Aufruf erzeugt PNG + SVG + HTML (SVG inline, Daten als Tabelle für Screenreader und Copy & Paste).

```python
import html
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from PIL import Image

# Tokens wie pitchkit (dark). Serienfarben: validiert (CVD + Kontrast) auf #0F172A, feste Reihenfolge.
T = dict(bg="#0F172A", ink="#F8FAFC", txt="#E2E8F0", mut="#94A3B8", grid="#334155",
         focus="#60A5FA", context="#475569", series=["#3987e5", "#d95926", "#199e70", "#c98500"])
plt.rcParams.update({
    "figure.facecolor": T["bg"], "axes.facecolor": T["bg"], "savefig.facecolor": T["bg"],
    "font.family": "DejaVu Sans", "font.size": 14, "text.color": T["txt"],
    "axes.edgecolor": T["grid"], "axes.labelcolor": T["mut"], "xtick.color": T["mut"], "ytick.color": T["mut"],
    "xtick.labelsize": 14, "ytick.labelsize": 14,
    "axes.spines.top": False, "axes.spines.right": False, "axes.spines.left": False,
    "axes.grid": True, "axes.grid.axis": "y", "grid.color": T["grid"], "grid.linewidth": 0.8,
    "svg.fonttype": "none",  # SVG-Text bleibt Text (durchsuchbar, editierbar)
})

# ---- Inhalt (nur diesen Block anpassen) ----
NAME = "zeitgewinn"
TITLE = "Recherche spart am meisten Zeit"                 # Aussage, nicht Thema
SUB = "Eingesparte Zeit je Aufgabe, Pilotgruppe (n = 48), in %"
SOURCE = "Quelle: Pilotauswertung · Stand 10/2026"
DATA = {"Recherche": 42, "Entwürfe": 31, "Zusammenfassen": 18, "Übersetzen": 9}
FOCUS = "Recherche"                                         # eine Kategorie hervorheben, Rest grau
# --------------------------------------------

labels, values = zip(*sorted(DATA.items(), key=lambda kv: kv[1]))  # größter Balken oben
fig, ax = plt.subplots(figsize=(16, 9), dpi=200)  # 3200x1800 px; 14 pt ≈ 19 px bei 16:9-Folie
bars = ax.barh(labels, values, height=0.62, color=[T["focus"] if l == FOCUS else T["context"] for l in labels])
ax.bar_label(bars, labels=[f"{v} %" for v in values], padding=10, fontsize=16, color=T["txt"])
ax.set_xlim(0, max(values) * 1.15)
ax.xaxis.set_visible(False)
ax.grid(False)
ax.tick_params(axis="y", length=0, pad=12, labelsize=17, labelcolor=T["txt"])
fig.text(0.04, 0.92, TITLE, fontsize=30, weight="bold", color=T["ink"])
fig.text(0.04, 0.865, SUB, fontsize=16, color=T["mut"])
fig.text(0.04, 0.04, SOURCE, fontsize=13, color=T["mut"])
fig.canvas.draw()  # linken Rand aus echter Label-Breite berechnen statt raten
label_w = max(t.get_window_extent().width for t in ax.get_yticklabels()) / fig.bbox.width
fig.subplots_adjust(left=0.04 + label_w + 0.015, right=0.95, top=0.80, bottom=0.12)


def lint(fig):
    """Alle sichtbaren Texte: innerhalb der Figur, keine Überlappung."""
    fig.canvas.draw()
    W, H = fig.bbox.width, fig.bbox.height
    boxes = [(t.get_text(), t.get_window_extent()) for t in fig.findobj(matplotlib.text.Text)
             if t.get_visible() and t.get_text().strip()]
    bad = [f"außerhalb: {s}" for s, b in boxes if b.x0 < 0.02 * W or b.x1 > 0.98 * W or b.y0 < 0.02 * H or b.y1 > 0.98 * H]
    bad += [f"Überlappung: {a} × {c}" for i, (a, b) in enumerate(boxes) for c, d in boxes[i + 1:] if b.overlaps(d)]
    assert not bad, "\n".join(bad)
    return len(boxes)


n = lint(fig)
fig.savefig(f"{NAME}.svg")
fig.savefig(f"{NAME}.png")
Image.open(f"{NAME}.png").convert("RGB").quantize(256, dither=Image.Dither.NONE).save(f"{NAME}.png", optimize=True)
rows = "".join(f"<tr><th scope='row'>{html.escape(k)}</th><td>{v} %</td></tr>" for k, v in DATA.items())
svg = open(f"{NAME}.svg", encoding="utf-8").read()
svg = svg[svg.index("<svg"):].replace("<svg ", '<svg role="img" aria-label="' + html.escape(TITLE) + '" style="width:100%;height:auto" ', 1)
open(f"{NAME}.html", "w", encoding="utf-8").write(
    f'<!doctype html><html lang="de"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
    f'<title>{html.escape(TITLE)}</title><body style="margin:0;background:{T["bg"]};color:{T["txt"]};font:16px system-ui,sans-serif">'
    f'<figure style="max-width:1500px;margin:0 auto;padding:16px">{svg}<details><summary>Daten als Tabelle</summary>'
    f'<table><thead><tr><th>Aufgabe</th><th>Zeitgewinn</th></tr></thead><tbody>{rows}</tbody></table></details></figure></body></html>')
print(f"PRÜFUNG OK · {n} Texte ohne Überlappung · {NAME}.png/.svg/.html")
print("ALT:", f"{TITLE}. {SUB}: " + ", ".join(f"{k} {v} %" for k, v in DATA.items()) + ".")
```

**Light-Theme:** `bg="#FFFFFF"`, `ink="#0F172A"`, `txt="#1E293B"`, `mut="#475569"`, `grid="#E2E8F0"`, `focus="#2563EB"`, `context="#94A3B8"`, `series=["#2a78d6", "#eb6834", "#1baf7a", "#eda100"]` – die beiden letzten liegen unter 3:1 auf Weiß, daher dort immer direkt beschriften.

**Linien statt Balken:** `ax.plot(x, y, lw=3, color=...)`, Serienname per `ax.annotate(name, (x[-1], y[-1]), xytext=(8, 0), textcoords="offset points", va="center")` ans Linienende, `ax.set_ylim(bottom=0)` wenn die Größe bei 0 sinnvoll beginnt.
