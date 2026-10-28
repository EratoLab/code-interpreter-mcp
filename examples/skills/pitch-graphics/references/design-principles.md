# Designprinzipien – Herkunft und Umsetzung

Recherche vom 27.10.2026. Jede Regel steht dort, wo sie am verlässlichsten wirkt: als **Prüfung** in pitchkit (blockiert), als **Hinweis** im Prüfbericht (blockiert nicht), als **Default** im Renderer oder als **Prompt-Regel** in `SKILL.md`.

## Umgesetzt

| Regel | Umsetzung | Quelle |
|---|---|---|
| Text ≥ 4,5:1 – auch große Schrift, weil sie beim Verkleinern nicht mehr „groß“ ist | Prüfung | [WCAG 1.4.3](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html) („size when the content is delivered“) |
| Linien, Pfeile, Umrisse ≥ 3:1 zum Hintergrund; dekorative Trennlinien ausgenommen | Prüfung; Tokens `line`/`edge` = `#64748B` | [WCAG 1.4.11](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html), [IBM Technical diagrams](https://www.ibm.com/design/language/infographics/technical-diagrams/design/) |
| Farbe nie allein; Farbpaare bei Rot-Grün-Schwäche unterscheidbar | Hinweis (Machado-Simulation, ΔE OKLab < 8); `legend` | [WCAG 1.4.1](https://www.w3.org/WAI/WCAG22/Understanding/use-of-color.html), [Apple HIG Accessibility](https://developer.apple.com/design/human-interface-guidelines/accessibility) |
| Farbe nur mit Bedeutung; ein Fokus-Element statt Regenbogen | Default neutral, `highlight`, `accent` + `legend`; Hinweis bei ≥ 2 Farben ohne Legende | [Mayer: Coherence/Signaling](https://www.cambridge.org/core/books/abs/cambridge-handbook-of-multimedia-learning/principles-for-reducing-extraneous-processing-in-multimedia-learning-coherence-signaling-redundancy-spatial-contiguity-and-temporal-contiguity-principles/C98AB3A6CE760DD63C048936EA0B3B44), IBM („A legend is always needed“) |
| Jede Verbindung mit Verb und einer Richtung | Hinweis bei fehlendem Label (flow, hub); Hub-Pfeile nach außen als Default | [C4 Notation](https://c4model.com/diagrams/notation), [C4 Checklist](https://c4model.com/diagrams/checklist) |
| Titel = Aussage als Satz | Prompt-Regel; Hinweis bei < 4 Wörtern oder Doppelpunkt | [Assertion-Evidence](https://writing.engr.psu.edu/research.html), [Duarte Glance Test](https://www.duarte.com/resources/guides-tools/the-glance-test/) |
| Wenige Schriftgrößen, klare Hierarchie | Feste Stufen `SCALE` statt 1-px-Schritten | [NN/g Visual Hierarchy](https://www.nngroup.com/articles/visual-hierarchy-ux-definition/) |
| Wenige Einheiten; lieber zwei Grafiken als quetschen | Elementgrenzen je Layout | [Cowan 2001](https://doi.org/10.1017/S0140525X01003922), Mayer Segmenting |
| Gruppierung über gemeinsame Fläche; verschachtelte Zonen mit abgestuften Füllungen + Umriss | `nested`, Karten | [NN/g Common Region](https://www.nngroup.com/articles/common-region/), IBM |
| Kurzer Alt-Text (≤ 2 Sätze) + sichtbare Langbeschreibung | `ALT:`; HTML-`figcaption` mit „Beschreibung als Text“ | [W3C WAI Complex Images](https://www.w3.org/WAI/tutorials/images/complex/), [GOV.UK Images](https://design-system.service.gov.uk/styles/images/) |
| Geometrie deterministisch prüfen; Bildurteil nur danach | Prüfung als Tor; Verständnisfragen in `SKILL.md` §6 | [ReDeck (2026)](https://arxiv.org/abs/2609.00194): Vision-Judge fand nur 78 % der Überlappungen; [Paper2Poster PaperQuiz](https://arxiv.org/abs/2505.21497) |
| Hierarchie über Layout und Gruppierung, nicht über Dekor; opake Flächen | Stil ohne Verläufe/Transparenz | [WWDC25 Session 356](https://developer.apple.com/videos/play/wwdc2025/356/), [HIG Layout](https://developer.apple.com/design/human-interface-guidelines/layout) |

## Flächen, Hintergründe, gestrichelte Linien

| Regel | Umsetzung | Quelle |
|---|---|---|
| Erst Abstand, dann Fläche: Gruppen über Weißraum bilden; eine Fläche/Box nur, wenn der Container selbst Information ist (Element, Zone, Gruppe) | Seitenhintergrund einfarbig; Karten nur für Elemente | [NN/g Common Region](https://www.nngroup.com/articles/common-region/) („Can I communicate this grouping by simply adding or removing whitespace?“) |
| Flächen sind das stärkste Gruppierungssignal – sparsam: höchstens eine kräftig gefüllte Fläche als Betonung (Band, Hub-Zentrum), alles andere ruhig | `band`-Farbe nur für Band/Zentrum | NN/g („strong visual cue that can overpower other grouping principles“; „borders … purely for decoration add clutter“) |
| Keine vollflächigen Farbsegmente quer über die Grafik („false floors“) | kein Abschnitts-Hintergrund | NN/g |
| Verschachtelung: je Ebene eine Stufe hellere/dunklere Fläche **plus** Umriss, nie Transparenz | `nested`: abgestufte Füllungen, Umriss ≥ 3:1 | IBM Technical diagrams (siehe oben), NN/g |
| Flächenunterschied allein reicht nicht: bedeutungstragende Grenzen brauchen einen Umriss ≥ 3:1 | Prüfung (WCAG 1.4.11) | [WCAG 1.4.11](https://www.w3.org/WAI/WCAG22/Understanding/non-text-contrast.html) |
| Gestrichelt hat **keine allgemeingültige Bedeutung** – pro Grafik genau eine Bedeutung, und die steht in der Legende | heute nur Band-Verbinder („gehört zu/läuft auf“) | [C4 Notation](https://c4model.com/diagrams/notation) (Legende soll „border styles, line types“ erklären); [OpenStack Diagram Guidelines](https://docs.openstack.org/doc-contrib-guide/diagram-guidelines/general-guidelines.html) (gestrichelt = über Netz verbundene Gruppe) |
| Sinnvolle Bedeutungen für gestrichelt: logische statt physische Grenze (Netz, Vertrauenszone, Zuständigkeit); geplant/optional; lose Zuordnung statt Fluss. Nie für den Hauptfluss, nie als Deko | Kandidat: `"status": "planned"` → gestrichelter Umriss + Legendeneintrag | Konvention, je Notation verschieden (UML, SysML, C4) |
| Gestrichelte Linien ≥ 2 px und ≥ 3:1, sonst verschwinden sie beim Verkleinern | Band-Verbinder 2 px, Farbe `line` | WCAG 1.4.11 |

## Offene Entscheidungen (bewusst nicht umgesetzt)

| Thema | Befund | Optionen |
|---|---|---|
| Schriftgröße je Ziel | 18 px auf 1600 px ≈ 10,8 pt auf Vollbild-Folie; Folien-Empfehlung für Fließtext ≥ 18 pt ([Gov. of Canada](https://a11y.canada.ca/en/tip-sheet-for-microsoft-powerpoint-presentation)). In einer Notion-Spalte (~700 px, UNVERIFIZIERT) wirken 18 px wie ~8 px. | `target: "slide"` mit Untergrenze 24 px und ~35 Wörtern; für Notion das HTML/SVG statt PNG oder eine 1200-px-Leinwand |
| Standard-Theme | Hell ist für normalsichtige Leser in hellen Räumen besser lesbar ([NN/g Dark Mode](https://www.nngroup.com/articles/dark-mode/), [W3C Accessible Presentations](https://www.w3.org/WAI/teach-advocate/accessible-presentations/)) | Kit-Default bleibt `dark` (etabliertes Design); `SKILL.md` empfiehlt `light` für Notion/Dokumente |
| Schrift | DejaVu Sans ist lesbar, aber breit | Im Wheel bündeln (alle SIL OFL 1.1): [Atkinson Hyperlegible Next](https://github.com/googlefonts/atkinson-hyperlegible-next), [IBM Plex Sans](https://github.com/IBM/plex), [Inter](https://github.com/rsms/inter). **Nicht** SF Pro (Lizenz beschränkt auf Apple-Plattform-Mockups) |
| Kontrast über AA hinaus | Apple empfiehlt für eigene Dark-Mode-Farben bis 7:1 bei kleiner Schrift ([HIG Dark Mode](https://developer.apple.com/design/human-interface-guidelines/dark-mode)); `mut` `#94A3B8` auf Karte = 5,7:1 | `mut` heller (`#CBD5E1`, 9,9:1) kostet Abstufung zu `txt` |
| Abkürzungen | C4: Akronyme erklären | Prüfung auf nie ausgeschriebene GROSSBUCHSTABEN-Wörter – bei Pitch-Grafiken (KI, IT, SSO) eher laut |
| WCAG 3 | Arbeitsentwurf 10.09.2026: Kontrast-Algorithmus „yet to be determined“, APCA nicht enthalten ([w3.org/TR/wcag-3.0](https://www.w3.org/TR/wcag-3.0/)) | WCAG-2-Verhältnisse bleiben das Tor |
