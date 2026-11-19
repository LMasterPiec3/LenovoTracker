#!/usr/bin/env python3
"""Voll-Deployment des Trackers im Alpine-Container 103 (OpenRC statt systemd)."""

import os
import sys
import time

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=300, tail=None):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", "        `sM"))]
    if tail and len(keep) > tail:
        keep = keep[-tail:]
    print("\n".join(keep))
    if err.strip():
        print("[stderr]", err.strip()[-400:])
    print(f"[rc={code}]")
    return code, out, err


# --- 1) Frische (LF-)Dateien auf Host laden und in Container pushen ------------
print("=== 1) Dateien hochladen ===")
import base64
for name in ("lenovo_tracker.py", "config.example.toml", "README.md"):
    data = base64.b64encode(open(name, "rb").read()).decode()
    stdin, stdout, _ = client.exec_command(f"base64 -d > /root/LenovoTracker/{name}")
    stdin.write(data + "\n")
    stdin.channel.shutdown_write()
    stdout.channel.recv_exit_status()
run("for f in lenovo_tracker.py config.example.toml README.md; do pct push 103 /root/LenovoTracker/$f /tmp/LenovoTracker/$f; done && pct exec 103 -- ls /tmp/LenovoTracker")

# --- 2) Im Container installieren ---------------------------------------------
print("=== 2) Installation ===")
run(
    "pct exec 103 -- sh -c '"
    "mkdir -p /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker && "
    "cp /tmp/LenovoTracker/lenovo_tracker.py /opt/lenovo-tracker/ && "
    "cp /tmp/LenovoTracker/config.example.toml /etc/lenovotracker/config.toml && "
    "sed -i \"s/\\r$//\" /opt/lenovo-tracker/lenovo_tracker.py /etc/lenovotracker/config.toml && "
    "rc-update add postfix default && rc-service postfix start && "
    "rc-update add crond default 2>/dev/null; true'",
    timeout=300,
)

# --- 3) OpenRC-Service anlegen ---------------------------------------------------
print("=== 3) OpenRC-Service ===")
INIT = r"""#!/sbin/openrc-run
export PYTHONUNBUFFERED=1

name="lenovo-tracker"
description="Lenovo Clearance-Sale Mainboard-Tracker"
command="/usr/bin/python3"
command_args="/opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml"
command_background="yes"
pidfile="/run/lenovo-tracker.pid"
output_log="/var/log/lenovo-tracker.log"
error_log="/var/log/lenovo-tracker.err.log"

depend() {
  need net
  after postfix
}
"""
import base64 as b64
init_b64 = b64.b64encode(INIT.replace("\r\n", "\n").encode()).decode()
run(f"pct exec 103 -- sh -c 'echo {init_b64} | base64 -d > /etc/init.d/lenovo-tracker && chmod +x /etc/init.d/lenovo-tracker && rc-update add lenovo-tracker default && rc-service lenovo-tracker start'", timeout=120)

time.sleep(6)
print("=== 4) Status + Baseline ===")
run("pct exec 103 -- rc-service lenovo-tracker status; pct exec 103 -- tail -3 /var/log/lenovo-tracker.log 2>/dev/null; pct exec 103 -- cat /var/lib/lenovotracker/state.json | head -c 200")

# --- 5) Testmail + Zustellungs-Check --------------------------------------------
print("=== 5) Testmail ===")
run("pct exec 103 -- /usr/bin/python3 /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml --test-mail", timeout=120)
time.sleep(4)
run("pct exec 103 -- mailq | head -5")
run("pct exec 103 -- sh -c 'grep -h \"status=\" /var/log/messages 2>/dev/null | tail -3; true'")

client.close()
