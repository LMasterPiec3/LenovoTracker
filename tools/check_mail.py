#!/usr/bin/env python3
"""Mailqueue + journalctl-Postfix-Einträge des Containers prüfen."""

import os

import paramiko

HOST = os.environ.get("PVE_SSH_HOST", "100.82.25.128")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")

client = paramiko.SSHClient()
client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
client.connect(HOST, username="root", password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)


def run(cmd, timeout=60):
    stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    print(f"\n$ {cmd[:100]}")
    print(out[-3000:])
    if err.strip():
        print("[stderr]", err.strip()[-300:])
    print(f"[rc={code}]")


run("pct exec 101 -- mailq | head -10")
run("pct exec 101 -- journalctl --no-pager -n 40 | grep -E 'postfix|smtp' | tail -20")
client.close()
