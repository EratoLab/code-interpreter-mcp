# Roadmap: Marke, Stil, Folienmaster

Stand 28.10.2026. Ziel: Kundenfarben, Formensprache und Folienmaster aufnehmen, **ohne den Prompt zu verkomplizieren**. Der Prompt beschreibt weiter nur Inhalt; alles andere sind Tokens.

## Architektur: vier Schichten, eine Richtung

| Schicht | Wer liefert | Wo | Status |
|---|---|---|---|
| 1. Inhalt (`SPEC`: Layout, Elemente, Verbindungen) | das Modell | `SKILL.md` | produktiv |
| 2. Stil (`style`: classic / modern / friendly) | Betreiber oder Nutzerwunsch | `pitchkit.STYLES` | im Kit, nicht im Prompt |
| 3. Marke (`tokens`: Farben, Radius …) | Kunde: Markenfarbe, DESIGN.md, DTCG, Folienmaster | `brand_colors.derive()` → `tokens` | im Kit, nicht im Prompt |
| 4. Prüfung (Kontrast, Kollisionen, Farbsehschwäche) | immer zuletzt | `pitchkit._lint` | produktiv – gilt auch für Kundentokens |

Reihenfolge der Auflösung: `THEMES[theme]` → `STYLES[style]` → `spec["tokens"]`. Weil die Prüfung danach läuft, kann eine Kundenvorgabe nie eine unlesbare Grafik erzeugen; sie scheitert mit Begründung.

## 1. Markenfarbe → zugängliche Tokens (`scripts/brand_colors.py`)

Regel (deckt sich mit Adobe Leonardo, Material Color Utilities, Radix Custom Palette, AntV Infographic):
- **Flächen** (Band, Hub-Zentrum, Badges) behalten die Markenfarbe exakt; die Schrift darauf wird dunkel oder weiß gewählt (≥ 4,5:1). Das klappte für jede getestete Marke.
- **Umrisse/Fokus** brauchen ≥ 3:1 zu Hintergrund und Karte. Nur die OKLCH-Helligkeit wandert zum nächsten bestehenden Wert, Farbton und Buntheit bleiben.

| Marke | Theme | focus (Umriss) | Abstand ΔE | band (Fläche) |
|---|---|---|---|---|
| `#4F46E5` Indigo | dunkel | `#5E5BFA` | 6 | exakt |
| `#FFD500` Gelb | hell | `#AA8E04` | 23 – wirkt oliv | exakt, dunkle Schrift |
| `#A7F3D0` Pastell | hell | `#559F7F` | 26 | exakt, dunkle Schrift |
| `#003366` Navy | dunkel | `#4575AD` | 23 | exakt, weiße Schrift |
| `#E30613` Rot | beide | ≈ exakt | ≤ 1 | exakt |

**Offen:** Gelb-/Pastellmarken auf Hell ergeben trübe Umrisse. Optionen: (a) neutrale Umrisse + Marke nur als Fläche/Badge, (b) Materials „Dislike“-Regel (dunkle Gelbgrüns aufhellen) – [DislikeAnalyzer](https://github.com/material-foundation/material-color-utilities/blob/main/typescript/dislike/dislike_analyzer.ts). Empfehlung: (a), weil es die Marke unverfälscht lässt.

## 2. Formensprache als Presets (`pitchkit.STYLES`)

Ein Basisradius steuert alles. Verschachtelte Flächen behalten **denselben Radius auf jeder Ebene**: Apples konzentrische Regel (außen = innen + Abstand, [WWDC25-356](https://developer.apple.com/videos/play/wwdc2025/356/)) ist für kleine Abstände gedacht – bei unseren 40 px blähte sie die äußeren Ringe auf (12 → 52 → 92 px) und wirkte wie eine andere Formensprache (verworfen 28.10.2026). Text rückt mit dem Radius ein (≥ 0,3 · r + 12), damit er nie in die Rundung ragt.

| Token | classic | **modern** (Standard) | friendly |
|---|---|---|---|
| `r0` Karte/Band | 4 | 12 | 24 |
| Pillen/Chips | r0 | min(h/2, r0) | h/2 |
| Badges | eckig (r 3) | rund | rund |
| Strich neutral / Akzent | 1,5 / 2,5 | 2 / 3 | 2,5 / 4 |
| Verschachtelung | gleicher Radius | gleicher Radius | gleicher Radius |

Vorbild: [Radix Themes radius](https://github.com/radix-ui/themes/blob/main/packages/radix-ui-themes/src/styles/tokens/radius.css) (Faktor 0/0,75/1/1,5/full), [shadcn `--radius`](https://ui.shadcn.com/docs/theming), [Material 3 shape scale](https://github.com/material-components/material-web/blob/main/tokens/versions/latest/sass/_md-sys-shape.scss).

**Noch nicht umgesetzt:** Abstandsfaktor (0,9 / 1,0 / 1,1), Serifen-Titel für classic (DejaVu Serif liegt in matplotlib), runde Linienenden für friendly. Inhalt und Stil bleiben getrennt wie bei [Penrose](https://github.com/penrose/penrose) (Substance/Style).

## 3. „Zeitlos“ als prüfbare Defaults

| Prinzip | Prüfbar als | Status |
|---|---|---|
| Wenige Größen, eine Familie (Vignelli) | feste `SCALE`-Stufen | umgesetzt |
| Keine Verläufe, Schatten, Transparenz (Rams; [NN/g zu Liquid Glass](https://www.nngroup.com/articles/liquid-glass/)) | nur opake Flächen | umgesetzt |
| ≤ 2 Strichstärken (Tufte) | `stroke`/`strong` | umgesetzt |
| Raster, wenige linke Kanten (Müller-Brockmann) | 8-px-Raster, Kanten zählen | offen |
| Fließtext linksbündig | nur einzeilige Labels zentriert | weitgehend |

Gegenbefund: verzierte Grafiken wurden besser erinnert ([Bateman, CHI 2010](https://doi.org/10.1145/1753326.1753716)); „expressive“ Designs wirken freundlicher ([Google Research](https://design.google/library/expressive-material-design-google-research)). Deshalb: **modern** als Standard, **friendly** als bewusste Wahl. Belege dafür, dass große Radien oder Verläufe „schlecht altern“, gibt es nur als Meinung (UNVERIFIZIERT).

## 4. Eingabeformate für Kunden

- **DESIGN.md** ([google-labs-code/design.md](https://github.com/google-labs-code/design.md), Apache-2.0, Spec „alpha“): YAML-Tokens (colors, typography, rounded, spacing) + Prosa „Do's and Don'ts“. Die Prosa kann dem Modell als Kontext dienen, die Tokens gehen nach `tokens`. Lint-Regeln (`missing-primary`, `contrast-ratio`) übernehmen, unsere strengeren Schwellen behalten.
- **DTCG 2025.10** ([Final CG Report](https://www.designtokens.org/tr/2025.10/), stabil, kein W3C-Standard): kleine Teilmenge per `json` lesen – Markenfarben, `radius.base`, Schrift, Preset unter `$extensions`; sRGB/Hex und OKLCH, alles andere mit Meldung ablehnen.
- **Website → Tokens** ([dembrandt](https://github.com/dembrandt/dembrandt), [designlang](https://github.com/Manavarya09/design-extract), MIT): brauchen Browser → auf dem Host laufen lassen, die Sandbox liest nur das Ergebnis.

## 5. Folienmaster-Evaluator (offline, Sandbox-tauglich)

Lesen mit `zipfile` + `xml.etree` (unkomprimierte Größe vorher begrenzen); python-pptx ist nicht im Pyodide-Paketset.

| Quelle in .pptx/.potx | Prüfung | → Token |
|---|---|---|
| `a:clrScheme` dk1/lt1/dk2/lt2 (+ `p:clrMap`) | Text ≥ 4,5:1 auf Hintergrund | `ink`, `txt`, `bg`, `card` |
| accent1–6 | ≥ 3:1 zum Hintergrund; Farbsehschwäche-Abstand paarweise; ≤ 4 trennbare | accent1 → `brand_colors.derive` → `focus`/`band`; Rest → `accents` |
| `a:fontScheme` major/minor | gebündelt oder Ersatz: Calibri → Carlito, Arial → Liberation Sans (OFL), sonst DejaVu; eingebettete Fonts nie extrahieren (fsType) | Schrift |
| `p:sldSz`, Platzhalter | Seitenverhältnis; eigener Titelplatzhalter → unseren Titel weglassen | Leinwand |

Vorbilder: [Presenton `pptx_font_utils.py`](https://github.com/presenton/presenton) (Apache-2.0, Font-Auflösung), [PPTAgent](https://github.com/icip-cas/PPTAgent) (MIT, Layout-Typisierung, PPTEval-Rubrik), [md2pptx](https://github.com/MartinPacker/md2pptx) (MIT, Theme-Farbreferenzen statt Hex).

## 6. Weitere Konzept-Layouts (Kandidaten)

Aus [AntV Infographic](https://github.com/antvis/Infographic) (MIT; nur Ideen – AntV-npm war im Mai 2026 kompromittiert) und [Mermaid](https://github.com/mermaid-js/mermaid) (MIT): Trichter, Pyramide, Zeitstrahl/Roadmap, „vs“-Vergleich, SWOT, Baum/Mindmap, Swimlane, Venn, Ishikawa. Von [D2](https://github.com/d2lang/d2) (MPL-2.0, nur Ideen): mehrere Layout-Kandidaten rendern, mit der Prüfung bewerten, den besten nehmen.

## Nächste Schritte (Vorschlag)

1. Entscheiden: Umriss-Strategie für helle Marken (Abschnitt 1), Standard-Stil bestätigen.
2. `brand` als einzelnes Spec-Feld in `SKILL.md` aufnehmen (eine Zeile), sobald 1. entschieden ist.
3. DESIGN.md/DTCG-Leser (stdlib) → `tokens`.
4. Folienmaster-Evaluator als eigenes Modul, Ergebnis = Prüfbericht + `tokens`.
5. Zwei bis drei neue Layouts (Zeitstrahl, Trichter, Vergleich) nach Bedarf.
