---
name: pitch-graphics
description: Erklärt Konzepte als Grafik – Abfolgen, Architektur-Schichten, 2×2-Einordnungen, Kern-und-Bausteine, Kreisläufe, verschachtelte Zonen – und rendert sie über das Tool run_python_code (code-interpreter-mcp / Pyodide) als 16:9-PNG plus HTML mit identischem Inline-SVG. Nutzen, wenn jemand ein Konzept, einen Ablauf, eine Architektur, Zuständigkeiten, Datenzonen oder eine Positionierung für Pitch, Slide, Notion oder Playbook visualisieren möchte. Nicht für Datencharts aus Zahlen (→ Chart-Werkzeug), Fotos/Illustrationen, UML-/Sequenzdiagramme (→ Mermaid/D2) oder Dashboards.
---

# Konzept-Grafiken (PNG + HTML)

**Prinzip: Du entscheidest, *welche Beziehung* gezeigt wird, und schreibst nur den Inhalt (`SPEC`). Layout, Text-Fitting, Rendering und Prüfung erledigt `pitchkit` deterministisch – in einem einzigen `run_python_code`-Aufruf.** Keine Koordinaten, kein Zeichencode.

## 0. Lohnt sich eine Grafik?

Ja, wenn sie eine Beziehung sichtbar macht, die man sonst aus Prosa zusammensetzen müsste: Reihenfolge, Aufbau, Position, Verbindung, Kreislauf, Enthaltensein. Wenn ein Satz es schneller sagt, schreib den Satz. **Eine Grafik, eine Aussage.**

## 1. Layout nach der Beziehung wählen

| Was erklärt wird | `layout` | Elemente (`nodes`) | Zusätzlich |
|---|---|---|---|
| Abfolge, Stufen, Zonen mit Übergängen; Vorher/Nachher (2 Elemente) | `flow` | 1–4, links → rechts | `links` (n−1: `label`, `note`, `lock`), `band` |
| Aufbau: was worauf aufsetzt | `layers` | 2–5, oben → unten | `side` (Querschnitt, z. B. Governance) |
| Einordnung nach zwei Kriterien | `matrix` | genau 4: oben links, oben rechts, unten links, unten rechts | `axes`, `highlight` (0–3) |
| Ein Kern und seine Bausteine/Beteiligten | `hub` | 3–6 rund um `center` | `links` (je Element: `label`, `dir`: `in`/`out`/`both`) |
| Wiederkehrender Ablauf, Feedback-Schleife | `cycle` | 3–6, im Uhrzeigersinn ab oben | `links` (n: Pfeil i → i+1), `center` |
| Enthaltensein: Zonen, Vertrauens-/Geltungsbereiche | `nested` | 2–4, außen → innen | – |

Passt keins (Nachrichtenfluss zwischen Systemen, Zustandsautomat, Datenmodell) → Mermaid/D2, nicht dieser Skill.

## 2. Modus aus der Tool-Beschreibung ablesen (kein Probe-Aufruf)

| Tool-Beschreibung von `run_python_code` | Modus | Code im Aufruf |
|---|---|---|
| „Pre-installed libraries: … pitchkit …“ | **Paket** (schnell) | `from pitchkit import render` + `SPEC` |
| sonst, und `list_code_snippets("pitchkit")` liefert ein Snippet | **Snippet** | Snippet-Code unverändert + `SPEC` |
| sonst | **Skript** | Inhalt von `scripts/pitchkit.py` unverändert + `SPEC` |

Snippet/Skript-Modus brauchen `pillow` + `matplotlib` vorinstalliert **oder** „Supports micropip.install()“ in der Beschreibung (dann lädt Pyodide beide anhand der Imports; matplotlib liefert nur die Fonts). Trifft keins zu: abbrechen und auf `references/server-setup.md` verweisen.

## 3. Inhalt schreiben

- **Titel = Aussage, nicht Thema** (≤ 60 Zeichen): „Je sensibler die Daten, desto enger der Kreis“ statt „Datenzonen“. Subtitle optional (≤ 120 Zeichen).
- **Element:** `title` 1–3 Wörter · `text` ein kurzer Satz (≤ 8 Wörter) · `items` als Chips (1–2 Wörter: Systeme, Beispiele) · `points` (≤ 3 kurze Punkte) nur wo eine Karte wirklich Liste ist.
- **Verbindungen mit Verb beschriften** („liest“, „gibt frei“, „meldet“). Ein unbeschrifteter Pfeil sagt nur „irgendwie verbunden“.
- Ziel ≤ 60 Wörter (Warnung ab 80). Erklärungen gehören in deinen Antworttext und den Alt-Text, nicht ins Bild.
- **Farbe nur mit Bedeutung:** Akzente laufen in fester Reihenfolge (0 grün = sicher/lokal, 1 blau = intern, 2 gelb = extern/wachsam, 3 violett = Premium). Bedeutung explizit per `accent` setzen; ab 5 Elementen ohne `accent` wird neutral gezeichnet. Die Bedeutung steht immer auch im Text.
- `footer`: Quelle · Stand (Monat/Jahr). `theme`: `dark` | `light`. `lang`: Sprache des Nutzers.
- Fehlende Angaben aus dem Kontext ableiten statt nachfragen; Annahmen im Bericht nennen.

## 4. Ein Aufruf: rendern + prüfen

```python
from pitchkit import render          # Snippet/Skript-Modus: stattdessen den Kit-Code unverändert voranstellen
SPEC = {
    "layout": "hub",
    "title": "Die Plattform verbindet, statt zu ersetzen",
    "subtitle": "Bestehende Systeme bleiben – der Assistent greift kontrolliert zu",
    "center": {"title": "Erato", "text": "Assistent mit Rollen & Richtlinien"},
    "nodes": [
        {"title": "Dokumente", "text": "SharePoint, DMS"},
        {"title": "Tickets", "text": "Jira, ServiceNow"},
        {"title": "Wissen", "text": "Confluence, Wikis"},
        {"title": "Modelle", "text": "Lokal, EU, Frontier"},
    ],
    "links": [{"label": "liest", "dir": "in"}, {"label": "erstellt", "dir": "out"},
              {"label": "durchsucht", "dir": "in"}, {"label": "wählt", "dir": "out"}],
    "footer": "Quelle: Erato Labs · Stand 10/2026",
}
render(SPEC, "plattform")            # -> plattform.png + plattform.html, druckt PRÜFUNG + ALT
```

Weitere Spec-Formen: `matrix` braucht `"axes": {"x": {"title": "Aufwand", "low": "gering", "high": "hoch"}, "y": {…}}`; `flow` nimmt `"links": [{"label": "Freigabe", "note": "Daten klassifiziert", "lock": True}]` und `"band": {"title": …, "text": …}`; `layers` nimmt `"side": {"title": "Governance", "text": …}`; `cycle` und `hub` nehmen `"center": {"title": …, "text": …}`.

- `timeout_ms` weglassen (Rendering ~1 s).
- Keine weiteren Dateien in `/workspace` anlegen – **jede neue Datei kommt Base64-kodiert im Tool-Ergebnis zurück.**
- Kit-Code nie ändern, um etwas passend zu machen – den Inhalt ändern.

## 5. Fehler → Inhalt anpassen, neu rendern (max. 2 Runden)

`render()` schreibt bei Problemen **keine** Dateien, sondern wirft `LayoutError` mit konkreter Anweisung, z. B. „Element 2 („Intern“): 37 px zu hoch (≈ 1 Zeile kürzen)“, „Übergang 1: Label „Datenschutzprüfung“ zu lang“, „hub: 7 Elemente – erlaubt sind 3–6“. Kürzen, zusammenfassen oder auf zwei Grafiken aufteilen. Andere Fehler: `references/sandbox-and-delivery.md`.

## 6. Ansehen, wenn du das Bild sehen kannst

Die Prüfung (Kollisionen, Containment, Kontrast ≥ 4,5:1, Schrift ≥ 18 px, Dichte) ist das Minimum. Wenn dein Client dir das PNG zeigt, prüfe streng und rendere bei einem „Nein“ einmal neu:
1. Versteht jemand ohne Vorwissen die **Beziehung** (wer liest von wem, was liegt worin, was folgt worauf) allein aus Titel + Bild?
2. Ist jede Verbindung beschriftet und zeigt in die richtige Richtung?
3. Trägt jede Farbe eine Bedeutung – oder ist sie Dekoration?
4. Ist ein Element deutlich voller als die anderen? → angleichen oder Text in den Antworttext verschieben.

## 7. Ausliefern

- **PNG** (16:9, 3200×1800, ~65–100 KB): für Slides und Notion (dort auf volle Breite).
- **HTML**: dasselbe Bild als Vektor (Inline-SVG, skaliert verlustfrei) plus Textfassung für Screenreader und schmale Bildschirme.
- Im Antworttext: die `PRÜFUNG`-Zeile, Annahmen und den **Alt-Text** (`ALT:`-Zeile).
- Nichts nach Notion o. Ä. schreiben ohne ausdrücklichen Wunsch → `references/sandbox-and-delivery.md`.

## Nie

- Datencharts mit diesem Skill (Zahlenreihen gehören in ein Chart-Werkzeug)
- Emoji, Pillow-Default-Font, `micropip.install("matplotlib")`, Fonts von PyPI/GitHub/CDNs nachladen
- Uploads (POST/PUT) aus der Sandbox
- Mehr als 4 Akzentfarben, Text unter 18 px, mehr Elemente als das Layout erlaubt
