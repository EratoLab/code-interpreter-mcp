# Sandbox-Fakten, Fehlerbehandlung, Auslieferung

Gilt für `code-interpreter-mcp` (Pyodide 314, Python 3.14, Pillow 12.2, matplotlib 3.10). Quelle der Fakten: `runtime/*.js`, `src/main.rs`, `src/runtime.rs` dieses Repos.

## Fakten, die das Vorgehen bestimmen

| Fakt | Konsequenz |
|---|---|
| Jeder Aufruf startet ein frisches Pyodide; nichts bleibt zwischen Aufrufen erhalten | Rendern + Prüfen in **einem** Script; kein separater Probe- oder Verify-Aufruf |
| Ist Runtime-Installation aktiv, lädt Pyodide Pakete anhand der `import`-Zeilen im Code (`loadPackagesFromImports`) – nur für **diesen** Aufruf | Fonts kommen aus matplotlib → der Code muss `import matplotlib` enthalten (pitchkit erledigt das per `if False:`-Import, ohne matplotlib wirklich zu laden) |
| Ist Runtime-Installation deaktiviert, sind **alle** Downloads aus der Sandbox gesperrt | Nur vorinstallierte Pakete existieren; PyPI-/CDN-Font-Fallbacks können dort nie funktionieren |
| Netz (falls erlaubt) nur GET und nur Hosts aus `allowed_hosts` (Default: jsDelivr, PyPI) | Keine Uploads aus der Sandbox; GitHub ist standardmäßig nicht erreichbar |
| Jede neue Datei unter `/workspace` kommt Base64-kodiert im Tool-Ergebnis zurück (max. 64 Dateien, 10 MiB je Datei) | Nur die Auslieferungsdateien schreiben; Hilfsdateien nach `/tmp` |
| `timeout_ms` ist eine Ganzzahl (Default 30 000, max. 120 000); manche Clients serialisieren sie als String → Validierungsfehler | Parameter weglassen |
| Die Tool-Beschreibung nennt vorinstallierte Pakete und ob `micropip` verfügbar ist | Umgebung daraus ablesen statt per Probe-Aufruf |

## Fehlerbehandlung

| Meldung | Ursache | Reaktion |
|---|---|---|
| `LayoutError: …` | Inhalt passt nicht | Anweisung befolgen (kürzen/aufteilen), neu rendern – max. 2 Runden |
| `Keine DejaVu-TTF gefunden` | matplotlib weder vorinstalliert noch ladbar | Abbrechen: „Der Code-Interpreter braucht `pillow` + `matplotlib` (vorinstalliert oder Runtime-Installation)“ |
| `ModuleNotFoundError: PIL` | Pillow fehlt, Runtime-Installation aus | Abbrechen mit derselben Meldung |
| `Runtime package installation is disabled` | `micropip` aufgerufen | Nie `micropip` nutzen; nur vorhandene Pakete |
| `network host is not allowlisted` | Fetch auf nicht freigegebenen Host | Nichts nachladen; Server-Konfiguration ist Sache des Betreibers |
| `Only GET requests are supported` | POST/PUT aus der Sandbox | Erwartet; Dateien als Tool-Output ausliefern |
| `invalid type: string, expected u64` | `timeout_ms` als String gesendet | Parameter weglassen |
| `The code execution queue is full` | Server ausgelastet | Einmal kurz später erneut versuchen |
| Timeout | Script zu groß/zu langsam | Nichts zusätzlich laden; bei Charts weniger Datenpunkte |

## Auslieferung

- PNG und HTML sind die Tool-Output-Dateien; der Client zeigt sie als Anhänge. Im Text: `PRÜFUNG`-Zeile, Annahmen, Alt-Text.
- **Notion nur auf ausdrücklichen Wunsch** und nur, wenn Notion-Tools vorhanden sind:
  1. Zielseite zuerst lesen (`fetch`), dann den kleinsten gezielten Edit machen.
  2. Das PNG kann nicht aus der Sandbox hochgeladen werden → der Nutzer fügt es ein, oder der Client lädt die Anhang-Datei hoch.
  3. Das HTML kann als Text-Anhang (`create-attachment`, max. ca. 200 KB) mit `<embed source="file-upload://…" />` eingebettet werden.
- Für Notion-Seiten: das PNG auf volle Breite stellen; in der Standard-Spaltenbreite (~700 px) ist das HTML die besser lesbare Variante.
