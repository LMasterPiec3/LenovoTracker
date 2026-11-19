# Lenovo Clearance-Sale Mainboard-Tracker

Überwacht die öffentliche [Lenovo Clearance-Sale-Seite](https://support.lenovo.com/de/de/parts-lookup/clearance-sale)
und sendet eine E-Mail an **Lenovo@aomme.de** (von **Lenovo@aomme.de**), sobald die
Gesamtsumme aller verfügbaren Mainboards (Commodity „System Boards") steigt.
Optionale weitere Empfänger: `extra_recipients` in der Config.

## Wie es funktioniert (reverse-engineered API)

Die Seite lädt ihre Daten von einem unauthentifizierten Endpunkt:

```
GET https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get
```

Die Antwort enthält pro Teil u. a. `id`, `name`, `commodityKey`, `stock`
(verfügbare Menge) und `promotionPrice`. Der Tracker:

1. filtert `commodityKey == "system boards"` (Mainboards),
2. summiert `stock`,
3. vergleicht mit dem letzten Lauf (`state.json`),
4. mailt bei Anstieg — **nur** dann. Die allererste gültige Prüfung legt still
   (ohne Mail) die Baseline fest; fehlgeschlagene Mailversände aktualisieren den
   State nicht, sodass der Anstieg im nächsten Lauf erneut gemeldet wird.

Lenovos Akamai-Schutz blockt normale HTTP-Clients (curl → HTTP 403). Der Tracker
nutzt deshalb [`curl_cffi`](https://pypi.org/project/curl-cffi/) mit Chrome-TLS-Imitation
(`impersonate="chrome"`) — verifiziert funktionierend (Stand 2026-11-18: 9.261 Teile,
3.128 Mainboards, 16.489 Stück Bestand).

Alle Reverse-Engineering-Details zur API (Envelope, Part-Schema, Bot-Schutz,
Regionen, Parameter-Matrix) stehen in [Lenovo-API.md](Lenovo-API.md).

## LXC-Container erstellen (Proxmox-Host)

```bash
pct create 200 local:vztmpl/debian-12-standard_12.7-1_amd64.tar.zst \
  --hostname lenovo-tracker \
  --memory 1024 --cores 1 --swap 512 \
  --net0 virtio,bridge=vmbr0,firewall=1 \
  --rootfs local-lvm:8 \
  --unprivileged 1 --features nesting=1 \
  --start 1
pct enter 200
```

(Template-Dateiname je nach lokalem `pveam list local` anpassen.)

## Installation im Container

Dieses Repo auf den Container bringen (z. B. `pct push 200 /tmp/LenovoTracker.tar.gz ...`
oder `git clone`), dann:

```bash
cd /pfad/zu/LenovoTracker
./install.sh
nano /etc/lenovotracker/config.toml   # Absender/Empfänger prüfen (Default: Lenovo@aomme.de)
systemctl restart lenovo-tracker
```

`install.sh` installiert `python3`, `postfix` (Non-interactive, Typ „Internet Site"),
baut eine venv mit `curl-cffi`, legt Service/Config/State-Verzeichnis an und startet
den Service.

## E-Mail-Versand (Proxmox-Stil)

Standard ist **sendmail-Modus**: Der Container-postfix („Internet Site") stellt
direkt per DNS/MX an den Mailserver von `aomme.de` zu — genau wie Proxmox VE es
macht: Adresse angeben, der MTA findet den Weg.

- **Testen:** `... lenovo_tracker.py --config /etc/lenovotracker/config.toml --test-mail`
- **Falls die Mail nicht ankommt** (Heimnetz ohne erreichbaren Port 25 / SPF-Probleme):
  1. Smarthost-Variante: in `/etc/postfix/main.cf` `relayhost = [dein.mailserver]:587`
     (ggf. mit `smtp_sasl_auth_enable` + Credentials in `/etc/postfix/sasl_passwd`)
     setzen, `postmap /etc/postfix/sasl_passwd && systemctl reload postfix`.
  2. Oder in der Config `mail.mode = "smtp"` setzen und `smtp_host`/`smtp_user`/
     `smtp_password` füllen — der Tracker verschickt dann direkt per SMTP.

## Bedienung

```bash
systemctl status lenovo-tracker          # läuft der Service?
journalctl -u lenovo-tracker -f          # Live-Logs (INFO je Zyklus: Summe + Teile)
/opt/lenovo-tracker/venv/bin/python /opt/lenovo-tracker/lenovo_tracker.py \
  --config /etc/lenovotracker/config.toml --once        # einmal prüfen (Exit 2 bei Abruf-Fehler)
/opt/lenovo-tracker/venv/bin/python /opt/lenovo-tracker/lenovo_tracker.py \
  --config /etc/lenovotracker/config.toml --dry-run     # Zusammenfassung ohne State/Mail
/opt/lenovo-tracker/venv/bin/python /opt/lenovo-tracker/lenovo_tracker.py \
  --config /etc/lenovotracker/config.toml --test-mail   # Testmail
```

Daten:
- `/etc/lenovotracker/config.toml` — Konfiguration
- `/var/lib/lenovotracker/state.json` — letzter Bestand (je Teil)
- `/var/lib/lenovotracker/history.csv` — Verlauf (`timestamp,total_stock,part_count`)

## Konfiguration (Auszug)

```toml
[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]

[mail]
mode = "sendmail"
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = []   # z. B. ["du@anderswo.de"]
```

## Alpine-Linux-Variante (OpenRC statt systemd)

Getestet auf Alpine 3.24 (LXC, Python 3.14, musl): `curl_cffi` installiert
sauber über pip (musl-Wheel) und liefert identische Daten. Abweichungen:

```bash
apk update && apk add python3 py3-pip postfix ca-certificates
pip3 install --break-system-packages curl-cffi
mkdir -p /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker
cp lenovo_tracker.py /opt/lenovo-tracker/
cp config.example.toml /etc/lenovotracker/config.toml
newaliases                       # aliases.db für postfix bauen
rc-update add postfix default && rc-service postfix start
cp lenovo-tracker.openrc /etc/init.d/lenovo-tracker && chmod +x /etc/init.d/lenovo-tracker
rc-update add lenovo-tracker default && rc-service lenovo-tracker start
```

- Dienst: `rc-service lenovo-tracker status|restart` — Log unter
  `/var/log/lenovo-tracker.log` (Postfix loggt nach `/var/log/messages`)
- Staat: `/var/lib/lenovotracker/state.json` + `history.csv` (identisch zu Debian)

### Keyword-Filter (nur Rust-Version, optional)

Die API kennt keine serverseitige Filterung (siehe [Lenovo-API.md](Lenovo-API.md),
§4) — aber der Rust-Tracker kann zusätzlich **innerhalb** der Mainboards nach
Keyword eingrenzen:

```toml
[tracker]
commodity_keys = ["system boards"]
name_keywords = ["B650", "AM5"]   # optional; leer/entfernt = alle Mainboards
```

- ODER-verknüpft, case-insensitiv, Substring-Match auf **Bezeichnung und
  Teilenummer** (`b650` findet „AMD B650M …", `5A10W86303` trackt ein
  konkretes Teil)
- Summe, Mail-Diff und Verlauf beziehen sich dann nur auf die passenden Teile
- Filterwechsel (Keywords oder `commodity_keys` geändert) setzt die Baseline
  **automatisch still neu** – der State trägt einen Filter-Fingerprint, es
  gibt keine Garbage-Mail mit pseudo „Neu im Sale"-Teilen

### Tech-Daten in der Mail

Bei jedem Alarm lädt der Tracker für die gemailten Teile (Top-20 je Liste) die
„Technischen Daten" nach — ein Batch-POST gegen `api/v4/lbom/parts/get`
(siehe [Lenovo-API.md](Lenovo-API.md), §10) — bereitet sie lesbar auf
(kompakte CPU-Codes wie `intelI79750H` → „Intel Core i7-9750H", GPU
DIS/UMA → „diskret"/„integriert", RAM aus dem `dram`-Attribut oder aus dem
Namen) und zeigt sie in fester Reihenfolge unter dem Teil an:

```
  +2  02DC237  AMD R7 2700U mit Vega10 WIN YT  (3 → 5)
      CPU: AMD Ryzen 7 2700U | GPU: integriert | DPK: WIN
```

Schlägt der Detail-Abruf fehl, geht die Mail trotzdem raus (nur ohne Specs).

### Produktivstand (CT 105, Alpine, maximal kompakt)

- **33 MB Disk-Limit** (~17 MB belegt — Alpine-Base + gestrippte Binaries, kein
  dhcpcd/logrotate/doas/alpine-conf/dcron; `pct resize 105 rootfs +16M` falls je nötig).
  **Achtung:** nur ~9 MB frei — der Tracker räumt seine Body-Temp-Datei selbst weg,
  aber Debug-/Probe-Tools müssen `/tmp` sauber hinterlassen, sonst schlagen Abrufe
  mit curl `exit=23` (Write error, ENOSPC) mitten im Transfer fehl
- **32 MB RAM-Limit** (Peak ~20 MB über 3 gemessene Läufe, Boot-Peak ~15 MB:
  curl streamt den Body in eine Temp-Datei, Rust parst mit `from_reader` streckend
  – der JSON-Body liegt nie komplett im RAM; State-I/O single-pass in typisierte
  Structs bzw. `to_writer` direkt in die Datei; `oom_kill 0` getestet; 32 MB Swap
  als Überlauf-Puffer)
- **Rust-Binary 949 KB** (gestrippt): TLS-Stack (`lettre`/native-tls → OpenSSL)
  und `time`-Crate entfernt, eigene RFC3339-Formatierung; Build im Alpine-CT 104
  ohne cmake/go/clang/openssl-dev in **~24 s**
- **Kein dcron-paket**: crond läuft als `::respawn:`-Eintrag in `/etc/inittab`
  (busybox crond im Vordergrund, init überwacht und startet bei Crash automatisch neu)
- Resident: init + syslogd + crond ≈ **2 MB RAM**, CPU ~0,1 % im Jahresmittel
- Mail: Direkt-SMTP an `mail.protonmail.ch:25` (Plain-SMTP). TLS-Support wurde
  aus dem Binary entfernt (`lettre` ohne TLS-Feature, kein OpenSSL mehr): der
  STARTTLS-Pfad hing bei Proton ohnehin, `smtp_starttls` in der Config wird
  ignoriert — bei Bedarf postfix nachrüsten
- Peak-Messung: `python tools/peak_check.py <vmid>` — Host-Cgroup-Messung;
  ein Reset von `memory.peak` *innerhalb* des CT scheitert still (Permission
  denied), daher `pct stop/start` zum Nullen des Peaks oder Lifetime-Peak nach
  Neustart ablesen

## Tests (Entwicklung)

```bash
python -m unittest tests.test_tracker -v
```

## Troubleshooting

| Symptom | Ursache / Lösung |
|---|---|
| `HTTP 403 – Akamai blockiert` im Log | Bot-Schutz schlägt `curl_cffi`. Update: `pip install -U curl-cffi` (neuere Chrome-Imitation). Bleibt es: Headless-Chromium-Fallback einplanen (API-Abruf aus echtem Browser-Kontext — im Browser verifiziert identische Daten). |
| Mail kommt nicht an | `mail.log` prüfen (`journalctl -u postfix -f`), `--test-mail`, dann Versandweg oben umstellen. |
| `Keine Mainboards in der API-Response` | Legitim möglich (alles verkauft) — WARNING im Log, keine Mail bei Abstieg. |
| Bestand „springt" stark | Lenovo aktualisiert den Bestand mehrmals täglich; `updateTime` in der API zeigt den letzten Pull. |
