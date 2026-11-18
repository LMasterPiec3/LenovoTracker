#!/usr/bin/env python3
"""Deployment der verbleibenden Schritte im LXC 101 (CRLF-Fix, install.sh, Verifikation)."""

import os
import sys

import paramiko

HOST = os.environ.get("PVE_SSH_HOST", "100.82.25.128")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

FILES = "lenovo_tracker.py config.example.toml install.sh lenovo-tracker.service README.md"
PY = "/opt/lenovo-tracker/venv/bin/python /opt/lenovo-tracker/lenovo_tracker.py"
CFG = "/etc/lenovotracker/config.toml"


def main():
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)

    def run(cmd, tail=None, timeout=900):
        print(f"\n### {cmd[:120]}")
        stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
        out = stdout.read().decode("utf-8", "replace")
        err = stderr.read().decode("utf-8", "replace")
        code = stdout.channel.recv_exit_status()
        if tail and len(out.splitlines()) > tail:
            out = "\n".join(out.splitlines()[-tail:])
        print(out)
        if err.strip():
            print("[stderr]", err.strip()[-500:])
        print(f"[rc={code}]")
        return code, out, err

    # 1) CRLF bereinigen
    fix = f'pct exec 101 -- bash -c "cd /tmp/LenovoTracker && sed -i \'s/\\r\\$//\' {FILES} && head -1 install.sh && file install.sh 2>/dev/null || true"'
    run(fix)

    # 2) install.sh ausführen
    run('pct exec 101 -- bash /tmp/LenovoTracker/install.sh', tail=18, timeout=900)

    # 3) Service-Status
    run("pct exec 101 -- systemctl is-active lenovo-tracker; pct exec 101 -- systemctl is-enabled lenovo-tracker")

    # 4) Dry-Run im Container (echter Lenovo-API-Abruf über NAT)
    run(f"pct exec 101 -- {PY} --config {CFG} --dry-run", timeout=120)

    # 5) Einmal-Lauf (Baseline)
    run(f"pct exec 101 -- {PY} --config {CFG} --once", timeout=120)

    # 6) Service-Status nach Baseline
    run("pct exec 101 -- systemctl status lenovo-tracker --no-pager -n 6 | head -14")

    client.close()


if __name__ == "__main__":
    sys.exit(main())
