#!/usr/bin/env bash
# ============================================================================
#  Lenovo Clearance-Sale Mainboard-Tracker – Proxmox-Helper-Setup
# ----------------------------------------------------------------------------
#  Im Stil der Proxmox-Community-Helper-Scripts: interaktives Menü, Spinner,
#  Settings-Box – läuft auf dem PVE-Host und richtet alles ein:
#
#    1. Quellen laden (lokales Repo bevorzugt, sonst GitHub raw)
#    2. Build-CT (temporär) – Rust-Toolchain, Binary, 12 Unit-Tests
#    3. Produktions-CT (Alpine, minimal)
#    4. Binary + curl-impersonate (Chrome-TLS-Imitation)
#    5. Config /etc/lenovotracker/config.toml
#    6. Cron (*/30, init-überwacht) + Dry-Run-Verifikation
#
#  Der Klassiker (auf dem Proxmox-Host):
#    bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
#
#  Unterkommandos (auch im One-Liner):
#    … --probe [VMID]    API-Parameter-Matrix live testen
#    … --peak [VMID]     Peak-RAM messen (CT-Restart)
#    … --uninstall       CT + Host-Artefakte entfernen
#    … --help            Usage
#
#  Automatisierung: NONINTERACTIVE=1 (keine Prompts, nur Env/Defaults)
#  Env-Overrides: VMID BUILD_VMID RAM SWAP CPULIMIT DISK NET0 MAIL_FROM …
#
#  Lizenz: MIT – free to use (siehe LICENSE im Repo).
#  Copyright (c) 2026 LMasterPiec3
# ============================================================================
set -uo pipefail

# --------------------------------- Farben -----------------------------------
# $'…' = echte ESC-Bytes in der Variable → funktionieren in echo, printf UND
# read -rp (kein echo -e nötig, keine rohen \033 im Output).
RED=$'\033[1;31m'; GREEN=$'\033[1;32m'; YELLOW=$'\033[1;33m'; BLUE=$'\033[1;34m'
BOLD=$'\033[1m'; NC=$'\033[0m'
C_INFO=$BLUE; C_OK=$GREEN; C_ERR=$RED; C_WARN=$YELLOW; C_HEAD=$BOLD

msg_info()  { echo -e "  ${C_INFO}⏳ ${*}${NC}"; }
msg_ok()    { echo -e "  ${C_OK}✔ ${*}${NC}"; }
msg_error() { echo -e "  ${C_ERR}✗ ${*}${NC}" >&2; }
msg_warn()  { echo -e "  ${C_WARN}⚠ ${*}${NC}"; }
die()       { msg_error "$*"; exit 1; }

LAST_LOG="/tmp/.lenovo-setup-last.log"

# Spinner um einen Hintergrund-Job (Output → $LAST_LOG, bei Fehler Tail-Ausgabe)
spin_run() {
  local desc="$1"; shift
  printf "  ${C_INFO}⏳ %s ...${NC}" "$desc"
  local rc=0
  ( "$@" ) >"$LAST_LOG" 2>&1 &
  local pid=$!
  local spin='|/-\' i=0
  while kill -0 "$pid" 2>/dev/null; do
    printf "\r  ${C_INFO}⏳ %s %s${NC}   " "$desc" "${spin:$((i % 4)):1}"
    i=$((i + 1)); sleep 0.12
  done
  wait "$pid" 2>/dev/null; rc=$?
  if [[ $rc -eq 0 ]]; then
    printf "\r  ${C_OK}✔ %s${NC}                          \n" "$desc"
  else
    printf "\r  ${C_ERR}✗ %s (rc=%s)${NC}                          \n" "$desc" "$rc"
    tail -6 "$LAST_LOG" | sed 's/^/      /'
  fi
  return $rc
}

# --------------------------------- Defaults ---------------------------------
VMID="${VMID:-105}"
BUILD_VMID="${BUILD_VMID:-104}"
CT_HOSTNAME="${CT_HOSTNAME:-lenovo-tracker-rust}"
RAM="${RAM:-32}"
SWAP="${SWAP:-32}"
CPULIMIT="${CPULIMIT:-0.25}"
DISK="${DISK:-local:0.05}"
BRIDGE="${BRIDGE:-vmbr1}"
GATEWAY="${GATEWAY:-10.10.10.1}"
BASE_IP="${BASE_IP:-10.10.10}"
NET0="${NET0:-name=eth0,bridge=${BRIDGE},gw=${GATEWAY},ip=${BASE_IP}.${VMID}/24,type=veth}"
BUILD_RAM="${BUILD_RAM:-3072}"
BUILD_DISK="${BUILD_DISK:-local:4}"
CI_VERSION="${CI_VERSION:-v2.3.2}"
CI_URL="https://github.com/lexiforest/curl-impersonate/releases/download/${CI_VERSION}/curl-impersonate-${CI_VERSION}.x86_64-linux-musl.tar.gz"

APP_DIR="/opt/lenovo-tracker"
CONF_DIR="/etc/lenovotracker"
STATE_DIR="/var/lib/lenovotracker"
LOG_FILE="/var/log/lenovo-tracker.log"
HOST_BIN="/root/lenovo-tracker-rs"

REPO_RAW="${REPO_RAW:-https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/${REPO_REF:-main}}"
MAIL_FROM="${MAIL_FROM:-Lenovo@aomme.de}"
MAIL_TO="${MAIL_TO:-Lenovo@aomme.de}"
EXTRA_RECIPIENTS="${EXTRA_RECIPIENTS:-}"
SUBJECT_PREFIX="${SUBJECT_PREFIX:-[Lenovo Tracker]}"
SMTP_HOST="${SMTP_HOST:-mail.protonmail.ch}"
SMTP_PORT="${SMTP_PORT:-25}"
COMMODITY_KEYS="${COMMODITY_KEYS:-system boards}"
KEYWORDS="${KEYWORDS:-}"
FORCE="${FORCE:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
NONINTERACTIVE="${NONINTERACTIVE:-0}"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-${0}}")" 2>/dev/null && pwd || echo "")"

# ------------------------------- Hilfsfunktionen -----------------------------
template_path() {
  ls /var/lib/vz/template/cache/alpine-*.tar.* 2>/dev/null | sort | tail -1
}

toml_array() {
  local in="${1// /}" out="" p
  if [[ -z "$in" ]]; then echo "[]"; return; fi
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
    if [[ $i -ge 60 ]]; then return 1; fi
  done
  return 0
}

ct_download() {
  local url="$1" out="$2"
  if command -v curl >/dev/null 2>&1; then curl -sSfL -o "$out" "$url"; else wget -qO "$out" "$url"; fi
}

ask() {  # ask VARNAME "Prompt" "Default"  (non-tty → Default)
  local input=""
  if [[ -t 0 ]]; then
    read -rp "  ${C_INFO}→ ${2} [${3}]: ${NC}" input || input=""
  fi
  input="${input:-$3}"
  printf -v "$1" '%s' "$input"
}

header_info() {
  clear
  echo -e "${GREEN}"
  cat <<'BANNER'
 _                          _____                    _
| |                        |_   _|                  | |
| |     ___  _ __   _   _    | |_  _ ____   ____ _  | | __ _ _   _
| |    / _ \| '_ \ | | | |   | | | | |_  / / _  |  | |/ _  | | | |
| |___| (_) | | | || |_| |  _| | |_| |/ / | (_| |  | | (_| | |_| |
\_____|\___/|_| |_|\__, |  \___/\__,_/____|\__,_|  |_|\__,_|\__, |
                   |___/                                    |___/
BANNER
  echo -e "${NC}  ${C_HEAD}Clearance-Sale Mainboard-Tracker${NC} – Proxmox-Setup  ${C_OK}(MIT, free to use)${NC}"
  echo
}

prereq() {
  [[ $EUID -eq 0 ]] || die "Bitte als root ausführen (pct braucht root)."
  command -v pct >/dev/null 2>&1 || die "pct nicht gefunden – das Script gehört auf einen Proxmox-VE-Host."
  local tpl; tpl="$(template_path)"
  [[ -n "$tpl" ]] || die "Kein Alpine-Template in /var/lib/vz/template/cache/ – erst laden (pveam)."
  echo "$tpl"
}

# Quellen: lokales Repo bevorzugt, sonst von GitHub raw laden
# (damit der curl-One-Liner ohne Repo-Klon funktioniert).
SRC_CARGO=""; SRC_MAIN=""
fetch_sources() {
  if [[ -f "${REPO_DIR}/rust-tracker/Cargo.toml" && -f "${REPO_DIR}/rust-tracker/src/main.rs" ]]; then
    SRC_CARGO="${REPO_DIR}/rust-tracker/Cargo.toml"
    SRC_MAIN="${REPO_DIR}/rust-tracker/src/main.rs"
    msg_ok "Quellen: lokales Repo (${REPO_DIR})"
    return
  fi
  local tmp="/root/.lenovo-tracker-src"
  msg_info "Quellen von GitHub laden (${REPO_RAW})"
  rm -rf "$tmp" && mkdir -p "$tmp"
  ct_download "${REPO_RAW}/rust-tracker/Cargo.toml" "${tmp}/Cargo.toml" || die "Cargo.toml nicht ladbar von ${REPO_RAW}"
  ct_download "${REPO_RAW}/rust-tracker/src/main.rs" "${tmp}/main.rs" || die "main.rs nicht ladbar von ${REPO_RAW}"
  [[ -s "${tmp}/Cargo.toml" && -s "${tmp}/main.rs" ]] \
    || die "Quellen nicht ladbar von ${REPO_RAW} – Repo public? REPO_RAW/REPO_REF prüfen."
  SRC_CARGO="${tmp}/Cargo.toml"
  SRC_MAIN="${tmp}/main.rs"
  msg_ok "Quellen geladen (main.rs $(stat -c %s "$SRC_MAIN") B)"
}

# ------------------------------- Whiptail-TUI --------------------------------
# Wie die Community-Scripts: Dialoge mit whiptail (blauer Fullscreen, Menü mit
# Pfeiltasten/TAB, InputBoxen, YesNo). Fallback ohne whiptail/nicht-tty:
# Defaults aus den Env-Variablen (NONINTERACTIVE-Verhalten).
BACKTITLE="Lenovo-Tracker Setup · MIT (free to use)"

ui_available() {
  command -v whiptail >/dev/null 2>&1 && [[ -t 0 || -t 1 ]] && [[ "$NONINTERACTIVE" != "1" ]]
}

wt_menu() {  # wt_menu <title> <text> item1 desc1 [item2 desc2 …] -> echo Auswahl
  local title="$1" text="$2"; shift 2
  whiptail --backtitle "$BACKTITLE" --title "$title" --menu "$text" 20 72 8 "$@" \
    --ok-button "<Select>" --cancel-button "<Exit Script>" \
    3>&1 1>&2 2>&3
}

wt_input() {  # wt_input <title> <prompt> <default> -> echo Wert (ESC/Cancel → leer)
  whiptail --backtitle "$BACKTITLE" --title "$1" --inputbox "$2" 10 70 "$3" \
    3>&1 1>&2 2>&3
}

wt_yesno() {  # wt_yesno <title> <text> -> rc 0=Ja 1=Nein
  whiptail --backtitle "$BACKTITLE" --title "$1" --yesno "$2" 14 70
}

ui_advanced() {
  local v
  v="$(wt_input "CT-ID (Produktion)" "Welche CT-ID soll der Tracker bekommen?" "$VMID")" || exit 0
  VMID="${v:-$VMID}"
  v="$(wt_input "CT-ID (Build)" "Temporärer Build-Container:" "$BUILD_VMID")" || exit 0
  BUILD_VMID="${v:-$BUILD_VMID}"
  v="$(wt_input "Hostname" "Hostname des Tracker-CTs:" "$CT_HOSTNAME")" || exit 0
  CT_HOSTNAME="${v:-$CT_HOSTNAME}"
  v="$(wt_input "RAM" "RAM in MB (Peak gemessen ~20 MB):" "$RAM")" || exit 0
  RAM="${v:-$RAM}"
  v="$(wt_input "Swap" "Swap in MB (Überlauf-Puffer):" "$SWAP")" || exit 0
  SWAP="${v:-$SWAP}"
  v="$(wt_input "CPU-Limit" "cpulimit (0.25 = Viertel-Kern):" "$CPULIMIT")" || exit 0
  CPULIMIT="${v:-$CPULIMIT}"
  v="$(wt_input "Rootfs" "Disk-Größe:" "$DISK")" || exit 0
  DISK="${v:-$DISK}"
  v="$(wt_input "Bridge" "Proxmox-Bridge:" "$BRIDGE")" || exit 0
  BRIDGE="${v:-$BRIDGE}"
  v="$(wt_input "IP-Basis" "CT bekommt ${BASE_IP}.<VMID>:" "$BASE_IP")" || exit 0
  BASE_IP="${v:-$BASE_IP}"
  NET0="name=eth0,bridge=${BRIDGE},gw=${GATEWAY},ip=${BASE_IP}.${VMID}/24,type=veth"
  v="$(wt_input "Mail von" "Absender-Adresse:" "$MAIL_FROM")" || exit 0
  MAIL_FROM="${v:-$MAIL_FROM}"
  v="$(wt_input "Mail an" "Empfänger-Adresse:" "$MAIL_TO")" || exit 0
  MAIL_TO="${v:-$MAIL_TO}"
  v="$(wt_input "SMTP-Host" "SMTP-Server (Plain-SMTP :25):" "$SMTP_HOST")" || exit 0
  SMTP_HOST="${v:-$SMTP_HOST}"
  v="$(wt_input "SMTP-Port" "SMTP-Port:" "$SMTP_PORT")" || exit 0
  SMTP_PORT="${v:-$SMTP_PORT}"
  v="$(wt_input "Commodity-Keys" "Komma-getrennt (lowercase):" "$COMMODITY_KEYS")" || exit 0
  COMMODITY_KEYS="${v:-$COMMODITY_KEYS}"
  v="$(wt_input "Keyword-Filter" "Komma-getrennt, leer = alle Mainboards\nBeispiel: RTX  oder  B650,AM5" "$KEYWORDS")" || exit 0
  KEYWORDS="${v//$'\n'/}"
}

ui_summary_confirm() {
  local text="CT: ${VMID} (${CT_HOSTNAME}) · Build-CT: ${BUILD_VMID}
RAM: ${RAM} MB + ${SWAP} MB Swap · CPU-Limit: ${CPULIMIT} · Disk: ${DISK}
Netz: ${BRIDGE} · ${BASE_IP}.${VMID}/24

Mail: ${MAIL_FROM} → ${MAIL_TO}
SMTP: ${SMTP_HOST}:${SMTP_PORT}

Filter: ${COMMODITY_KEYS}
Keywords: ${KEYWORDS:-alle}"
  wt_yesno "Setup starten" "$text"
}

ui_ct_exists() {  # rc 0 = neu aufbauen
  wt_yesno "CT ${VMID} existiert" "CT ${VMID} läuft bereits.\n\nLöschen und NEU aufbauen?\n(State/History gehen verloren)" \
    || die "Abgebrochen – CT ${VMID} bleibt unangetastet."
}

# ------------------------------- Unterkommandos ------------------------------
cmd_setup() {
  local tpl; tpl="$(prereq)"

  if ui_available; then
    local sel
    sel="$(wt_menu "Lenovo-Tracker" "Choose an option:\nUse TAB or Arrow keys to navigate, ENTER to select." \
      "default"  "Standard-Install (Defaults)" \
      "advanced" "Advanced Install (alles abfragen)" \
      "probe"    "API-Probe (Live-Test der Parameter-Matrix)" \
      "peak"     "Peak-RAM-Messung (CT-Restart)" \
      "uninstall" "Deinstallieren" \
      "exit"     "Beenden")" || exit 0
    case "$sel" in
      probe)     cmd_probe "$VMID"; exit 0 ;;
      peak)      cmd_peak "$VMID"; exit 0 ;;
      uninstall) cmd_uninstall; exit 0 ;;
      exit)      exit 0 ;;
      advanced)  ui_advanced ;;
    esac
    ui_summary_confirm || die "Abgebrochen."
    clear
    header_info
  else
    # Fallback (kein whiptail / Automation): Defaults, keine Dialoge
    header_info
    msg_warn "Whiptail/TTY nicht verfügbar – fahre mit Env-Defaults fort (NONINTERACTIVE)."
  fi

  fetch_sources

  # ---------- 1) Binary bauen ----------
  if [[ "$SKIP_BUILD" == "1" && -f "$HOST_BIN" ]]; then
    msg_ok "SKIP_BUILD=1 – nutze vorhandenes Binary ${HOST_BIN}"
  else
    msg_info "1/6  Build-CT ${BUILD_VMID} vorbereiten"
    pct destroy "${BUILD_VMID}" --purge >/dev/null 2>&1 || true
    spin_run "Build-CT ${BUILD_VMID} anlegen" pct create "${BUILD_VMID}" "$tpl" \
      --hostname rust-build --memory "${BUILD_RAM}" --cores 4 --swap 512 \
      --net0 "${NET0/${VMID}/${BUILD_VMID}}" \
      --rootfs "${BUILD_DISK}" --unprivileged 1 --start 1
    if ! wait_ct "${BUILD_VMID}"; then die "Build-CT startet nicht"; fi
    msg_ok "Build-CT läuft"

    spin_run "Rust-Toolchain installieren (kein OpenSSL/cmake/go)" \
      pct exec "${BUILD_VMID}" -- sh -c 'apk update -q && apk add -q rust cargo build-base pkgconf musl-dev ca-certificates'

    msg_info "1/6  Quellen übertragen + bauen (12 Unit-Tests inklusive)"
    pct exec "${BUILD_VMID}" -- mkdir -p /build/src
    pct push "${BUILD_VMID}" "$SRC_CARGO" /build/Cargo.toml
    pct push "${BUILD_VMID}" "$SRC_MAIN" /build/src/main.rs
    spin_run "cargo build --release" pct exec "${BUILD_VMID}" -- sh -c 'cd /build && cargo build --release'
    spin_run "cargo test --release (12 Tests)" pct exec "${BUILD_VMID}" -- sh -c 'cd /build && cargo test --release 2>&1 | grep "test result"'
    pct pull "${BUILD_VMID}" /build/target/release/lenovo-tracker "$HOST_BIN"
    command -v strip >/dev/null 2>&1 && strip -s "$HOST_BIN" || true
    pct destroy "${BUILD_VMID}" --purge >/dev/null
    msg_ok "Binary gebaut: ${HOST_BIN} ($(stat -c %s "$HOST_BIN") B) – Build-CT entfernt"
  fi

  # ---------- 2) Produktions-CT ----------
  msg_info "2/6  Produktions-CT ${VMID} anlegen"
  if pct status "${VMID}" >/dev/null 2>&1; then
    if [[ "$FORCE" == "1" ]]; then
      msg_warn "CT ${VMID} existiert – FORCE=1: stoppe + lösche"
      pct stop "${VMID}" >/dev/null 2>&1 || true
      pct destroy "${VMID}" --purge
    elif ui_available; then
      ui_ct_exists
      pct stop "${VMID}" >/dev/null 2>&1 || true
      pct destroy "${VMID}" --purge
    else
      die "CT ${VMID} existiert bereits. FORCE=1 für Neuaufbau (State geht verloren!) oder VMID ändern."
    fi
  fi
  spin_run "CT ${VMID} erstellen" pct create "${VMID}" "$tpl" \
    --hostname "${CT_HOSTNAME}" --memory "${RAM}" --swap "${SWAP}" --cpulimit "${CPULIMIT}" \
    --net0 "$NET0" --rootfs "${DISK}" --unprivileged 1 --onboot 1 \
    --tags lenovo-tracker --start 1
  sleep 3
  if ! wait_ct "${VMID}"; then die "CT ${VMID} startet nicht"; fi
  msg_ok "CT ${VMID} läuft"

  # ---------- 3) Binary + curl-impersonate ----------
  msg_info "3/6  Binary + curl-impersonate installieren"
  pct exec "${VMID}" -- mkdir -p "$APP_DIR" "$CONF_DIR" "$STATE_DIR"
  pct push "${VMID}" "$HOST_BIN" "${APP_DIR}/lenovo-tracker"
  pct exec "${VMID}" -- chmod +x "${APP_DIR}/lenovo-tracker"
  local ci_tar="/tmp/curl-impersonate-${CI_VERSION}.tar.gz"
  spin_run "curl-impersonate ${CI_VERSION} laden" ct_download "$CI_URL" "$ci_tar"
  rm -rf /tmp/ci-extract && mkdir -p /tmp/ci-extract
  tar xzf "$ci_tar" -C /tmp/ci-extract
  local ci_bin
  ci_bin="$(find /tmp/ci-extract -name 'curl-impersonate' -type f | head -1)"
  [[ -n "$ci_bin" ]] || die "curl-impersonate-Binary nicht im Tarball gefunden"
  pct push "${VMID}" "$ci_bin" "${APP_DIR}/curl-impersonate"
  pct exec "${VMID}" -- chmod +x "${APP_DIR}/curl-impersonate"
  rm -rf /tmp/ci-extract "$ci_tar"
  if pct exec "${VMID}" -- "${APP_DIR}/curl-impersonate" --version >/dev/null 2>&1; then
    msg_ok "curl-impersonate einsatzbereit"
  else
    msg_warn "curl-impersonate --version schlug fehl – prüfen!"
  fi

  # ---------- 4) Config ----------
  msg_info "4/6  Config generieren (${CONF_DIR}/config.toml)"
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
  msg_ok "Config installiert"

  # ---------- 5) Cron + Logging ----------
  msg_info "5/6  Cron (*/30) + Logging einrichten"
  pct exec "${VMID}" -- sh -c \
    "grep -q 'lenovo-tracker' /etc/crontabs/root 2>/dev/null || echo '*/30 * * * * ${APP_DIR}/lenovo-tracker --config ${CONF_DIR}/config.toml --once >> ${LOG_FILE} 2>&1' >> /etc/crontabs/root"
  pct exec "${VMID}" -- sh -c \
    "grep -q 'crond -f' /etc/inittab || echo '::respawn:/usr/sbin/crond -f -l 8 -c /etc/crontabs' >> /etc/inittab"
  pct exec "${VMID}" -- touch "$LOG_FILE"
  pct exec "${VMID}" -- sh -c 'kill -HUP 1 2>/dev/null || true' >/dev/null 2>&1 || true
  msg_ok "crond läuft init-überwacht (::respawn), Intervall */30"

  # ---------- 6) Verifikation ----------
  msg_info "6/6  Verifikation (--dry-run gegen die Live-API)"
  local dry
  dry="$(pct exec "${VMID}" -- "${APP_DIR}/lenovo-tracker" --config "${CONF_DIR}/config.toml" --dry-run 2>&1 || true)"
  if echo "$dry" | grep -q "DRY-RUN"; then
    msg_ok "$dry"
  else
    msg_warn "Dry-Run lieferte keine Daten (Akamai 403? Netzwerk?): $dry"
    msg_warn "Der Cron versucht es alle 30 Min erneut – Log: pct exec ${VMID} -- tail ${LOG_FILE}"
  fi

  pct set "${VMID}" --onboot 1 --tags lenovo-tracker >/dev/null

  echo
  echo -e "${GREEN}  ╔══════════════════════════════════════════════════════════╗${NC}"
  echo -e "${GREEN}  ║   ✔ Setup fertig – Tracker läuft                        ║${NC}"
  echo -e "${GREEN}  ╚══════════════════════════════════════════════════════════╝${NC}"
  echo
  echo "   CT ${VMID} · Cron */30 · State: ${STATE_DIR}/state.json · Log: ${LOG_FILE}"
  echo "   Config:    pct exec ${VMID} -- vi ${CONF_DIR}/config.toml"
  echo "   Test-Mail: pct exec ${VMID} -- ${APP_DIR}/lenovo-tracker --config ${CONF_DIR}/config.toml --test-mail"
  echo "   Peak messen: bash $0 --peak ${VMID}"
  echo "   Die erste Baseline setzt der erste Cron-Lauf still (keine Mail)."
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
  msg_warn "CT ${id} wird neu gestartet (nullt den Cgroup-Peak) – kurze Unterbrechung"
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
}

cmd_uninstall() {
  prereq >/dev/null
  if [[ "$FORCE" != "1" ]]; then
    if ui_available; then
      wt_yesno "Deinstallieren" "Löscht CT ${VMID} inkl. State/History\nund die Host-Binaries.\n\nWirklich deinstallieren?" || { echo "Abbruch."; return; }
    else
      die "Löschen braucht FORCE=1 im nicht-interaktiven Modus."
    fi
  fi
  pct stop "${VMID}" >/dev/null 2>&1 || true
  pct destroy "${VMID}" --purge && msg_ok "CT ${VMID} entfernt"
  rm -f /root/lenovo-tracker-rs /root/lenovo-tracker-rs2 /root/lt-new \
        /root/curl-impersonate /root/curl-impersonate-check /root/lenovotracker-config.toml \
        /root/.lenovo-tracker-src
  msg_ok "Host-Artefakte entfernt"
}

cmd_help() {
  # Statischer Text: im curl-One-Liner (bash -c) gibt es kein BASH_SOURCE
  cat <<'EOF'
Lenovo Clearance-Sale Mainboard-Tracker – Proxmox-Helper-Setup

Der Klassiker (auf dem Proxmox-Host):
  bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"

Mit Repo-Klon:
  bash lenovo-tracker-setup.sh                 # interaktives Menü
  NONINTERACTIVE=1 bash lenovo-tracker-setup.sh  # ohne Prompts (Env/Defaults)
  SKIP_BUILD=1 bash lenovo-tracker-setup.sh    # vorhandenes Binary nutzen
  FORCE=1 bash lenovo-tracker-setup.sh         # existierenden CT löschen+neu
  VMID=110 BUILD_VMID=111 bash lenovo-tracker-setup.sh   # eigene IDs

Unterkommandos (auch im One-Liner):
  … --probe [VMID]     API-Parameter-Matrix live testen
  … --peak [VMID]      Peak-RAM messen (CT-Restart, 3 Läufe)
  … --uninstall        CT + Host-Artefakte entfernen

Env-Defaults (überschreibbar): VMID=105 BUILD_VMID=104 RAM=32 SWAP=32 CPULIMIT=0.25
  DISK=local:0.05 NET0 MAIL_FROM MAIL_TO EXTRA_RECIPIENTS SMTP_HOST SMTP_PORT
  COMMODITY_KEYS KEYWORDS CI_VERSION FORCE SKIP_BUILD REPO_REF NONINTERACTIVE

Lizenz: MIT – free to use (siehe LICENSE). Copyright (c) 2026 LMasterPiec3
EOF
}

# bash -c "$(curl …)" --probe legt das Argument in $0 (nicht $1) — beides akzeptieren:
ARGS=()
if [[ "${0:-}" == --* ]]; then ARGS+=("$0"); fi
ARGS+=("$@")
MODE="${ARGS[0]:-setup}"

case "$MODE" in
  setup)        cmd_setup ;;
  --probe)      cmd_probe "${ARGS[1]:-$VMID}" ;;
  --peak)       cmd_peak "${ARGS[1]:-$VMID}" ;;
  --uninstall)  cmd_uninstall ;;
  --help|-h)    cmd_help ;;
  *)            msg_error "Unbekanntes Kommando: $MODE (siehe --help)"; exit 1 ;;
esac
