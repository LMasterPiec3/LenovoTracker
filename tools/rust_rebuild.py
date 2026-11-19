#!/usr/bin/env python3
"""Rust-Build in CT 104 (neu) mit leichter Toolchain + Peak-Messung."""

import base64
import os
import sys
import time
from pathlib import Path

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]
VMID = 104
REPO = Path(__file__).resolve().parents[1]

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


print("=== 1) Build-CT 104 neu ===")
run(f"pct destroy {VMID} --purge 2>/dev/null; true", timeout=120)
code, _, err = run(
    f"pct create {VMID} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname rust-build --memory 3072 --cores 4 --swap 512 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{VMID}/24,type=veth "
    f"--rootfs local:4 --unprivileged 1 --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-300:])
    sys.exit(1)
time.sleep(5)

print("=== 2) Leichte Toolchain (kein cmake/go/clang mehr noetig) ===")
run(
    f"pct exec {VMID} -- sh -c 'apk update -q && apk add -q "
    "rust cargo build-base pkgconf musl-dev openssl-dev openssl-libs-static ca-certificates "
    "&& rustc --version'",
    timeout=900,
)

print("=== 3) Quellen ===")
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
run(f"pct exec {VMID} -- mkdir -p /build/src")
run(f"for f in Cargo.toml src/main.rs; do pct push {VMID} /root/rustbuild/$f /build/$f; done")

print("=== 4) Build (synchron) + Tests ===")
code, out, _ = run(
    f"pct exec {VMID} -- sh -c 'cd /build && cargo build --release 2>&1 | tail -2 && cargo test --release 2>&1 | grep -E \"test result|running\"'",
    timeout=900,
)
if "error" in out.lower():
    print("BUILD FEHLER – Log:")
    run(f"pct exec {VMID} -- tail -30 /build/target/release/build/*/output 2>/dev/null | head -10")
    sys.exit(1)

print("=== 5) Peak-Messung (ein voller --once-Lauf) ===")
data = base64.b64encode(open(REPO / "tools" / "test_config_rust.toml", "rb").read() if (REPO / "tools" / "test_config_rust.toml").exists() else b"", ).decode() if False else ""
# Test-Config im Container erzeugen
TESTCONFIG = """\
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3
curl_path = "/opt/curl-impersonate/curl-impersonate"
impersonate_target = "chrome124"

[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]

[mail]
mode = "smtp"
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = []
subject_prefix = "[Lenovo Tracker]"
smtp_host = "mail.protonmail.ch"
smtp_port = 25
smtp_starttls = false

[paths]
state_dir = "/tmp/teststate"
"""
tb64 = base64.b64encode(TESTCONFIG.replace("\r\n", "\n").encode()).decode()
run(f"pct exec {VMID} -- sh -c 'echo {tb64} | base64 -d > /root/testconfig.toml && mkdir -p /tmp/teststate'")
# curl-impersonate fuer den Testlauf benoetigt – aus dem alten Tarball-Download (CT104 frisch -> neu laden)
run(f"pct exec {VMID} -- sh -c 'apk add -q curl >/dev/null 2>&1; mkdir -p /opt/curl-impersonate && cd /opt/curl-impersonate && curl -sL -o ci.tar.gz https://github.com/lexiforest/curl-impersonate/releases/download/v2.3.2/curl-impersonate-v2.3.2.x86_64-linux-musl.tar.gz && tar xzf ci.tar.gz && echo ci-ok'", timeout=600)
# Peak-Reset (write 0) und Lauf
run(f"pct exec {VMID} -- sh -c 'echo 0 > /sys/fs/cgroup/memory.peak 2>/dev/null; /build/target/release/lenovo-tracker --config /root/testconfig.toml --once; echo peak-bytes:; cat /sys/fs/cgroup/memory.peak 2>/dev/null'", timeout=300)

print("=== 6) Unit-Tests nochmal explizit ===")
run(f"pct exec {VMID} -- sh -c 'cd /build && cargo test --release 2>&1 | grep \"test result\"'", timeout=600)

client.close()
print("Build + Peak-Messung fertig.")
