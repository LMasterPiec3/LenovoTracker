#!/usr/bin/env python3
"""Alpine-Container 103: fehlende Schritte nachziehen."""

import os
import time

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=300):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", "        `sM"))]
    print("\n".join(keep[-12:]))
    if err.strip():
        print("[stderr]", err.strip()[-400:])
    print(f"[rc={code}]")
    return code, out, err


print("=== 1) Ordner + Dateien ===")
run("pct exec 103 -- mkdir -p /tmp/LenovoTracker")
run("for f in lenovo_tracker.py config.example.toml README.md; do pct push 103 /root/LenovoTracker/$f /tmp/LenovoTracker/$f; done")
run("pct exec 103 -- ls -la /tmp/LenovoTracker")

print("=== 2) Installieren ===")
run(
    "pct exec 103 -- sh -c '"
    "mkdir -p /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker && "
    "cp /tmp/LenovoTracker/lenovo_tracker.py /opt/lenovo-tracker/ && "
    "cp /tmp/LenovoTracker/config.example.toml /etc/lenovotracker/config.toml && "
    "sed -i \"s/\\r$//\" /opt/lenovo-tracker/lenovo_tracker.py /etc/lenovotracker/config.toml && "
    "echo installiert'"
)

print("=== 3) postfix (newaliases + start) ===")
run("pct exec 103 -- sh -c 'newaliases 2>&1; rc-service postfix start; rc-service postfix status'", timeout=120)

print("=== 4) Tracker-Service neu starten ===")
run("pct exec 103 -- rc-service lenovo-tracker restart", timeout=120)
time.sleep(8)

print("=== 5) Status + Baseline ===")
run("pct exec 103 -- rc-service lenovo-tracker status")
run("pct exec 103 -- tail -4 /var/log/lenovo-tracker.log 2>/dev/null")
run("pct exec 103 -- head -c 150 /var/lib/lenovotracker/state.json 2>/dev/null")

print("=== 6) Testmail ===")
run("pct exec 103 -- /usr/bin/python3 /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml --test-mail", timeout=120)
time.sleep(4)
run("pct exec 103 -- mailq | head -4")
run("pct exec 103 -- sh -c 'grep -h \"status=sent\\|status=deferred\\|status=bounced\" /var/log/messages 2>/dev/null | tail -2; true'")

client.close()
