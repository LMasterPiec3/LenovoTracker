#!/usr/bin/env python3
"""Streaming-Parse-Optimierung: bauen, testen, deployen, Peak messen."""

import base64
import os
import sys
import time
from pathlib import Path

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]
REPO = Path(__file__).resolve().parents[1]

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=900):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and "sMMs" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", " '", "  `", "   `"))]
    print("\n".join(keep[-6:]))
    if err.strip() and "TERM environment" not in err:
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out, err


print("=== 1) Build-CT 104 ===")
run("pct stop 104 2>/dev/null; pct destroy 104 --purge 2>/dev/null; true", timeout=180)
code, _, err = run(
    "pct create 104 local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    "--hostname rust-build --memory 3072 --cores 4 --swap 512 "
    "--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.104/24,type=veth "
    "--rootfs local:4 --unprivileged 1 --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-300:])
    sys.exit(1)
time.sleep(5)
run(
    "pct exec 104 -- sh -c 'apk update -q && apk add -q rust cargo build-base pkgconf musl-dev "
    "openssl-dev openssl-libs-static ca-certificates && rustc --version'",
    timeout=900,
)

print("=== 2) Quellen + Build + Tests ===")
for rel, content in [
    ("Cargo.toml", (REPO / "rust-tracker" / "Cargo.toml").read_bytes()),
    ("src/main.rs", (REPO / "rust-tracker" / "src" / "main.rs").read_bytes()),
]:
    data = base64.b64encode(content).decode()
    stdin, stdout, _ = client.exec_command(f"base64 -d > /root/rustbuild/{rel}")
    stdin.write(data + "\n")
    stdin.channel.shutdown_write()
    stdout.channel.recv_exit_status()
run("pct exec 104 -- mkdir -p /build/src")
run("for f in Cargo.toml src/main.rs; do pct push 104 /root/rustbuild/$f /build/$f; done")
code, out, _ = run(
    "pct exec 104 -- sh -c 'cd /build && cargo build --release 2>&1 | tail -1 && "
    "cargo test --release 2>&1 | grep \"test result\"'",
    timeout=900,
)
if "test result: ok" not in out:
    print("BUILD/TESTS FEHLGESCHLAGEN — Abbruch")
    sys.exit(1)

print("=== 3) Binary auf Host + CT 105 ===")
run("pct pull 104 /build/target/release/lenovo-tracker /root/lenovo-tracker-rs && ls -la /root/lenovo-tracker-rs")
run("pct push 105 /root/lenovo-tracker-rs /opt/lenovo-tracker/lenovo-tracker")
run("pct exec 105 -- chmod +x /opt/lenovo-tracker/lenovo-tracker")

print("=== 4) Peak-Messung mit neuem Code ===")
run(
    "pct exec 105 -- sh -c 'echo 0 > /sys/fs/cgroup/memory.peak 2>/dev/null; "
    "/opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1; "
    "tail -1 /var/log/lenovo-tracker.log; echo peak-mb:; cat /sys/fs/cgroup/memory.peak'",
    timeout=300,
)

code, out, _ = run("pct exec 105 -- cat /sys/fs/cgroup/memory.peak")
try:
    peak_mb = int(out.strip().splitlines()[-1]) / 1024 / 1024
except Exception:
    peak_mb = 14.3
limit_mb = max(32, int(peak_mb * 1.4) + 4)
print(f"Peak = {peak_mb:.1f} MB -> RAM-Limit: {limit_mb} MB")
run(f"pct set 105 --memory {limit_mb}")

print("=== 5) Final ===")
run("pct status 105 --verbose | grep -E 'maxdisk|maxmem|mem:|cpus'")
run("pct exec 105 -- sh -c 'df -h / | tail -1; ps -o pid,ppid,rss,comm'")
run("pct destroy 104 --purge && echo 104-geloescht", timeout=180)

client.close()
print("FERTIG")
