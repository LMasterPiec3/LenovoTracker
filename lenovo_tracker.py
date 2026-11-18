#!/usr/bin/env python3
"""Lenovo Clearance-Sale Mainboard-Tracker.

Überwacht die Summe verfügbbarer Mainboards auf der Lenovo-Clearance-Seite
(https://support.lenovo.com/de/de/parts-lookup/clearance-sale) und sendet bei
Anstieg eine E-Mail über den lokalen MTA (Proxmox-Stil: Adresse angeben, der
MTA findet den Weg).

Nur Python-Stdlib + curl_cffi. Läuft als systemd-Service (Dauer-Loop).
"""

import argparse
import csv
import json
import logging
import os
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


# ---------------------------------------------------------------------------
# API-Abruf
# ---------------------------------------------------------------------------


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
        raise FetchError(
            "HTTP 403 – Akamai blockiert den Abruf (Fallback: Headless-Chromium, siehe README)",
            status=403,
        )
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


# ---------------------------------------------------------------------------
# Mainboard-Filter, Summe, Diff
# ---------------------------------------------------------------------------


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


# ---------------------------------------------------------------------------
# State & Verlauf
# ---------------------------------------------------------------------------


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
