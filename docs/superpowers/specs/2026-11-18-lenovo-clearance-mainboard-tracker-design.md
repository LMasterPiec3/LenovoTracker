# Design: Lenovo Clearance-Sale Mainboard-Tracker

**Datum:** 2026-11-18
**Status:** Von User freigegeben (Design-Review)
**Ziel-Plattform:** Proxmox LXC (Debian 12)

## 1. Ziel & Scope

Ein Tracker überwacht die öffentliche Lenovo-„Clearance Sale"-Seite
(https://support.lenovo.com/de/de/parts-lookup/clearance-sale) und ermittelt
die **Gesamtsumme aller verfügbaren Mainboards** (Commodity „System Boards").

- Prüfung alle **30 Minuten**
- **E-Mail nur bei Anstieg** der Summe, von `Lenovo@aomme.de` an `Lenovo@aomme.de`
  (+ optionale weitere Empfänger)
- Versand über den **lokalen MTA (sendmail)** im Container — Proxmox-Stil:
  nur die Adresse angeben, der MTA findet den Weg (DNS/MX)
- Läuft als systemd-Service in einem unprivilegierten LXC

**Nicht-Ziele (YAGNI):** kein Web-Dashboard, kein Telegram/Matrix, keine
Preis-Alarme, kein Multi-Country-Support (nur `de/de`).

## 2. Reverse-Engineering-Ergebnis (verifiziert am 2026-11-18)

Die Seite ist eine Vue-SPA (`standalone-partslookup`-Bundle). Die Teile-Daten
kommen von einem einzigen unauthentifizierten Endpunkt:

```
GET https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get
Accept: application/json, text/plain, */*
Referer: https://support.lenovo.com/de/de/parts-lookup/clearance-sale
```

- Response: `{"code": 0, "data": [ … ]}` — aktuell 9.261 Teile (DE)
- Relevante Felder je Teil:
  | Feld | Bedeutung |
  |---|---|
  | `id` | Lenovo-Teilenummer (z. B. `01YT022`) |
  | `name` | Beschreibung |
  | `commodityKey` / `commodityVal` | Warengruppe (`system boards` / `SYSTEM BOARDS`) |
  | `stock` | verfügbare Menge |
  | `promotionPrice` / `price` | Aktions-/Normalpreis |
  | `partSaleType` | `Local_Promotion`, `WorldWide_Promotion`, … |
  | `products` | kompatible Machine Types |
  | `updateTime` | letzter Bestands-Update |

- **Mainboard-Erkennung:** `commodityKey == "system boards"`
  (stabil, unabhängig von der übersetzten `commodityVal`)
- **Summe = Σ `stock`** aller Mainboard-Einträge
- **Akamai-Bot-Schutz:** Plain-HTTP-Clients (curl, requests, WebFetch) erhalten
  HTTP 403. Getestet und funktionierend: Python `curl_cffi` mit
  `impersonate='chrome'` (Chrome-TLS-/HTTP2-Fingerprint) → HTTP 200, identische
  Daten wie im echten Browser. `curl_cffi` ist daher einzige externe Abhängigkeit.

## 3. Architektur

Ein Python-3-Skript als Dauer-Prozess unter systemd:

```
┌────────────┐   alle 30 min    ┌─────────────────────────────────────────┐
│ systemd    │─────────────────▶│ lenovo_tracker.py                       │
│ (Service)  │                  │  1. API abrufen (curl_cffi, 3 Retries)  │
└────────────┘                  │  2. Mainboards filtern, Σ stock         │
                                │  3. Diff gegen state.json               │
                                │     ├─ Anstieg  → Mail → State+CSV      │
                                │     ├─ gleich/runter → Log+CSV          │
                                │     └─ API-Fehler → nur Log             │
                                │  4. sleep(interval)                     │
                                └─────────────────────────────────────────┘
```

Begründung für Dauer-Loop statt systemd-Timer: alle Logik bleibt in einem
Prozess, journald-Logging, kein Zombie-State zwischen Läufen.

## 4. Dateien

```
LenovoTracker/
├── lenovo_tracker.py        # komplette Anwendung (fetch/diff/mail/CLI)
├── config.example.toml      # Vorlage → /etc/lenovotracker/config.toml
├── install.sh               # läuft im LXC: Pakete, venv, Service, Config
├── lenovo-tracker.service   # systemd-Unit
├── README.md                # LXC-Erstellung (pct create), postfix-Hinweise, Betrieb
├── tests/
│   ├── fixture_clearance.json   # gespeicherte API-Response (Fixtur)
│   └── test_tracker.py          # Unittests: Filter, Summe, Diff, Mail-Inhalt
└── docs/superpowers/specs/…-design.md
```

## 5. Konfiguration (`/etc/lenovotracker/config.toml`)

```toml
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3                 # Backoff: 5s, 15s, 45s

[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]   # Filter = Mainboards

[mail]
mode = "sendmail"                    # "sendmail" (Default) | "smtp"
sendmail_path = "/usr/sbin/sendmail"
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = []                # weitere Adressen, optionale Liste
subject_prefix = "[Lenovo Tracker]"
# nur relevant bei mode = "smtp":
smtp_host = ""
smtp_port = 587
smtp_user = ""
smtp_password = ""
smtp_starttls = true

[paths]
state_dir = "/var/lib/lenovotracker"  # state.json + history.csv
```

Fehlt die Config → Skript bricht mit klarer Fehlermeldung ab (kein Silent-Fail).

## 6. Zustand & Verlauf

- `state.json`: `{last_check, total_stock, part_count, parts: {"01YT022": 2, …}}`
  (~ einige hundert KB, okay)
- `history.csv`: `timestamp,total_stock,part_count` — eine Zeile pro **gültiger**
  Prüfung (auch wenn die Summe unverändert ist), für spätere Grafik/Auswertung
- State wird **nur** nach verifizierter gültiger Response (`code == 0` und
  `data` nicht leer) geschrieben → API-Ausfälle erzeugen weder Fehlalarme
  noch verlieren die Baseline.

## 7. E-Mail (nur bei Anstieg)

- **Trigger:** `neue_summe > alte_summe` (erster Lauf = Baseline, keine Mail)
- **Betreff:** `[Lenovo Tracker] Mainboard-Bestand gestiegen: 16.489 → 16.655 (+166)`
- **Inhalt (HTML + Plaintext-Alternative):**
  - Gesamtsumme alt → neu, +Δ, Anzahl Mainboard-Teile
  - Tabelle „Top-Neuzugänge": Teilenummer, Name, alt→neu (+Δ)
    (top 20 nach Δ, Rest als Summenzeile „weitere n Teile")
  - Anzahl komplett neuer Teile (neu im Sale)
  - Link zur Clearance-Seite, Zeitstempel der Prüfung
- **Versand:** `sendmail -t` mit vollständigem Header (From/To/Subject/
  MIME multipart/alternative, UTF-8). Optionaler `smtp`-Modus über `smtplib`
  (Standardbibliothek) als Plan B, falls der MTA-Pfad Probleme macht.
- `--test-mail` sendet eine Testmail mit aktuellem Bestand (ohne State zu
  verändern).

## 8. Fehlerbehandlung & Rate-Limit-Verhalten

| Fall | Verhalten |
|---|---|
| Netzwerk-/Timeout-/5xx-Fehler | Retry ×3 (5s/15s/45s), dann Log-Warnung, nächster Zyklus |
| HTTP 403 | Explizite Log-Warnung „Akamai blockiert" (Signal für Fallback Chromium, im README dokumentiert) |
| `code != 0` oder `data` leer/fehlt | Als API-Fehler behandelt: Log, kein State-Update, keine Mail |
| Mainboards plötzlich 0 | Log-Warning (kann legitime Ausverkauf-Situation sein); Summen-Abfall erzeugt wie gewünscht keine Mail |
| Config fehlt/fault | Start bricht mit verständlicher Meldung ab |

Journald-Logging (INFO pro Zyklus: Summe, Dauer; WARNING/ERROR wie oben).

## 9. LXC-Deployment

1. Container (README-Beispiel):
   `pct create 200 local:vztmpl/debian-12-standard_12.7-1_amd64.tar.zst
   --memory 1024 --cores 1 --net0 virtio,bridge=vmbr0 --unprivileged 1 --start 1`
2. `install.sh` (im Container als root): `apt install python3 python3-venv postfix`,
   venv + `pip install curl-cffi`, kopiert Skript/Config/Service, `systemctl enable --now`
3. **postfix wie Proxmox:** Default „Internet Site" — direkte Zustellung per
   DNS/MX an den Mailserver von `aomme.de` (genau das macht Proxmox-Host-postfix
   auch). Optional dokumentiert: `relayhost`-Smarthost-Variante in
   `/etc/postfix/main.cf`, falls direkte MX-Zustellung vom Heimnetz blockiert
   wird (Port-25-Ausgang vieler Provider).
4. Befehle: `lenovo-tracker --once` (einmal prüfen), `--test-mail`,
   `--dry-run` (Abruf + Zusammenfassung, kein State/Mail)

## 10. Testing

- **Unittests** (`tests/test_tracker.py`, mit Fixtur-JSON):
  Mainboard-Filter (`commodityKey`), Summenbildung, Diff alt/neu,
  Betreff-/Mail-Inhalt (Anstieg → Mailtext enthält Δ; Abstieg → kein Mailtext)
- **Live-Check auf dem Entwicklungsrechner** (Windows, curl_cffi funktioniert
  dort getestet): `--dry-run` muss 3.128 Teile / 16.489 Stück liefern
- **Mail-Pfad:** im Container `--test-mail` gegen lokales postfix

## 11. Risiken & Fallback

- Akamai kann `curl_cffi` someday erkennen → Skript loggt 403 deutlich;
  dokumentierter Fallback: Headless-Chromium (Playwright) im LXC, API-Abruf
  aus Seitenkontext (im Browser live verifiziert). Kein Code dafür (YAGNI),
  nur Pfad im README.
- „In-stock"-Definition ist Lenovos `stock`-Feld — genau das, was die Seite
  selbst anzeigt („Only X left in stock").
