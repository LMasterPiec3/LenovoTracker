#!/usr/bin/env python3
"""Cgroup-Peak-Messung vom PVE-Host aus (kein Reset innerhalb des CT).

Der alte In-Container-Reset (`echo 0 > /sys/fs/cgroup/memory.peak`) scheitert
still mit Permission denied – der gemessene "Peak" war dann der Lifetime-Peak
des Containers (z. B. cargo-Build, > 2 GB). Diese Variante:
  1. Page-Cache des CT per host-weitem drop_caches fallen lassen (sonst zaehlt
     der Build-Cache als memory.current und dominiert jeden Peak).
  2. memory.peak auf dem Host-Cgroup-Pfad des CT auf 0 setzen.
  3. Den Tracker-Lauf N-mal ausfuehren, max(peak) melden.

Aufruf:  PVE_SSH_HOST=... PVE_SSH_PASS=... python tools/peak_check.py <vmid> [laeufe]
Default: vmid 104, 3 Laeufe.
"""

import os
import sys

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]
VMID = sys.argv[1] if len(sys.argv) > 1 else "104"
RUNS = int(sys.argv[2]) if len(sys.argv) > 2 else 3
TRACKER_CMD = "/build/target/release/lenovo-tracker --config /root/testconfig.toml --once"

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=300):
    _, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    return code, out, err


cg = f"/sys/fs/cgroup/lxc/{VMID}"

print("=== 1) Cgroup-Pfad pruefen ===")
code, out, _ = run(f"test -f {cg}/memory.peak && echo pfad-ok || echo PFAD-FEHLT: {cg}")
print(out.strip() or f"[rc={code}]")
if "pfad-ok" not in out:
    sys.exit(1)

print("=== 2) Aktuelle Werte (vorher) ===")
code, out, _ = run(f"grep -H '' {cg}/memory.max {cg}/memory.current {cg}/memory.peak")
print(out.strip()[-300:])

print("=== 3) Host-weiter Page-Cache-Drop (Build-Cache faellt sonst in den Peak) ===")
code, out, err = run("sync; echo 3 > /proc/sys/vm/drop_caches && echo drop-ok")
print((out + err).strip()[:200])

print("=== 4) Peak-Reset auf Host-Cgroup ===")
code, out, err = run(f"echo 0 > {cg}/memory.peak && echo reset-ok || echo RESET-FEHLGESCHLAGEN")
print((out + err).strip()[:200])
if "reset-ok" not in out:
    sys.exit(1)

print(f"=== 5) {RUNS} Tracker-Laeufe + Peak je Lauf ===")
for i in range(1, RUNS + 1):
    code, out, err = run(f"pct exec {VMID} -- sh -c '{TRACKER_CMD}' 2>&1 | tail -1", timeout=300)
    last = [ln for ln in out.splitlines() if ln.strip()][-1:] or ["(keine Ausgabe)"]
    code, peak, _ = run(f"cat {cg}/memory.peak")
    peak_b = peak.strip()
    peak_mb = int(peak_b) / 1024 / 1024 if peak_b.isdigit() else float("nan")
    print(f"Lauf {i}: {last[0].strip()}  |  peak = {peak_b} B = {peak_mb:.1f} MB  [rc={code}]")
    if i < RUNS:
        run(f"echo 0 > {cg}/memory.peak")

print("=== 6) Werte (nachher) ===")
code, out, _ = run(f"grep -H '' {cg}/memory.current {cg}/memory.peak")
print(out.strip()[-200:])

client.close()
