#!/usr/bin/env python3
"""Datei-Upload per stdin/base64 (umgeht SFTP-Banner-Probleme).

Nutzung: PVE_SSH_PASS=... python put_via_exec.py <lokal1> [<lokal2> ...] <remote_dir>
"""

import base64
import os
import sys

import paramiko

HOST = os.environ["PVE_SSH_HOST"]  # keine Default-IP im Repo
USER = os.environ.get("PVE_SSH_USER", "root")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")


def main():
    paths = sys.argv[1:-1]
    remote_dir = sys.argv[-1]
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(HOST, username=USER, password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)
    try:
        client.exec_command(f"mkdir -p '{remote_dir}'")[1].channel.recv_exit_status()
        for local in paths:
            name = os.path.basename(local)
            data = base64.b64encode(open(local, "rb").read()).decode()
            cmd = f"base64 -d > '{remote_dir}/{name}'"
            stdin, stdout, _ = client.exec_command(cmd)
            stdin.write(data + "\n")
            stdin.channel.shutdown_write()
            code = stdout.channel.recv_exit_status()
            size = os.path.getsize(local)
            print(f"OK {name} ({size} Bytes) -> {remote_dir}/{name} (rc={code})")
    finally:
        client.close()


if __name__ == "__main__":
    sys.exit(main())
