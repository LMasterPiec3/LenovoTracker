#!/usr/bin/env bash
# Lenovo Clearance-Sale Mainboard-Tracker – Installation im LXC (Debian 12, als root).
# Usage: ./install.sh
set -euo pipefail

APP_DIR="/opt/lenovo-tracker"
CONF_DIR="/etc/lenovotracker"
STATE_DIR="/var/lib/lenovotracker"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "== Pakete installieren =="
echo "postfix postfix/main_mailer_type string 'Internet Site'" | debconf-set-selections
echo "postfix postfix/mailname string $(hostname --fqdn)" | debconf-set-selections
apt-get update
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  python3 python3-venv postfix ca-certificates

echo "== Dateien installieren =="
mkdir -p "$APP_DIR" "$CONF_DIR" "$STATE_DIR"
install -m 0755 "$SCRIPT_DIR/lenovo_tracker.py" "$APP_DIR/lenovo_tracker.py"
install -m 0644 "$SCRIPT_DIR/config.example.toml" "$CONF_DIR/config.toml.new"
if [ ! -f "$CONF_DIR/config.toml" ]; then
  cp "$CONF_DIR/config.toml.new" "$CONF_DIR/config.toml"
  echo "Neue Config angelegt: $CONF_DIR/config.toml (bitte prüfen/anpassen)"
else
  echo "Bestehende Config bleibt erhalten: $CONF_DIR/config.toml (Referenz: config.toml.new)"
fi

echo "== Python-Umgebung =="
python3 -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --upgrade pip >/dev/null
"$APP_DIR/venv/bin/pip" install "curl-cffi>=0.15"

echo "== systemd-Service =="
install -m 0644 "$SCRIPT_DIR/lenovo-tracker.service" /etc/systemd/system/lenovo-tracker.service
systemctl daemon-reload
systemctl enable --now lenovo-tracker.service

echo
echo "== Fertig =="
echo "  Config:      $CONF_DIR/config.toml"
echo "  Testmail:    $APP_DIR/venv/bin/python $APP_DIR/lenovo_tracker.py --config $CONF_DIR/config.toml --test-mail"
echo "  Einmal-Lauf: $APP_DIR/venv/bin/python $APP_DIR/lenovo_tracker.py --config $CONF_DIR/config.toml --once"
echo "  Logs:        journalctl -u lenovo-tracker -f"
echo "  Postfix:     default 'Internet Site' liefert direkt per DNS/MX an aomme.de (wie Proxmox)."
echo "               Falls Port-25-Ausgang blockiert ist: relayhost in /etc/postfix/main.cf setzen (siehe README)."
