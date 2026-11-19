#!/usr/bin/env python3
"""Finaler Alpine-Container 103: minimaler Ressourcenverbrauch.

- 128 MB RAM / 64 MB Swap / 512 MB Disk / cpulimit 0.5
- Kein postfix: Tracker mailt direkt per SMTP (smtplib) an mail.protonmail.ch:25
- Kein Dauer-Loop: busybox-crond startet --once alle 30 Minuten
- getty deaktiviert, apk cache geleert
"""

import base64
import os
import sys
import time

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
PASSWORD = os.environ.get("PVE_SSH_PASS", "")
VMID = 103

CONFIG_TOML = """\
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3

[tracker]
interval_minutes = 30
commodity_keys = ["system boards"]

[mail]
mode = "smtp"                        # kein Mail-Daemon im Container
from = "Lenovo@aomme.de"
to = ["Lenovo@aomme.de"]
extra_recipients = []
subject_prefix = "[Lenovo Tracker]"
smtp_host = "mail.protonmail.ch"     # direkte MX-Zustellung an Proton (Port 25)
smtp_port = 25
smtp_user = ""
smtp_password = ""
smtp_starttls = true

[paths]
state_dir = "/var/lib/lenovotracker"
"""

CRON_LINE = "*/30 * * * * /usr/bin/python3 /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1"

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=600):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    keep = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and "sMMs" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", " '", "  `", "   `", " *  C"))]
    print("\n".join(keep[-10:]))
    if err.strip() and "TERM environment" not in err:
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out, err


def put_in_container(content, path):
    b64 = base64.b64encode(content.replace("\r\n", "\n").encode()).decode()
    run(f"pct exec {VMID} -- sh -c 'echo {b64} | base64 -d > {path}'")


print("=== 1) Container 103 neu aufsetzen (128 MB / 64 MB Swap / 512 MB Disk / cpulimit 0.5) ===")
run(f"pct stop {VMID} 2>/dev/null; pct destroy {VMID} --purge && echo zerstoert", timeout=120)
code, _, err = run(
    f"pct create {VMID} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname lenovo-tracker-alpine --memory 128 --swap 64 --cpulimit 0.5 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{VMID}/24,type=veth "
    f"--rootfs local:0.5 --unprivileged 1 --onboot 1 --tags lenovo-tracker --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-400:])
    sys.exit(1)
time.sleep(5)

print("=== 2) Pakete (python, crond — kein postfix!) ===")
run(f"pct exec {VMID} -- apk update -q && pct exec {VMID} -- apk add -q python3 py3-pip ca-certificates dcron && pct exec {VMID} -- python3 --version", timeout=600)
run(f"pct exec {VMID} -- pip3 install --break-system-packages -q curl-cffi && pct exec {VMID} -- python3 -c 'import curl_cffi; print(\"curl_cffi\", curl_cffi.__version__)'", timeout=600)

print("=== 3) Dateien + SMTP-Config ===")
run(f"pct exec {VMID} -- mkdir -p /tmp/LenovoTracker /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker")
for name in ("lenovo_tracker.py", "config.example.toml", "README.md"):
    data = base64.b64encode(open(name, "rb").read()).decode()
    stdin, stdout, _ = client.exec_command(f"base64 -d > /root/LenovoTracker/{name}")
    stdin.write(data + "\n")
    stdin.channel.shutdown_write()
    stdout.channel.recv_exit_status()
run(f"for f in lenovo_tracker.py config.example.toml README.md; do pct push {VMID} /root/LenovoTracker/$f /tmp/LenovoTracker/$f; done")
run(f"pct exec {VMID} -- sh -c 'cp /tmp/LenovoTracker/lenovo_tracker.py /opt/lenovo-tracker/ && sed -i \"s/\\r$//\" /opt/lenovo-tracker/lenovo_tracker.py && echo ok'")
put_in_container(CONFIG_TOML, "/etc/lenovotracker/config.toml")

print("=== 4) Cron statt Dauer-Loop ===")
put_in_container(CRON_LINE + "\n", "/etc/crontabs/root.alpine-tracker")
run(f"pct exec {VMID} -- sh -c 'cat /etc/crontabs/root.alpine-tracker >> /etc/crontabs/root && rm /etc/crontabs/root.alpine-tracker && rc-update add crond default && rc-service crond restart && echo cron-ok'", timeout=120)
run(f"pct exec {VMID} -- crontab -l | tail -2")

print("=== 5) Aufräumen: getty aus, apk cache leeren ===")
run(f"pct exec {VMID} -- sh -c 'sed -i -E \"s/^tty[1-6]/#tty[1-6]/\" /etc/inittab; kill -HUP 1; rm -f /var/cache/apk/*; echo aufgeraeumt'", timeout=120)

print("=== 6) Verifikation: einmal --once (Baseline) ===")
run(f"pct exec {VMID} -- /usr/bin/python3 /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml --once", timeout=180)

print("=== 7) Verifikation: SMTP-Testmail ===")
run(f"pct exec {VMID} -- /usr/bin/python3 /opt/lenovo-tracker/lenovo_tracker.py --config /etc/lenovotracker/config.toml --test-mail", timeout=180)

print("=== 8) Endstand ===")
run("pct status 103 --verbose | head -8")
run(f"pct exec {VMID} -- df -h / | tail -1")
run(f"pct exec {VMID} -- free -m | head -2")
run(f"pct exec {VMID} -- ps -o pid,rss,comm | head -12")

client.close()
