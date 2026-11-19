#!/usr/bin/env python3
"""Alpine-Container 103 mit 1-GB-Boot-Disk neu aufsetzen."""

import base64
import os
import sys
import time

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")
VMID = 103

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

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=600):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", "        `sM"))]
    print("\n".join(keep[-10:]))
    if err.strip():
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out, err


def exec_vmid(cmd, timeout=600):
    return run(f"pct exec {VMID} -- sh -c '{cmd}'", timeout=timeout)


print("=== 1) Alten Container 103 stoppen + löschen ===")
run(f"pct stop {VMID} 2>/dev/null; pct destroy {VMID} --purge && echo zerstoert", timeout=120)

print("=== 2) Neu erstellen mit 1 GB rootfs ===")
code, _, err = run(
    f"pct create {VMID} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname lenovo-tracker-alpine --memory 512 --cores 1 --swap 256 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{VMID}/24,type=veth "
    f"--rootfs local:1 --unprivileged 1 --onboot 1 --tags lenovo-tracker --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-300:])
    sys.exit(1)
time.sleep(5)

print("=== 3) Pakete + curl_cffi ===")
exec_vmid("apk update -q && apk add -q python3 py3-pip ca-certificates postfix && python3 --version", timeout=600)
exec_vmid("pip3 install --break-system-packages -q curl-cffi && python3 -c 'import curl_cffi; print(\"curl_cffi\", curl_cffi.__version__)'", timeout=600)

print("=== 4) Dateien pushen + installieren ===")
exec_vmid("mkdir -p /tmp/LenovoTracker /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker")
for name in ("lenovo_tracker.py", "config.example.toml", "README.md"):
    data = base64.b64encode(open(name, "rb").read()).decode()
    stdin, stdout, _ = client.exec_command(f"base64 -d > /root/LenovoTracker/{name}")
    stdin.write(data + "\n")
    stdin.channel.shutdown_write()
    stdout.channel.recv_exit_status()
run(f"for f in lenovo_tracker.py config.example.toml README.md; do pct push {VMID} /root/LenovoTracker/$f /tmp/LenovoTracker/$f; done")
exec_vmid(
    "cp /tmp/LenovoTracker/lenovo_tracker.py /opt/lenovo-tracker/ && "
    "cp /tmp/LenovoTracker/config.example.toml /etc/lenovotracker/config.toml && "
    "sed -i \"s/\\r$//\" /opt/lenovo-tracker/lenovo_tracker.py /etc/lenovotracker/config.toml && echo ok"
)

print("=== 5) postfix + OpenRC-Service ===")
init_b64 = base64.b64encode(INIT.replace("\r\n", "\n").encode()).decode()
exec_vmid("newaliases && rc-update add postfix default && rc-service postfix start && rc-service postfix status", timeout=120)
exec_vmid(f"echo {init_b64} | base64 -d > /etc/init.d/lenovo-tracker && chmod +x /etc/init.d/lenovo-tracker && rc-update add lenovo-tracker default && rc-service lenovo-tracker start", timeout=120)
time.sleep(8)

print("=== 6) Verifikation ===")
exec_vmid("rc-service lenovo-tracker status")
exec_vmid("tail -4 /var/log/lenovo-tracker.log")
exec_vmid("df -h / | tail -1")
exec_vmid("/usr/bin/python3 /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml --test-mail", timeout=120)
time.sleep(5)
exec_vmid("mailq | head -3")
exec_vmid("grep -h 'to=<Lenovo@aomme.de>' /var/log/messages 2>/dev/null | tail -1")

client.close()
