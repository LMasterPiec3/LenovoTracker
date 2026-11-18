# Lenovo Clearance-Sale Mainboard-Tracker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ein Python-Service im Proxmox-LXC, der alle 30 Minuten die Lenovo-Clearance-API abfragt, die Summe aller verfügbaren Mainboards berechnet und bei Anstieg eine E-Mail (sendmail) von `Lenovo@aomme.de` an `Lenovo@aomme.de` (+ optionale Extra-Empfänger) sendet.

**Architecture:** Ein einzelnes Python-3-Skript (`lenovo_tracker.py`) als Dauer-Loop unter systemd. Datenfluss: API-Abruf (curl_cffi mit Chrome-Imitation gegen Akamai) → Mainboard-Filter (`commodityKey == "system boards"`) → Summe `stock` → Diff gegen `state.json` → Mail bei Anstieg → State/CSV aktualisieren.

**Tech Stack:** Python 3.11 (Debian 12) / stdlib (`tomllib`, `unittest`, `email`, `smtplib`, `subprocess`, `csv`, `argparse`) + einzige externe Abhängigkeit `curl_cffi` (Akamai-Umgehung, verifiziert). Auf Python < 3.11 (Dev-Maschine) ersetzt `tomli` das `tomllib`.

**Spec:** `docs/superpowers/specs/2026-11-18-lenovo-clearance-mainboard-tracker-design.md`

## Global Constraints

- Nur Python-Stdlib + `curl_cffi` (+ `tomli` nur als Fallback für Python < 3.11). Kein pytest, kein requests.
- Mainboard-Filter: `commodityKey`-Vergleich **lowercase/strip** gegen Config-Liste; Default `["system boards"]`.
- Mail **nur bei Anstieg** (`delta > 0`); erster gültiger Lauf = Baseline ohne Mail; Abstieg/gleich = nur Log + CSV.
- State/CSV nur nach verifizierter gültiger Response (`code == 0`, `data` nicht leer) schreiben.
- Mail-Modi: `sendmail` (Default, `/usr/sbin/sendmail -t -oi`) und `smtp` (smtplib, optional).
- `From: Lenovo@aomme.de`, `To: Lenovo@aomme.de`, `extra_recipients` optional.
- Betreff-Präfix `[Lenovo Tracker]`; Zahlen deutsches Format (`16.489`).
- Intervall-Default 30 Minuten; Retry-Backoff 5s/15s/45s; bei HTTP 403 **kein** Retry (Bot-Block, sofort aufgeben).
- Alle Dateien UTF-8; Logs via `logging` auf stdout (journald).
- Exit-Codes (`--once`): 0 = OK, 1 = Config-Fehler, 2 = Abruf-Fehler.

## Review Focus

Eingangsklassen, die die Spec impliziert, aber kein Test der einzelnen Tasks direkt abdeckt — hier jeweils mit Pinning-Test:

1. API liefert `code != 0`, leere oder fehlende `data` → als FetchError behandelt, kein State-Update, keine Mail, Exit 2 bei `--once` → Test in Task 2 (`test_fetch_rejects_bad_code`, `test_fetch_rejects_empty_data`) und Task 6 (`test_run_once_fetch_error_keeps_state`).
2. Teil ohne `stock`-Feld bzw. `stock` als `None`/Trash-Wert → zählt als 0, kein Crash → Test in Task 3 (`test_stock_of_handles_missing_and_garbage`).
3. Mainboards komplett verschwunden (0 Treffer, aber gültige Response) → WARNING im Log, Summe 0, State wird trotzdem geschrieben, keine Mail (Abstieg) → Test in Task 6 (`test_run_once_no_mainboards_zero_sum_no_mail`).
4. Teilnamen mit HTML-Sonderzeichen (`<`, `>`, `&`) → in der HTML-Mail escaped → Test in Task 5 (`test_mail_html_escapes_part_names`).
5. Config als String statt Liste für `to`/`extra_recipients` → wird tolerant zu Liste normalisiert → Test in Task 1 (`test_load_config_accepts_string_recipients`).

---

### Task 1: Config-Modul (`load_config`, `Config`, `ConfigError`) + config.example.toml

**Files:**
- Create: `lenovo_tracker.py` (Grundgerüst: Docstring, Imports, Logging, Exceptions, `Config`, `load_config`)
- Create: `config.example.toml`
- Create: `tests/test_tracker.py`
- Create: `tests/__init__.py` (leer)

**Interfaces:**
- Produces: `class ConfigError(Exception)`; `@dataclass Config` mit Feldern `api_url: str`, `referer: str`, `timeout_seconds: int`, `retries: int`, `interval_minutes: int`, `commodity_keys: list[str]` (lowercase), `mail_mode: str` (`"sendmail"|"smtp"`), `sendmail_path: str`, `mail_from: str`, `mail_to: list[str]`, `extra_recipients: list[str]`, `subject_prefix: str`, `smtp_host: str`, `smtp_port: int`, `smtp_user: str`, `smtp_password: str`, `smtp_starttls: bool`, `state_dir: str`; `load_config(path: str) -> Config` (wirft `ConfigError` bei allem Ungültigen).

- [ ] **Step 1: Dev-Umgebung vorbereiten (einmalig, Dev-Maschine)**

```bash
pip install tomli   # nur für Python 3.10 auf der Dev-Maschine; Debian 12 hat tomllib ab 3.11
```

- [ ] **Step 2: Failing tests schreiben (`tests/test_tracker.py`)**

```python
import json
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO))

import lenovo_tracker as lt  # noqa: E402

VALID_TOML = """\
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3

[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]

[mail]
mode = "sendmail"
sendmail_path = "/usr/sbin/sendmail"
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = ["other@example.com"]
subject_prefix = "[Lenovo Tracker]"

[paths]
state_dir = "/var/lib/lenovotracker"
"""


def write_config(tmpdir, content):
    p = Path(tmpdir) / "config.toml"
    p.write_text(content, encoding="utf-8")
    return str(p)


class TestLoadConfig(unittest.TestCase):
    def test_load_valid_config(self):
        with tempfile.TemporaryDirectory() as td:
            cfg = lt.load_config(write_config(td, VALID_TOML))
            self.assertEqual(cfg.api_url, "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get")
            self.assertEqual(cfg.mail_from, "Lenovo@aomme.de")
            self.assertEqual(cfg.mail_to, ["Lenovo@aomme.de"])
            self.assertEqual(cfg.extra_recipients, ["other@example.com"])
            self.assertEqual(cfg.commodity_keys, ["system boards"])  # lowercase
            self.assertEqual(cfg.interval_minutes, 30)
            self.assertEqual(cfg.mail_mode, "sendmail")
            self.assertEqual(cfg.state_dir, "/var/lib/lenovotracker")

    def test_missing_file_raises(self):
        with self.assertRaises(lt.ConfigError):
            lt.load_config("/nicht/existiert/config.toml")

    def test_missing_from_raises(self):
        toml = VALID_TOML.replace('from = "Lenovo@aomme.de"\n', "")
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(lt.ConfigError):
                lt.load_config(write_config(td, toml))

    def test_invalid_mode_raises(self):
        toml = VALID_TOML.replace('mode = "sendmail"', 'mode = "pigeon"')
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(lt.ConfigError):
                lt.load_config(write_config(td, toml))

    def test_smtp_without_host_raises(self):
        toml = VALID_TOML.replace('mode = "sendmail"', 'mode = "smtp"')
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(lt.ConfigError):
                lt.load_config(write_config(td, toml))

    def test_load_config_accepts_string_recipients(self):
        toml = VALID_TOML.replace('to = ["Lenovo@aomme.de"]', 'to = "Lenovo@aomme.de"')
        with tempfile.TemporaryDirectory() as td:
            cfg = lt.load_config(write_config(td, toml))
            self.assertEqual(cfg.mail_to, ["Lenovo@aomme.de"])


if __name__ == "__main__":
    unittest.main()
```

Zusätzlich `tests/__init__.py` (leer) anlegen.

- [ ] **Step 3: Test laufen lassen — muss FAILEN**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: FAIL/ERROR (`ModuleNotFoundError: No module named 'lenovo_tracker'`).

- [ ] **Step 4: `lenovo_tracker.py` Grundgerüst + `load_config` implementieren**

```python
#!/usr/bin/env python3
"""Lenovo Clearance-Sale Mainboard-Tracker.

Überwacht die Summe verfügbbarer Mainboards auf der Lenovo-Clearance-Seite
und sendet bei Anstieg eine E-Mail über den lokalen MTA (Proxmox-Stil).
Nur Python-Stdlib + curl_cffi. Läuft als systemd-Service (Dauer-Loop).
"""

import argparse
import csv
import json
import logging
import smtplib
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from email.message import EmailMessage
from email.utils import formatdate
from html import escape
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python < 3.11 (Dev-Maschine)
    import tomli as tomllib

try:
    from curl_cffi import requests as cc_requests
except ImportError:
    cc_requests = None

PAGE_URL = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
DEFAULT_CONFIG_PATH = "/etc/lenovotracker/config.toml"
BACKOFFS = (5, 15, 45)

log = logging.getLogger("lenovo-tracker")


class ConfigError(Exception):
    """Config fehlt oder ist ungültig."""


@dataclass
class Config:
    api_url: str = ""
    referer: str = PAGE_URL
    timeout_seconds: int = 60
    retries: int = 3
    interval_minutes: int = 30
    commodity_keys: list = field(default_factory=lambda: ["system boards"])
    mail_mode: str = "sendmail"
    sendmail_path: str = "/usr/sbin/sendmail"
    mail_from: str = ""
    mail_to: list = field(default_factory=list)
    extra_recipients: list = field(default_factory=list)
    subject_prefix: str = "[Lenovo Tracker]"
    smtp_host: str = ""
    smtp_port: int = 587
    smtp_user: str = ""
    smtp_password: str = ""
    smtp_starttls: bool = True
    state_dir: str = "/var/lib/lenovotracker"


def _as_recipient_list(value):
    if isinstance(value, (list, tuple)):
        return [str(v).strip() for v in value if str(v).strip()]
    text = str(value).strip()
    return [text] if text else []


def load_config(path: str) -> Config:
    p = Path(path)
    if not p.is_file():
        raise ConfigError(f"Config-Datei nicht gefunden: {path}")
    try:
        with open(p, "rb") as fh:
            raw = tomllib.load(fh)
    except tomllib.TOMLDecodeError as exc:
        raise ConfigError(f"Config ist kein gültiges TOML: {exc}")
    try:
        api, tracker, mail, paths = raw["api"], raw["tracker"], raw["mail"], raw["paths"]
        cfg = Config(
            api_url=str(api["url"]),
            referer=str(api.get("referer", PAGE_URL)),
            timeout_seconds=int(api.get("timeout_seconds", 60)),
            retries=int(api.get("retries", 3)),
            interval_minutes=int(tracker.get("interval_minutes", 30)),
            commodity_keys=[str(k).strip().lower() for k in tracker.get("commodity_keys", ["system boards"])],
            mail_mode=str(mail.get("mode", "sendmail")),
            sendmail_path=str(mail.get("sendmail_path", "/usr/sbin/sendmail")),
            mail_from=str(mail["from"]),
            mail_to=_as_recipient_list(mail["to"]),
            extra_recipients=_as_recipient_list(mail.get("extra_recipients", [])),
            subject_prefix=str(mail.get("subject_prefix", "[Lenovo Tracker]")),
            smtp_host=str(mail.get("smtp_host", "")),
            smtp_port=int(mail.get("smtp_port", 587)),
            smtp_user=str(mail.get("smtp_user", "")),
            smtp_password=str(mail.get("smtp_password", "")),
            smtp_starttls=bool(mail.get("smtp_starttls", True)),
            state_dir=str(paths.get("state_dir", "/var/lib/lenovotracker")),
        )
    except (KeyError, TypeError, ValueError) as exc:
        raise ConfigError(f"Config unvollständig/ungültig ({path}): {exc}")
    if not cfg.api_url:
        raise ConfigError("api.url fehlt in der Config")
    if not cfg.mail_from or not cfg.mail_to:
        raise ConfigError("mail.from und mail.to müssen gesetzt sein")
    if cfg.mail_mode not in ("sendmail", "smtp"):
        raise ConfigError(f"mail.mode muss 'sendmail' oder 'smtp' sein, ist: {cfg.mail_mode!r}")
    if cfg.mail_mode == "smtp" and not cfg.smtp_host:
        raise ConfigError("mail.smtp_host fehlt, obwohl mail.mode = 'smtp'")
    return cfg
```

- [ ] **Step 5: Tests laufen lassen — PASS**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: 6 Tests PASS.

- [ ] **Step 6: `config.example.toml` anlegen**

```toml
# Lenovo Clearance-Sale Mainboard-Tracker
# Kopie nach /etc/lenovotracker/config.toml und anpassen.

[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3                # Retry-Backoff: 5s, 15s, 45s

[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]   # Filter: Mainboards (lowercase, translationssicher)

[mail]
mode = "sendmail"                    # "sendmail" (Default, wie Proxmox) | "smtp"
sendmail_path = "/usr/sbin/sendmail"
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = []                # weitere Adressen, optionale Liste
subject_prefix = "[Lenovo Tracker]"

# Nur relevant bei mode = "smtp" (Plan B, falls der MTA-Pfad Probleme macht):
smtp_host = ""
smtp_port = 587
smtp_user = ""
smtp_password = ""
smtp_starttls = true

[paths]
state_dir = "/var/lib/lenovotracker"  # state.json + history.csv
```

- [ ] **Step 7: Commit**

```bash
git add lenovo_tracker.py config.example.toml tests/
git commit -m "feat: Config-Modul mit Validierung (load_config)"
```

---

### Task 2: API-Client (`FetchError`, `_fetch_once`, `fetch_clearance_parts`)

**Files:**
- Modify: `lenovo_tracker.py` (Abschnitt „Fetch" nach dem Config-Modul anhängen)
- Modify: `tests/test_tracker.py` (Testklasse ergänzen)

**Interfaces:**
- Consumes: `Config` (Felder `api_url`, `referer`, `timeout_seconds`, `retries`) aus Task 1.
- Produces: `class FetchError(Exception)` mit Attribut `status: int | None`; `fetch_clearance_parts(cfg: Config, session=None, sleep=time.sleep) -> list[dict]` — wirft `FetchError` bei allen Fehlschlägen; `status == 403` → kein Retry. `session` ist ein injizierbares Objekt mit `.get(url, headers=..., timeout=...) -> Response` (für Tests).

- [ ] **Step 1: Failing tests ergänzen**

```python
class FakeResponse:
    def __init__(self, status_code=200, payload=None, raw=b"not json"):
        self.status_code = status_code
        self._payload = payload
        self._raw = raw

    def json(self):
        if self._payload is None:
            raise ValueError("kein JSON")
        return self._payload


class FakeSession:
    def __init__(self, responses):
        self.responses = list(responses)  # Liste von FakeResponse oder Exception-Instanzen
        self.calls = []

    def get(self, url, headers=None, timeout=None):
        self.calls.append({"url": url, "headers": headers, "timeout": timeout})
        item = self.responses.pop(0)
        if isinstance(item, Exception):
            raise item
        return item


def make_cfg(**overrides):
    return lt.Config(
        api_url="https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get",
        referer=lt.PAGE_URL,
        mail_from="Lenovo@aomme.de",
        mail_to=["Lenovo@aomme.de"],
        **overrides,
    )


GOOD_PAYLOAD = {"code": 0, "data": [{"id": "01YT022", "commodityKey": "system boards", "stock": 2}]}


class TestFetch(unittest.TestCase):
    def test_fetch_ok(self):
        cfg = make_cfg()
        session = FakeSession([FakeResponse(200, GOOD_PAYLOAD)])
        parts = lt.fetch_clearance_parts(cfg, session=session)
        self.assertEqual(len(parts), 1)
        self.assertEqual(session.calls[0]["headers"]["Accept"], "application/json, text/plain, */*")
        self.assertEqual(session.calls[0]["headers"]["Referer"], lt.PAGE_URL)

    def test_fetch_403_no_retry(self):
        cfg = make_cfg(retries=3)
        session = FakeSession([FakeResponse(403)])
        with self.assertRaises(lt.FetchError) as ctx:
            lt.fetch_clearance_parts(cfg, session=session, sleep=lambda s: None)
        self.assertEqual(ctx.exception.status, 403)
        self.assertEqual(len(session.calls), 1)  # kein Retry bei Bot-Block

    def test_fetch_retries_then_raises(self):
        cfg = make_cfg(retries=2)
        sleeps = []
        session = FakeSession([OSError("net down"), OSError("net down"), OSError("net down")])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session, sleep=sleeps.append)
        self.assertEqual(len(session.calls), 3)
        self.assertEqual(sleeps, [5, 15])  # BACKOFFS

    def test_fetch_rejects_bad_code(self):
        cfg = make_cfg(retries=0)
        session = FakeSession([FakeResponse(200, {"code": 104, "msg": "err"})])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session)

    def test_fetch_rejects_empty_data(self):
        cfg = make_cfg(retries=0)
        session = FakeSession([FakeResponse(200, {"code": 0, "data": []})])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session)

    def test_fetch_rejects_invalid_json(self):
        cfg = make_cfg(retries=0)
        session = FakeSession([FakeResponse(200, None, raw=b"<html>blocked</html>")])
        with self.assertRaises(lt.FetchError):
            lt.fetch_clearance_parts(cfg, session=session)
```

Hinweis: Das vorläufige `make_cfg` oben ist absichtlich simpel — Task 6 ersetzt es durch die endgültige Variante mit State-Verzeichnis.

- [ ] **Step 2: Test laufen lassen — FAIL**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: `AttributeError: module 'lenovo_tracker' has no attribute 'FetchError'` (neue Klasse fehlt).

- [ ] **Step 3: Fetch-Implementierung ergänzen**

```python
class FetchError(Exception):
    """API-Abruf fehlgeschlagen (inkl. 403 Bot-Block)."""

    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


def _fetch_once(cfg, session=None):
    headers = {
        "Accept": "application/json, text/plain, */*",
        "Accept-Language": "de-DE,de;q=0.9,en;q=0.8",
        "Referer": cfg.referer,
    }
    try:
        if session is None:
            if cc_requests is None:
                raise FetchError("curl_cffi ist nicht installiert (pip install curl-cffi)")
            resp = cc_requests.get(cfg.api_url, headers=headers, timeout=cfg.timeout_seconds, impersonate="chrome")
        else:
            resp = session.get(cfg.api_url, headers=headers, timeout=cfg.timeout_seconds)
    except FetchError:
        raise
    except Exception as exc:  # Netzwerk-/TLS-/HTTP-Fehler → FetchError (Retry-fähig)
        raise FetchError(f"Abruf fehlgeschlagen: {type(exc).__name__}: {exc}")
    if resp.status_code == 403:
        raise FetchError("HTTP 403 – Akamai blockiert den Abruf (Fallback: Headless-Chromium, siehe README)", status=403)
    if resp.status_code >= 400:
        raise FetchError(f"HTTP {resp.status_code} von der Lenovo-API")
    try:
        payload = resp.json()
    except Exception as exc:
        raise FetchError(f"Antwort ist kein gültiges JSON: {exc}")
    if not isinstance(payload, dict) or payload.get("code") != 0:
        raise FetchError(f"API-Fehlercode: {repr(payload)[:200]}")
    data = payload.get("data")
    if not isinstance(data, list) or not data:
        raise FetchError("API lieferte keine/leere Datenliste – Abbruch, State bleibt unangetastet")
    return data


def fetch_clearance_parts(cfg, session=None, sleep=time.sleep):
    attempts = max(1, int(cfg.retries) + 1)
    last_error = None
    for attempt in range(attempts):
        if attempt:
            sleep(BACKOFFS[min(attempt - 1, len(BACKOFFS) - 1)])
        try:
            return _fetch_once(cfg, session=session)
        except FetchError as exc:
            if exc.status == 403:
                raise  # Bot-Block: sofort aufgeben
            last_error = exc
            log.warning("API-Abruf fehlgeschlagen (Versuch %d/%d): %s", attempt + 1, attempts, exc)
    raise last_error
```

- [ ] **Step 4: Tests laufen lassen — PASS (alle)**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: 12 Tests PASS.

- [ ] **Step 5: Commit**

```bash
git add lenovo_tracker.py tests/test_tracker.py
git commit -m "feat: API-Client mit Retry-Backoff und 403-Kurzschluss"
```

---

### Task 3: Filter, Summe, Diff (`_stock_of`, `filter_mainboards`, `compute_totals`, `parts_to_map`, `build_diff`) + Fixture

**Files:**
- Create: `tests/fixture_clearance.json`
- Modify: `lenovo_tracker.py` (Abschnitt „Filter & Diff")
- Modify: `tests/test_tracker.py`

**Interfaces:**
- Produces: `_stock_of(part: dict) -> int`; `filter_mainboards(parts: list[dict], commodity_keys: list[str]) -> list[dict]` (lowercase/strip-Vergleich); `compute_totals(mainboards: list[dict]) -> tuple[int, int]` (Summe, Anzahl); `parts_to_map(mainboards: list[dict]) -> dict[str, dict]` (Format `{"<id>": {"s": int, "n": str}}`); `@dataclass Diff` mit `old_total`, `new_total`, `increased: list[dict]` (`{id,name,old,new,delta}`, sortiert delta-desc), `new_parts: list[dict]` (`{id,name,stock}`) und Property `delta`; `build_diff(old_map: dict, new_map: dict) -> Diff`.

- [ ] **Step 1: Fixture anlegen (`tests/fixture_clearance.json`)**

```json
{
  "code": 0,
  "data": [
    {"id": "01YT022", "name": "i3-7100U System Board", "commodityKey": "system boards", "commodityVal": "SYSTEM BOARDS", "stock": 2, "promotionPrice": 59.89, "partSaleType": "Local_Promotion", "products": ["20J1"]},
    {"id": "00FC857", "name": "W7 WB R2 PCH <Scorpius> & Co", "commodityKey": "system boards", "stock": 1},
    {"id": "5A10W86303", "name": "Mainboard Test Entry", "commodityKey": "system boards", "stock": 97},
    {"id": "01HW999", "name": "Heat Sink XL", "commodityKey": "heat sinks", "stock": 500},
    {"id": "02KM001", "name": "Keyboard DE", "commodityKey": "keyboards internal", "stock": 10},
    {"id": "03NOSTOCK", "name": "Board ohne Stock-Feld", "commodityKey": "system boards"}
  ]
}
```
(Mainboard-Summe der Fixture: 2 + 1 + 97 + 0 = **100** über **4** Teile.)

- [ ] **Step 2: Failing tests ergänzen**

```python
import copy

FIXTURE = json.loads((REPO / "tests" / "fixture_clearance.json").read_text(encoding="utf-8"))


class TestFilterDiff(unittest.TestCase):
    def test_filter_mainboards_case_insensitive(self):
        boards = lt.filter_mainboards(FIXTURE["data"], ["system boards"])
        self.assertEqual(len(boards), 4)
        self.assertEqual({b["id"] for b in boards}, {"01YT022", "00FC857", "5A10W86303", "03NOSTOCK"})

    def test_compute_totals(self):
        boards = lt.filter_mainboards(FIXTURE["data"], ["system boards"])
        self.assertEqual(lt.compute_totals(boards), (100, 4))

    def test_stock_of_handles_missing_and_garbage(self):
        self.assertEqual(lt._stock_of({}), 0)
        self.assertEqual(lt._stock_of({"stock": None}), 0)
        self.assertEqual(lt._stock_of({"stock": "7"}), 7)
        self.assertEqual(lt._stock_of({"stock": -5}), 0)
        self.assertEqual(lt._stock_of({"stock": "kaputt"}), 0)

    def test_build_diff_increase_sorted(self):
        old_map = {"A": {"s": 5, "n": "A"}, "B": {"s": 90, "n": "B"}}
        new_map = {"A": {"s": 25, "n": "A"}, "B": {"s": 95, "n": "B"}, "C": {"s": 3, "n": "C"}}
        diff = lt.build_diff(old_map, new_map)
        self.assertEqual(diff.delta, 28)  # 123 - 95
        self.assertEqual([d["id"] for d in diff.increased], ["A", "B"])  # delta 20 vor 5
        self.assertEqual(diff.increased[0]["delta"], 20)
        self.assertEqual([p["id"] for p in diff.new_parts], ["C"])

    def test_build_diff_decrease_and_equal(self):
        old_map = {"A": {"s": 5, "n": "A"}}
        self.assertEqual(lt.build_diff(old_map, {"A": {"s": 2, "n": "A"}}).delta, -3)
        self.assertEqual(lt.build_diff(old_map, {"A": {"s": 5, "n": "A"}}).delta, 0)
        self.assertEqual(lt.build_diff(old_map, {"A": {"s": 2, "n": "A"}}).increased, [])
```

- [ ] **Step 3: Test laufen lassen — FAIL**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: `AttributeError` (`filter_mainboards` fehlt).

- [ ] **Step 4: Implementierung ergänzen**

```python
def _stock_of(part):
    try:
        return max(0, int(part.get("stock") or 0))
    except (TypeError, ValueError):
        return 0


def filter_mainboards(parts, commodity_keys):
    keys = {str(k).strip().lower() for k in commodity_keys}
    return [p for p in parts if str(p.get("commodityKey", "")).strip().lower() in keys]


def compute_totals(mainboards):
    return sum(_stock_of(p) for p in mainboards), len(mainboards)


def parts_to_map(mainboards):
    return {
        str(p["id"]): {"s": _stock_of(p), "n": str(p.get("name") or "")}
        for p in mainboards
        if p.get("id")
    }


@dataclass
class Diff:
    old_total: int
    new_total: int
    increased: list
    new_parts: list

    @property
    def delta(self):
        return self.new_total - self.old_total


def build_diff(old_map, new_map):
    old_total = sum(v["s"] for v in old_map.values())
    new_total = sum(v["s"] for v in new_map.values())
    increased, new_parts = [], []
    for pid, nv in new_map.items():
        ov = old_map.get(pid)
        if ov is None:
            new_parts.append({"id": pid, "name": nv["n"], "stock": nv["s"]})
        elif nv["s"] > ov["s"]:
            increased.append({"id": pid, "name": nv["n"], "old": ov["s"], "new": nv["s"], "delta": nv["s"] - ov["s"]})
    increased.sort(key=lambda item: (-item["delta"], item["id"]))
    new_parts.sort(key=lambda item: (-item["stock"], item["id"]))
    return Diff(old_total, new_total, increased, new_parts)
```

- [ ] **Step 5: Tests laufen lassen — PASS (alle)**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: 17 Tests PASS.

- [ ] **Step 6: Commit**

```bash
git add lenovo_tracker.py tests/
git commit -m "feat: Mainboard-Filter, Bestandssumme und Diff"
```

---

### Task 4: State & Verlauf (`load_state`, `save_state`, `append_history`)

**Files:**
- Modify: `lenovo_tracker.py` (Abschnitt „State")
- Modify: `tests/test_tracker.py`

**Interfaces:**
- Produces: `load_state(state_path: Path) -> dict | None` (None bei fehlender/kaputter Datei); `save_state(state_path: Path, ts: str, total: int, count: int, part_map: dict) -> None` (atomar via `.tmp` + `os.replace`); `append_history(history_path: Path, ts: str, total: int, count: int) -> None` (CSV-Header nur bei Neuanlage).

- [ ] **Step 1: Failing tests ergänzen**

```python
class TestState(unittest.TestCase):
    def test_load_state_missing_returns_none(self):
        with tempfile.TemporaryDirectory() as td:
            self.assertIsNone(lt.load_state(Path(td) / "state.json"))

    def test_save_and_load_roundtrip(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "state.json"
            lt.save_state(path, "2026-11-18T20:00:00+00:00", 100, 3, {"A": {"s": 5, "n": "A"}})
            state = lt.load_state(path)
            self.assertEqual(state["total_stock"], 100)
            self.assertEqual(state["part_count"], 3)
            self.assertEqual(state["parts"]["A"]["s"], 5)

    def test_load_state_corrupt_returns_none(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "state.json"
            path.write_text("{kaputt", encoding="utf-8")
            self.assertIsNone(lt.load_state(path))

    def test_append_history_writes_header_once(self):
        with tempfile.TemporaryDirectory() as td:
            path = Path(td) / "history.csv"
            lt.append_history(path, "2026-11-18T20:00:00+00:00", 100, 3)
            lt.append_history(path, "2026-11-18T20:30:00+00:00", 101, 3)
            lines = path.read_text(encoding="utf-8").strip().splitlines()
            self.assertEqual(lines[0], "timestamp,total_stock,part_count")
            self.assertEqual(lines[1], "2026-11-18T20:00:00+00:00,100,3")
            self.assertEqual(lines[2], "2026-11-18T20:30:00+00:00,101,3")
```

- [ ] **Step 2: Test laufen lassen — FAIL**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: `AttributeError` (`load_state` fehlt).

- [ ] **Step 3: Implementierung ergänzen (benötigt `import os` im Header)**

```python
import os  # oben im Import-Block ergänzen


def load_state(state_path):
    try:
        return json.loads(Path(state_path).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except json.JSONDecodeError as exc:
        log.error("state.json ist beschädigt (%s) – Baseline wird neu gesetzt", exc)
        return None


def save_state(state_path, ts, total, count, part_map):
    payload = {
        "last_check": ts,
        "total_stock": total,
        "part_count": count,
        "parts": part_map,
    }
    tmp = Path(str(state_path) + ".tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
    os.replace(tmp, state_path)


def append_history(history_path, ts, total, count):
    history_path = Path(history_path)
    history_path.parent.mkdir(parents=True, exist_ok=True)
    is_new = not history_path.exists()
    with open(history_path, "a", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        if is_new:
            writer.writerow(["timestamp", "total_stock", "part_count"])
        writer.writerow([ts, total, count])
```

- [ ] **Step 4: Tests laufen lassen — PASS (alle)**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: 21 Tests PASS.

- [ ] **Step 5: Commit**

```bash
git add lenovo_tracker.py tests/test_tracker.py
git commit -m "feat: State-Verwaltung mit atomarem Schreiben und CSV-Verlauf"
```

---

### Task 5: Mail (`fmt_de`, `build_subject`, `build_mail_text`, `build_mail_html`, `compose_mail`, `deliver_mail`, `send_test_mail`)

**Files:**
- Modify: `lenovo_tracker.py` (Abschnitt „Mail")
- Modify: `tests/test_tracker.py`

**Interfaces:**
- Consumes: `Diff` (Task 3), `Config` (Task 1), `fetch_clearance_parts`/`filter_mainboards`/`compute_totals` (Task 2/3).
- Produces: `fmt_de(n: int) -> str` (deutsches Tausenderformat); `build_subject(cfg: Config, diff: Diff) -> str`; `build_mail_text(diff: Diff, page_url: str, checked_at: str) -> str`; `build_mail_html(diff: Diff, page_url: str, checked_at: str) -> str` (Teilnamen via `html.escape`); `compose_mail(cfg, subject, text, html) -> EmailMessage`; `deliver_mail(cfg, msg) -> None` (sendmail `-t -oi` oder SMTP); `send_test_mail(cfg, deliver=deliver_mail) -> bool`. Kein weiterer Hilfer (YAGNI).

- [ ] **Step 1: Failing tests ergänzen**

```python
DIFF = lt.build_diff(
    {"01YT022": {"s": 2, "n": "i3-7100U System Board"},
     "5A10W86303": {"s": 97, "n": "Mainboard <Top> & Board"}},
    {"01YT022": {"s": 30, "n": "i3-7100U System Board"},
     "5A10W86303": {"s": 112, "n": "Mainboard <Top> & Board"},
     "NEU0001": {"s": 5, "n": "Neues Board"}},
)  # alt 99 → neu 147, delta 48; increased: 01YT022 (+28), 5A10W86303 (+15); new_parts: NEU0001


class TestMail(unittest.TestCase):
    def setUp(self):
        self.cfg = make_cfg()

    def test_fmt_de(self):
        self.assertEqual(lt.fmt_de(16489), "16.489")
        self.assertEqual(lt.fmt_de(999), "999")

    def test_subject_contains_delta(self):
        subject = lt.build_subject(self.cfg, DIFF)
        self.assertTrue(subject.startswith("[Lenovo Tracker]"))
        self.assertIn("99 → 147 (+48)", subject)

    def test_mail_text_lists_top_increase(self):
        text = lt.build_mail_text(DIFF, lt.PAGE_URL, "2026-11-18T20:00:00+00:00")
        self.assertIn("+28", text)
        self.assertIn("01YT022", text)
        self.assertIn("Neu im Sale: 1", text)

    def test_mail_html_escapes_part_names(self):
        html = lt.build_mail_html(DIFF, lt.PAGE_URL, "2026-11-18T20:00:00+00:00")
        self.assertIn("Mainboard &lt;Top&gt; &amp; Board", html)  # Teil ist in der increased-Liste
        self.assertNotIn("<Top>", html)

    def test_compose_mail_headers_and_bodies(self):
        msg = lt.compose_mail(
            self.cfg, "Betreff", "TEXT", "<html>HTML</html>"
        )
        self.assertEqual(msg["From"], "Lenovo@aomme.de")
        self.assertEqual(msg["To"], "Lenovo@aomme.de")
        self.assertEqual(msg["Subject"], "Betreff")
        raw = msg.as_bytes().decode("utf-8")
        self.assertIn("TEXT", raw)
        self.assertIn("<html>HTML</html>", raw)

    def test_compose_mail_with_extra_recipients(self):
        cfg = make_cfg(extra_recipients=["chef@example.com"])
        msg = lt.compose_mail(cfg, "B", "T", "<p>H</p>")
        self.assertEqual(msg["To"], "Lenovo@aomme.de, chef@example.com")
```

- [ ] **Step 2: Test laufen lassen — FAIL**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: `AttributeError` (`fmt_de` fehlt).

- [ ] **Step 3: Mail-Implementierung ergänzen**

```python
def fmt_de(number):
    return f"{number:,}".replace(",", ".")


def build_subject(cfg, diff):
    return (
        f"{cfg.subject_prefix} Mainboard-Bestand gestiegen: "
        f"{fmt_de(diff.old_total)} → {fmt_de(diff.new_total)} (+{fmt_de(diff.delta)})"
    )


def build_mail_text(diff, page_url, checked_at):
    lines = [
        "Mainboard-Bestand gestiegen!",
        "",
        f"Vorher: {fmt_de(diff.old_total)} Stück",
        f"Jetzt:  {fmt_de(diff.new_total)} Stück",
        f"Änderung: +{fmt_de(diff.delta)}",
        "",
    ]
    if diff.increased:
        lines.append(f"Top-Neuzugänge ({len(diff.increased)} Teile mit mehr Bestand):")
        for item in diff.increased[:20]:
            lines.append(f"  +{item['delta']}  {item['id']}  {item['name']}  ({item['old']} → {item['new']})")
        rest = diff.increased[20:]
        if rest:
            lines.append(f"  (+ {len(rest)} weitere Teile: +{fmt_de(sum(i['delta'] for i in rest))})")
        lines.append("")
    if diff.new_parts:
        lines.append(f"Neu im Sale: {len(diff.new_parts)} Teile")
        for item in diff.new_parts[:20]:
            lines.append(f"  {item['id']}  {item['name']}  ({item['stock']} Stück)")
        lines.append("")
    lines += ["Seite: " + page_url, "Geprüft: " + checked_at]
    return "\n".join(lines)


def build_mail_html(diff, page_url, checked_at):
    rows = []
    for item in diff.increased[:20]:
        rows.append(
            f"<tr><td>+{item['delta']}</td><td>{escape(item['id'])}</td>"
            f"<td>{escape(item['name'])}</td><td>{item['old']} → {item['new']}</td></tr>"
        )
    new_rows = [
        f"<tr><td>{escape(item['id'])}</td><td>{escape(item['name'])}</td><td>{item['stock']} Stück</td></tr>"
        for item in diff.new_parts[:20]
    ]
    parts = [
        "<html><body style='font-family:sans-serif'>",
        "<h2>Mainboard-Bestand gestiegen!</h2>",
        "<p><b>"
        f"{fmt_de(diff.old_total)} → {fmt_de(diff.new_total)} Stück "
        f"(+{fmt_de(diff.delta)})</b></p>",
    ]
    if rows:
        parts += [
            "<h3>Top-Neuzugänge</h3>",
            "<table border='1' cellpadding='4' cellspacing='0'>",
            "<tr><th>Δ</th><th>Teilenummer</th><th>Bezeichnung</th><th>Bestand</th></tr>",
            *rows,
            "</table>",
        ]
    if new_rows:
        parts += [
            f"<h3>Neu im Sale ({len(diff.new_parts)} Teile)</h3>",
            "<table border='1' cellpadding='4' cellspacing='0'>",
            "<tr><th>Teilenummer</th><th>Bezeichnung</th><th>Bestand</th></tr>",
            *new_rows,
            "</table>",
        ]
    parts += [
        f"<p><a href='{escape(page_url)}'>{escape(page_url)}</a><br>Geprüft: {escape(checked_at)}</p>",
        "</body></html>",
    ]
    return "\n".join(parts)


def compose_mail(cfg, subject, text, html):
    msg = EmailMessage()
    msg["From"] = cfg.mail_from
    msg["To"] = ", ".join(list(cfg.mail_to) + list(cfg.extra_recipients))
    msg["Subject"] = subject
    msg["Date"] = formatdate(localtime=True)
    msg["X-Mailer"] = "lenovo-tracker"
    msg.set_content(text)
    msg.add_alternative(html, subtype="html")
    return msg


def deliver_mail(cfg, msg):
    if cfg.mail_mode == "sendmail":
        proc = subprocess.run(
            [cfg.sendmail_path, "-t", "-oi"],
            input=msg.as_bytes(),
            capture_output=True,
        )
        if proc.returncode != 0:
            stderr = proc.stderr.decode(errors="replace")[:300]
            raise RuntimeError(f"sendmail fehlgeschlagen (rc={proc.returncode}): {stderr}")
        log.info("Mail an sendmail übergeben: %s", msg["To"])
    else:
        with smtplib.SMTP(cfg.smtp_host, cfg.smtp_port, timeout=60) as smtp:
            if cfg.smtp_starttls:
                smtp.starttls()
            if cfg.smtp_user:
                smtp.login(cfg.smtp_user, cfg.smtp_password)
            smtp.send_message(msg)
        log.info("Mail via SMTP %s gesendet: %s", cfg.smtp_host, msg["To"])


def send_test_mail(cfg, deliver=deliver_mail):
    try:
        parts = fetch_clearance_parts(cfg)
        boards = filter_mainboards(parts, cfg.commodity_keys)
        total, count = compute_totals(boards)
    except FetchError as exc:
        log.error("Testmail ohne Datenabruf nicht möglich: %s", exc)
        return False
    ts = datetime.now(timezone.utc).isoformat(timespec="seconds")
    subject = f"{cfg.subject_prefix} Testmail – Tracker aktiv"
    text = (
        "Testmail des Lenovo-Mainboard-Trackers.\n\n"
        f"Aktueller Mainboard-Bestand: {fmt_de(total)} Stück über {count} Teile.\n"
        f"Versandweg: {cfg.mail_mode}\nGeprüft: {ts}\nSeite: {PAGE_URL}\n"
    )
    html = (
        "<html><body><h2>Testmail – Tracker aktiv</h2>"
        f"<p>Aktueller Mainboard-Bestand: <b>{fmt_de(total)} Stück</b> über {count} Teile.</p>"
        f"<p>Versandweg: {cfg.mail_mode}<br>Geprüft: {escape(ts)}<br>"
        f"<a href='{PAGE_URL}'>{PAGE_URL}</a></p></body></html>"
    )
    deliver(cfg, compose_mail(cfg, subject, text, html))
    return True
```

- [ ] **Step 4: Tests laufen lassen — PASS (alle)**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: 27 Tests PASS.

- [ ] **Step 5: Commit**

```bash
git add lenovo_tracker.py tests/test_tracker.py
git commit -m "feat: Mail-Erzeugung (HTML+Text) und Versand via sendmail/SMTP"
```

---

### Task 6: `run_once`, CLI (`--once`/`--dry-run`/`--test-mail`), Dauer-Loop

**Files:**
- Modify: `lenovo_tracker.py` (Abschnitte „Ablauf", „CLI")
- Modify: `tests/test_tracker.py` (E2E-Tests, `make_cfg` finalisieren)

**Interfaces:**
- Consumes: alle bisherigen Funktionen.
- Produces: `@dataclass CheckResult` mit `total: int`, `count: int`, `sent_mail: bool`, `baseline: bool`, `error: str | None`; `run_once(cfg: Config, session=None, sleep=time.sleep, deliver=deliver_mail, dry_run=False) -> CheckResult`; `run_loop(cfg) -> None`; `main(argv: list[str] | None) -> int`; `make_cfg(state_dir, **overrides) -> Config` (Test-Helfer, ersetzt die Task-2-Variante).

- [ ] **Step 1: `make_cfg` in Tests finalisieren und Failing E2E-Tests ergänzen**

Das Task-2-`make_cfg` durch diese Version ersetzen (bestehende Aufrufe bleiben kompatibel):

```python
def make_cfg(state_dir=None, **overrides):
    if state_dir is None:
        state_dir = tempfile.mkdtemp()
    defaults = dict(state_dir=state_dir)
    defaults.update(overrides)
    return lt.Config(
        api_url="https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get",
        referer=lt.PAGE_URL,
        mail_from="Lenovo@aomme.de",
        mail_to=["Lenovo@aomme.de"],
        **defaults,
    )


class CapturingDeliver:
    def __init__(self):
        self.messages = []

    def __call__(self, cfg, msg):
        self.messages.append(msg)


def fixture_payload(mutate=None):
    payload = copy.deepcopy(FIXTURE)
    if mutate:
        mutate(payload["data"])
    return payload


class TestRunOnce(unittest.TestCase):
    def test_run_once_baseline_no_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        session = FakeSession([FakeResponse(200, fixture_payload())])
        result = lt.run_once(cfg, session=session, deliver=deliver)
        self.assertTrue(result.baseline)
        self.assertFalse(result.sent_mail)
        self.assertEqual(result.total, 100)
        self.assertEqual(deliver.messages, [])
        state = lt.load_state(Path(cfg.state_dir) / "state.json")
        self.assertEqual(state["total_stock"], 100)

    def test_run_once_increase_sends_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        # Lauf 1: Baseline
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        # Lauf 2: Bestand steigt +15, neues Teil +3 → +18 gesamt
        def bump(data):
            for part in data:
                if part["id"] == "5A10W86303":
                    part["stock"] = 112
            data.append({"id": "NEU0001", "name": "Neues Board", "commodityKey": "system boards", "stock": 3})
        result = lt.run_once(
            cfg,
            session=FakeSession([FakeResponse(200, fixture_payload(bump))]),
            deliver=deliver,
        )
        self.assertTrue(result.sent_mail)
        self.assertEqual(result.total, 118)
        self.assertEqual(len(deliver.messages), 1)
        subject = deliver.messages[0]["Subject"]
        self.assertIn("100 → 118 (+18)", subject)

    def test_run_once_decrease_no_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        def drop(data):
            for part in data:
                if part["id"] == "5A10W86303":
                    part["stock"] = 10
        result = lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload(drop))]), deliver=deliver)
        self.assertFalse(result.sent_mail)
        self.assertEqual(result.total, 13)  # 2 + 1 + 10 + 0 (Teil ohne stock-Feld)
        self.assertEqual(deliver.messages, [])

    def test_run_once_fetch_error_keeps_state(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        before = lt.load_state(Path(cfg.state_dir) / "state.json")
        result = lt.run_once(cfg, session=FakeSession([OSError("boom")]), deliver=deliver)
        self.assertIsNotNone(result.error)
        self.assertEqual(lt.load_state(Path(cfg.state_dir) / "state.json"), before)
        self.assertEqual(deliver.messages, [])

    def test_run_once_no_mainboards_zero_sum_no_mail(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
        def no_boards(data):
            data[:] = [p for p in data if p.get("commodityKey") != "system boards"]
        result = lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload(no_boards))]), deliver=deliver)
        self.assertEqual(result.total, 0)
        self.assertFalse(result.sent_mail)
        self.assertEqual(deliver.messages, [])
        self.assertEqual(lt.load_state(Path(cfg.state_dir) / "state.json")["total_stock"], 0)

    def test_run_once_dry_run_touches_nothing(self):
        cfg = make_cfg()
        deliver = CapturingDeliver()
        result = lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver, dry_run=True)
        self.assertEqual(result.total, 100)
        self.assertFalse((Path(cfg.state_dir) / "state.json").exists())
        self.assertFalse((Path(cfg.state_dir) / "history.csv").exists())
```

- [ ] **Step 2: Test laufen lassen — FAIL**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: `AttributeError: ... has no attribute 'run_once'`.

- [ ] **Step 3: `run_once`, `run_loop`, `main` implementieren**

```python
@dataclass
class CheckResult:
    total: int = 0
    count: int = 0
    sent_mail: bool = False
    baseline: bool = False
    error: str = None


def _state_paths(cfg):
    state_dir = Path(cfg.state_dir)
    return state_dir / "state.json", state_dir / "history.csv"


def run_once(cfg, session=None, sleep=time.sleep, deliver=deliver_mail, dry_run=False):
    ts = datetime.now(timezone.utc).isoformat(timespec="seconds")
    state_path, history_path = _state_paths(cfg)
    try:
        parts = fetch_clearance_parts(cfg, session=session, sleep=sleep)
    except FetchError as exc:
        log.error("Abruf fehlgeschlagen – kein State-Update: %s", exc)
        return CheckResult(error=str(exc))
    boards = filter_mainboards(parts, cfg.commodity_keys)
    if not boards:
        log.warning("Keine Mainboards in der API-Response (Filter: %s)", cfg.commodity_keys)
    total, count = compute_totals(boards)
    if dry_run:
        log.info("DRY-RUN: %d Mainboard-Teile, Summe %d Stück", count, total)
        return CheckResult(total=total, count=count)
    part_map = parts_to_map(boards)
    old_state = load_state(state_path)
    save_state(state_path, ts, total, count, part_map)
    append_history(history_path, ts, total, count)
    if old_state is None:
        log.info("Baseline gesetzt: %d Stück über %d Mainboard-Teile", total, count)
        return CheckResult(total=total, count=count, baseline=True)
    diff = build_diff(old_state.get("parts", {}), part_map)
    if diff.delta > 0:
        subject = build_subject(cfg, diff)
        text = build_mail_text(diff, PAGE_URL, ts)
        html = build_mail_html(diff, PAGE_URL, ts)
        deliver(cfg, compose_mail(cfg, subject, text, html))
        log.info("Mail gesendet: %s", subject)
        return CheckResult(total=total, count=count, sent_mail=True)
    log.info("Kein Anstieg (vorher %d → jetzt %d)", diff.old_total, diff.new_total)
    return CheckResult(total=total, count=count)


def run_loop(cfg):
    interval = max(1, cfg.interval_minutes) * 60
    while True:
        started = time.monotonic()
        try:
            run_once(cfg)
        except Exception:
            log.exception("Unerwarteter Fehler im Zyklus")
        elapsed = time.monotonic() - started
        sleep_s = max(60, interval - elapsed)
        log.debug("Nächster Lauf in %.0f Sekunden", sleep_s)
        time.sleep(sleep_s)


def main(argv=None):
    parser = argparse.ArgumentParser(description="Lenovo Clearance-Sale Mainboard-Tracker")
    parser.add_argument("--config", default=DEFAULT_CONFIG_PATH, help="Pfad zur config.toml")
    parser.add_argument("--once", action="store_true", help="einmal prüfen und beenden")
    parser.add_argument("--dry-run", action="store_true", help="einmal abrufen und zusammenfassen (kein State, keine Mail)")
    parser.add_argument("--test-mail", action="store_true", help="Testmail mit aktuellem Bestand senden")
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args(argv)
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
        stream=sys.stdout,
    )
    try:
        cfg = load_config(args.config)
    except ConfigError as exc:
        log.error("%s", exc)
        return 1
    if args.dry_run:
        run_once(cfg, dry_run=True)
        return 0
    if args.test_mail:
        return 0 if send_test_mail(cfg) else 2
    if args.once:
        result = run_once(cfg)
        return 2 if result.error else 0
    run_loop(cfg)
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Tests laufen lassen — PASS (alle)**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: 33 Tests PASS.

- [ ] **Step 5: Smoke-Test der CLI auf Dev-Maschine (Live-Abruf, kein State)**

```bash
python -c "
import json, tempfile, sys
sys.argv = ['x']
import lenovo_tracker as lt
cfg = lt.Config(api_url='https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get', mail_from='Lenovo@aomme.de', mail_to=['Lenovo@aomme.de'], state_dir=tempfile.mkdtemp())
import logging; logging.basicConfig(level=logging.INFO)
r = lt.run_once(cfg, dry_run=True)
print('Mainboards:', r.count, '| Summe:', r.total)
"
```
Erwartet: `Mainboards: 3128 | Summe: 16489` (oder ähnlich aktuell, >3000 Teile).

- [ ] **Step 6: Commit**

```bash
git add lenovo_tracker.py tests/test_tracker.py
git commit -m "feat: run_once-Pipeline, CLI (--once/--dry-run/--test-mail) und Dauer-Loop"
```

---

### Task 7: Deployment-Dateien (`install.sh`, `lenovo-tracker.service`, `README.md`)

**Files:**
- Create: `install.sh`
- Create: `lenovo-tracker.service`
- Create: `README.md`

**Interfaces:**
- Consumes: fertiges `lenovo_tracker.py` (Task 6); `config.example.toml` (Task 1).
- Produces: lauffähige Installation im LXC (Service aktiv, Config unter `/etc/lenovotracker/config.toml`, State unter `/var/lib/lenovotracker`).

- [ ] **Step 1: `lenovo-tracker.service` anlegen**

```ini
[Unit]
Description=Lenovo Clearance-Sale Mainboard-Tracker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/opt/lenovo-tracker/venv/bin/python /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml
Restart=on-failure
RestartSec=60
Environment=PYTHONUNBUFFERED=1

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: `install.sh` anlegen**

```bash
#!/usr/bin/env bash
# Lenovo Clearance-Sale Mainboard-Tracker – Installation im LXC (Debian 12, als root).
# Usage: ./install.sh
set -euo pipefail

APP_DIR="/opt/lenovo-tracker"
CONF_DIR="/etc/lenovotracker"
STATE_DIR="/var/lib/lenovotracker"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "== Pakete installieren =="
echo "postfix postfix/main_mailer_type string 'Internet Site'" | debconf-set-selections
echo "postfix postfix/mailname string $(hostname --fqdn)" | debconf-set-selections
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  python3 python3-venv postfix ca-certificates

echo "== Dateien installieren =="
mkdir -p "$APP_DIR" "$CONF_DIR" "$STATE_DIR"
install -m 0755 "$SCRIPT_DIR/lenovo_tracker.py" "$APP_DIR/lenovo_tracker.py"
install -m 0644 "$SCRIPT_DIR/config.example.toml" "$CONF_DIR/config.toml.new"
if [ ! -f "$CONF_DIR/config.toml" ]; then
  cp "$CONF_DIR/config.toml.new" "$CONF_DIR/config.toml"
  echo "Neue Config angelegt: $CONF_DIR/config.toml (bitte prüfen/anpassen)"
else
  echo "Bestehende Config bleibt erhalten: $CONF_DIR/config.toml (Referenz: config.toml.new)"
fi

echo "== Python-Umgebung =="
python3 -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --upgrade pip >/dev/null
"$APP_DIR/venv/bin/pip" install "curl-cffi>=0.15"

echo "== systemd-Service =="
install -m 0644 "$SCRIPT_DIR/lenovo-tracker.service" /etc/systemd/system/lenovo-tracker.service
systemctl daemon-reload
systemctl enable --now lenovo-tracker.service

echo
echo "== Fertig =="
echo "  Config:      $CONF_DIR/config.toml"
echo "  Testmail:    $APP_DIR/venv/bin/python $APP_DIR/lenovo_tracker.py --config $CONF_DIR/config.toml --test-mail"
echo "  Einmal-Lauf: $APP_DIR/venv/bin/python $APP_DIR/lenovo_tracker.py --config $CONF_DIR/config.toml --once"
echo "  Logs:        journalctl -u lenovo-tracker -f"
echo "  Postfix:     default 'Internet Site' liefert direkt per DNS/MX an aomme.de (wie Proxmox)."
echo "               Falls Port-25-Ausgang blockiert ist: relayhost in /etc/postfix/main.cf setzen (siehe README)."
```

- [ ] **Step 3: `README.md` anlegen**

```markdown
# Lenovo Clearance-Sale Mainboard-Tracker

Überwacht die öffentliche [Lenovo Clearance-Sale-Seite](https://support.lenovo.com/de/de/parts-lookup/clearance-sale)
und sendet eine E-Mail an **Lenovo@aomme.de** (von **Lenovo@aomme.de**), sobald die
Gesamtsumme aller verfügbaren Mainboards (Commodity „System Boards") steigt.

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
4. mailt bei Anstieg — **nur** dann.

Lenovos Akamai-Schutz blockt normale HTTP-Clients (curl → HTTP 403). Der Tracker
nutzt deshalb [`curl_cffi`](https://pypi.org/project/curl-cffi/) mit Chrome-TLS-Imitation
(`impersonate="chrome"`) — verifiziert funktionierend (Stand 2026-11-18: 9.261 Teile,
3.128 Mainboards, 16.489 Stück Bestand).

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

Dieses Repo auf den Container bringen (z. B. `pct push 200 ...` oder git clone), dann:

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

Standard ist **sendmail-Modus**: Der Container postfix („Internet Site") stellt
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
... lenovo_tracker.py --config ... --once      # einmal prüfen (Exit 2 bei Abruf-Fehler)
... lenovo_tracker.py --config ... --dry-run   # Zusammenfassung ohne State/Mail
... lenovo_tracker.py --config ... --test-mail # Testmail
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

## Troubleshooting

| Symptom | Ursache / Lösung |
|---|---|
| `HTTP 403 – Akamai blockiert` im Log | Bot-Schutz schlägt `curl_cffi`. Update: `pip install -U curl-cffi` (neuere Chrome-Imitation). Bleibt es: Headless-Chromium-Fallback einplanen (API-Abruf aus echtem Browser-Kontext — im Browser verifiziert identische Daten). |
| Mail kommt nicht an | `mail.log` prüfen (`journalctl -u postfix -f`), `--test-mail`, dann Versandweg oben umstellen. |
| `Keine Mainboards in der API-Response` | Legitim möglich (alles verkauft) — WARNING im Log, keine Mail bei Abstieg. |
| Bestand „springt" stark | Lenovo aktualisiert den Bestand mehrmals täglich; `updateTime` in der API zeigt den letzten Pull. |
```

- [ ] **Step 4: Syntax-Checks**

```bash
bash -n install.sh && echo "install.sh OK"
python -c "import configparser; print('service-ini ok')" && python - <<'EOF'
# Service-Datei grob prüfen: Pflichtzeilen vorhanden
content = open("lenovo-tracker.service", encoding="utf-8").read()
assert "ExecStart=/opt/lenovo-tracker/venv/bin/python" in content
assert "[Install]" in content
print("service OK")
EOF
```

- [ ] **Step 5: Commit**

```bash
git add install.sh lenovo-tracker.service README.md
git commit -m "feat: LXC-Deployment (install.sh, systemd-Service, README)"
```

---

### Task 8: Gesamte Verifikation + finaler Commit

**Files:**
- Modify: keine (nur Verifikation; ggf. Bugfixes aus den Checks)

- [ ] **Step 1: Komplette Testsuite**

```bash
python -m unittest tests.test_tracker -v
```
Erwartet: alle 33 Tests PASS.

- [ ] **Step 2: Live-`--dry-run` auf der Dev-Maschine (echter API-Abruf)**

Temporäre Config (Windows-Pfad!) anlegen und laufen lassen:

```bash
python lenovo_tracker.py --config config.example.toml --dry-run
```
Erwartet (Stand 2026-11-18): `DRY-RUN: 3128 Mainboard-Teile, Summe 16489 Stück` (oder ähnlich aktuell) und **keine** `state.json`-Erzeugung — `--dry-run` berührt das State-Verzeichnis nicht.

- [ ] **Step 3: Synthetischer Mail-Pfad-Check ohne echten Versand**

```bash
python - <<'EOF'
import json, tempfile
import lenovo_tracker as lt
from tests.test_tracker import FakeSession, FakeResponse, fixture_payload, CapturingDeliver, make_cfg
cfg = make_cfg()
deliver = CapturingDeliver()
lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload())]), deliver=deliver)
def bump(data):
    for p in data:
        if p["id"] == "5A10W86303":
            p["stock"] = 112
lt.run_once(cfg, session=FakeSession([FakeResponse(200, fixture_payload(bump))]), deliver=deliver)
msg = deliver.messages[0]
print("Subject:", msg["Subject"])
print(msg.get_body(preferencelist=("plain",)).get_content()[:400])
EOF
```
Erwartet: Betreff `… 100 → 118 (+18)` und lesbarer Plaintext mit Top-Neuzugängen.

- [ ] **Step 4: Finaler Commit (falls Bugfixes nötig waren)**

```bash
git add -A
git commit -m "fix: Nacharbeiten aus der Gesamtverifikation" || echo "nichts zu committen"
git log --oneline
```
