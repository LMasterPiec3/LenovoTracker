# Lenovo Clearance-Sale Mainboard-Tracker (Rust)

Überwacht die öffentliche [Lenovo Clearance-Sale-Seite](https://support.lenovo.com/de/de/parts-lookup/clearance-sale)
und mailt, sobald der Mainboard-Bestand (Commodity „system boards") **steigt**.
API-Reverse-Engineering (Endpunkte, Bot-Schutz, Part-Schema): **[Lenovo-API.md](Lenovo-API.md)**.

## Architektur

```
Proxmox-Host
└── LXC „lenovo-tracker-rust" (Alpine, 16 MB RAM / 32 MB Disk – belegt ~10,7 MB, Image ~12 MB)
    ├── /opt/lenovo-tracker/lenovo-tracker        # Rust-Binary (~1,9 MB, statisch; `--serve` = Web-UI)
    ├── /opt/lenovo-tracker/curl-impersonate      # Chrome-TLS-Imitation (Akamai, gestrippt ~5 MB)
    ├── /etc/lenovotracker/config.toml            # Konfiguration
    ├── /var/lib/lenovotracker/state.json + history.csv
    ├── /opt/lenovo-tracker/self-update.sh        # wöchentlich: Release prüfen
    ├── /opt/lenovo-tracker/self-backup.sh        # täglich: State/History (7 Tage)
    ├── /var/backups/lenovo-tracker/              # Backups im CT
    └── crond (*/10; self-update Mo 06:17, self-backup 03:40; via ::respawn, kein syslogd)
```

Der CT läuft mit 16 MB Limit (real ~10 MB inkl. Cache, ~0,2 MB App-Speicher).
Die 32-MB-Disk ist mit ~10,7 MB belegt (Raw-Image ~12 MB); `apk` ist nicht
installiert – Selbst-Update und Backup laufen im CT (kein Host-Timer). Die PVE-Konsole bleibt über
`tty: 1` nutzbar; syslogd läuft nicht.

Ablauf je Lauf: API abrufen (Body kommt aus der stdout-Pipe, wird bis 8 MB im
RAM gepuffert und als Slice geparst – rund 3× schneller als der
Streaming-Parser; größere Bodies streamen weiter. Kein Temp-File, kein
Disk-I/O; der `commodity_keys`-Filter greift bereits beim Parsen und
überspringt nicht-mathende Teile. Keywords prüfen danach Name, Teilenummer
**und** technische Daten; die Specs kommen aus `lbom/parts/get` und liegen
als Cache neben dem State, sodass nur neue Teile nachgeladen werden) →
Summe `stock` → Diff gegen `state.json` → **nur bei Anstieg**: Tech-Daten
nachladen + Mail senden → State/CSV schreiben.

## Setup (Proxmox-Host)

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
```

Interaktives Menü mit Defaults: Bridge/Gateway/IP/SMTP werden automatisch
vom Host erkannt, Commodity-Keys per Checkliste gewählt. Pflicht sind nur
die Mail-Adressen. Oder per Env:

```bash
VMID=110 MAIL_FROM="du@example.de" MAIL_TO="du@example.de" \
SMTP_HOST="mail.example.de" COMMODITY_KEYS="system boards" KEYWORDS="RTX" \
  bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
```

Binary kommt aus den GitHub Releases; alternativ `SKIP_BUILD=1`
(vorhandenes `/root/lenovo-tracker-rs`) oder `BUILD_METHOD=build
BUILD_VMID=<id>` (temporärer Build-CT, statisches musl-Binary).

| Variable | Default | Bedeutung |
|---|---|---|
| `VMID` | erste freie ab 100 | Produktions-CT |
| `CT_HOSTNAME` | `LenovoTracker` | Hostname |
| `RAM` / `SWAP` | `16` / `32` | MB |
| `CPULIMIT` / `DISK` | `0.25` / `local:0.03125` | Viertel-Kern / Rootfs (32 MB) |
| `BRIDGE` / `GATEWAY` / `BASE_IP` | automatisch erkannt | Netz (CT-IP = `<BASE_IP>.<VMID>`) |
| `MAIL_FROM` / `MAIL_TO` | – (Pflicht) | Absender/Empfänger (komma-getrennt) |
| `SMTP_HOST` / `SMTP_PORT` | automatisch ermittelt | Mailserver |
| `SMTP_USER` / `SMTP_PASS` | leer = Plain-SMTP | STARTTLS-Login (siehe unten) |
| `COMMODITY_KEYS` / `KEYWORDS` | `system boards` / leer | Filter (Keywords: Name, Teilenummer, Tech-Daten) |
| `EXTRA_RECIPIENTS` | leer | weitere Empfänger |
| `FORCE=1` | aus | existierenden CT löschen + neu aufbauen |
| `SKIP_BUILD=1` | aus | vorhandenes Binary nutzen |
| `AUTO_UPDATE=1` | aus | Selbst-Update (Mo 06:17) + tägliches State-Backup im CT (7 Tage, kein Host-Timer); größere Releases werden zugunsten des optimierten Binaries verworfen |

Weitere Kommandos:

```bash
bash lenovo-tracker-setup.sh --edit-config 101 # Config bearbeiten
bash lenovo-tracker-setup.sh --smtp-auth 101   # SMTP-Login + Test-Mail
bash lenovo-tracker-setup.sh --status 101      # Status, Cron, Log, State
bash lenovo-tracker-setup.sh --test-mail 101   # Test-Mail senden
bash lenovo-tracker-setup.sh --probe 101       # API-Parameter-Matrix testen
bash lenovo-tracker-setup.sh --peak 101        # Peak-RAM messen
bash lenovo-tracker-setup.sh --uninstall       # CT + Host-Artefakte entfernen
```

## Conditional Requests prüfen (ETag)

`conditional-check.sh` prüft, ob die API auf `If-None-Match`/`If-Modified-Since`
mit `304 Not Modified` antwortet – dann könnte der Tracker den 3,3-MB-Download
bei unverändertem Bestand sparen. Der Check läuft im CT mit dem dortigen
curl-impersonate:

```bash
pct push 105 conditional-check.sh /tmp/conditional-check.sh
pct exec 105 -- sh /tmp/conditional-check.sh             # einmalig
pct exec 105 -- sh /tmp/conditional-check.sh --delay 600 # Stabilität über 10 min
```

Optionen: `--url`, `--impersonate`, `--delay`; `CURL=/pfad` überschreibt die
Suche. Exit-Code: 0 = 304 unterstützt, 1 = nicht unterstützt, 2 = Fehler
(z. B. HTTP 403 durch Akamai).

## Konfiguration

Vollständiges Beispiel: [config.example.toml](config.example.toml).

```toml
[tracker]
commodity_keys = ["system boards"]
name_keywords = ["RTX"]   # leer = alle Mainboards
```

Filter: ODER-verknüpft, case-insensitiv, auf Bezeichnung, Teilenummer **und**
technische Daten (CPU/GPU/RAM/DPK/…). Die Specs holt der Tracker beim ersten
Lauf mit Keywords einmalig per Batch-POST (`lbom/parts/get`, 200 IDs pro
Request – ~16 Requests bei ~3.100 Boards) und cacht sie in `specs-cache.json`
neben dem State; danach
werden nur neue Teile nachgeladen. Ein Filterwechsel baselined still neu
(keine Garbage-Mail). Bei jedem Alarm steht **jedes** betroffene Teil in der
Mail – mit den Kosten je Teil (Stückpreis × Bestand, Währung aus dem Locale der
`api.url`; nur der aktuelle Preis, kein Streichpreis) und aufbereiteten
Tech-Daten (CPU/GPU/RAM beautifiziert). Sortiert wird nach grober
Leistungsfähigkeit (`api::performance_key`: GPU-Klasse, dann CPU-Benchmark,
dann RAM) – die stärksten Teile stehen oben. Gaming-Karten (RTX/GTX)
bekommen auf die Modellnummer eine Generation Bonus, damit eine RTX 3070 über
einer Quadro RTX 4000 liegt; Einstiegskarten (MX, GeForce 9xx, Radeon 5xx) und
Workstation-Karten (Quadro, RTX-A-Serie in jeder Schreibweise – `RTX A2000`,
`RTXA2000`, `RTX-A2000` –, Radeon Pro/Instinct) bleiben ohne Bonus, deutlich
höhere Nummern stehen weiter vorn. MX-Serien- und Quadro-T-Karten (T500–T2000,
auch „NVIDIA T500") fliegen danach ganz aus der Mail (`api::is_low_end_gpu` +
`diff::drop_low_end_parts`); der State bleibt vollständig, und wenn nur solche
Teile übrig wären, wird keine leere Mail verschickt. Schlägt der Specs-Abruf fehl,
geht die Mail trotzdem raus (Kosten-Spalte zeigt dann `–`, die Reihenfolge
fällt auf Δ/Bestand zurück).

## SMTP (optional)

`bash lenovo-tracker-setup.sh --smtp-auth <VMID>` richtet Login + Test-Mail
ein. Mit Login signiert der Provider DKIM/SPF (empfohlen); ohne Login läuft
es Plain-SMTP an den MX — die Host-IP muss dann im SPF der Absender-Domain
stehen.

| Provider | Host | Passwort |
|---|---|---|
| Proton | `smtp.protonmail.ch:587` | [SMTP-Token](https://proton.me/support/smtp-submission) |
| Gmail | `smtp.gmail.com:587` | App-Passwort ([Google-Konto](https://myaccount.google.com/apppasswords)) |
| Andere | Submission-Host des Providers | normaler SMTP-Login/Token |

## Bedienung

```bash
pct exec 105 -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --dry-run # Zusammenfassung, kein State/Mail
pct exec 105 -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once   # ein Zyklus
pct exec 105 -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --serve  # Web-UI (siehe unten)
pct exec 105 -- tail -f /var/log/lenovo-tracker.log                                                 # Live-Log
```

Mail-Trigger: Gesamtbestand steigt **oder** ein Keyword hat mehr Bestand/
Teile als beim letzten Lauf. Abstieg und unveränderte Läufe senden nichts.
Die Mail listet alle Anstiege und alle neuen Teile (kein 20er-Limit, kein
"+ N weitere Teile").

## Web-UI (Bestand live, Verlauf, Config)

`--serve` startet eine kleine Oberfläche im selben Binary (nur std, kein
zusätzlicher Dienst, kein zusätzlicher Port nach außen im Standard):

- **Bestand** – Karten mit Teilbild, Bezeichnung, Tech-Chips und Preisen
  (Stückpreis + Summe). Bilder kommen direkt von `download.lenovo.com`
  (`/Images/Parts/<ID>/<ID>_A.jpg`, aus der Teilenummer ableitbar); fehlt eins,
  erscheint ein Platzhalter.
- **CPU-Leistung** – Grundlage sind gemessene Benchmark-Werte aus
  `src/cpu-scores.tsv` (`Serie:Modell`, Name, Single, Multi, Quelle), die per
  `include_str!` im Binary liegen: **1.965 CPU-Modelle** (Desktop und Laptop,
  alle Generationen), nicht nur die Teile aus dem Lenovo-Sortiment. Der Rang
  ist `Single × 10 + Multi`, damit sowohl schnelle Kerne als auch viele Kerne
  zählen (i7-12800H 33.842 vor i7-12700H 33.738 vor i9-11950H 28.576 vor
  i9-9880H 20.033). Der Tooltip am CPU-Chip nennt Werte und Herkunft.

  Quellen je Zeile (Spalte 5):
  - `r23` – Cinebench R23, direkt gemessen (523 der 612 Teile mit CPU).
  - `cb24`, `r20`, `r15` – Cinebench 2024/R20/R15, per log-linearem Fit auf die
    R23-Skala umgerechnet (R² Multi 0,96 / 0,98 / 0,82 auf den gemeinsamen
    Modellen).
  - `gb6` – Geekbench 6 (offener Datensatz, MIT), ebenfalls auf R23-Skala
    umgerechnet (R² 0,88).

  Cinebench-Daten stammen aus `sp00n/cinebench_scores` (GitHub, speist sich aus
  cpu-monkey, cgdirector, nanoreview, anandtech); das sind Community-Messungen,
  keine offizielle Zertifizierung. PassMark/Geekbench-Datenbanken sind
  kostenpflichtig bzw. untersagen das Kopieren, deshalb sind sie nicht
  eingebettet.

  Nachgetragen wurden 14 Modelle, für die es keinen R23-Messwert gibt. R15/R20
  wurden **per log-linearem Fit** auf die R23-Skala umgerechnet, wie der Rest
  der Tabelle: aus sieben Modellen, die in `cpu-scores.tsv` eine R23-Zeile haben
  und bei cpu-monkey zusätzlich R15/R20 (5700U-Klasse, 6200U, 7500U, 8250U,
  8265U, 8145U, 8565U, 9880H, 3500U), ergibt sich
  `R23 = 5,90·x^0,997` (R15 Single, R² 0,72), `7,12·x^0,952` (R15 Multi, R² 0,95),
  `3,78·x^0,928` (R20 Single, R² 0,80) und `2,75·x^0,970` (R20 Multi, R² 0,95).
  Die Koeffizienten stehen auch im Kopf von `cpu-scores.tsv`.
  Betroffen (Spalte 5 nennt den gemessenen Lauf): i5-6300U (R23 777/1.834), i5-7300U, i5-7Y54, i5-7Y57,
  i7-6600U, i7-7600U, Core m3-8100Y, Core m7-6Y75, Pentium Gold 5405U
  (R23 539/1.390), A6-9220, A12-9700P sowie Ryzen 3 3250C (R23 785/1.989) und
  Ryzen 5 3500C (Werte der baugleichen 3500U). Der Xeon E3-1505M v6 hatte
  bereits eine `gb6`-Zeile und nutzt die weiter.

  Ohne Messwert bleiben nur noch 5 Modelle im Bestand: A4-9120C, A6-9220C
  (keine Cinebench-Ergebnisse bei Notebookcheck/cpu-monkey), Pentium Gold
  6405U (dort ebenfalls ohne Cinebench) und Snapdragon 7c/8cx Gen 3 (ARM, nur
  Geekbench – nicht auf die R23-Skala übertragbar). Dort schätzt
  `src/cpu-fallback.tsv` aus Serie und Generation (Median, bei unbekannter
  Generation der Mindestwert der Serie) und der Tooltip sagt „geschätzt".
  Neue Modelle einfach in die TSV eintragen
  (`serie:modell<TAB>Name<TAB>Single<TAB>Multi<TAB>quelle`, Serie `i7`/`r5`/leer).
  Lenovo schreibt Modellnummern teils ohne Trennzeichen („INTELM76Y75",
  „AMD A69220C", „Intel E31505M"). Dafür gibt es **keine zusätzlichen Zeilen**:
  `cpu_model_key` nimmt als letzte Stufe das letzte Token mit Ziffern („7Y54"),
  und `cpu_bench` sucht danach den Tabellenschlüssel, dessen Modellnummer am
  Tokenende steht (Suffix-Treffer, mindestens vier Zeichen, nur wenn eindeutig –
  sonst wird geschätzt). Jeder CPU-Name steht dadurch genau einmal in der
  Tabelle; der Test `cpu_tabelle_ohne_doppelte_namen` hält das fest.
- **GPU-Leistung** – `src/gpu-scores.tsv` enthält **2.262 Grafikeinheiten**
  (NVIDIA, AMD, Intel, inklusive iGPUs wie Iris Xe, UHD, Vega, Radeon 680M)
  mit 3DMark-Time-Spy-Graphics-Punkten, ebenfalls per `include_str!` im Binary.
  Spalte 4 nennt die Quelle: `ts` = direkt gemessen (162 Modelle, u. a. alle
  RTX-Generationen, Quadro T/RTX, MX, GTX, iGPUs), `pm3d` = aus dem
  PassMark-G3Dmark über einen Fit auf die Time-Spy-Skala umgerechnet
  (R² 0,87, 72 gemeinsame Modelle). Der Tooltip am GPU-Chip zeigt „3DMark Time
  Spy: 8.222 Punkte". Sortiert wird nach diesem Wert („GPU-Leistung ↓" und
  „Leistung ↓"), nicht mehr nach der alten Modellnummern-Heuristik; die greift
  nur noch bei Karten, die in keiner Tabelle stehen (3 Teile mit der
  Sammelangabe „diskret").

  **iGPUs bei „integriert"/„UMA"**: Lenovo nennt dort kein Grafikmodell. Dafür
  steht `src/cpu-igpu.tsv` – **eine Zeile pro CPU-Modell** mit dem exakten
  Grafiknamen laut Intel ARK/AMD-Produktdaten, dem Time-Spy-Wert und dessen
  Quelle (`ts` gemessen, `pm3d` umgerechnet, `est` geschätzt):

  | CPU-Modell | iGPU | Time Spy |
  |---|---|---|
  | i7-1165G7 / i7-1185G7 / i7-11320H / i7-11370H | Iris Xe G7 **96 EU** | 1.560 |
  | i5-1135G7 / i5-1145G7 / i5-11300H | Iris Xe G7 **80 EU** | 1.181 |
  | i7-11800H / i9-11950H | UHD Graphics Xe **32 EU** (Tiger Lake-H) | 658 |
  | i3-1115G4 | UHD Graphics Xe G4 **48 EU** | 801 |
  | i3-1005G1 / i5-1035G1 | UHD Graphics **G1 (32 EU)** | 450 (est) |
  | i5-1035G4 | Iris Plus Graphics **G4 (48 EU)** | 800 (est) |
  | i7-1065G7 | Iris Plus Graphics **G7 (64 EU)** | 982 |
  | i3-1215U / i3-1220P | UHD Graphics **64 EU** (12. Gen) | 1.110 |
  | i5-1235U / i5-1240P / i5-1335U | Iris Xe **80 EU** | 1.181 |
  | i7-1255U / i7-1260P / i7-12700H / i7-12800H | Iris Xe **96 EU** | 1.560 |
  | i5-12450H | UHD Graphics **48 EU** (12. Gen) | 832 (est) |
  | i7-6560U | Iris **540** | 679 |
  | Skylake-U · Kaby-U · Kaby-H · Kaby-Y (7Y5x) | HD 520 · HD 620 · HD 630 · HD 615 | 476 · 506 · 608 · 402 |
  | 8.–10. Gen U · H | UHD 620/610 · UHD 630 | 566/194 · 698 |
  | Xeon E-2xxxM / W-10855M | UHD **P630** | 1.328 |
  | Ryzen 2000/3000 | Vega 10 · Vega 8 · Vega 6 · Vega 3 | 737 · 851 · 699 · 561 |
  | Ryzen 4000/5000, 7530U | Vega 8 · **Vega 7** · Vega 6 · Vega 5 | 1.163 · 1.017 · 852 · 733 |
  | Ryzen 6000 / 7x35 | Radeon 680M · 660M | 2.298 · 1.535 |
  | Ryzen 7x20/7x30 (Mendocino/Barcelo) | Radeon **610M** · Vega 6 | 515 · 852 |
  | Ryzen 7x40/8040 | Radeon 780M · 760M · 740M | 2.808 · 2.281 · 1.527 |
  | Celeron/Pentium/Atom | UHD 600/605/610 · HD 500/610 · Xe 16 EU · Jasper-Lake-UHD | 109–375 |

  Der Schlüssel der Tabelle ist der CPU-Name klein und nur aus Buchstaben und
  Ziffern – „AMD ATHLON3050U“, „AMD Athlon3050U“ und „AMD Athlon 3050U“
  treffen also denselben Eintrag. 416 der 427 UMA-Teile bekommen so einen
  Wert; im Tooltip steht **„iGPU laut CPU-Modell (abgeleitet, nicht am Board
  gemessen): … · 3DMark Time Spy der iGPU: N Punkte“**. Am Chip steht statt
  „integriert“ der echte Chip in Kurzform („Iris Xe G7 96EU“, „UHD 620“,
  „Radeon 610M“); Kategorie und Filter bleiben „integriert“, das Chip-Styling
  ist unverändert das der iGPU. Nur bei `est`-Zeilen hängt ein „≈“ am Chip und
  der Tooltip sagt „aus dem EU-/CU-Ausbau geschätzt".

  Trägt die iGPU denselben Namen in mehreren CPU-Generationen, steht die
  Generation im Kurznamen: „**Vega 8 (2000/3000)**“ (851) vs. „**Vega 8
  (4000/5000)**“ (1.163), ebenso „Vega 6 (2000/3000)“ 699 / „Vega 6
  (4000/5000)“ 852, „Vega 7 (4000/5000)“ 1.017, „Vega 5 (4000/5000)“ 733,
  „Vega 10 (2000/3000)“ 737 und „Vega 3 (2000/3000)“ 561. Die Generation wird
  aus dem Klammerzusatz des Tabellennamens abgeleitet („(Ryzen 4000/5000)“),
  nicht extra gepflegt.
  Unbekannte CPUs fallen auf `igpu_key` zurück (Reihe + Generation). Ohne Wert
  bleiben 11 Teile: 8 ohne CPU-Angabe sowie Snapdragon SC7180/SC8280XP.
- **Filter** – Suche, CPU-Reihe (`Core i5`, `Ryzen 7`, `Celeron`, …), RAM-Größe,
  GPU-Familie (`RTX 40`, `GTX 16`, `Quadro`, `MX`, `Radeon`, `integriert`, …)
  und GPU-Typ (dediziert/integriert), dazu Sortierung nach Bestand, Leistung,
  Preis oder Name sowie Karten-/Tabellenansicht. Die Auswahl bleibt im Browser
  gespeichert.
- **Laden** – fragt die Lenovo-API direkt ab, **ohne Mail** und ohne den State
  zu verändern; zeigt je Teil das Δ zum gespeicherten Stand.
- **Live-Stand** – der Server hält den Abruf im Speicher und erneuert ihn jede
  Minute, solange eine Seite ihn abruft (die Seite lädt dadurch in
  Millisekunden). 5 Minuten ohne Aufruf pausieren ihn – kein Lenovo-Traffic –,
  der nächste Aufruf holt frisch und startet ihn wieder.
- **Alle Teile statt nur Mail-Liste** – die Oberfläche führt einen eigenen
  Bestand (`data/tracked.json` + `data/tracked-history.csv`): jeder Lauf legt
  dort **alle** Teile des Abrufs ab, mit Technik-Daten und Preisen, unabhängig
  von `name_keywords`. Der Umschalter „Alle / Mail“ zeigt zusätzlich, welche
  Teile die Mail-Kriterien erfüllen (Karten tragen dann ein `MAIL`-Tag).
  Mails, Diff und die Rückgangs-Sperre bleiben unverändert auf der gefilterten
  Liste – die Web-UI ist davon getrennt.
- **Verlauf** – Sparkline + letzte Zeilen aus `history.csv`.
- **Einstellungen** – nur nach Admin-Login (`[webui] admin_user`/`admin_pass`,
  HTTP-Basic): ein Formular mit allen Werten (API, Filter, Mail, Daten,
  Web-Interface), gruppiert mit Kurzhinweisen. Gespeichert wird Feld für Feld in
  `config.toml`; Kommentare und unbekannte Schlüssel bleiben stehen, vorher wird
  geprüft, die alte Datei bleibt als `config.toml.bak`. SMTP- und Admin-Passwort
  erscheinen nie (leeres Feld = unverändert). Darunter liegt „Rohdatei (TOML)
  bearbeiten“ für Sonderfälle.

- **Anmeldung** – sobald `[webui] admin_pass` gesetzt ist oder Benutzer in
  `users.toml` (neben der Config) stehen, verlangt jede Daten-API eine Sitzung:
  Formular-Login mit Benutzername/Passwort, Cookie `lt_sid` (HttpOnly,
  SameSite=Lax, 12 Stunden). Passwörter liegen als PBKDF2-HMAC-SHA256
  (120 000 Runden, eigenes Salt) in `users.toml` (Modus 600). Rollen: `user`
  sieht Bestand und Verlauf, `admin` zusätzlich Einstellungen und den
  Benutzer-Tab (anlegen, Passwort/Rolle ändern, löschen). Der letzte Admin
  lässt sich weder löschen noch degradieren. Solange kein Benutzer angelegt
  ist, gilt der Config-Admin als Startzugang.

Die Stückpreise legt der Tracker im State ab (Anzeige only – ein Preiswechsel
schreibt `state.json` neu, löst aber keine Mail aus). `tracked.json` wird bei
jedem Lauf neu geschrieben (nur nach bestandener Rückgangs-Prüfung, damit eine
unvollständige Antwort die Anzeige nicht leert); die Tech-Daten aller Teile
liegen im `specs-cache.json` und werden nur einmal je Teil geladen.

```toml
[webui]
bind = "127.0.0.1:8787"   # Standard: nur per SSH-Tunnel erreichbar
token = ""                # HTTP-Basic-Token für die ganze Seite (leer = offen)
admin_user = "admin"      # Login nur für die Einstellungen
admin_pass = ""           # leer = Einstellungen gesperrt
admin_attempts = 5        # danach ist der Login kurz gesperrt (0 = keine Bremse)
admin_lockout_secs = 60   # Sperrdauer in Sekunden
```

Die Bremse zählt Fehlversuche im Speicher (ein Neustart hebt sie auf) und
antwortet ab `admin_attempts` mit **429** plus `Retry-After`; die Seite zeigt
dann einen Countdown am Anmeldeknopf. Die Sperre trifft nur Rateversuche –
richtige Zugangsdaten kommen weiterhin durch (sonst könnte jeder den Admin
aussperren) und setzen den Zähler zurück. Gesetztes `token` und Admin-Zugang
schließen sich nicht aus: der Admin-Login öffnet die ganze Seite mit, weil es
nur einen `Authorization`-Kopf gibt.

```bash
# im CT starten (z. B. per cron-Watchdog, siehe unten)
pct exec 120 -- /opt/lenovo-tracker/lenovo-tracker --serve --config /etc/lenovotracker/config.toml
# vom eigenen Rechner aus durchreichen:
ssh -N -L 8787:127.0.0.1:8787 root@<proxmox-host>
# → http://127.0.0.1:8787
```

Für Zugriff aus dem LAN `bind = "0.0.0.0:8787"` **und** ein `token` setzen –
ohne Token startet der Server auf einer nicht-Loopback-Adresse nicht, weil die
Oberfläche die Config schreiben darf. Schreibende Anfragen verlangen zusätzlich
den Header `X-Requested-With` (kein CSRF über fremde Seiten).

## Neue Hardware (14. Gen, Core Ultra 200, Ryzen AI, RTX 50)

Die Tabellen (`cpu-scores.tsv`, `gpu-scores.tsv`, `cpu-igpu.tsv`) sind
Momentaufnahmen. Neue Mainboards brauchen deshalb **keinen Code**: taucht ein
Modell auf, das in der passenden Tabelle steht, wird es sofort richtig bewertet
und sortiert. Es gibt drei Stufen:

1. **Exakter Treffer** – Zeile in `cpu-scores.tsv` (bzw. `gpu-scores.tsv`,
   `cpu-igpu.tsv`) vorhanden → gemessener Wert, Tooltip nennt die Quelle.
2. **Ableitung** – keine Zeile: die CPU wird über Serie + Generation geschätzt
   (`cpu-fallback.tsv`), die iGPU über `igpu_key` aus Reihe/Generation/Modell
   bestimmt, unbekannte GPUs über die Modellnummern-Heuristik. Der Tooltip sagt
   dann „kein Messwert – aus Serie/Generation geschätzt“ bzw. zeigt „≈“.
3. **Sichtbar machen** – `--unknowns` listet genau die Modelle, die in keiner
   Tabelle stehen (CPU ohne Benchmark, GPU ohne Score, iGPU ohne Zuordnung,
   je mit Anzahl der Teile). Dann eine Zeile ergänzen – fertig.

```bash
./target/release/lenovo-tracker --unknowns --config config.toml
# CPU ohne Benchmark-Zeile (cpu-scores.tsv): 19 …
# GPU ohne Score-Zeile (gpu-scores.tsv): 0
# iGPU ohne Zuordnung (cpu-igpu.tsv): 2 (Snapdragon)
```

Sonderfälle stehen **als Zeilen in `cpu-igpu.tsv`**, nicht als Verzweigung im
Code – `igpu_key` macht nur noch die groben Reihen-Regeln (Intel-Generation,
Ryzen-Serie/Modellende, Celeron/Pentium/Athlon, sonst nichts):

| Modelle | Zeile in `cpu-igpu.tsv` |
|---|---|
| i7-/i9-HX (12.–14. Gen, 12800HX … 14900HX) | UHD 770 (2.655) |
| i5-HX (13450HX … 14500HX) | UHD Xe 16EU (348) – nur 16 EU statt 32 |
| i5-13420H | UHD 48 EU (832, `est`) |
| i7-13620H, i7-12650H | UHD 64 EU (1.110) |
| Core Ultra 200V (226V–288V) | Arc 130V (3.388) bzw. 140V (4.044) ab Modell 250 |
| Core Ultra 200H (225H, 255H, 285H) | Arc 140T (3.843) |
| Core Ultra 200HX (275HX) | Intel Graphics 4-Core (1.902) – nur 4 Xe-Kerne, kein Arc |
| Core Ultra 200K (265K, 285K) | Intel Graphics 4-Core (1.902) – Arrow Lake-S, nicht UHD 770 |
| Intel N100 | UHD 24EU (321) |
| Snapdragon 7c (SC7180) / 8cx Gen 3 (SC8280XP) | Adreno 618 (73) / Adreno 8cx Gen 3 (891) |
| Ryzen AI 7 350 / 9 365 / 9 HX 370 / HX 375 | 860M / 880M / 890M |
| Ryzen AI Max 390 / Max+ 395 | genähert 890M (Radeon 8060S fehlt in der Tabelle) |

Der Parser selbst kennt die Familien trotzdem: Intel `14. Gen` als Filter,
„Core Ultra Series 1/2“, `Ryzen AI` mit eigenem Tabellenschlüssel `rai:NNN`
(vorher landete „Ryzen AI 7 350“ auf der Zeile des alten AMD E-350) und
Generation „Ryzen AI 300“; ohne Messzeile greift der Familien-Median aus
`cpu-fallback.tsv`. Die Tabellen haben aktuell 489 Zeilen (iGPU) bzw. 1.965
(CPU) und 2.262 (GPU).

Grenzen: `est`-Zeilen in `cpu-igpu.tsv` sind aus dem EU-Ausbau geschätzt; für
Radeon 8060S (Strix Halo) gibt es keinen Messwert, dort steht genähert die
890M. Intels „F"/„KF"-Modelle (z. B. Core Ultra 7 265KF, Core i5-9400F) haben
gar keine iGPU – `igpu_key` liefert dafür bewusst nichts, statt die Reihen-iGPU
einzusetzen.

## Schutz vor sprunghaften API-Antworten

Lenovo nimmt verkaufte Teile aus der Liste; ein Rückgang kann aber auch eine
unvollständige Antwort sein. Damit daraus keine falschen Anstiegs-Mails werden:

- Fehlen in einem Lauf **mindestens 3 Teile und mindestens 20 %** des bekannten
  Bestands, gilt die Antwort als verdächtig: Der State bleibt unverändert, der
  nächste Lauf entscheidet. Antwortet die API zweimal so, wird der Rückgang
  übernommen (Log: „Rückgang bestätigt").
- Verschwundene Teile bleiben mit ihrem letzten Bestand als `gone` im State.
  Kehren sie zurück, ist das **kein Anstieg** und löst keine Mail aus – auch
  keine Keyword-Mail. Erst ein Bestand über dem letzten bekannten Wert meldet
  wieder.
- Eine leere oder kaputte API-Antwort bricht den Lauf schon vorher ab
  (`Abruf fehlgeschlagen – kein State-Update`).

## Entwicklung

```bash
cd rust-tracker && cargo test   # Unit-Tests (Filter, Diff, State, Specs-Pipeline, Mail)
cargo test write_preview_mail -- --ignored --nocapture  # Beispiel-Mail → preview/beispiel-mail.html (kein Versand)
cargo build --release           # opt-level="s" (Parse wie Stufe 3, −20 % Binärgröße), LTO, strip, panic=abort
                                # Mail: lettre nur als MIME-Builder, SMTP/STARTTLS/AUTH über curl
                                # (kein rustls/ring/Root-Store: 1,85 MB → 0,94 MB Windows / 1,24 MB musl)
```

## Troubleshooting

| Symptom | Lösung |
|---|---|
| `HTTP 403 – Akamai blockiert` | Neuere curl-impersonate-Version (`CI_VERSION`) oder anderes Imitation-Target (chrome99…131, edge101, safari17_0, firefox133). |
| Mail kommt nicht an | Log prüfen, `--test-mail`, Versandweg in der Config (`smtp_host`/`smtp_port`). |
| `Keine Mainboards in der API-Response` | legitim möglich (alles verkauft) — WARNING im Log, keine Mail bei Abstieg. |
| curl `exit=23` | Rootfs voll (`df -h /`) — Logs/State brauchen Platz; aufräumen oder `pct resize 105 rootfs +8M`. (API-Transfers selbst laufen über Pipes und brauchen kein `/tmp`.) |
| Bestand „springt" stark | Meist echte Verkäufe (Teile verschwinden aus der Liste) – der Guard übernimmt einen Rückgang erst nach Bestätigung; Details in `pending.json`/Log. |

## Lizenz

**MIT** — siehe [LICENSE](LICENSE). © 2026 LMasterPiec3
