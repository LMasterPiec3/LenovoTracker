#!/usr/bin/env python3
"""Testmail im Container senden und postfix-Zustellung verifizieren."""

import os

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

PY = "/opt/lenovo-tracker/venv/bin/python /opt/lenovo-tracker/lenovo_tracker.py"
CFG = "/etc/lenovotracker/config.toml"

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=120):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    lines = [line for line in out.splitlines() if len(line) > 15 and not line.startswith((" ", "\x1b", "[48C"))]
    print("\n".join(lines[-25:]))
    if err.strip():
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out


run(f"pct exec 101 -- {PY} --config {CFG} --test-mail")
# postfix eine Sekunde Zeit geben, dann Log auswerten
import time
time.sleep(3)
run("pct exec 101 -- bash -c 'tail -12 /var/log/mail.log 2>/dev/null || tail -12 /var/log/syslog | grep postfix' ")
client.close()
