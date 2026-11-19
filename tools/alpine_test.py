#!/usr/bin/env python3
"""Alpine-LXC erstellen und Tracker-Portierung testen (curl_cffi auf musl)."""

import os
import sys
import time

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=600, show=True):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    if show:
        # Banner-Zeilen (ASCII-Art) herausfiltern
        keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and not ln.startswith(("`", ":", "`", "-/+", "[", " '", "(", ">", "###"))]
        print("\n".join(keep[-30:]))
        if err.strip():
            print("[stderr]", err.strip()[-400:])
        print(f"[rc={code}]")
    return code, out, err


# 1) Nächste freie VMID (Banner-Zeilen ignorieren: letzte nicht-leere Zeile nehmen)
import re as _re
code, out, _ = run("pvesh get /cluster/nextid", show=False)
lines = [ln for ln in out.splitlines() if ln.strip()]
vmid = _re.sub(r"\x1b\[[0-9;]*m", "", lines[-1]).strip()
print(f"Nächste freie VMID: {vmid}")

# 2) Alpine-Container erstellen
code, out, err = run(
    f"pct create {vmid} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname lenovo-tracker-alpine --memory 512 --cores 1 --swap 256 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{vmid}/24,type=veth "
    f"--rootfs local:2 --unprivileged 1 --onboot 1 --tags lenovo-tracker --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-300:])
    sys.exit(1)

time.sleep(5)
run(f"pct status {vmid}")

# 3) Pakete + curl_cffi-Test auf musl
run(f"pct exec {vmid} -- sh -c 'apk update && apk add python3 py3-pip ca-certificates postfix 2>&1 | tail -3'", timeout=600)
run(f"pct exec {vmid} -- python3 --version")
code, out, err = run(f"pct exec {vmid} -- pip3 install --break-system-packages curl-cffi 2>&1 | tail -5", timeout=600)
print("\n=== curl_cffi-Musl-Test ===")
run(f"pct exec {vmid} -- python3 -c \"from curl_cffi import requests; r=requests.get('https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get', impersonate='chrome', timeout=30); j=r.json(); print('HTTP', r.status_code, '| code', j.get('code'), '| Teile:', len(j.get('data') or []))\"", timeout=120)

client.close()
