#!/usr/bin/env bash
# ============================================================================
#  Lenovo Clearance-Sale Mainboard-Tracker – Proxmox-Helper-Setup
# ----------------------------------------------------------------------------
#  Läuft auf dem PVE-Host und richtet alles ein (im Stil der bekannten
#  Proxmox-Community-Helper-Scripts):
#
#    1. Build-CT (temporär) erstellen, Rust-Toolchain installieren,
#       Binary bauen + Tests laufen lassen, Binary auf den Host ziehen
#    2. Produktions-CT (Alpine, minimal) erstellen
#    3. Binary + curl-impersonate (Chrome-TLS-Imitation) installieren
#    4. Config (/etc/lenovotracker/config.toml) generieren
#    5. Cron (*/30) via busybox-crond (init-überwacht) einrichten
#    6. Verifikation (--dry-run gegen die Live-API)
#
#  Aufruf (auf dem PVE-Host, im Repo-Verzeichnis):
#    bash lenovo-tracker-setup.sh                 # komplettes Setup
#    SKIP_BUILD=1 bash lenovo-tracker-setup.sh    # vorhandenes Binary nutzen
#    FORCE=1 bash lenovo-tracker-setup.sh         # existierenden CT löschen+neu
#    VMID=110 BUILD_VMID=111 bash lenovo-tracker-setup.sh   # eigene IDs
#
#  Weitere Unterkommandos:
#    bash lenovo-tracker-setup.sh --probe [VMID]  # API-Parameter-Matrix testen
#    bash lenovo-tracker-setup.sh --peak [VMID]   # Peak-RAM messen (CT-Restart)
#    bash lenovo-tracker-setup.sh --uninstall     # CT + Host-Artefakte entfernen
#
#  Alle Werte sind per Umgebungsvariable übersteuerbar (siehe DEFAULTS unten).
# ============================================================================
set -euo pipefail

# --------------------------------- Farben -----------------------------------
RED='\033[1;31m'; GREEN='\033[1;32m'; YELLOW='\033[1;33m'; BLUE='\033[1;34m'; NC='\033[0m'
msg()  { echo -e "${BLUE}⏳${NC} $*"; }
ok()   { echo -e "${GREEN}✓${NC} $*"; }
warn() { echo -e "${YELLOW}⚠${NC} $*"; }
err()  { echo -e "${RED}✗${NC} $*" >&2; }
die()  { err "$*"; exit 1; }

# --------------------------------- Defaults ---------------------------------
VMID="${VMID:-105}"
BUILD_VMID="${BUILD_VMID:-104}"
CT_HOSTNAME="${CT_HOSTNAME:-lenovo-tracker-rust}"
RAM="${RAM:-32}"              # MB – Peak liegt gemessen bei ~20 MB
SWAP="${SWAP:-32}"            # MB – Überlauf-Puffer
CPULIMIT="${CPULIMIT:-0.25}"
DISK="${DISK:-local:0.05}"    # ~50 MB Rootfs (Alpine + Binaries + ~10 MB Luft)
BRIDGE="${BRIDGE:-vmbr1}"
GATEWAY="${GATEWAY:-10.10.10.1}"
BASE_IP="${BASE_IP:-10.10.10}"   # CT bekommt ${BASE_IP}.${VMID}/24
NET0="${NET0:-name=eth0,bridge=${BRIDGE},gw=${GATEWAY},ip=${BASE_IP}.${VMID}/24,type=veth}"
BUILD_RAM="${BUILD_RAM:-3072}"
BUILD_DISK="${BUILD_DISK:-local:4}"
CI_VERSION="${CI_VERSION:-v2.3.2}"   # curl-impersonate Release (musl)
CI_URL="https://github.com/lexiforest/curl-impersonate/releases/download/${CI_VERSION}/curl-impersonate-${CI_VERSION}.x86_64-linux-musl.tar.gz"

APP_DIR="/opt/lenovo-tracker"
CONF_DIR="/etc/lenovotracker"
STATE_DIR="/var/lib/lenovotracker"
LOG_FILE="/var/log/lenovo-tracker.log"
HOST_BIN="/root/lenovo-tracker-rs"

MAIL_FROM="${MAIL_FROM:-Lenovo@aomme.de}"
MAIL_TO="${MAIL_TO:-Lenovo@aomme.de}"
EXTRA_RECIPIENTS="${EXTRA_RECIPIENTS:-}"
SUBJECT_PREFIX="${SUBJECT_PREFIX:-[Lenovo Tracker]}"
SMTP_HOST="${SMTP_HOST:-mail.protonmail.ch}"
SMTP_PORT="${SMTP_PORT:-25}"
COMMODITY_KEYS="${COMMODITY_KEYS:-system boards}"
KEYWORDS="${KEYWORDS:-}"     # Komma-Liste, z. B. "RTX" oder "RTX,B650" – leer = alle Mainboards
FORCE="${FORCE:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# ------------------------------- Hilfsfunktionen -----------------------------
template_path() {
  ls /var/lib/vz/template/cache/alpine-*.tar.* 2>/dev/null | sort | tail -1
}

toml_array() {
  # "a,b,c" -> '["a", "b", "c"]', leer -> "[]"
  local in="${1// /}"
  if [[ -z "$in" ]]; then echo "[]"; return; fi
  local out=""
  IFS=',' read -ra parts <<< "$in"
  for p in "${parts[@]}"; do
    [[ -z "$p" ]] && continue
    out+=", \"${p}\""
  done
  echo "[${out#, }]"
}

wait_ct() {
  local id="$1" i=0
  until pct exec "$id" -- true >/dev/null 2>&1; do
    sleep 2; i=$((i+2))
    if [[ $i -ge 60 ]]; then die "CT $id startet nicht (60 s Timeout)"; fi
  done
}

ct_download() {  # curl- oder wget-Fallback auf dem Host
  local url="$1" out="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -sSfL -o "$out" "$url"
  else
    wget -qO "$out" "$url"
  fi
}

prereq() {
  [[ $EUID -eq 0 ]] || die "Bitte als root ausführen (pct braucht root)."
  command -v pct >/dev/null 2>&1 || die "pct nicht gefunden – das Script gehört auf einen Proxmox-VE-Host."
  [[ -f "${REPO_DIR}/rust-tracker/Cargo.toml" && -f "${REPO_DIR}/rust-tracker/src/main.rs" ]] \
    || die "Repo unvollständig: ${REPO_DIR}/rust-tracker/{Cargo.toml,src/main.rs} fehlt."
  local tpl; tpl="$(template_path)"
  [[ -n "$tpl" ]] || die "Kein Alpine-Template in /var/lib/vz/template/cache/ – erst laden (pveam)."
  echo "$tpl"
}

# ------------------------------- Unterkommandos ------------------------------
cmd_setup() {
  local tpl; tpl="$(prereq)"
  echo -e "${BLUE}"
  echo "  ╔══════════════════════════════════════════════════════════╗"
  echo "  ║   Lenovo Clearance-Tracker – Proxmox-Setup               ║"
  echo "  ╚══════════════════════════════════════════════════════════╝"
  echo -e "${NC}"
  msg "CT ${VMID} (${CT_HOSTNAME}) · Build-CT ${BUILD_VMID} · RAM ${RAM} MB · Disk ${DISK}"
  msg "Mail: ${MAIL_FROM} → ${MAIL_TO} via ${SMTP_HOST}:${SMTP_PORT}"
  msg "Filter: commodity=[${COMMODITY_KEYS}] keywords=[${KEYWORDS:-alle}]"

  # ---------- 1) Binary bauen ----------
  if [[ "$SKIP_BUILD" == "1" && -f "$HOST_BIN" ]]; then
    ok "SKIP_BUILD=1 – nutze vorhandenes Binary ${HOST_BIN}"
  else
    msg "1/6  Build-CT ${BUILD_VMID} vorbereiten"
    pct destroy "${BUILD_VMID}" --purge >/dev/null 2>&1 || true
    pct create "${BUILD_VMID}" "$tpl" \
      --hostname rust-build --memory "${BUILD_RAM}" --cores 4 --swap 512 \
      --net0 "${NET0/${VMID}/${BUILD_VMID}}" \
      --rootfs "${BUILD_DISK}" --unprivileged 1 --start 1 >/dev/null
    wait_ct "${BUILD_VMID}"
    ok "Build-CT läuft"

    msg "1/6  Rust-Toolchain installieren (kein OpenSSL/cmake/go nötig)"
    pct exec "${BUILD_VMID}" -- sh -c \
      'apk update -q && apk add -q rust cargo build-base pkgconf musl-dev ca-certificates' \
      >/dev/null
    ok "Toolchain bereit"

    msg "1/6  Quellen übertragen + bauen (12 Unit-Tests inklusive)"
    pct exec "${BUILD_VMID}" -- mkdir -p /build/src
    pct push "${BUILD_VMID}" "${REPO_DIR}/rust-tracker/Cargo.toml" /build/Cargo.toml
    pct push "${BUILD_VMID}" "${REPO_DIR}/rust-tracker/src/main.rs" /build/src/main.rs
    pct exec "${BUILD_VMID}" -- sh -c 'cd /build && cargo build --release 2>&1 | tail -1'
    pct exec "${BUILD_VMID}" -- sh -c 'cd /build && cargo test --release 2>&1 | grep "test result"'
    pct pull "${BUILD_VMID}" /build/target/release/lenovo-tracker "$HOST_BIN"
    command -v strip >/dev/null 2>&1 && strip -s "$HOST_BIN" || true
    pct destroy "${BUILD_VMID}" --purge >/dev/null
    ok "Binary gebaut: ${HOST_BIN} ($(stat -c %s "$HOST_BIN") B) – Build-CT entfernt"
  fi

  # ---------- 2) Produktions-CT ----------
  msg "2/6  Produktions-CT ${VMID} anlegen"
  if pct status "${VMID}" >/dev/null 2>&1; then
    if [[ "$FORCE" == "1" ]]; then
      warn "CT ${VMID} existiert – FORCE=1: stoppe + lösche"
      pct stop "${VMID}" >/dev/null 2>&1 || true
      pct destroy "${VMID}" --purge
    else
      die "CT ${VMID} existiert bereits. FORCE=1 für Neuaufbau (State geht verloren!) oder VMID ändern."
    fi
  fi
  pct create "${VMID}" "$tpl" \
    --hostname "${CT_HOSTNAME}" --memory "${RAM}" --swap "${SWAP}" --cpulimit "${CPULIMIT}" \
    --net0 "$NET0" --rootfs "${DISK}" --unprivileged 1 --onboot 1 \
    --tags lenovo-tracker --start 1 >/dev/null
  sleep 3; wait_ct "${VMID}"
  ok "CT ${VMID} läuft"

  # ---------- 3) Binary + curl-impersonate ----------
  msg "3/6  Binary + curl-impersonate installieren"
  pct exec "${VMID}" -- mkdir -p "$APP_DIR" "$CONF_DIR" "$STATE_DIR"
  pct push "${VMID}" "$HOST_BIN" "${APP_DIR}/lenovo-tracker"
  pct exec "${VMID}" -- chmod +x "${APP_DIR}/lenovo-tracker"
  local ci_tar="/tmp/curl-impersonate-${CI_VERSION}.tar.gz"
  msg "3/6  curl-impersonate ${CI_VERSION} laden"
  ct_download "$CI_URL" "$ci_tar"
  rm -rf /tmp/ci-extract && mkdir -p /tmp/ci-extract
  tar xzf "$ci_tar" -C /tmp/ci-extract
  local ci_bin
  ci_bin="$(find /tmp/ci-extract -name 'curl-impersonate' -type f | head -1)"
  [[ -n "$ci_bin" ]] || die "curl-impersonate-Binary nicht im Tarball gefunden"
  pct push "${VMID}" "$ci_bin" "${APP_DIR}/curl-impersonate"
  pct exec "${VMID}" -- chmod +x "${APP_DIR}/curl-impersonate"
  rm -rf /tmp/ci-extract "$ci_tar"
  pct exec "${VMID}" -- "${APP_DIR}/curl-impersonate" --version >/dev/null 2>&1 \
    && ok "curl-impersonate einsatzbereit" || warn "curl-impersonate --version schlug fehl – prüfen!"

  # ---------- 4) Config ----------
  msg "4/6  Config generieren (${CONF_DIR}/config.toml)"
  local cfg="/root/lenovotracker-config.toml"
  cat > "$cfg" <<EOF
# Lenovo Clearance-Tracker – generiert von lenovo-tracker-setup.sh
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
referer = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
timeout_seconds = 60
retries = 3
curl_path = "${APP_DIR}/curl-impersonate"
impersonate_target = "chrome124"

[tracker]
interval_minutes = 30        # dokumentarisch – cron steuert das Intervall
commodity_keys = $(toml_array "$COMMODITY_KEYS")
name_keywords = $(toml_array "$KEYWORDS")

[mail]
mode = "smtp"
from = "${MAIL_FROM}"
to = $(toml_array "$MAIL_TO")
extra_recipients = $(toml_array "$EXTRA_RECIPIENTS")
subject_prefix = "${SUBJECT_PREFIX}"
smtp_host = "${SMTP_HOST}"
smtp_port = ${SMTP_PORT}

[paths]
state_dir = "${STATE_DIR}"
EOF
  pct push "${VMID}" "$cfg" "${CONF_DIR}/config.toml"
  rm -f "$cfg"
  ok "Config installiert"

  # ---------- 5) Cron + Logging ----------
  msg "5/6  Cron (*/30) + Logging einrichten"
  pct exec "${VMID}" -- sh -c \
    "grep -q 'lenovo-tracker' /etc/crontabs/root 2>/dev/null || echo '*/30 * * * * ${APP_DIR}/lenovo-tracker --config ${CONF_DIR}/config.toml --once >> ${LOG_FILE} 2>&1' >> /etc/crontabs/root"
  pct exec "${VMID}" -- sh -c \
    "grep -q 'crond -f' /etc/inittab || echo '::respawn:/usr/sbin/crond -f -l 8 -c /etc/crontabs' >> /etc/inittab"
  pct exec "${VMID}" -- touch "$LOG_FILE"
  # init re-read inittab (startet crond, falls noch nicht laufend)
  pct exec "${VMID}" -- sh -c 'kill -HUP 1 2>/dev/null || true' >/dev/null 2>&1 || true
  ok "crond läuft init-überwacht (::respawn), Intervall */30"

  # ---------- 6) Verifikation ----------
  msg "6/6  Verifikation (--dry-run gegen die Live-API)"
  local dry
  dry="$(pct exec "${VMID}" -- "${APP_DIR}/lenovo-tracker" --config "${CONF_DIR}/config.toml" --dry-run 2>&1 || true)"
  if echo "$dry" | grep -q "DRY-RUN"; then
    ok "$dry"
  else
    warn "Dry-Run lieferte keine Daten (Akamai 403? Netzwerk?): $dry"
    warn "Der Cron versucht es alle 30 Min erneut – Log: pct exec ${VMID} -- tail ${LOG_FILE}"
  fi

  pct set "${VMID}" --onboot 1 --tags lenovo-tracker >/dev/null
  echo
  echo -e "${GREEN}  ╔══════════════════════════════════════════════════════════╗${NC}"
  echo -e "${GREEN}  ║   Setup fertig – Tracker läuft                           ║${NC}"
  echo -e "${GREEN}  ╚══════════════════════════════════════════════════════════╝${NC}"
  echo
  echo "   CT ${VMID} · Cron */30 · State: ${STATE_DIR}/state.json · Log: ${LOG_FILE}"
  echo "   Config:  pct exec ${VMID} -- vi ${CONF_DIR}/config.toml"
  echo "   Test-Mail: pct exec ${VMID} -- ${APP_DIR}/lenovo-tracker --config ${CONF_DIR}/config.toml --test-mail"
  echo "   Peak messen: bash $0 --peak ${VMID}"
  echo "   Erste Baseline wird beim ersten Cron-Lauf still gesetzt (keine Mail)."
  echo
}

cmd_probe() {
  local id="${2:-$VMID}"
  local CURLBIN="${APP_DIR}/curl-impersonate"
  local BASE="https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
  local REF="https://support.lenovo.com/de/de/parts-lookup/clearance-sale"
  local BODY="/tmp/probe-body.json"
  echo "== API-Parameter-Matrix via CT ${id} (Referenz: keine Params) =="
  probe_one() {
    local label="$1" url="$2"
    local st size parts boards
    st="$(pct exec "$id" -- sh -c "${CURLBIN} --impersonate chrome124 --compressed -s --max-time 90 -o ${BODY} -w '%{http_code}' -H 'Accept: application/json' -H 'Referer: ${REF}' '${url}' 2>/dev/null")"
    size="$(pct exec "$id" -- sh -c "wc -c < ${BODY} 2>/dev/null || echo 0")"
    parts="$(pct exec "$id" -- sh -c "grep -o '\"commodityKey\"' ${BODY} 2>/dev/null | wc -l")"
    boards="$(pct exec "$id" -- sh -c "grep -o '\"system boards\"' ${BODY} 2>/dev/null | wc -l")"
    printf "  %-34s HTTP=%s  %8s B  Teile=%-5s Boards=%s\n" "$label" "$st" "$size" "$parts" "$boards"
  }
  probe_one "baseline" "$BASE"
  probe_one "keyword=system boards" "$BASE?keyword=system%20boards"
  probe_one "q=system boards" "$BASE?q=system%20boards"
  probe_one "commodityKey=system boards" "$BASE?commodityKey=system%20boards"
  probe_one "page=1" "$BASE?page=1"
  probe_one "pageSize=10" "$BASE?pageSize=10"
  pct exec "$id" -- rm -f "$BODY"
  echo "(alle Zeilen identisch = API ignoriert Parameter → Client-Filter nötig)"
}

cmd_peak() {
  local id="${2:-$VMID}"
  local CG="/sys/fs/cgroup/lxc/${id}"
  [[ -f "${CG}/memory.peak" ]] || die "Cgroup-Pfad ${CG}/memory.peak nicht gefunden"
  warn "CT ${id} wird neu gestartet (nullt den Cgroup-Peak) – kurze Unterbrechung"
  pct stop "$id" && pct start "$id" && sleep 5
  echo "== Peak-RAM-Messung (3 Läufe, Produktionsconfig) =="
  local peak max=0
  for i in 1 2 3; do
    pct exec "$id" -- /opt/lenovo-tracker/lenovo-tracker --config /etc/lenovotracker/config.toml --once 2>&1 | tail -1 || true
    peak="$(cat "${CG}/memory.peak")"
    [[ "$peak" -gt "$max" ]] && max="$peak"
    echo "  Lauf $i: Peak kumulativ = $((peak/1024)) KB"
  done
  echo "  MAX: $((max/1024)) KB = $((max/1024/1024)) MB"
  echo "(Neustart des CT setzt den Peak wieder auf 0 – Werte gelten für die gemessenen Läufe)"
}

cmd_uninstall() {
  prereq >/dev/null
  if [[ "$FORCE" != "1" ]]; then
    warn "Löscht CT ${VMID} inkl. State/History und Host-Binaries."
    read -rp "Wirklich? (ja/NO): " a
    [[ "$a" == "ja" ]] || { echo "Abbruch."; return; }
  fi
  pct stop "${VMID}" >/dev/null 2>&1 || true
  pct destroy "${VMID}" --purge && ok "CT ${VMID} entfernt"
  rm -f /root/lenovo-tracker-rs /root/lenovo-tracker-rs2 /root/lt-new \
        /root/curl-impersonate /root/curl-impersonate-check /root/lenovotracker-config.toml
  ok "Host-Artefakte entfernt"
}

cmd_help() {
  sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

case "${1:-setup}" in
  setup)        cmd_setup ;;
  --probe)      cmd_probe "${2:-$VMID}" ;;
  --peak)       cmd_peak "${2:-$VMID}" ;;
  --uninstall)  cmd_uninstall ;;
  --help|-h)    cmd_help ;;
  *)            err "Unbekanntes Kommando: $1 (siehe --help)"; exit 1 ;;
esac
