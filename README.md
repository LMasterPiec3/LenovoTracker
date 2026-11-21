# Lenovo Clearance-Sale Mainboard-Tracker (Rust)

Überwacht die öffentliche [Lenovo Clearance-Sale-Seite](https://support.lenovo.com/de/de/parts-lookup/clearance-sale)
und sendet eine E-Mail sobald die Gesamtsumme des verfügbaren Mainboard-Bestands
(Commodity „system boards") **steigt** — inkl. „Technischen Daten" der
betroffenen Teile.

Alles Reverse-Engineering zur API (Endpunkte, Bot-Schutz, Part-Schema,
Regionen, Parameter-Matrix): **[Lenovo-API.md](Lenovo-API.md)**.

## Architektur

```
Proxmox-Host
└── LXC „lenovo-tracker-rust" (Alpine, 32 MB RAM / 32 MB Swap / ~256 MB Disk)
    ├── /opt/lenovo-tracker/lenovo-tracker        # Rust-Binary (~1 MB, gestrippt)
    ├── /opt/lenovo-tracker/curl-impersonate      # Chrome-TLS-Imitation (Akamai-Umgehung)
    ├── /etc/lenovotracker/config.toml            # Konfiguration
    ├── /var/lib/lenovotracker/state.json + history.csv
    ├── /var/log/lenovo-tracker.log
    └── cron: */30 … --once  (crond init-überwacht via ::respawn in /etc/inittab)
```

Ablauf je Lauf: API abrufen (curl-impersonate, Body streamt in Temp-Datei) →
`commodity_keys`/`name_keywords`-Filter → Summe `stock` → Diff gegen
`state.json` → **nur bei Anstieg**: Tech-Daten der Top-Teile nachladen
(Batch-POST) und Mail senden → State + CSV schreiben. Fehlgeschlagene
Mailversände aktualisieren den State nicht (Anstieg wird erneut gemeldet).

## Schnellstart (Proxmox-Host)

**Der Klassiker** — eine Zeile, lädt das Script direkt von GitHub (Repo muss
public sein, Quellen werden automatisch von GitHub raw nachgeladen):

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
```

Im interaktiven Menü sind **alle technischen Felder mit sinnvollen Defaults
voreingestellt** (Hostname, RAM, Swap, CPU-Limit, Disk, Bridge, Gateway,
IP-Basis) und bleiben editierbar. Bridge, Gateway und IP-Basis werden dabei
automatisch vom PVE-Host erkannt (Default-Route + Host-IP), der SMTP-Host
aus dem Postfix-Relayhost bzw. dem MX/A-Record der Absender-Domain, die
CT-ID mit der ersten freien ID vorbelegt und Commodity-Keys per
Mehrfachauswahl-Checkliste gewählt. Nur Mail-Adressen fragt das Setup leer
ab. Per Env gesetzte Werte überschreiben die Defaults.

```bash
VMID=110 MAIL_FROM="du@example.de" MAIL_TO="du@example.de" \
SMTP_HOST="mail.example.de" SMTP_PORT=25 COMMODITY_KEYS="system boards" \
KEYWORDS="RTX" \
  bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
```

**Oder mit Repo-Klon** (lokales Repo wird bevorzugt — praktisch für Entwicklung):

```bash
git clone https://github.com/LMasterPiec3/LenovoTracker.git && cd LenovoTracker
bash lenovo-tracker-setup.sh
```

Das Script richtet **alles** ein (Stil der Proxmox-Community-Helper-Scripts):
interaktives whiptail-Menü (wird bei Bedarf via `apt` nachinstalliert) →
**Wizard mit 4 Formular-Seiten** (1 Container & Ressourcen, 2 Netzwerk mit
Bridge-Auswahl, 3 Mail & SMTP, 4 Filter) → Binary aus den GitHub Releases
laden → Produktions-CT (Alpine, minimal) → curl-impersonate → Config →
Cron → Dry-Run-Verifikation → optionale Test-Mail.

Binary-Quelle: **GitHub-Release-Download (Standard – kein zweiter LXC, kein
Build-CT)** → vorhandenes `/root/lenovo-tracker-rs` (`SKIP_BUILD=1`) →
temporärer Build-CT nur als Opt-in-Notfall (`BUILD_METHOD=build
BUILD_VMID=<id>`), wenn mal kein Release-Asset existiert; er baut dank
`+crt-static` ein statisches musl-Binary.

| Variable | Pflicht | Bedeutung |
|---|---|---|
| `VMID` | ja (als Vorlage wird die erste freie ID ab 100 vorbelegt) | Produktions-CT |
| `BUILD_VMID` | nur bei `BUILD_METHOD=build` | temporärer Build-Container |
| `CT_HOSTNAME` | nein (Default `LenovoTracker`) | Hostname des Tracker-CTs |
| `RAM` | nein (Default `32`) | MB (Peak gemessen ~16–20 MB) |
| `SWAP` | nein (Default `32`) | MB |
| `CPULIMIT` | nein (Default `0.25`) | Viertel-Kern |
| `DISK` | nein (Default `local:0.25`) | Rootfs-Größe |
| `BRIDGE` | nein (Default automatisch erkannt, sonst `vmbr0`) | Proxmox-Bridge – im Setup als Menü aller Host-Bridges wählbar, Gateway/IP-Basis passen sich der Wahl automatisch an |
| `GATEWAY` / `BASE_IP` | nein (automatisch vom Host erkannt; Gateway ohne Bridge-Route: Vorschlag `<IP-Basis>.1`) | Netz (CT-IP = `<BASE_IP>.<VMID>`) |
| `NET0` | nein | komplette Netzkonfiguration statt `BRIDGE`/`GATEWAY`/`BASE_IP` |
| `MAIL_FROM` / `MAIL_TO` | ja | Absender/Empfänger (komma-getrennt) |
| `SMTP_HOST` / `SMTP_PORT` | nein (automatisch ermittelt: Postfix-Relayhost des Hosts, sonst MX/A der Absender-Domain; Port sonst `25`) | Plain-SMTP |
| `SMTP_USER` / `SMTP_PASS` | nein (leer = Plain-SMTP ohne Login) | SMTP-Login – gesetzt: authentifizierte Submission über STARTTLS (Port `587` automatisch); Provider signiert DKIM/SPF → kein "failed authentication"-Banner. Proton: `smtp.protonmail.ch:587` + App-Passwort |
| `COMMODITY_KEYS` | nein (Default `system boards`) | Commodity-Filter – im Setup als Mehrfachauswahl-Checkliste (Leertaste an/aus) über alle bekannten Warengruppen |
| `KEYWORDS` | nein | Keyword-Filter, z. B. `KEYWORDS="RTX"` |
| `EXTRA_RECIPIENTS` | nein | weitere Empfänger (komma-getrennt) |
| `FORCE=1` | nein | existierenden CT löschen + neu aufbauen |
| `SKIP_BUILD=1` | nein | vorhandenes `/root/lenovo-tracker-rs` nutzen |

Weitere Kommandos (nur per One-Liner/CLI, nicht im Menü):

```bash
bash lenovo-tracker-setup.sh --edit-config 105 # bestehende Config bearbeiten
bash lenovo-tracker-setup.sh --status 105     # CT-Status, Cron, Log und State
bash lenovo-tracker-setup.sh --test-mail 105  # Test-Mail senden
bash lenovo-tracker-setup.sh --probe 105      # API-Parameter-Matrix live testen
bash lenovo-tracker-setup.sh --peak 105       # Peak-RAM messen (3 Läufe, CT-Restart)
bash lenovo-tracker-setup.sh --uninstall      # CT + Host-Artefakte entfernen
```

Existiert bereits eine Installation (CT mit Tag `lenovo-tracker`), erkennt das
Menü sie automatisch und bietet **„Config bearbeiten“** an: Mail, SMTP und
Filter werden vorbelegt abgefragt, die übrige Config bleibt unangetastet
(Backup: `config.toml.bak`), danach läuft ein Dry-Run als Prüfung.

## Konfiguration (`/etc/lenovotracker/config.toml`)

Vollständiges Beispiel: [config.example.toml](config.example.toml).

### Keyword-Filter (optional)

```toml
[tracker]
commodity_keys = ["system boards"]
name_keywords = ["RTX"]   # leer/entfernt = alle Mainboards
```

- ODER-verknüpft, case-insensitiv, Substring auf **Bezeichnung UND Teilenummer**
  (`b650` findet „AMD B650M …", `5A10W86303` trackt ein konkretes Teil)
- Summe, Mail-Diff und Verlauf beziehen sich dann nur auf die passenden Teile
- Filterwechsel setzt die Baseline **automatisch still neu** (Filter-Fingerprint
  im State) — keine Garbage-Mail mit pseudo „Neu im Sale"-Teilen

### Tech-Daten in der Mail

Bei jedem Alarm werden die „Technischen Daten" der gemailten Teile (Top-20 je
Diff-Liste) nachgeladen (Batch-POST `api/v4/lbom/parts/get`, siehe
[Lenovo-API.md](Lenovo-API.md) §10), lesbar aufbereitet und in fester
Reihenfolge angezeigt:

```
  +2  02DC237  AMD R7 2700U mit Vega10 WIN YT  (3 → 5)
      CPU: AMD Ryzen 7 2700U | GPU: integriert | DPK: WIN
```

- kompakte CPU-Codes werden beautifiziert (`intelI79750H` → „Intel Core i7-9750H")
- GPU `DIS`/`UMA` → „diskret"/„integriert"
- RAM aus dem `dram`-Attribut oder aus dem Namen (mit VRAM-Ausschluss bei
  diskreten GPUs; mehrdeutig → kein RAM-Chip)
- Schlägt der Detail-Abruf fehl, geht die Mail trotzdem raus (nur ohne Specs)

Die HTML-Mail ist client-sicher gestylt (Markenkopf, Delta-Badge, Spec-Chips,
CTA-Button) — alles Inline-Styles, keine externen Bilder.

## Bedienung

```bash
pct exec 105 -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --dry-run    # Zusammenfassung, kein State/Mail
pct exec 105 -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once      # ein Zyklus (Exit 2 bei Abruf-Fehler)
pct exec 105 -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --test-mail # Testmail
pct exec 105 -- tail -f /var/log/lenovo-tracker.log                                                    # Live-Log
pct exec 105 -- crontab -l                                                                             # Cron-Eintrag
```

Daten:
- `/etc/lenovotracker/config.toml` — Konfiguration
- `/var/lib/lenovotracker/state.json` — letzter Bestand (je Teil) + Filter-Fingerprint
- `/var/lib/lenovotracker/history.csv` — Verlauf (`timestamp,total_stock,part_count`)

## Produktion (CT 105)

- **32 MB RAM-Limit** (Peak ~16–20 MB, Boot-Peak ~15 MB; 32 MB Swap als Puffer)
- **~44 MB Rootfs belegt** (Alpine + 1,1 MB statisches Binary + 33 MB
  curl-impersonate; 256-MB-Disk → ~170 MB frei für Logs/History; Debug-Tools
  müssen `/tmp` sauber hinterlassen, sonst curl `exit=23`/ENOSPC im Transfer)
- Binary ~1,1 MB, statisch gelinkt (musl `+crt-static`): kein TLS-Stack (lettre
  ohne TLS-Feature → kein OpenSSL), keine `time`-Crate (eigene
  RFC3339-Formatierung); Build ~25 s im Alpine-CT
- Mail: Direkt-SMTP an `mail.protonmail.ch:25` (Plain; `smtp_starttls` wird
  ignoriert — der STARTTLS-Pfad hing bei Proton ohnehin)
- crond ohne dcron-Paket: `::respawn`-Eintrag in `/etc/inittab`, init startet
  bei Crash automatisch neu

## Entwicklung

```bash
cd rust-tracker
cargo test          # 12 Unit-Tests (Filter, Diff, State, Specs-Pipeline, Mail)
cargo build --release
```

Release-Profil: `opt-level=z`, LTO, `codegen-units=1`, `strip`, `panic=abort`.

## Lizenz

**MIT** — free to use. Siehe [LICENSE](LICENSE).

Copyright © 2026 LMasterPiec3

## Troubleshooting

| Symptom | Ursache / Lösung |
|---|---|
| `HTTP 403 – Akamai blockiert` im Log | Bot-Schutz schlägt curl-impersonate. Neuere curl-impersonate-Version einsetzen (Setup-Variable `CI_VERSION`) oder Imitation-Target in der Config ändern (chrome99…131, edge101, safari17_0, firefox133 — alle verifiziert funktionierend). |
| Mail kommt nicht an | `pct exec 105 -- tail /var/log/lenovo-tracker.log`, `--test-mail`; Versandweg in der Config (`smtp_host`/`smtp_port`). |
| `Keine Mainboards in der API-Response` | Legitim möglich (alles verkauft) — WARNING im Log, keine Mail bei Abstieg. |
| curl `exit=23` / Transfers brechen ab | Rootfs voll (`pct exec 105 -- df -h /`) — `/tmp` aufräumen oder `pct resize 105 rootfs +8M`. |
| Bestand „springt" stark | Lenovo aktualisiert mehrmals täglich; `updateTime` in der API zeigt den letzten Pull. |
