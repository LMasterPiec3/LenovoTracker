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
        log.warning("HTTP 403 – Akamai blockiert den Abruf (Fallback: Headless-Chromium, siehe README)")
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
    except (TypeError, ValueError, OverflowError):
        return 0


def filter_mainboards(parts, commodity_keys):
    keys = {str(k).strip().lower() for k in commodity_keys}
    return [p for p in parts if str(p.get("commodityKey", "")).strip().lower() in keys]


def compute_totals(mainboards):
    # Nur Teile mit id zählen – identisch zum Filter in parts_to_map,
    # damit total_stock immer zur Summe der State-Map passt.
    with_id = [p for p in mainboards if p.get("id")]
    return sum(_stock_of(p) for p in with_id), len(with_id)


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
        data = json.loads(Path(state_path).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except json.JSONDecodeError as exc:
        log.error("state.json ist beschädigt (%s) – Baseline wird neu gesetzt", exc)
        return None
    except OSError as exc:
        log.error("state.json nicht lesbar (%s) – Baseline wird neu gesetzt", exc)
        return None
    if not isinstance(data, dict) or not isinstance(data.get("parts"), dict):
        log.error("state.json hat ein unerwartetes Format – Baseline wird neu gesetzt")
        return None
    return data


def save_state(state_path, ts, total, count, part_map):
    Path(state_path).parent.mkdir(parents=True, exist_ok=True)
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


# ---------------------------------------------------------------------------
# E-Mail
# ---------------------------------------------------------------------------


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
            timeout=60,
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


# ---------------------------------------------------------------------------
# Ablauf: run_once, Dauer-Loop, CLI
# ---------------------------------------------------------------------------


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
    if old_state is None:
        save_state(state_path, ts, total, count, part_map)
        append_history(history_path, ts, total, count)
        log.info("Baseline gesetzt: %d Stück über %d Mainboard-Teile", total, count)
        return CheckResult(total=total, count=count, baseline=True)
    diff = build_diff(old_state.get("parts", {}), part_map)
    if diff.delta > 0:
        subject = build_subject(cfg, diff)
        text = build_mail_text(diff, PAGE_URL, ts)
        html = build_mail_html(diff, PAGE_URL, ts)
        try:
            deliver(cfg, compose_mail(cfg, subject, text, html))
        except Exception as exc:
            # State NICHT aktualisieren: der Anstieg wird beim nächsten Lauf
            # erneut gemeldet (Duplikat-Alarm schlägt verlorenen Alarm).
            log.error("Mailversand fehlgeschlagen – Anstieg wird erneut gemeldet: %s", exc)
            return CheckResult(total=total, count=count, error=f"Mailversand fehlgeschlagen: {exc}")
        save_state(state_path, ts, total, count, part_map)
        append_history(history_path, ts, total, count)
        log.info("Mail gesendet: %s", subject)
        return CheckResult(total=total, count=count, sent_mail=True)
    save_state(state_path, ts, total, count, part_map)
    append_history(history_path, ts, total, count)
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
        log.info("Zyklus beendet (%.1fs)", elapsed)
        sleep_s = max(60, interval - elapsed)
        log.debug("Nächster Lauf in %.0f Sekunden", sleep_s)
        time.sleep(sleep_s)


def main(argv=None):
    parser = argparse.ArgumentParser(description="Lenovo Clearance-Sale Mainboard-Tracker")
    parser.add_argument("--config", default=DEFAULT_CONFIG_PATH, help="Pfad zur config.toml")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--once", action="store_true", help="einmal prüfen und beenden")
    mode.add_argument("--dry-run", action="store_true", help="einmal abrufen und zusammenfassen (kein State, keine Mail)")
    mode.add_argument("--test-mail", action="store_true", help="Testmail mit aktuellem Bestand senden")
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
        result = run_once(cfg, dry_run=True)
        return 2 if result.error else 0
    if args.test_mail:
        return 0 if send_test_mail(cfg) else 2
    if args.once:
        result = run_once(cfg)
        return 2 if result.error else 0
    run_loop(cfg)
    return 0


if __name__ == "__main__":
    sys.exit(main())
