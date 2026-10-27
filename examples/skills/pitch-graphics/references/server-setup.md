# Server-Setup für schnelle Grafiken (Betreiber)

Der Skill funktioniert ohne Server-Anpassung (Skript-Modus), aber dann muss das Modell bei **jedem** Aufruf ~44 KB Kit-Code (≈ 12 000 Output-Tokens) mitsenden. Mit vorinstalliertem Paket sendet es nur die Spec (≈ 1 KB).

| Modus | Code pro Aufruf | Netz zur Laufzeit nötig | Einrichtung |
|---|---|---|---|
| Paket | ~1 KB (nur `SPEC`) | nein | Wheel bauen + hosten, `preinstall_packages` |
| Snippet | ~45 KB | nein, wenn pillow/matplotlib vorinstalliert | `code_snippets` in der TOML |
| Skript | ~45 KB | ja, wenn pillow/matplotlib nicht vorinstalliert | keine |

## Paket-Modus (empfohlen)

```bash
pip wheel --no-deps -w dist examples/skills/pitch-graphics/scripts
# -> dist/pitchkit-2.0.0-py3-none-any.whl (~15 KB); auf einem Host aus allowed_hosts ablegen
```

```toml
preinstall_packages = ["pillow", "matplotlib", "https://<host-aus-allowed_hosts>/pitchkit-2.0.0-py3-none-any.whl"]
runtime_package_installation = false   # optional – pitchkit braucht danach kein Netz
```

Die Tool-Beschreibung listet dann „Pre-installed libraries: pillow, matplotlib, pitchkit“ – daran erkennt der Skill den Paket-Modus. matplotlib wird nur als Font-Quelle (DejaVu) gebraucht und von pitchkit nicht importiert.

## Snippet-Modus (ohne Wheel-Hosting)

```toml
preinstall_packages = ["pillow", "matplotlib"]

[code_snippets.snippets.pitchkit]
title = "pitchkit – Konzept-Grafik (PNG + HTML/SVG) aus SPEC"
description = "Layouts flow, layers, matrix, hub, cycle, nested; prüft Kollisionen, Kontrast, Schriftgröße. Danach SPEC definieren und render(SPEC, name) aufrufen."
required_python_packages = ["pillow", "matplotlib"]
code_snippet = '''
# Inhalt von scripts/pitchkit.py hier einfügen
'''
```

## Geprüft (Pyodide 314.0.2, wie `package.json`)

- Skript-Modus: ein Aufruf, Paketladen ~0,45 s (aus Cache), Rendern ~0,95 s.
- Paket-Modus: Wheel per micropip installierbar; Spec-only-Aufruf (1 KB Code) rendert in ~0,9 s; v1-Specs (`cards`/`gates`/`bullets`) laufen weiter.
- Ohne `import matplotlib` im Code findet ein frisches Sandbox-Pyodide keine DejaVu-Fonts (matplotlib wird nur pro Aufruf anhand der Imports geladen) – pitchkit enthält dafür einen nicht ausgeführten Import für den Scanner.
