---
name: pitch-graphics
description: Erzeugt Pitch-, Konzept- und Playbook-Grafiken als 16:9-PNG (3200×1800) plus responsives HTML über das Tool run_python_code (code-interpreter-mcp / Pyodide). Layout „1–4 Karten in einer Reihe, optional mit Übergängen (Gates) und gemeinsamem Band“ – z. B. Zonen, Stufen, Phasen, Layer, Vorher/Nachher – sowie einfache Datencharts (Balken, Linien). Nutzen, wenn jemand eine Grafik, ein Schaubild, eine Infografik oder ein Chart für Slide, Pitch, Notion-Seite oder Playbook möchte. Nicht für Fotos/Illustrationen, UML-/Sequenzdiagramme (→ Mermaid) oder Dashboards mit Backend.
---

# Pitch- & Konzept-Grafiken (PNG + HTML)

**Prinzip: Du schreibst nur den Inhalt (`SPEC`). Layout, Text-Fitting, Rendering und Prüfung erledigt `pitchkit` deterministisch – in einem einzigen `run_python_code`-Aufruf.** Du rechnest keine Koordinaten und schreibst keinen Zeichencode.

## 1. Modus aus der Tool-Beschreibung ablesen (kein Probe-Aufruf)

Die Beschreibung von `run_python_code` listet vorinstallierte Pakete und ob `micropip.install()` verfügbar ist.

| Tool-Beschreibung | Modus | Code im Aufruf |
|---|---|---|
| „Pre-installed libraries: … pitchkit …“ | **Paket** (schnell) | `from pitchkit import render` + `SPEC` |
| sonst, und `list_code_snippets("pitchkit")` liefert ein Snippet | **Snippet** | Snippet-Code unverändert + `SPEC` |
| sonst | **Skript** | Inhalt von `scripts/pitchkit.py` unverändert + `SPEC` |

Voraussetzung in Snippet/Skript-Modus: `pillow` und `matplotlib` vorinstalliert **oder** „Supports micropip.install()“ in der Beschreibung (dann lädt Pyodide beide anhand der Import-Zeilen im Code automatisch – matplotlib liefert die DejaVu-Fonts). Trifft keins zu: abbrechen und sagen, dass der Server `pillow` + `matplotlib` braucht (siehe `references/server-setup.md`).

## 2. Inhalt festlegen (vor dem Code)

- **Titel = Aussage, nicht Thema** (≤ 60 Zeichen): „Sensible Daten verlassen nie das Haus“ statt „Datenzonen“. Subtitle ≤ 120 Zeichen.
- **1–4 Karten.** Mehr Elemente → auf zwei Grafiken aufteilen oder zusammenfassen.
- Pro Karte: Titel 1–2 Wörter · 2–3 Bullets à ≤ 6 Wörter · optional 1 Zitat ≤ 60 Zeichen.
- **Gates** (optional, genau n−1): `label` ≤ 10 Zeichen, aus dem Übergang abgeleitet („Freigabe“, „Lernen“, „Budget“) · `note` ≤ 3 kurze Wörter · `lock: True` nur bei Zugriffs-/Freigabe-Bedeutung.
- **Band** (optional): gemeinsame Querschicht (Plattform, Governance), Titel + 1 Satz.
- **Footer**: Quelle · Stand (Monat/Jahr).
- Gesamt ≤ 90 Wörter. Sprache = Sprache des Nutzers (`lang`).
- Fehlende Angaben aus dem Kontext ableiten, nicht nachfragen; Annahmen im Abschlussbericht nennen.
- Farbe trägt nie allein Bedeutung: Akzente laufen in fester Reihenfolge; die Bedeutung steht im Kartentitel.

## 3. Ein Aufruf: rendern + prüfen

```python
from pitchkit import render          # Snippet/Skript-Modus: stattdessen den Kit-Code unverändert voranstellen
SPEC = {
    "title": "Sensible Daten verlassen nie das Haus",
    "subtitle": "Drei Zonen – geteilt wird nur, was freigegeben ist",
    "theme": "dark",                 # "dark" | "light"
    "lang": "de",
    "cards": [
        {"title": "Lokal",  "bullets": ["Modelle laufen on-prem", "Für Personal & Recht"], "quote": "„Was nicht rausgeht, kann nicht leaken.“"},
        {"title": "Intern", "bullets": ["Private Cloud in der EU", "SSO & Rollen"]},
        {"title": "Extern", "bullets": ["Frontier-Modelle via API", "Nur freigegebene Inhalte"]},
    ],
    "gates": [{"label": "Freigabe", "note": "Daten klassifiziert", "lock": True},
              {"label": "Prüfung", "note": "DLP & Audit-Log", "lock": True}],
    "band": {"title": "Erato-Plattform", "text": "Ein Login, gemeinsame Richtlinien für alle Zonen"},
    "footer": "Quelle: Erato Labs · Stand 10/2026",
}
render(SPEC, "datenzonen")           # -> datenzonen.png + datenzonen.html, druckt PRÜFUNG + ALT
```

- `timeout_ms` weglassen (Default 30 s reicht; Rendering dauert ~1–2 s).
- Keine weiteren Dateien in `/workspace` (Arbeitsverzeichnis) anlegen: **jede neue Datei kommt Base64-kodiert im Tool-Ergebnis zurück.**
- Kit-Code nie ändern, um etwas passend zu machen – den Inhalt ändern.

## 4. Fehler → Inhalt anpassen, neu rendern (max. 2 Runden)

`render()` schreibt bei Problemen **keine** Dateien, sondern wirft `LayoutError` mit konkreter Anweisung, z. B.:
- „Karte 2: 37 px zu hoch (≈ 1 Zeile kürzen)“ → Bullet kürzen/streichen
- „Gate 1: Label „Datenschutzprüfung“ zu lang (≈ 9 Zeichen/Zeile)“ → „Prüfung“
- „Wort … breiter als die Karte“ → Kompositum umformulieren oder mit Bindestrich trennen
- „5 Karten: erlaubt sind 1–4“ → aufteilen

Eine `WARNUNG: … Wörter > 90` blockiert nicht, ist aber ein Signal zu kürzen. Andere Fehler: `references/sandbox-and-delivery.md`.

## 5. Ansehen, wenn du das Bild sehen kannst

Die Prüfung (Kollisionen, Containment, Kontrast ≥ 4,5:1, Schrift ≥ 18 px, Dichte) ist das Minimum, nicht das Ziel. Wenn dein Client dir das PNG zeigt, bewerte es streng gegen diese Rubrik und rendere bei einem „Nein“ einmal neu:
1. Ist die Kernaussage in 5 Sekunden klar (Titel + Kartentitel allein erzählen die Geschichte)?
2. Hierarchie eindeutig: Titel > Kartentitel > Bullets > Notizen?
3. Wirkt eine Karte deutlich voller/leerer als die anderen? → Inhalte angleichen.
4. Passt der Akzent zur Bedeutung (grün = sicher/lokal, blau = intern, gelb = extern/wachsam, violett = Premium)? Sonst `accent` (0–3) je Karte setzen.

## 6. Ausliefern

- **PNG** (16:9, 3200×1800, ~70–150 KB): für Slides und Bilder in Notion – dort auf volle Breite stellen.
- **HTML** (self-contained, responsiv, echter Text): für Notion-Embed, Web, Screenreader und Mobile.
- Im Bericht: die `PRÜFUNG`-Zeile, getroffene Annahmen und den **Alt-Text** (`ALT:`-Zeile) zum Einfügen in Folie/Notion.
- Nichts nach Notion o. Ä. schreiben ohne ausdrücklichen Wunsch → `references/sandbox-and-delivery.md`.

## Datencharts

Für zahlengetriebene Grafiken (Balken, Linien) `references/charts.md` lesen: matplotlib-Vorlage mit Aussage-Titel, Fokusfarbe, Direktbeschriftung und Text-Kollisionsprüfung; Ausgabe PNG + SVG + HTML mit Datentabelle.

## Nie

- Emoji (weder im PNG – kein Emoji-Font – noch im HTML, damit beide identisch bleiben)
- Pillow-Default-Font, `micropip.install("matplotlib")`, Fonts von PyPI/GitHub/CDNs nachladen
- Uploads (POST/PUT) aus der Sandbox – das Netz ist GET-only bzw. ganz gesperrt
- Mehr als 4 Akzentfarben + 1 Bandfarbe, Text unter 18 px, mehr als 4 Karten pro Grafik
