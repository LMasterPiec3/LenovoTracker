#!/usr/bin/env python3
"""Ressourcen-Messung beider Tracker-Container (101 Debian, 103 Alpine)."""

import os

import paramiko

HOST = os.environ.get("PVE_SSH_HOST", "100.82.25.128")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=60):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and "sMMs" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", " '", "  `", "   `"))]
    print("\n".join(keep[-16:]))
    if err.strip() and "TERM environment" not in err:
        print("[stderr]", err.strip()[-200:])
    print(f"[rc={code}]")
    return code, out


print("======== ALPINE 103 ========")
run("pct exec 103 -- free -m | head -3")
run("pct exec 103 -- ps -o pid,rss,comm | head -20")
run("pct exec 103 -- df -h / | tail -1")
run("pct status 103 --verbose 2>/dev/null | head -8 || pct status 103")

print("======== DEBIAN 101 ========")
run("pct exec 101 -- free -m | head -3")
run("pct exec 101 -- ps -eo rss,comm --sort=-rss | head -14")
run("pct exec 101 -- df -h / | tail -1")
run("pct status 101 --verbose 2>/dev/null | head -8 || pct status 101")

print("======== CPU-Arbeit pro Pruefung (Alpine) ========")
run("pct exec 103 -- tail -3 /var/log/lenovo-tracker.log")

client.close()
