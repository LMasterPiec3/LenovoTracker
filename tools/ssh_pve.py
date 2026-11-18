#!/usr/bin/env python3
"""SSH-Helfer für die Proxmox-Deployment-Automatisierung.

Nutzung:
  PVE_SSH_PASS=... python ssh_pve.py "<befehl>"          # Befehl ausführen
  PVE_SSH_PASS=... python ssh_pve.py --put <lokal> <remote>   # Datei hochladen
  PVE_SSH_PASS=... python ssh_pve.py --put-dir <dir> <remote_dir>  # Ordner hochladen

Host/User kommen aus PVE_SSH_HOST / PVE_SSH_USER (Default: root).
Das Passwort wird nie auf Disk gespeichert.
"""

import os
import stat as stat_module
import sys
import time

import paramiko

HOST = os.environ.get("PVE_SSH_HOST", "100.82.25.128")
USER = os.environ.get("PVE_SSH_USER", "root")
PASSWORD = os.environ.get("PVE_SSH_PASS", "")


def connect():
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(HOST, username=USER, password=PASSWORD, timeout=20, look_for_keys=False, allow_agent=False)
    return client


def run(client, command, timeout=600):
    stdin, stdout, stderr = client.exec_command(command, timeout=timeout)
    out = stdout.read().decode("utf-8", "replace")
    err = stderr.read().decode("utf-8", "replace")
    code = stdout.channel.recv_exit_status()
    if out:
        print(out, end="" if out.endswith("\n") else "\n")
    if err:
        print("[stderr]", err.strip(), file=sys.stderr)
    return code


def mkdirs_p(sftp, remote_path):
    parts = remote_path.strip("/").split("/")
    current = ""
    for part in parts:
        current += "/" + part
        try:
            sftp.stat(current)
        except FileNotFoundError:
            sftp.mkdir(current)


def put(sftp, local, remote):
    sftp.put(local, remote)


def put_dir(sftp, local_dir, remote_dir):
    local_dir = os.path.abspath(local_dir)
    mkdirs_p(sftp, remote_dir)
    for root, dirs, files in os.walk(local_dir):
        rel = os.path.relpath(root, local_dir).replace("\\", "/")
        target_dir = remote_dir if rel == "." else f"{remote_dir}/{rel}"
        try:
            sftp.stat(target_dir)
        except FileNotFoundError:
            sftp.mkdir(target_dir)
        for name in files:
            if "__pycache__" in name or name.endswith(".pyc"):
                continue
            sftp.put(os.path.join(root, name), f"{target_dir}/{name}")


def main():
    args = sys.argv[1:]
    if not args:
        print("usage: ssh_pve.py \"<cmd>\" | --put <l> <r> | --put-dir <dir> <rdir>", file=sys.stderr)
        return 2
    client = connect()
    try:
        if args[0] == "--put":
            put(connect_client_sftp(client), args[1], args[2])
            print(f"OK put {args[1]} -> {args[2]}")
            return 0
        if args[0] == "--put-dir":
            put_dir(connect_client_sftp(client), args[1], args[2])
            print(f"OK put-dir {args[1]} -> {args[2]}")
            return 0
        return run(client, args[0])
    finally:
        client.close()


def connect_client_sftp(client):
    return client.open_sftp()


if __name__ == "__main__":
    # kleine Warte-Hilfe: Verbindungsfehler 2x wiederholen
    for attempt in (1, 2, 3):
        try:
            sys.exit(main())
        except paramiko.AuthenticationException:
            print("SSH-Authentifizierung fehlgeschlagen – Passwort/PermitRootLogin prüfen", file=sys.stderr)
            sys.exit(10)
        except (paramiko.SSHException, OSError) as exc:
            if attempt == 3:
                print(f"SSH-Verbindung fehlgeschlagen: {exc}", file=sys.stderr)
                sys.exit(11)
            time.sleep(2)
