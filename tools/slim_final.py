#!/usr/bin/env python3
"""CT 105 final: Disk-Entschlackung + Neuaufbau mit 44 MB rootfs."""

import base64
import os
import sys
import time

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]
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


print("=== 1) CT 105: Pakete entschlacken + gestripptes curl-Binary ===")
run(
    "pct exec 105 -- sh -c '"
    "apk del -q dhcpcd dhcpcd-openrc dcron dcron-openrc logrotate logrotate-openrc "
    "doas alpine-conf busybox-suid bridge 2>&1 | tail -1; "
    "rm -rf /var/cache/apk/* /var/log/messages*; "
    "mv /opt/lenovo-tracker/curl-impersonate /opt/lenovo-tracker/curl-impersonate.old; "
    "echo cleanup-ok'"
)
run("pct push 105 /root/curl-imp-check /opt/lenovo-tracker/curl-impersonate")
run("pct exec 105 -- sh -c 'chmod +x /opt/lenovo-tracker/curl-impersonate && rm -rf /opt/lenovo-tracker/curl-impersonate.old && df -h / | tail -1'")

print("=== 2) CT 105 mit 44 MB rootfs neu erstellen ===")
run(f"pct stop {VMID} && pct destroy {VMID} --purge && echo zerstoert", timeout=180)
code, _, err = run(
    f"pct create {VMID} local:vztmpl/alpine-3.24-default_20261105_amd64.tar.xz "
    f"--hostname lenovo-tracker-rust --memory 96 --swap 32 --cpulimit 0.25 "
    f"--net0 name=eth0,bridge=vmbr1,gw=10.10.10.1,ip=10.10.10.{VMID}/24,type=veth "
    f"--rootfs local:0.04 --unprivileged 1 --onboot 1 --tags lenovo-tracker --start 1",
    timeout=300,
)
if code != 0:
    print("CREATE FEHLGESCHLAGEN:", err[-400:])
    sys.exit(1)
time.sleep(5)

print("=== 3) Minimal-Setup (entschlacken + ca-certs; crond via inittab) ===")
run(
    f"pct exec {VMID} -- sh -c '"
    "apk update -q && apk add -q ca-certificates && "
    "apk del -q dhcpcd dhcpcd-openrc logrotate logrotate-openrc doas alpine-conf "
    "busybox-suid bridge dcron dcron-openrc 2>&1 | tail -1; "
    "rm -rf /var/cache/apk/* /var/log/messages*; "
    "echo setup-ok'",
    timeout=300,
)
run(f"pct exec {VMID} -- mkdir -p /opt/lenovo-tracker /etc/lenovotracker /var/lib/lenovotracker")
run(f"pct push {VMID} /root/lenovo-tracker-rs /opt/lenovo-tracker/lenovo-tracker")
run(f"pct push {VMID} /root/curl-imp-check /opt/lenovo-tracker/curl-impersonate")
run(f"pct exec {VMID} -- chmod +x /opt/lenovo-tracker/lenovo-tracker /opt/lenovo-tracker/curl-impersonate")
data = base64.b64encode(CONFIG_TOML.replace("\r\n", "\n").encode()).decode()
run(f"pct exec {VMID} -- sh -c 'echo {data} | base64 -d > /etc/lenovotracker/config.toml && echo config-ok'")
data = base64.b64encode(INITTAB.replace("\r\n", "\n").encode()).decode()
run(f"pct exec {VMID} -- sh -c 'echo {data} | base64 -d > /etc/inittab && kill -HUP 1 && sleep 2 && echo inittab-ok'")
cron = base64.b64encode((CRON_LINE + "\n").replace("\r\n", "\n").encode()).decode()
run(f"pct exec {VMID} -- sh -c 'echo {cron} | base64 -d >> /etc/crontabs/root && echo cron-ok'")

print("=== 4) Baseline + Verifikation ===")
run(f"pct exec {VMID} -- sh -c '/opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once >> /var/log/lenovo-tracker.log 2>&1; tail -2 /var/log/lenovo-tracker.log'", timeout=300)
run("pct status 105 --verbose | grep -E 'maxdisk|maxmem|mem:|cpus'")
run(f"pct exec {VMID} -- sh -c 'df -h / | tail -1; ps -o pid,rss,comm; ip -4 addr show eth0 | grep inet'")
run("pct list")

client.close()
print("FERTIG")
