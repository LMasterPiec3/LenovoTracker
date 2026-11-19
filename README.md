# Lenovo Clearance-Sale Mainboard-Tracker (Rust)

Überwacht die öffentliche [Lenovo Clearance-Sale-Seite](https://support.lenovo.com/de/de/parts-lookup/clearance-sale)
und sendet eine E-Mail (von/an **Lenovo@aomme.de**, weitere Empfänger via
`extra_recipients`), sobald die Gesamtsumme des verfügbaren Mainboard-Bestands
(Commodity „system boards") **steigt** — inkl. „Technischen Daten" der
betroffenen Teile.

Alles Reverse-Engineering zur API (Endpunkte, Bot-Schutz, Part-Schema,
Regionen, Parameter-Matrix): **[Lenovo-API.md](Lenovo-API.md)**.

## Architektur

```
Proxmox-Host
└── LXC „lenovo-tracker-rust" (Alpine, 32 MB RAM / 32 MB Swap / ~50 MB Disk)
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

Mit Parametern (Environment-Variablen funktionieren wie gehabt):

```bash
VMID=110 KEYWORDS="RTX" MAIL_TO="du@example.de" \
  bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
```

**Oder mit Repo-Klon** (lokales Repo wird bevorzugt — praktisch für Entwicklung):

```bash
git clone https://github.com/LMasterPiec3/LenovoTracker.git && cd LenovoTracker
bash lenovo-tracker-setup.sh
```

Das Script richtet **alles** ein (Stil der Proxmox-Community-Helper-Scripts):
Binary aus den GitHub Releases laden (GitHub Actions baut das musl-Binary bei
jedem Push — **kein Build nötig**) → Produktions-CT (Alpine, minimal) →
curl-impersonate → Config → Cron → Dry-Run-Verifikation.

Binary-Quelle mit Fallback-Kette: **GitHub-Release-Download** → vorhandenes
`/root/lenovo-tracker-rs` (`SKIP_BUILD=1`) → temporärer Build-CT (Fallback,
`BUILD_METHOD=build` erzwingen). Der Build-CT wird nur noch gebraucht, wenn
mal kein Release-Asset existiert.

| Variable | Default | Bedeutung |
|---|---|---|
| `VMID` | `105` | Produktions-CT |
| `BUILD_VMID` | `104` | temporärer Build-Container |
| `RAM` / `SWAP` | `32` / `32` | MB (Peak gemessen ~20 MB) |
| `DISK` | `local:0.05` | Rootfs-Größe |
| `NET0` | statisch `10.10.10.$VMID/24` an `vmbr1` | oder komplett überschreiben |
| `MAIL_FROM` / `MAIL_TO` | `Lenovo@aomme.de` | Absender/Empfänger |
| `SMTP_HOST` / `SMTP_PORT` | `mail.protonmail.ch` / `25` | Plain-SMTP |
| `KEYWORDS` | leer | Keyword-Filter, z. B. `KEYWORDS="RTX"` |
| `COMMODITY_KEYS` | `system boards` | Commodity-Filter |
| `FORCE=1` | – | existierenden CT löschen + neu aufbauen |
| `SKIP_BUILD=1` | – | vorhandenes `/root/lenovo-tracker-rs` nutzen |

Weitere Kommandos (funktionieren auch im One-Liner):

```bash
bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)" --probe 105
bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)" --peak 105
bash lenovo-tracker-setup.sh --uninstall    # bei Repo-Klon
```

```bash
bash lenovo-tracker-setup.sh --probe 105    # API-Parameter-Matrix live testen
bash lenovo-tracker-setup.sh --peak 105     # Peak-RAM messen (3 Läufe, CT-Restart)
bash lenovo-tracker-setup.sh --uninstall    # CT + Host-Artefakte entfernen
```

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

- **32 MB RAM-Limit** (Peak ~20 MB, Boot-Peak ~15 MB; 32 MB Swap als Puffer)
- **~41 MB Rootfs** (Alpine + gestrippte Binaries; nur ~10 MB frei — der Tracker
  räumt seine Body-Temp-Datei selbst weg, Debug-Tools müssen `/tmp` sauber
  hinterlassen, sonst curl `exit=23`/ENOSPC mitten im Transfer)
- Binary ~1 MB: kein TLS-Stack (lettre ohne TLS-Feature → kein OpenSSL), keine
  `time`-Crate (eigene RFC3339-Formatierung); Build ~25 s im Alpine-CT
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
