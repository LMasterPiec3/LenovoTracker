#!/usr/bin/env python3
"""Temporären Rust-Build-Container (104) erstellen, Toolchain + Quellen rein, Build starten."""

import base64
import os
import sys
import time

import paramiko

HOST = os.environ.get("PVE_SSH_HOST", "100.82.25.128")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")
VMID = 104

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=600):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and "sMMs" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", " '", "  `", "   `"))]
    print("\n".join(keep[-8:]))
    if err.strip() and "TERM environment" not in err:
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out, err


print("=== 1) Build-Container erstellen ===")
code, _, err = run(
    f"pct create {VMID} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname rust-build --memory 5120 --cores 4 --swap 512 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{VMID}/24,type=veth "
    f"--rootfs local:8 --unprivileged 1 --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-400:])
    sys.exit(1)
time.sleep(5)

print("=== 2) Toolchain installieren (rust, cargo, cmake, clang, go …) ===")
run(
    f"pct exec {VMID} -- sh -c 'apk update -q && apk add -q "
    "rust cargo build-base cmake ninja clang go perl pkgconf git musl-dev "
    "zlib-dev zlib-static ca-certificates && rustc --version && cargo --version'",
    timeout=900,
)

print("=== 3) Quellen hochladen ===")
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
client.exec_command("mkdir -p /root/rustbuild/src")[1].channel.recv_exit_status()
for rel, content in [
    ("Cargo.toml", (REPO / "rust-tracker" / "Cargo.toml").read_bytes()),
    ("src/main.rs", (REPO / "rust-tracker" / "src" / "main.rs").read_bytes()),
]:
    data = base64.b64encode(content).decode()
    stdin, stdout, _ = client.exec_command(f"base64 -d > /root/rustbuild/{rel}")
    stdin.write(data + "\n")
    stdin.channel.shutdown_write()
    stdout.channel.recv_exit_status()
    print(f"OK {rel} ({len(content)} Bytes)")

run(f"pct exec {VMID} -- mkdir -p /build/src")
run(f"for f in Cargo.toml src/main.rs; do pct push {VMID} /root/rustbuild/$f /build/$f; done")
run(f"pct exec {VMID} -- ls -la /build /build/src | head -12")

print("=== 4) Build im Hintergrund starten ===")
run(
    f"pct exec {VMID} -- sh -c '"
    "cd /build && rm -f build.log && setsid sh -c "
    "\"cargo build --release 2>&1 | tee /build/build.log\" "
    "</dev/null >/dev/null 2>&1 & sleep 1; echo BUILD_GESTARTET'",
    timeout=60,
)

client.close()
print("Build läuft im Hintergrund – jetzt mit Polling überwachen.")
