#!/usr/bin/env python3
"""API-Parameter-Probe: unterstuetzt der Lenovo-Clearance-Endpoint serverseitige Filter?

Laesst curl-impersonate im Container (Default: CT 105) verschiedene Query-Strings
gegen clearanceParts/get abfeuern und misst: HTTP-Status, Body-Groesse, Anzahl
Teile (heuristic: "commodityKey"-Hits), Anzahl Mainboards ("system boards"-Hits),
plus die Nicht-Data-Felder der Response.

Aufruf:  PVE_SSH_HOST=... PVE_SSH_PASS=... python tools/api_probe.py [vmid]
"""

import os

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]
VMID = os.environ.get("PVE_PROBE_VMID", "105")
CURL = "/opt/lenovo-tracker/curl-impersonate"
BASE = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"

PROBES = [
    ("baseline (keine Params)", BASE),
    ("keyword=system boards", BASE + "?keyword=system%20boards"),
    ("keyword=mainboard", BASE + "?keyword=mainboard"),
    ("q=system boards", BASE + "?q=system%20boards"),
    ("searchText=system boards", BASE + "?searchText=system%20boards"),
    ("search=system boards", BASE + "?search=system%20boards"),
    ("commodityKey=system boards", BASE + "?commodityKey=system%20boards"),
    ("category=system boards", BASE + "?category=system%20boards"),
    ("filter=system boards", BASE + "?filter=system%20boards"),
    ("name=mainboard", BASE + "?name=mainboard"),
]

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def exec_ct(cmd, timeout=120):
    _, stdout, stderr = client.exec_command(f"pct exec {VMID} -- sh -c '{cmd}'", timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    return code, out, err


BODY = "/tmp/probe-body.json"

print(f"Probe auf CT {VMID} mit {CURL}\n")
for label, url in PROBES:
    code, out, err = exec_ct(
        f"{CURL} --impersonate chrome124 --compressed -s --max-time 60 "
        f"-o {BODY} -w '%{{http_code}}' -H 'Accept: application/json' "
        f"-H 'Referer: https://support.lenovo.com/de/de/parts-lookup/clearance-sale' '{url}'"
    )
    status = out.strip()
    if status != "200":
        head = exec_ct(f"head -c 200 {BODY}")[1].strip()
        print(f"{label:35s} -> HTTP {status}  Body: {head[:120]}")
        continue
    stats = exec_ct(
        f"wc -c < {BODY}; "
        f"grep -o '\"commodityKey\"' {BODY} | wc -l; "
        f"grep -o '\"system boards\"' {BODY} | wc -l; "
        f"head -c 120 {BODY}"
    )[1]
    lines = stats.strip().split("\n")
    size = lines[0].strip() if lines else "?"
    n_parts = lines[1].strip() if len(lines) > 1 else "?"
    n_boards = lines[2].strip() if len(lines) > 2 else "?"
    head = lines[3].strip() if len(lines) > 3 else ""
    print(f"{label:35s} -> HTTP {status}  {int(size)/1024:.0f} KB  Teile~{n_parts}  Boards~{n_boards}")
    if label.startswith("baseline"):
        print(f"    Body-Head: {head[:110]}")

exec_ct(f"rm -f {BODY}")
client.close()
