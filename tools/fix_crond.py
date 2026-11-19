#!/usr/bin/env python3
"""CT 105 finaler Fix: crond via OpenRC supervise-daemon (1 Instanz, Auto-Restart)."""

import base64
import os

import paramiko

HOST = os.environ["PVE_SSH_HOST"]
PASSWORD = os.environ["PVE_SSH_PASS"]

CROND_INIT = """\
#!/sbin/openrc-run
supervisor=supervise-daemon
name="crond"
description="Busybox crond (Lenovo-Tracker Scheduler)"
command="/usr/sbin/crond"
command_args="-f -l 8 -c /etc/crontabs"
output_log="/var/log/crond.log"
error_log="/var/log/crond.err.log"
"""

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=300):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    print(f"$ {cmd[:100]}")
    lines = [ln for ln in out.splitlines() if ln.strip() and "\x1b" not in ln and "sMMs" not in ln and not ln.startswith(("`", ":", "-/+", "[4", " '", "(", ">", "###", "/_/", " .:", " '", "  `", "   `"))]
    print("\n".join(lines[-6:]))
    if err.strip() and "TERM environment" not in err:
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")
    return code, out


data = base64.b64encode(CROND_INIT.replace("\r\n", "\n").encode()).decode()

run("pct exec 105 -- sh -c 'sed -i \"/::respawn:\\/usr\\/sbin\\/crond/d\" /etc/inittab && kill -HUP 1 && sleep 1 && echo inittab-bereinigt'")
run("pct exec 105 -- sh -c 'pkill -x crond 2>/dev/null; sleep 1; true'")
run(f"pct exec 105 -- sh -c 'echo {data} | base64 -d > /etc/init.d/crond && chmod +x /etc/init.d/crond && rc-update add crond default && rc-service crond start && rc-service crond status'")
run("pct exec 105 -- sh -c 'sleep 2; ps -o pid,ppid,rss,comm; echo mem-current:; cat /sys/fs/cgroup/memory.current'")

client.close()
print("FERTIG")
