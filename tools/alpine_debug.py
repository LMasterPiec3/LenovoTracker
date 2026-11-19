#!/usr/bin/env python3
"""Debug: Was liefert die Lenovo-API im Alpine-Container?"""

import os

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=120):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    lines = [ln for ln in out.splitlines() if len(ln.strip()) > 3 and "\x1b" not in ln]
    print("\n".join(lines[-15:]))
    if err.strip():
        print("[stderr]", err.strip()[-400:])
    print(f"[rc={code}]")


# 1) Ohne Header (wie im ersten Test)
run("pct exec 103 -- python3 -c \"from curl_cffi import requests; r=requests.get('https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get', impersonate='chrome', timeout=30); print('OHNE HEADERS -> HTTP', r.status_code); print(r.text[:150])\"")

# 2) Mit Tracker-Headern
run("pct exec 103 -- python3 -c \""
    "from curl_cffi import requests; "
    "r=requests.get('https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get', "
    "headers={'Accept':'application/json, text/plain, */*','Accept-Language':'de-DE,de;q=0.9,en;q=0.8','Referer':'https://support.lenovo.com/de/de/parts-lookup/clearance-sale'}, "
    "impersonate='chrome', timeout=30); "
    "print('MIT HEADERS -> HTTP', r.status_code); "
    "j=r.json(); print('code', j.get('code'), '| Teile:', len(j.get('data') or []))\"")

client.close()
