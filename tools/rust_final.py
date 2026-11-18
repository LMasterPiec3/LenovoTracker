#!/usr/bin/env python3
"""Finale Rust-Deployment: CT 105 (Alpine + Rust-Binary + curl-impersonate)."""

import base64
import os
import sys
import time

import paramiko

HOST = os.environ.get("PVE_SSH_HOST", "100.82.25.128")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")
VMID = 105

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
mode = "smtp"                        # direkt per SMTP, kein Mail-Daemon im Container
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = []
subject_prefix = "[Lenovo Tracker]"
smtp_host = "mail.protonmail.ch"
smtp_port = 25
smtp_starttls = false                # Plain-SMTP (verifiziert zugestellt; STARTTLS haengt in lettre)

[paths]
state_dir = "/var/lib/lenovotracker"
"""

CRON_LINE = "*/30 * * * * /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1"

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=600):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and "sMMs" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", " '", "  `", "   `"))]
    print("\n".join(keep[-10:]))
    if err.strip() and "TERM environment" not in err:
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out, err


print("=== 1) Binaries aus Build-Container 104 pullen ===")
run(f"pct pull 104 /build/target/release/lenovo-tracker /root/lenovo-tracker-rs")
run(f"pct pull 104 /opt/curl-impersonate/curl-impersonate /root/curl-impersonate")
run("ls -la /root/lenovo-tracker-rs /root/curl-impersonate")

print(f"=== 2) CT {VMID} neu erstellen (64 MB RAM / 32 MB Swap / 512 MB Disk / 0.25 Cores) ===")
run(f"pct destroy {VMID} --purge 2>/dev/null; true", timeout=120)
code, _, err = run(
    f"pct create {VMID} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname lenovo-tracker-rust --memory 64 --swap 32 --cpulimit 0.25 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{VMID}/24,type=veth "
    f"--rootfs local:0.5 --unprivileged 1 --onboot 1 --tags lenovo-tracker --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-400:])
    sys.exit(1)
time.sleep(5)

print("=== 3) Pakete + Dateien ===")
run(f"pct exec {VMID} -- sh -c 'apk update -q && apk add -q dcron ca-certificates && echo pakete-ok'", timeout=300)
run(f"pct exec {VMID} -- mkdir -p /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker")
run(f"pct push {VMID} /root/lenovo-tracker-rs /opt/lenovo-tracker/lenovo-tracker")
run(f"pct push {VMID} /root/curl-impersonate /opt/lenovo-tracker/curl-impersonate")
run(f"pct exec {VMID} -- chmod +x /opt/lenovo-tracker/lenovo-tracker /opt/lenovo-tracker/curl-impersonate")

# Config per base64 (LF-sicher)
data = base64.b64encode(CONFIG_TOML.replace("\r\n", "\n").encode()).decode()
run(f"pct exec {VMID} -- sh -c 'echo {data} | base64 -d > /etc/lenovotracker/config.toml && echo config-ok'")

print("=== 4) Cron alle 30 Minuten ===")
cron_b64 = base64.b64encode((CRON_LINE + "\n").replace("\r\n", "\n").encode()).decode()
run(f"pct exec {VMID} -- sh -c 'echo {cron_b64} | base64 -d >> /etc/crontabs/root && rc-update add crond default && rc-service crond restart && crontab -l | tail -1'", timeout=120)

print("=== 5) Baseline setzen ===")
run(f"pct exec {VMID} -- sh -c '/opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1; tail -2 /var/log/lenovo-tracker.log'", timeout=300)

print("=== 6) Verifikation ===")
run("pct status 105 --verbose | head -8")
run(f"pct exec {VMID} -- df -h / | tail -1")
run(f"pct exec {VMID} -- ps -o pid,rss,comm")

print("=== 7) Build-Container 104 aufräumen ===")
run("pct stop 104 && pct destroy 104 --purge && echo 104-geloescht", timeout=180)

client.close()
print("FERTIG")
