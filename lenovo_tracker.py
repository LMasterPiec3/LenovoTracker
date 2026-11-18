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
