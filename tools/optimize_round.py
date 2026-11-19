#!/usr/bin/env python3
"""Komplett-Runde: Build-CT, Rust-Code-Optimierungen bauen, CT 105 sauber neu (1 crond)."""

import base64
import os
import sys
import time
from pathlib import Path

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]
REPO = Path(__file__).resolve().parents[1]

CONFIG_TOML = """\
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3
curl_path = "/opt/lenovo-tracker/curl-impersonate"
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
state_dir = "/var/lib/lenovotracker"
"""

CRON_LINE = "*/30 * * * * /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1"

INITTAB = """\
# /etc/inittab
::sysinit:/sbin/openrc sysinit
::sysinit:/sbin/openrc boot
::wait:/sbin/openrc default

::ctrlaltdel:/sbin/reboot

::shutdown:/sbin/openrc shutdown

# Lenovo-Tracker: crond von init ueberwacht (Auto-Restart)
::respawn:/usr/sbin/crond -f -l 8 -c /etc/crontabs
"""

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


def put_ct(vmid, host_path, ct_path):
    run(f"pct push {vmid} {host_path} {ct_path}")


def put_text_ct(text, host_path, ct_path):
    data = base64.b64encode(text.replace("\r\n", "\n").encode()).decode()
    stdin, stdout, _ = client.exec_command(f"base64 -d > {host_path}")
    stdin.write(data + "\n")
    stdin.channel.shutdown_write()
    stdout.channel.recv_exit_status()
    put_ct(105, host_path, ct_path)


# === 1) Build-CT 104 (leicht) ===
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
    "openssl-dev openssl-libs-static ca-certificates curl && rustc --version'",
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

print("=== 3) Binaries auf Host ===")
run("pct pull 104 /build/target/release/lenovo-tracker /root/lenovo-tracker-rs && ls -la /root/lenovo-tracker-rs")

print("=== 4) CT 105 sauber neu (inittab vor erstem Start!) ===")
run("pct stop 105 && pct destroy 105 --purge && echo zerstoert", timeout=180)
code, _, err = run(
    "pct create 105 local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    "--hostname lenovo-tracker-rust --memory 96 --swap 32 --cpulimit 0.25 "
    "--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.105/24,type=veth "
    "--rootfs local:0.04 --unprivileged 1 --onboot 1 --tags lenovo-tracker",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-300:])
    sys.exit(1)
time.sleep(5)
run("pct start 105")
time.sleep(4)
# Entschlacken + Dateien (laufender Container, inittab noch Template-Original)
run(
    "pct exec 105 -- sh -c '"
    "apk update -q && apk add -q ca-certificates && "
    "apk del -q dhcpcd dhcpcd-openrc logrotate logrotate-openrc doas alpine-conf "
    "busybox-suid bridge dcron dcron-openrc 2>&1 | tail -1; "
    "rm -rf /var/cache/apk/* /var/log/messages*; "
    "mkdir -p /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker; "
    "echo setup-ok'",
    timeout=300,
)
put_ct(105, "/root/lenovo-tracker-rs", "/opt/lenovo-tracker/lenovo-tracker")
put_ct(105, "/root/curl-imp-check", "/opt/lenovo-tracker/curl-impersonate")
run("pct exec 105 -- chmod +x /opt/lenovo-tracker/lenovo-tracker /opt/lenovo-tracker/curl-impersonate")
put_text_ct(CONFIG_TOML, "/root/config.toml", "/etc/lenovotracker/config.toml")
put_text_ct(INITTAB, "/root/inittab.final", "/etc/inittab")
put_text_ct(CRON_LINE + "\n", "/root/crontab.root", "/etc/crontabs/root")

print("=== 5) Neustart: init liest saubere inittab (genau 1 crond) ===")
run("pct stop 105 && pct start 105")
time.sleep(4)
run("pct exec 105 -- sh -c 'ps -o pid,ppid,rss,comm; echo crond-anzahl:; ps | grep -c \"crond\"'")

print("=== 6) Baseline + Peak-Messung ===")
run("pct exec 105 -- sh -c 'echo 0 > /sys/fs/cgroup/memory.peak 2>/dev/null; "
    "/opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1; "
    "tail -1 /var/log/lenovo-tracker.log; echo peak-mb:; cat /sys/fs/cgroup/memory.peak'", timeout=300)

code, out, _ = run("pct exec 105 -- cat /sys/fs/cgroup/memory.peak")
try:
    peak_mb = int(out.strip().splitlines()[-1]) / 1024 / 1024
except Exception:
    peak_mb = 28.0
limit_mb = max(40, int(peak_mb * 1.35) + 4)
print(f"Peak = {peak_mb:.1f} MB -> RAM-Limit: {limit_mb} MB")
run(f"pct set 105 --memory {limit_mb}")

print("=== 7) Final ===")
run("pct status 105 --verbose | grep -E 'maxdisk|maxmem|mem:|cpus'")
run("pct exec 105 -- sh -c 'df -h / | tail -1; ps -o pid,ppid,rss,comm'")
run("pct destroy 104 --purge && echo 104-geloescht", timeout=180)

client.close()
print("FERTIG")
