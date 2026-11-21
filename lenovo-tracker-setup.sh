#!/usr/bin/env bash
# ============================================================================
#  Lenovo Clearance-Sale Mainboard-Tracker – Proxmox-Helper-Setup
# ----------------------------------------------------------------------------
#  Im Stil der Proxmox-Community-Helper-Scripts: interaktives Menü, Spinner,
#  Settings-Box – läuft auf dem PVE-Host und richtet alles ein:
#
#    1. Binary aus den GitHub Releases laden (kein Build-CT nötig)
#    2. Produktions-CT (Alpine, minimal)
#    3. Binary + curl-impersonate (Chrome-TLS-Imitation)
#    4. Config /etc/lenovotracker/config.toml
#    5. Cron (*/30, init-überwacht) + Dry-Run-Verifikation
#
#  Der Klassiker (auf dem Proxmox-Host):
#    bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"
#
#  Unterkommandos (auch im One-Liner):
#    … --probe [VMID]    API-Parameter-Matrix live testen
#    … --peak [VMID]     Peak-RAM messen (CT-Restart)
#    … --uninstall       CT + Host-Artefakte entfernen
#    … --smtp-auth [VMID] SMTP-Login/Token (optional, alle Provider) + Test-Mail
#    … --help            Usage
#
#  Automatisierung: NONINTERACTIVE=1 (keine Prompts – Pflichtwerte per Env, Rest bekommt Defaults)
#  Pflichtwerte: MAIL_FROM MAIL_TO  (VMID/COMMODITY_KEYS bekommen sinnvolle Vorlagen,
#                 SMTP_BRIDGE/NETZ/TECHNIK-Defaults siehe unten)
#  Defaults (per Env überschreibbar, im Dialog editierbar):
#    VMID=erste freie ID ab 100 (pct+qm) · CT_HOSTNAME=LenovoTracker
#    RAM=32 SWAP=32 CPULIMIT=0.25 DISK=local:0.25 COMMODITY_KEYS=system boards
#    BRIDGE=vmbr0 (sonst automatisch erkannt), GATEWAY/BASE_IP automatisch vom Host
#    (Gateway ohne Route an der Bridge: Vorschlag <IP-Basis>.1 – editierbar)
#    SMTP_HOST automatisch ermittelt (Postfix-Relayhost, sonst MX/A der
#    Absender-Domain bzw. der eigenen Domain),
#    SMTP_PORT aus dem Relayhost, sonst 25 – beides nur Vorschläge, editierbar
#    SMTP_USER/SMTP_PASS: gesetzt = STARTTLS-Submission (Port 587), leer = Plain-SMTP
#    (BUILD_VMID nur bei BUILD_METHOD=build – Standard ist Release-Download)
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

# ------------------------------- i18n (DE/EN) --------------------------------
# UI_LANG=de|en – Standard: automatisch nach $LANG (de* → DE, sonst EN), Override
# per Env (z. B. UI_LANG=en). Schlüssel sind die deutschen Originaltexte; fehlt
# ein EN-Eintrag, wird der deutsche Text angezeigt.
UI_LANG="${UI_LANG:-}"
[[ "$UI_LANG" == "de" || "$UI_LANG" == "en" ]] || case "${LANG:-}" in de*|DE*) UI_LANG="de" ;; *) UI_LANG="en" ;; esac

declare -A MSG_EN=(
  ["Option wählen (Pfeiltasten/TAB, ENTER bestätigt):"]="Choose an option (arrows/TAB, ENTER to confirm):"
  ["Installation"]="Install tracker"
  ["Status & Log anzeigen"]="Show status & log"
  ["Beenden"]="Exit"
  ["Erkannte Installation: CT"]="Detected installation: CT"
  ["Clearance-Sale Mainboard-Tracker – Proxmox-Setup"]="Clearance-Sale Mainboard Tracker – Proxmox setup"
  ["CT-ID des Tracker-CTs (numerisch):"]="Tracker CT ID (numeric):"
  ["Hostname des Tracker-CTs:"]="Tracker CT hostname:"
  ["RAM in MB:"]="RAM in MB:"
  ["Swap in MB:"]="Swap in MB:"
  ["CPU-Limit, z. B. 0.25 (Viertel-Kern):"]="CPU limit, e. g. 0.25 (quarter core):"
  ["Rootfs, z. B. local:0.25:"]="Rootfs, e. g. local:0.25:"
  ["Temporärer Build-Container (numerisch):"]="Temporary build container (numeric):"
  ["CT-IDs gleich"]="CT IDs identical"
  ["Produktions- und Build-CT-ID dürfen nicht identisch sein.\n\nErneut eingeben?"]="Production and build CT IDs must differ.\n\nEnter again?"
  ["Gateway-IP (z. B. 10.10.10.1):"]="Gateway IP (e. g. 10.10.10.1):"
  ["IP-Basis ohne Punkt, z. B. 10.10.10 (CT erhält <Basis>.<CT-ID>):"]="IP base without dot, e. g. 10.10.10 (CT gets <base>.<CT-ID>):"
  ["Absender-Adresse:"]="Sender address:"
  ["Empfänger-Adresse(n), komma-getrennt:"]="Recipient address(es), comma separated:"
  ["SMTP-Server (z. B. smtp.protonmail.ch):"]="SMTP server (e. g. smtp.protonmail.ch):"
  ["SMTP-Benutzer (optional, alle Provider – leer = plain):"]="SMTP user (optional, any provider – empty = plain):"
  ["SMTP-Passwort / Token (Proton: SMTP-Token · Gmail: App-Passwort):"]="SMTP password / token (Proton: SMTP token · Gmail: app password):"
  ["SMTP-Port (mit Login: 587):"]="SMTP port (with login: 587):"
  ["Komma-getrennt, leer = alle Mainboards:"]="Comma separated, empty = all mainboards:"
  ["CT-ID"]="CT ID"
  ["CT-ID erforderlich."]="CT ID required."
  ["Bridge"]="Bridge"
  ["Proxmox-Bridge (z. B. vmbr0):"]="Proxmox bridge (e. g. vmbr0):"
  ["Bridge wählen"]="Choose bridge"
  ["Bridge für das CT – Gateway/IP-Basis passen sich der Auswahl an:"]="Bridge for the CT – gateway/IP base follow the selection:"
  ["· Default-Route"]="· default route"
  ["Bridge manuell eingeben"]="Enter bridge manually"
  ["Commodity-Keys"]="Commodity keys"
  ["Komma-getrennt, lowercase (z. B. system boards):"]="Comma separated, lowercase (e. g. system boards):"
  ["Was soll getrackt werden? (Leertaste = an/aus, ENTER = bestätigen):"]="What should be tracked? (space = on/off, ENTER = confirm):"
  ["Mainboards – Tracker-Standard"]="Mainboards – tracker default"
  ["Cover mit Tastatur"]="Cover with keyboard"
  ["Interne Tastaturen"]="Internal keyboards"
  ["Externe Tastaturen"]="External keyboards"
  ["Kühlkörper"]="Heat sinks"
  ["Lüfter"]="Fans"
  ["LCD-Einheit"]="LCD assembly"
  ["Prozessoren"]="Processors"
  ["WLAN-Adapter"]="WLAN adapters"
  ["WWAN-Adapter"]="WWAN adapters"
  ["Optische Laufwerke"]="Optical drives"
  ["Netzteile (extern)"]="AC adapters"
  ["Festplatten"]="Hard drives"
  ["Zeigegeräte"]="Pointing devices"
  ["Dockingstations"]="Docking stations"
  ["LCD-Panels"]="LCD panels"
  ["Externe HDDs"]="External HDDs"
  ["Interne Kabel"]="Internal cables"
  ["Interne Netzteile"]="Internal power supplies"
  ["Komplettaustausch"]="Whole unit exchange"
  ["Setup starten"]="Start setup"
  ["CT-IDs gleich?"]="CT IDs identical?"
  ["Deinstallieren"]="Uninstall"
  ["SMTP-Passwort"]="SMTP password"
  ["SMTP-Token"]="SMTP token"
  ["Test-Mail"]="Test e-mail"
  ["Mail von"]="Mail from"
  ["Mail an"]="Mail to"
  ["Weitere Empfänger"]="More recipients"
  ["SMTP-Host"]="SMTP host"
  ["SMTP-Login"]="SMTP login"
  ["SMTP-Port"]="SMTP port"
  ["Keyword-Filter"]="Keyword filter"
  ["Absender-Adresse: (edit)"]="Sender address:"
  ["Empfänger, komma-getrennt:"]="Recipients, comma separated:"
  ["Komma-getrennt, leer = keine:"]="Comma separated, empty = none:"
  ["SMTP-Server:"]="SMTP server:"
  ["SMTP-Benutzer (leer = kein Login/Plain-SMTP):"]="SMTP user (empty = no login/plain SMTP):"
  ["SMTP- / App-Passwort (leer = kein Login):"]="SMTP / app password (empty = no login):"
  ["SMTP-Benutzer (optional; Proton: deine Adresse – leer = plain):"]="SMTP user (optional; Proton: your address – empty = plain):"
  ["SMTP-Passwort / Token (Proton: SMTP-Token · Gmail: App-Passwort · Provider-abhängig):"]="SMTP password / token (Proton: SMTP token · Gmail: app password · provider specific):"
  ["SMTP-Server für die Submission:"]="SMTP server for submission:"
  ["Eingabe prüfen"]="Check input"
  ["Netz"]="Network"
  ["Kein Alpine-Template via pveam verfügbar – Repos/Internet prüfen."]="No Alpine template available via pveam - check repos/internet."
  ["Quellen nicht ladbar von"]="Cannot fetch sources from"
  ["– Repo privat? Dann Repo lokal klonen und Script aus dem Repo-Ordner starten."]="- private repo? Clone it locally and run the script from the repo folder."
  ["Sende Test-Mail (CT"]="Sending test e-mail (CT"
  ["from/to/smtp_host prüfen"]="check from/to/smtp_host"
  ["CT-ID (VMID) erforderlich."]="CT ID (VMID) required."
  ["Löschen braucht FORCE=1 im nicht-interaktiven Modus."]="Deletion needs FORCE=1 in non-interactive mode."
  ["entfernt"]="removed"
  ["existiert nicht – nichts zu löschen"]="does not exist - nothing to delete"
  ["Host-Artefakte entfernt"]="host artifacts removed"
  ["Unbekanntes Kommando:"]="Unknown command:"
  ["siehe --help"]="see --help"
  ["Abgebrochen."]="Aborted."
  ["SMTP-Login gesetzt:"]="SMTP login set:"
  ["Port:"]="Port:"
  ["Config-Editor braucht whiptail/TTY."]="Config editor needs whiptail/TTY."
  ["nicht lesbar."]="not readable."
  ["nicht ladbar."]="cannot be loaded."
  ["existiert nicht."]="does not exist."
  ["Cgroup-Pfad"]="Cgroup path"
  ["nicht gefunden"]="not found"
  ["vorbereiten (Fallback-Build)"]="prepare (fallback build)"
  ["anlegen"]="create"
  ["existiert – FORCE=1: stoppe + lösche"]="exists - FORCE=1: stop + delete"
  ["läuft"]="running"
  ["läuft nicht –"]="not running -"
  ["bitte starten."]="please start it."
  ["1/6  Binary von GitHub Releases laden (kein Build-CT nötig)"]="1/6  Download binary from GitHub releases (no build CT needed)"
  ["Binary aus Releases:"]="Binary from releases:"
  ["Release-Download fehlgeschlagen"]="Release download failed"
  ["nutze vorhandenes Binary"]="using existing binary"
  ["Kein Binary verfügbar (Release-Download fehlgeschlagen). Später erneut versuchen – oder BUILD_METHOD=build BUILD_VMID=<id> setzen, um explizit einen temporären Build-CT zu nutzen."]="No binary available (release download failed). Retry later - or set BUILD_METHOD=build BUILD_VMID=<id> to explicitly use a temporary build CT."
  ["2/6  Produktions-CT"]="2/6  Production CT"
  ["3/6  Binary + curl-impersonate installieren"]="3/6  Install binary + curl-impersonate"
  ["curl-impersonate-Binary nicht im Tarball gefunden"]="curl-impersonate binary not found in tarball"
  ["curl-impersonate einsatzbereit"]="curl-impersonate ready"
  ["curl-impersonate --version schlug fehl – prüfen!"]="curl-impersonate --version failed - check!"
  ["4/6  Config generieren"]="4/6  Generate config"
  ["Config installiert"]="Config installed"
  ["5/6  Cron (*/30) + Logging einrichten"]="5/6  Set up cron (*/30) + logging"
  ["crond läuft init-überwacht (::respawn), Intervall */30"]="crond supervised by init (::respawn), interval */30"
  ["6/6  Verifikation (--dry-run gegen die Live-API)"]="6/6  Verification (--dry-run against live API)"
  ["Binary läuft im CT nicht (dynamisch gelinkt?) – Build mit crt-static prüfen."]="Binary does not run in CT (dynamically linked?) - check crt-static build."
  ["Dry-Run lieferte keine Daten (Akamai 403? Netzwerk?):"]="Dry-run returned no data (Akamai 403? network?):"
  ["Der Cron versucht es alle 30 Min erneut – Log:"]="The cron retries every 30 min - log:"
  ["Mailzustellung geprüft."]="Mail delivery verified."
  ["Setup bleibt abgeschlossen – Mail erst bei korrigierter Config."]="Setup stays complete - mail only after fixing the config."
  ["SMTP-Host auf Submission-Server gesetzt:"]="SMTP host set to submission server:"
  ["SMTP-Login geleert – Versand läuft wieder plain (Auth-Banner möglich)."]="SMTP login cleared - sending plain again (auth banner possible)."
  ["Test-Mail fehlgeschlagen – Config korrigieren und erneut senden:"]="Test e-mail failed - fix config and send again:"
  ["Whiptail/TTY nicht verfügbar – NONINTERACTIVE: alle Werte müssen als Env-Variablen gesetzt sein."]="Whiptail/TTY not available - NONINTERACTIVE: all values must be set via env."
  ["Keine Standardwerte – interaktiv ausführen oder per Env setzen. Fehlend:"]="No defaults - run interactively or set via env. Missing:"
  ["Quellen: lokales Repo"]="Sources: local repo"
  ["Quellen von GitHub laden"]="Fetching sources from GitHub"
  ["Quellen geladen"]="Sources loaded"
  ["whiptail fehlt – installiere Setup-UI"]="whiptail missing - installing setup UI"
  ["whiptail installiert"]="whiptail installed"
  ["whiptail nicht installierbar – Fallback ohne Dialoge"]="whiptail not installable - fallback without dialogs"
  ["Bitte als root ausführen (pct braucht root)."]="Please run as root (pct needs root)."
  ["pct nicht gefunden – das Script gehört auf einen Proxmox-VE-Host."]="pct not found - this script belongs on a Proxmox VE host."
  ["Binary: GitHub Release (Download, kein Build-CT)"]="Binary: GitHub release (download, no build CT)"
  ["Binary: Build-CT"]="Binary: build CT"
  ["Fallback-Build"]="fallback build"
  ["Login"]="login"
  ["Statischer Build fehlgeschlagen (Log:"]="Static build failed (log:"
  ["Binary nicht aus dem Build-CT ladbar – Build fehlgeschlagen?"]="Cannot pull binary from build CT - build failed?"
  ["Binary gebaut:"]="Binary built:"
  ["Config gespeichert (Backup:"]="Config saved (backup:"
  ["smtp_user nicht setzbar."]="cannot set smtp_user."
  ["smtp_pass nicht setzbar."]="cannot set smtp_pass."
  ["smtp_host nicht gefunden."]="smtp_host not found."
  ["smtp_port nicht gefunden."]="smtp_port not found."
  ["commodity_keys nicht gefunden."]="commodity_keys not found."
  ["name_keywords nicht gefunden."]="name_keywords not found."
  ["from nicht gefunden."]="from not found."
  ["to nicht gefunden."]="to not found."
  ["extra_recipients nicht gefunden."]="extra_recipients not found."
  ["VMID muss numerisch sein."]="VMID must be numeric."
  ["BUILD_VMID muss numerisch sein."]="BUILD_VMID must be numeric."
  ["VMID und BUILD_VMID dürfen nicht identisch sein."]="VMID and BUILD_VMID must differ."
  ["RAM muss numerisch sein."]="RAM must be numeric."
  ["SWAP muss numerisch sein."]="SWAP must be numeric."
  ["CPULIMIT muss numerisch sein (z. B. 0.25)."]="CPULIMIT must be numeric (e. g. 0.25)."
  ["SMTP_PORT muss numerisch sein."]="SMTP_PORT must be numeric."
  ["GATEWAY muss eine gültige IPv4 sein (z. B. 10.10.10.1), aktuell:"]="GATEWAY must be a valid IPv4 (e. g. 10.10.10.1), currently:"
  ["BASE_IP muss eine IP-Basis ohne Punkt am Ende sein (z. B. 10.10.10), aktuell:"]="BASE_IP must be an IP base without trailing dot (e. g. 10.10.10), currently:"
  ["Mail von darf nicht leer sein."]="Mail from must not be empty."
  ["Mail an darf nicht leer sein."]="Mail to must not be empty."
  ["SMTP-Host darf nicht leer sein."]="SMTP host must not be empty."
  ["Kein Alpine-Template gefunden – lade via pveam"]="No Alpine template found - downloading via pveam"
  ["bleibt unangetastet."]="left untouched."
  ["startet nicht"]="does not start"
  ["konnte nicht entfernt werden"]="could not be removed"
  ["Build-CT"]="build CT"
  ["Löscht CT"]="Deletes CT"
  ["inkl. State/History und die Host-Binaries.

Wirklich deinstallieren?"]="incl. state/history and host binaries.

Really uninstall?"
  ["existiert bereits. FORCE=1 für Neuaufbau (State geht verloren!) oder VMID ändern."]="already exists. FORCE=1 to rebuild (state will be lost!) or choose another VMID."
)
t() {
  local key="$1"
  if [[ "$UI_LANG" == "en" ]]; then
    printf '%s' "${MSG_EN[$key]:-$key}"
  else
    printf '%s' "$key"
  fi
}

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

# ---------------------- Konfiguration (keine Defaults) -----------------------
# Alle Setup-Werte werden interaktiv abgefragt oder müssen per Env gesetzt
# werden – es gibt keine vorgegebenen Standardwerte.
VMID="${VMID:-}"
BUILD_VMID="${BUILD_VMID:-}"
CT_HOSTNAME="${CT_HOSTNAME:-}"
RAM="${RAM:-}"
SWAP="${SWAP:-}"
CPULIMIT="${CPULIMIT:-}"
DISK="${DISK:-}"
BRIDGE="${BRIDGE:-}"
GATEWAY="${GATEWAY:-}"
BASE_IP="${BASE_IP:-}"
NET0="${NET0:-}"
MAIL_FROM="${MAIL_FROM:-}"
MAIL_TO="${MAIL_TO:-}"
EXTRA_RECIPIENTS="${EXTRA_RECIPIENTS:-}"
SMTP_HOST="${SMTP_HOST:-}"
SMTP_PORT="${SMTP_PORT:-}"
COMMODITY_KEYS="${COMMODITY_KEYS:-}"
KEYWORDS="${KEYWORDS:-}"

# Technische Konstanten (keine Setup-Werte)
BUILD_RAM="${BUILD_RAM:-3072}"
BUILD_DISK="${BUILD_DISK:-local:4}"
CI_VERSION="${CI_VERSION:-v2.3.2}"
CI_URL="https://github.com/lexiforest/curl-impersonate/releases/download/${CI_VERSION}/curl-impersonate-${CI_VERSION}.x86_64-linux-musl.tar.gz"

APP_DIR="/opt/lenovo-tracker"
CONF_DIR="/etc/lenovotracker"
STATE_DIR="/var/lib/lenovotracker"
LOG_FILE="/var/log/lenovo-tracker.log"
HOST_BIN="/root/lenovo-tracker-rs"
RELEASE_URL="https://github.com/LMasterPiec3/LenovoTracker/releases/latest/download/lenovo-tracker"

REPO_RAW="${REPO_RAW:-https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/${REPO_REF:-main}}"
SUBJECT_PREFIX="${SUBJECT_PREFIX:-[Lenovo Tracker]}"
FORCE="${FORCE:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
BUILD_METHOD="${BUILD_METHOD:-release}"   # release (GitHub-Download) | build (Build-CT erzwingen)
NONINTERACTIVE="${NONINTERACTIVE:-0}"

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-${0}}")" 2>/dev/null && pwd || echo "")"

# ------------------------------- Hilfsfunktionen -----------------------------
template_path() {
  ls /var/lib/vz/template/cache/alpine-*.tar.* 2>/dev/null | sort | tail -1
}

toml_array() {
  local in="$1" out="" p
  if [[ -z "$in" ]]; then echo "[]"; return; fi
  IFS=',' read -ra parts <<< "$in"
  for p in "${parts[@]}"; do
    p="${p#"${p%%[![:space:]]*}"}"
    p="${p%"${p##*[![:space:]]}"}"
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

header_info() {
  [[ -n "${TERM:-}" ]] && clear
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
  echo -e "${NC}  ${C_HEAD}$(t 'Clearance-Sale Mainboard-Tracker – Proxmox-Setup')${NC}  ${C_OK}(MIT, free to use)${NC}"
  echo
}

prereq() {
  [[ $EUID -eq 0 ]] || die "$(t 'Bitte als root ausführen (pct braucht root).')"
  command -v pct >/dev/null 2>&1 || die "$(t 'pct nicht gefunden – das Script gehört auf einen Proxmox-VE-Host.')"
}

require_ctid() {  # require_ctid <id> <unterkommando>
  [[ -n "${1:-}" ]] || die "$(t 'CT-ID angeben:') --${2} <VMID>"
}

require_running_ct() {  # require_running_ct <id> <hinweis>
  pct status "$1" 2>/dev/null | grep -q running || die "$(t 'CT') ${1} $(t 'läuft nicht –') ${2}"
}

unquote() {  # umschließende Anführungszeichen entfernen (toml_value-Ergebnisse)
  sed 's/^"//; s/"$//'
}

push_ct_config() {  # push_ct_config <id> <lokale-config> — mit .bak-Backup im CT
  pct exec "$1" -- cp "${CONF_DIR}/config.toml" "${CONF_DIR}/config.toml.bak" 2>/dev/null || true
  pct push "$1" "$2" "${CONF_DIR}/config.toml" || die "Config-Push fehlgeschlagen."
}

# Setup-UI: whiptail bei Bedarf nachinstallieren (auf PVE-Hosts oft nicht vorhanden)
ensure_ui() {
  command -v whiptail >/dev/null 2>&1 && return 0
  [[ "$NONINTERACTIVE" == "1" ]] && return 1
  [[ -t 0 || -t 1 ]] || return 1
  msg_info "$(t 'whiptail fehlt – installiere Setup-UI')"
  { apt-get install -y -qq whiptail || { apt-get update -qq && apt-get install -y -qq whiptail; }; } \
    >"$LAST_LOG" 2>&1 || true
  if command -v whiptail >/dev/null 2>&1; then
    msg_ok "$(t 'whiptail installiert')"
    return 0
  fi
  msg_warn "$(t 'whiptail nicht installierbar – Fallback ohne Dialoge')"
  return 1
}

# Alpine-Template automatisch laden, falls keins vorhanden ist
TEMPLATE_TPL=""
ensure_template() {
  TEMPLATE_TPL="$(template_path)"
  [[ -n "$TEMPLATE_TPL" ]] && return 0
  msg_info "$(t 'Kein Alpine-Template gefunden – lade via pveam')"
  pveam update >"$LAST_LOG" 2>&1 || true
  local name
  name="$(pveam available --section system 2>/dev/null | awk '/alpine-/ {print $2}' | sort -V | tail -1)"
  [[ -n "$name" ]] || die "$(t 'Kein Alpine-Template via pveam verfügbar – Repos/Internet prüfen.')"
  spin_run "Alpine-Template ${name} laden" pveam download local "$name"
  TEMPLATE_TPL="$(template_path)"
  [[ -n "$TEMPLATE_TPL" ]] || die "Template-Download fehlgeschlagen."
}

# Quellen: lokales Repo bevorzugt, sonst von GitHub raw laden
# (damit der curl-One-Liner ohne Repo-Klon funktioniert).
SRC_CARGO=""; SRC_MAIN=""
fetch_sources() {
  if [[ -f "${REPO_DIR}/rust-tracker/Cargo.toml" && -f "${REPO_DIR}/rust-tracker/src/main.rs" ]]; then
    SRC_CARGO="${REPO_DIR}/rust-tracker/Cargo.toml"
    SRC_MAIN="${REPO_DIR}/rust-tracker/src/main.rs"
    msg_ok "$(t 'Quellen: lokales Repo') (${REPO_DIR})"
    return
  fi
  local tmp="/root/.lenovo-tracker-src"
  msg_info "$(t 'Quellen von GitHub laden') (${REPO_RAW})"
  rm -rf "$tmp" && mkdir -p "$tmp"
  ct_download "${REPO_RAW}/rust-tracker/Cargo.toml" "${tmp}/Cargo.toml" || true
  ct_download "${REPO_RAW}/rust-tracker/src/main.rs" "${tmp}/main.rs" || true
  [[ -s "${tmp}/Cargo.toml" && -s "${tmp}/main.rs" ]] \
    || die "$(t 'Quellen nicht ladbar von') ${REPO_RAW} $(t '– Repo privat? Dann Repo lokal klonen und Script aus dem Repo-Ordner starten.')"
  SRC_CARGO="${tmp}/Cargo.toml"
  SRC_MAIN="${tmp}/main.rs"
  msg_ok "$(t 'Quellen geladen') (main.rs $(stat -c %s "$SRC_MAIN") B)"
}

# ------------------------------- Whiptail-TUI --------------------------------
# Wie die Community-Scripts: Dialoge mit whiptail (blauer Fullscreen, Menü mit
# Pfeiltasten/TAB, InputBoxen, YesNo). Es gibt keine Standardwerte: alle
# Setup-Felder werden leer abgefragt und müssen ausgefüllt werden.
BACKTITLE="Lenovo-Tracker Setup · MIT (free to use)"

ui_available() {
  command -v whiptail >/dev/null 2>&1 && [[ -t 0 || -t 1 ]] && [[ "$NONINTERACTIVE" != "1" ]]
}

wt_menu() {  # wt_menu <title> <text> item1 desc1 [item2 desc2 …] -> echo Auswahl
  local title="$1" text="$2"; shift 2
  whiptail --backtitle "$BACKTITLE" --title "$title" --menu "$text" 20 74 10 "$@" \
    --ok-button "<Select>" --cancel-button "<Exit Script>" \
    3>&1 1>&2 2>&3
}

wt_input() {  # wt_input <title> <prompt> <default> -> echo Wert (ESC/Cancel → rc 1)
  whiptail --backtitle "$BACKTITLE" --title "$1" --inputbox "$2" 10 70 "$3" \
    3>&1 1>&2 2>&3
}

wt_yesno() {  # wt_yesno <title> <text> -> rc 0=Ja 1=Nein
  whiptail --backtitle "$BACKTITLE" --title "$1" --yesno "$2" 14 70
}

# Wert-Validator: gibt den normalisierten Wert zurück, rc!=0 bei ungültig.
# kind: text | num | dec | ipv4 | baseip | email
valid_value() {
  local kind="$1" v="$2" o ok
  case "$kind" in
    num)   [[ "$v" =~ ^[0-9]+$ ]] || return 1; echo "$v" ;;
    dec)   [[ "$v" =~ ^[0-9]+([.][0-9]+)?$ ]] || return 1; echo "$v" ;;
    ipv4)  is_ipv4 "$v" || return 1; echo "$v" ;;
    baseip)
      v="$(normalize_base_ip "$v")"
      [[ "$v" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]] || return 1
      ok=1
      for o in ${v//./ }; do (( o <= 255 )) || ok=0; done
      [[ $ok -eq 1 ]] || return 1
      echo "$v"
      ;;
    email) [[ "$v" =~ ^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$ ]] || return 1; echo "$v" ;;
    *)     [[ -n "$v" ]] || return 1; echo "$v" ;;
  esac
}

# Pflichtfeld-Dialog: schreibt den Wert in die Variable $1.
# Vorbelegt mit dem aktuellen Variablenstand (ENV oder Default) – editierbar.
# kind siehe valid_value – leere/ungültige Eingaben werden erneut abgefragt.
wt_fill() {
  local var="$1" title="$2" prompt="$3" kind="${4:-text}" v
  while true; do
    v="$(wt_input "$title" "$prompt" "${!var:-}")" || return 1
    v="$(valid_value "$kind" "$v")" || continue
    printf -v "$var" '%s' "$v"
    return 0
  done
}

wt_msgbox() {  # wt_msgbox <title> <text> – Fehler-/Hinweis-Box
  whiptail --backtitle "$BACKTITLE" --title "$1" --msgbox "$2" 14 70 3>&1 1>&2 2>&3 || true
}


# Validierung: echte IPv4 (4 Oktette, je 0-255)
is_ipv4() {
  local a b c d
  [[ "$1" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  a="${BASH_REMATCH[1]}"; b="${BASH_REMATCH[2]}"; c="${BASH_REMATCH[3]}"; d="${BASH_REMATCH[4]}"
  (( a <= 255 && b <= 255 && c <= 255 && d <= 255 )) || return 1
  return 0
}

# IP-Basis normalisieren: Punkt(e) am Ende weg; vollständige IPv4 → letztes Oktett ab
# ("10.10.10." / "10.10.10" / "10.10.10.1"  →  "10.10.10")
normalize_base_ip() {
  local v="$1"
  while [[ "$v" == *. ]]; do v="${v%.}"; done
  if is_ipv4 "$v"; then v="${v%.*}"; fi
  echo "$v"
}

# Erste freie CT/VM-ID ab 100 (pct + qm gelten als belegt), optional ausschließen
next_free_ctid() {
  local excl="${1:-}" id used
  used="$( { pct list 2>/dev/null; qm list 2>/dev/null; } | awk 'NR>1 {print $1}' | sort -n | uniq)"
  for ((id = 100; id <= 9999; id++)); do
    [[ "$id" == "$excl" ]] && continue
    grep -qx "$id" <<< "$used" || { echo "$id"; return 0; }
  done
  return 1
}

# Bekannte Warengruppen (Lenovo-API, Stand 2026-11) für die Mehrfachauswahl
KNOWN_COMMODITIES=(
  "system boards"           "Mainboards – Tracker-Standard"
  "cover with keyboard"     "Cover mit Tastatur"
  "keyboard internal"       "Interne Tastaturen"
  "keyboards external"      "Externe Tastaturen"
  "heat sink"               "Kühlkörper"
  "fans"                    "Lüfter"
  "lcd assembly"            "LCD-Einheit"
  "processors"              "Prozessoren"
  "wireless lan adapter"    "WLAN-Adapter"
  "wireless wan adapter"    "WWAN-Adapter"
  "optical drives"          "Optische Laufwerke"
  "ac adapter"              "Netzteile (extern)"
  "hard drives"             "Festplatten"
  "pointing device"         "Zeigegeräte"
  "docking stations"        "Dockingstations"
  "lcd panel"               "LCD-Panels"
  "drive external hdd"      "Externe HDDs"
  "cables internal"         "Interne Kabel"
  "power supplies internal" "Interne Netzteile"
  "whole unit exchange"     "Komplettaustausch"
)

# Commodity-Keys als Mehrfachauswahl (Checkliste wie community-scripts):
# Leertaste wählt an/aus, ENTER bestätigt. ENV/bestehende Keys sind vorgewählt,
# ohne Auswahl gilt der Tracker-Standard "system boards". Ohne TTY/whiptail
# fällt es auf die Freitext-Eingabe zurück.
ui_commodity_keys() {
  if ! ui_available; then
    wt_fill COMMODITY_KEYS "$(t 'Commodity-Keys')" "$(t 'Komma-getrennt, lowercase (z. B. system boards):')" || return 1
    return 0
  fi
  local -a sel=""
  IFS=',' read -ra sel <<< "${COMMODITY_KEYS:-system boards}"
  local -a items=()
  local key desc state k i
  for ((i = 0; i < ${#KNOWN_COMMODITIES[@]}; i += 2)); do
    key="${KNOWN_COMMODITIES[i]}"
    desc="$(t "${KNOWN_COMMODITIES[i + 1]}")"
    state="OFF"
    for k in "${sel[@]}"; do
      k="${k#"${k%%[![:space:]]*}"}"; k="${k%"${k##*[![:space:]]}"}"
      [[ "$k" == "$key" ]] && state="ON"
    done
    items+=("$key" "$desc" "$state")
  done
  local raw
  raw="$(whiptail --backtitle "$BACKTITLE" --title "Commodity-Keys" --checklist \
    "$(t 'Was soll getrackt werden? (Leertaste = an/aus, ENTER = bestätigen):')" \
    22 74 14 "${items[@]}" 3>&1 1>&2 2>&3)" || return 1
  eval "set -- $raw"
  local out=""
  for k in "$@"; do
    [[ -z "$out" ]] && out="$k" || out="$out,$k"
  done
  COMMODITY_KEYS="${out:-system boards}"
  return 0
}

# Wizard: fragt alle Setup-Werte ab (Vorschläge vorbelegt – ENV/Erkennung/
# Defaults). Felder validieren direkt (wt_fill), Cancel beendet das Setup.
ui_configure() {
  local v

  # --- Container & Ressourcen ---
  wt_fill VMID "CT-ID (Produktion)" "$(t 'CT-ID des Tracker-CTs (numerisch):')" num || exit 0
  wt_fill CT_HOSTNAME "Hostname" "$(t 'Hostname des Tracker-CTs:')" || exit 0
  wt_fill RAM "RAM" "$(t 'RAM in MB:')" num || exit 0
  wt_fill SWAP "Swap" "$(t 'Swap in MB:')" num || exit 0
  wt_fill CPULIMIT "CPU-Limit" "$(t 'CPU-Limit, z. B. 0.25 (Viertel-Kern):')" dec || exit 0
  wt_fill DISK "Rootfs" "$(t 'Rootfs, z. B. local:0.25:')" || exit 0

  # Nur bei explizitem BUILD_METHOD=build – Standard ist der Release-Download
  if [[ "$BUILD_METHOD" == "build" ]]; then
    while true; do
      [[ -n "$BUILD_VMID" ]] || BUILD_VMID="$(next_free_ctid "$VMID")"
      wt_fill BUILD_VMID "CT-ID (Build)" "$(t 'Temporärer Build-Container (numerisch):')" num || exit 0
      [[ "$VMID" != "$BUILD_VMID" ]] && break
      wt_yesno "$(t 'CT-IDs gleich')" "$(t 'Produktions- und Build-CT-ID dürfen nicht identisch sein.

Erneut eingeben?')" || exit 0
    done
  fi

  # --- Netzwerk ---
  ui_pick_bridge || exit 0   # Menü der Host-Bridges; setzt BRIDGE/GATEWAY/BASE_IP
  wt_fill GATEWAY "Gateway" "$(t 'Gateway-IP (z. B. 10.10.10.1):')" ipv4 || exit 0
  wt_fill BASE_IP "IP-Basis" "$(t 'IP-Basis ohne Punkt, z. B. 10.10.10 (CT erhält <Basis>.<CT-ID>):')" baseip || exit 0

  # --- Mail & SMTP (Login OPTIONAL – alle Provider) ---
  wt_fill MAIL_FROM "Mail von" "$(t 'Absender-Adresse:')" email || exit 0
  # SMTP-Host aus der Absender-Domain ableiten, falls die Vorab-Erkennung nichts fand
  if [[ -z "${SMTP_HOST:-}" && "$MAIL_FROM" == *"@"* ]]; then
    SMTP_HOST="$(mx_for_domain "${MAIL_FROM##*@}")"
  fi
  wt_fill MAIL_TO "Mail an" "$(t 'Empfänger-Adresse(n), komma-getrennt:')" || exit 0
  wt_fill SMTP_HOST "SMTP-Host" "$(t 'SMTP-Server (z. B. smtp.protonmail.ch):')" || exit 0
  wt_fill SMTP_USER "SMTP-Login" "$(t 'SMTP-Benutzer (optional, alle Provider – leer = plain):')" || exit 0
  if [[ -n "$SMTP_USER" ]]; then
    v="$(whiptail --backtitle "$BACKTITLE" --title "SMTP-Passwort" --passwordbox       "$(t 'SMTP-Passwort / Token (Proton: SMTP-Token · Gmail: App-Passwort):')"       11 70 "${SMTP_PASS}" 3>&1 1>&2 2>&3)" || exit 0
    SMTP_PASS="$v"
    # Login gesetzt → Submission-Port (STARTTLS) als Default
    [[ "$SMTP_PORT" == "25" ]] && SMTP_PORT="587"
  else
    SMTP_PASS=""
  fi
  wt_fill SMTP_PORT "SMTP-Port" "$(t 'SMTP-Port (mit Login: 587):')" num || exit 0

  # --- Filter ---
  ui_commodity_keys || exit 0
  v="$(wt_input "Keyword-Filter" "$(t 'Komma-getrennt, leer = alle Mainboards:')" "${KEYWORDS:-}")" || exit 0
  KEYWORDS="${v//$'
'/}"
}

# CT-ID für Status/Test-Mail/Probe/Peak/Uninstall erfragen, falls nicht gesetzt
ui_vmid() {
  [[ -n "$VMID" ]] && return 0
  local v
  v="$(wt_input "$(t 'CT-ID')" "$(t 'CT-ID des Tracker-CTs (numerisch):')" "")" || exit 0
  [[ -n "$v" ]] || die "$(t 'CT-ID erforderlich.')"
  VMID="$v"
}

# Pflichtwerte prüfen – ENV/User-Eingabe gewinnt, leere Felder ohne Default sind Pflicht.
# BUILD_VMID nur erforderlich, wenn BUILD_METHOD=build (Standard ist Release-Download).
# Standardwerte – reiner Prefill: ENV überschreibt, der User kann im Dialog alles ändern.
apply_defaults() {
  [[ -n "$VMID" ]] || VMID="$(next_free_ctid 2>/dev/null)"   # erste freie ID als Vorlage
  CT_HOSTNAME="${CT_HOSTNAME:-LenovoTracker}"
  RAM="${RAM:-32}"
  SWAP="${SWAP:-32}"
  CPULIMIT="${CPULIMIT:-0.25}"
  DISK="${DISK:-local:0.25}"
  BRIDGE="${BRIDGE:-${DETECTED_BRIDGE:-vmbr0}}"
  BASE_IP="${BASE_IP:-${DETECTED_BASE_IP:-}}"
  GATEWAY="${GATEWAY:-${DETECTED_GATEWAY:-${BASE_IP:+${BASE_IP}.1}}}"
  SMTP_HOST="${SMTP_HOST:-${DETECTED_SMTP_HOST:-}}"
  SMTP_PORT="${SMTP_PORT:-${DETECTED_SMTP_PORT:-25}}"   # Plain-SMTP-Default
  SMTP_USER="${SMTP_USER:-}"
  SMTP_PASS="${SMTP_PASS:-}"
  # Mit Login ist der Submission-Port (STARTTLS) der richtige Default
  [[ -n "$SMTP_USER" && "$SMTP_PORT" == "25" ]] && SMTP_PORT="587"
  COMMODITY_KEYS="${COMMODITY_KEYS:-system boards}"     # Tracker-Standard, Checkliste editierbar
}

# Netz-Defaults vom PVE-Host erkennen (Default-Route → Bridge/Gateway, Host-IP → IP-Basis)
DETECTED_BRIDGE=""; DETECTED_GATEWAY=""; DETECTED_BASE_IP=""
detect_net_defaults() {
  command -v ip >/dev/null 2>&1 || return 0
  local route dev ip_cidr
  route="$(ip -4 route show default 2>/dev/null | head -1)"
  dev="$(awk '{for(i=1;i<=NF;i++) if($i=="dev") {print $(i+1); exit}}' <<<"$route")"
  DETECTED_GATEWAY="$(awk '{for(i=1;i<=NF;i++) if($i=="via") {print $(i+1); exit}}' <<<"$route")"
  [[ -n "$dev" ]] || return 0
  DETECTED_BRIDGE="$dev"
  ip_cidr="$(ip -4 addr show dev "$dev" scope global 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++) if($i=="inet") {print $2; exit}}')"
  [[ -n "$ip_cidr" ]] || return 0
  DETECTED_BASE_IP="${ip_cidr%/*}"   # Subnetz-Maske ab
  DETECTED_BASE_IP="${DETECTED_BASE_IP%.*}"   # letztes Oktett ab → Netzpräfix
}

# Mailhost für <domain> laut DNS: MX-Record, sonst A-Record der Domain
# (RFC 5321 Implicit-MX – wenn kein MX existiert, gilt der A-Record).
mx_for_domain() {
  local dom="$1" mx=""
  if command -v host >/dev/null 2>&1; then
    mx="$(host -t mx "$dom" 2>/dev/null | awk '/ is handled by /{print $NF; exit}')"
    mx="${mx%.}"
    if [[ -z "$mx" ]]; then
      mx="$(host -t a "$dom" 2>/dev/null | awk '{print $NF; exit}' | grep -E '^[0-9.]+$' | head -1)"
    fi
  elif command -v nslookup >/dev/null 2>&1; then
    mx="$(nslookup -type=mx "$dom" 2>/dev/null | awk '/mail exchanger/ {print $NF; exit}')"
    mx="${mx%.}"
    if [[ -z "$mx" ]]; then
      # erst nach einer "Name:"-Zeile lesen – sonst fängt man die Adresse des DNS-Servers
      mx="$(nslookup "$dom" 2>/dev/null \
        | awk '/^Name/{f=1; next} f && /^Address/{print $NF; exit}')"
    fi
  fi
  [[ -n "$mx" ]] && echo "$mx"
}

# SMTP-Host automatisch ermitteln (nur Vorschlag – ENV/editierbar gewinnt):
# 1) Postfix-Relayhost des PVE-Hosts (postconf bzw. main.cf),
# 2) sonst MX/A der Absender-Domain (MAIL_FROM), sonst der eigenen Domain.
DETECTED_SMTP_HOST=""; DETECTED_SMTP_PORT=""
detect_smtp() {
  [[ -n "${SMTP_HOST:-}" ]] && return 0
  local relay host port
  if command -v postconf >/dev/null 2>&1; then
    relay="$(postconf -h relayhost 2>/dev/null | head -1 | tr -d '[:space:]')"
  elif [[ -r /etc/postfix/main.cf ]]; then
    relay="$(awk -F= '$1 ~ /^[[:space:]]*relayhost[[:space:]]*$/ {print $2; exit}' \
      /etc/postfix/main.cf | tr -d '[:space:]')"
  fi
  if [[ -n "$relay" ]]; then
    host="$relay"; port=""
    if [[ "$relay" =~ ^\[([^\]]+)\](:([0-9]+))?$ ]]; then        # [mail.tld]:587 | [1.2.3.4]
      host="${BASH_REMATCH[1]}"; port="${BASH_REMATCH[3]:-}"
    elif [[ "$relay" =~ ^([^:]+):([0-9]+)$ ]]; then              # mail.tld:587
      host="${BASH_REMATCH[1]}"; port="${BASH_REMATCH[2]}"
    fi
    DETECTED_SMTP_HOST="$host"; DETECTED_SMTP_PORT="$port"
    return 0
  fi
  local dom
  if [[ "${MAIL_FROM:-}" == *"@"* ]]; then
    dom="${MAIL_FROM##*@}"                                       # lenovo@aomme.de -> aomme.de
  else
    local fqdn
    fqdn="$(hostname -f 2>/dev/null | head -1)"
    dom="${fqdn#*.}"
  fi
  [[ -n "$dom" && "$dom" == *.* ]] || return 0
  DETECTED_SMTP_HOST="$(mx_for_domain "$dom")"
}

# ---------- Bridge-Helfer (per-Bridge Gateway/IP-Basis, Auswahl-Menü) ----------
list_bridges() {
  local out
  out="$(ip -o link show type bridge 2>/dev/null | awk -F': ' '{print $2}')"
  [[ -n "$out" ]] || out="$(ip -o link 2>/dev/null | awk -F': ' '$2 ~ /^vmbr/ {print $2}')"
  [[ -n "$out" ]] && printf '%s\n' "$out" | sort
}

bridge_gateway() {  # Gateway-IP, die über <bridge1> geroutet wird (leer = keine)
  ip -4 route show default dev "$1" 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++) if($i=="via") {print $(i+1); exit}}'
}

bridge_base_ip() {  # Netzpräfix der Host-IP auf <bridge1>, z. B. 192.168.1 aus 192.168.1.5/24
  local ip_cidr
  ip_cidr="$(ip -4 addr show dev "$1" scope global 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++) if($i=="inet") {print $2; exit}}')"
  [[ -n "$ip_cidr" ]] || return 0
  local ip="${ip_cidr%/*}"
  echo "${ip%.*}"
}

# Bridge-Auswahl: Menü aller Host-Bridges (mit je-Bridge Gateway/IP-Basis im
# Beschreibungstext, Default-Route markiert) oder freie Eingabe. Nach der Wahl
# passen sich GATEWAY/BASE_IP der gewählten Bridge an (nur wenn dort erkannt).
ui_pick_bridge() {
  local bridges sel b gw base desc
  bridges="$(list_bridges)"
  if [[ -z "$bridges" ]] || ! ui_available; then
    wt_fill BRIDGE "$(t 'Bridge')" "$(t 'Proxmox-Bridge (z. B. vmbr0):')" || return 1
  else
    local items=()
    while IFS= read -r b; do
      [[ -n "$b" ]] || continue
      gw="$(bridge_gateway "$b")"; base="$(bridge_base_ip "$b")"
      desc="Gateway ${gw:-—} · IP-Basis ${base:-—}"
      [[ "$b" == "${DETECTED_BRIDGE:-}" ]] && desc+="$(t ' · Default-Route')"
      items+=("$b" "$desc")
    done <<< "$bridges"
    items+=("custom" "$(t 'Bridge manuell eingeben')")
    sel="$(wt_menu "$(t 'Bridge wählen')" \
      "$(t 'Bridge für das CT – Gateway/IP-Basis passen sich der Auswahl an:')" \
      "${items[@]}")" || return 1
    if [[ "$sel" == "custom" ]]; then
      wt_fill BRIDGE "Bridge" "Proxmox-Bridge (z. B. vmbr0):" || return 1
    else
      BRIDGE="$sel"
    fi
  fi
  # Gateway/IP-Basis konsequent aus der gewählten Bridge ableiten – passt je nach
  # vmbr. Hat die Bridge keine Route (wurde also nicht erkannt), gilt die gängige
  # Konvention <IP-Basis>.1 als Vorschlag – editierbar; wenn auch die IP-Basis
  # fehlt, fragt das Gateway-Feld leer ab.
  GATEWAY="$(bridge_gateway "$BRIDGE")"
  BASE_IP="$(bridge_base_ip "$BRIDGE")"
  [[ -n "$GATEWAY" ]] || GATEWAY="${BASE_IP:+${BASE_IP}.1}"
  return 0
}

require_config() {
  local missing=() v
  local need=(VMID CT_HOSTNAME RAM SWAP CPULIMIT DISK BRIDGE GATEWAY BASE_IP \
              MAIL_FROM MAIL_TO SMTP_HOST SMTP_PORT COMMODITY_KEYS)
  [[ "$BUILD_METHOD" == "build" ]] && need+=(BUILD_VMID)
  for v in "${need[@]}"; do
    [[ -n "${!v:-}" ]] || missing+=("$v")
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    die "$(t 'Keine Standardwerte – interaktiv ausführen oder per Env setzen. Fehlend:') ${missing[*]}"
  fi
  [[ "$VMID" =~ ^[0-9]+$ ]] || die "$(t 'VMID muss numerisch sein.')"
  if [[ "$BUILD_METHOD" == "build" ]]; then
    [[ "$BUILD_VMID" =~ ^[0-9]+$ ]] || die "$(t 'BUILD_VMID muss numerisch sein.')"
    [[ "$VMID" != "$BUILD_VMID" ]] || die "$(t 'VMID und BUILD_VMID dürfen nicht identisch sein.')"
  fi
  [[ "$RAM" =~ ^[0-9]+$ ]] || die "$(t 'RAM muss numerisch sein.')"
  [[ "$SWAP" =~ ^[0-9]+$ ]] || die "$(t 'SWAP muss numerisch sein.')"
  [[ "$CPULIMIT" =~ ^[0-9]+([.][0-9]+)?$ ]] || die "$(t 'CPULIMIT muss numerisch sein (z. B. 0.25).')"
  [[ "$SMTP_PORT" =~ ^[0-9]+$ ]] || die "$(t 'SMTP_PORT muss numerisch sein.')"
  BASE_IP="$(normalize_base_ip "$BASE_IP")"
  is_ipv4 "$GATEWAY" || die "$(t 'GATEWAY muss eine gültige IPv4 sein (z. B. 10.10.10.1), aktuell:') '$GATEWAY'."
  [[ "$BASE_IP" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]] \
    || die "$(t 'BASE_IP muss eine IP-Basis ohne Punkt am Ende sein (z. B. 10.10.10), aktuell:') '$BASE_IP'."
}

prepare_net() {
  [[ -n "$NET0" ]] && return 0
  BASE_IP="$(normalize_base_ip "$BASE_IP")"   # defensiv: trailing dots / volle IP
  NET0="name=eth0,bridge=${BRIDGE},gw=${GATEWAY},ip=${BASE_IP}.${VMID}/24,type=veth"
}

# --------------------- Bestehende Installation erkennen ----------------------
DETECTED_VMID=""
detect_installed_ct() {
  local id
  DETECTED_VMID=""
  for id in $(pct list 2>/dev/null | awk 'NR>1 {print $1}'); do
    if pct config "$id" 2>/dev/null | grep -q '^tags:.*lenovo-tracker'; then
      DETECTED_VMID="$id"
      return 0
    fi
  done
  return 1
}

# ------------------------- Config-Editor (bestehend) -------------------------
toml_value() {  # toml_value <file> <section> <key> -> Rohwert
  awk -v s="[$2]" -v k="$3" '
    /^[[:space:]]*\[/ { sec=$0; gsub(/[[:space:]]/,"",sec) }
    sec==s {
      line=$0
      sub(/^[[:space:]]*/,"",line)
      if (line ~ "^" k "[[:space:]]*=") {
        sub("^" k "[[:space:]]*=[[:space:]]*","",line)
        print line; exit
      }
    }
  ' "$1"
}

toml_array_items() {  # ["a", "b"] -> a,b
  grep -o '"[^"]*"' <<< "$1" | tr -d '"' | tr '\n' ',' | sed 's/,$//' || true
}

toml_set() {  # toml_set <file> <section> <key> <value>
  local file="$1" section="$2" key="$3" val="$4"
  awk -v s="[$section]" -v k="$key" -v v="$val" '
    /^[[:space:]]*\[/ { sec=$0; gsub(/[[:space:]]/,"",sec) }
    sec==s {
      line=$0
      sub(/^[[:space:]]*/,"",line)
      if (line ~ "^" k "[[:space:]]*=") {
        print k " = " v
        found=1
        next
      }
    }
    { print }
    END { if (!found) exit 3 }
  ' "$file" > "$file.tmp" || { rm -f "$file.tmp"; return 1; }
  mv "$file.tmp" "$file"
}

toml_insert_after() {  # toml_insert_after <file> <section> <afterkey> <key> <value>
  local file="$1" section="$2" after="$3" key="$4" val="$5"
  awk -v s="[$section]" -v a="$after" -v k="$key" -v v="$val" '
    /^[[:space:]]*\[/ { sec=$0; gsub(/[[:space:]]/,"",sec) }
    sec==s {
      line=$0
      sub(/^[[:space:]]*/,"",line)
      if (line ~ "^" a "[[:space:]]*=") {
        print
        print k " = " v
        inserted=1
        next
      }
    }
    { print }
    END { if (!inserted) exit 3 }
  ' "$file" > "$file.tmp" || { rm -f "$file.tmp"; return 1; }
  mv "$file.tmp" "$file"
}

cmd_edit_config() {
  local id="${1:-${DETECTED_VMID:-$VMID}}"
  require_ctid "$id" "edit-config"
  ui_available || die "$(t 'Config-Editor braucht whiptail/TTY.')"
  require_running_ct "$id" "$(t 'bitte starten.')"

  local cfg="/root/lenovotracker-config-edit.toml"
  pct exec "$id" -- cat "${CONF_DIR}/config.toml" > "$cfg" 2>/dev/null
  [[ -s "$cfg" ]] || die "$(t 'Config in CT') ${id} $(t 'nicht lesbar.')"

  local commodity keywords mail_from mail_to mail_extra smtp_host smtp_port v
  commodity="$(toml_array_items "$(toml_value "$cfg" tracker commodity_keys)")"
  keywords="$(toml_array_items "$(toml_value "$cfg" tracker name_keywords)")"
  mail_from="$(toml_value "$cfg" mail from | unquote)"
  mail_to="$(toml_array_items "$(toml_value "$cfg" mail to)")"
  mail_extra="$(toml_array_items "$(toml_value "$cfg" mail extra_recipients)")"
  smtp_host="$(toml_value "$cfg" mail smtp_host | unquote)"
  smtp_port="$(toml_value "$cfg" mail smtp_port)"

  v="$(wt_input "$(t 'Commodity-Keys')" "$(t 'Komma-getrennt, lowercase:')" "$commodity")" || exit 0
  [[ -n "$v" ]] || die "$(t 'Commodity-Keys dürfen nicht leer sein.')"
  commodity="$v"
  v="$(wt_input "$(t 'Keyword-Filter')" "$(t 'Komma-getrennt, leer = alle Mainboards:')" "$keywords")" || exit 0
  keywords="$v"
  v="$(wt_input "$(t 'Mail von')" "$(t 'Absender-Adresse:')" "$mail_from")" || exit 0
  [[ -n "$v" ]] || die "$(t 'Mail von darf nicht leer sein.')"
  mail_from="$v"
  v="$(wt_input "$(t 'Mail an')" "$(t 'Empfänger, komma-getrennt:')" "$mail_to")" || exit 0
  [[ -n "$v" ]] || die "$(t 'Mail an darf nicht leer sein.')"
  mail_to="$v"
  v="$(wt_input "$(t 'Weitere Empfänger')" "$(t 'Komma-getrennt, leer = keine:')" "$mail_extra")" || exit 0
  mail_extra="$v"
  v="$(wt_input "$(t 'SMTP-Host')" "$(t 'SMTP-Server:')" "$smtp_host")" || exit 0
  [[ -n "$v" ]] || die "$(t 'SMTP-Host darf nicht leer sein.')"
  smtp_host="$v"
  smtp_user="$(toml_value "$cfg" mail smtp_user | unquote)"
  smtp_pass="$(toml_value "$cfg" mail smtp_pass | unquote)"
  v="$(wt_input "$(t 'SMTP-Login')" "$(t 'SMTP-Benutzer (leer = kein Login/Plain-SMTP):')" "$smtp_user")" || exit 0
  smtp_user="$v"
  v="$(wt_input "$(t 'SMTP-Passwort')" "$(t 'SMTP- / App-Passwort (leer = kein Login):')" "$smtp_pass")" || exit 0
  smtp_pass="$v"
  v="$(wt_input "$(t 'SMTP-Port')" "$(t 'SMTP-Port (mit Login: 587):')" "$smtp_port")" || exit 0
  [[ "$v" =~ ^[0-9]+$ ]] || die "$(t 'SMTP-Port muss numerisch sein.')"
  smtp_port="$v"

  toml_set "$cfg" tracker commodity_keys "$(toml_array "$commodity")" || die "commodity_keys nicht gefunden."
  toml_set "$cfg" tracker name_keywords "$(toml_array "$keywords")" || die "name_keywords nicht gefunden."
  toml_set "$cfg" mail from "\"${mail_from}\"" || die "from nicht gefunden."
  toml_set "$cfg" mail to "$(toml_array "$mail_to")" || die "to nicht gefunden."
  toml_set "$cfg" mail extra_recipients "$(toml_array "$mail_extra")" || die "extra_recipients nicht gefunden."
  toml_set "$cfg" mail smtp_host "\"${smtp_host}\"" || die "smtp_host nicht gefunden."
  toml_set "$cfg" mail smtp_port "$smtp_port" || die "smtp_port nicht gefunden."
  # Alte Configs haben die Keys evtl. noch nicht – dann nach smtp_port einfügen
  toml_set "$cfg" mail smtp_user "\"${smtp_user}\"" \
    || toml_insert_after "$cfg" mail smtp_port "smtp_user" "\"${smtp_user}\"" || die "smtp_user nicht setzbar."
  toml_set "$cfg" mail smtp_pass "\"${smtp_pass}\"" \
    || toml_insert_after "$cfg" mail smtp_port "smtp_pass" "\"${smtp_pass}\"" || die "smtp_pass nicht setzbar."

  push_ct_config "$id" "$cfg"
  rm -f "$cfg"
  msg_ok "$(t 'Config gespeichert (Backup:') ${CONF_DIR}/config.toml.bak)"

  local dry
  dry="$(pct exec "$id" -- "${APP_DIR}/lenovo-tracker" --config "${CONF_DIR}/config.toml" --dry-run 2>&1 || true)"
  if echo "$dry" | grep -q "DRY-RUN"; then
    msg_ok "$dry"
  else
    msg_warn "Dry-Run: $dry"
  fi
}

ui_summary_confirm() {
  local bininfo="$(t 'Binary: GitHub Release (Download, kein Build-CT)')"
  [[ "$BUILD_METHOD" == "build" ]] && bininfo="$(t 'Binary: Build-CT') ${BUILD_VMID} ($(t 'Fallback-Build'))"
  local smtpinfo="${SMTP_HOST}:${SMTP_PORT}"
  [[ -n "${SMTP_USER:-}" ]] && smtpinfo+=" · $(t 'Login') ${SMTP_USER}" || smtpinfo+=" · plain"
  local text="$(t 'CT'): ${VMID} (${CT_HOSTNAME}) · ${bininfo}
RAM: ${RAM} MB + ${SWAP} MB Swap · CPU-Limit: ${CPULIMIT} · Disk: ${DISK}
$(t 'Netz'): ${BRIDGE} · ${BASE_IP}.${VMID}/24 · GW ${GATEWAY}

Mail: ${MAIL_FROM} -> ${MAIL_TO}
SMTP: ${smtpinfo}

Filter: ${COMMODITY_KEYS}
Keywords: ${KEYWORDS:-alle}"
  wt_yesno "$(t 'Setup starten')" "$text"
}

ui_ct_exists() {  # rc 0 = neu aufbauen
  local sel
  sel="$(wt_menu "$(t 'CT ${VMID} existiert')" "$(t 'Was möchtest du tun?')" \
    "edit"    "Config bearbeiten (State bleibt erhalten)" \
    "rebuild" "Neu aufbauen (State/History gehen verloren)" \
    "cancel"  "Abbrechen")" || exit 0
  case "$sel" in
    edit)    cmd_edit_config "$VMID"; exit 0 ;;
    rebuild) return 0 ;;
    cancel)  die "$(t 'Abgebrochen – CT') ${VMID} $(t 'bleibt unangetastet.')" ;;
  esac
}

# ------------------------------- Unterkommandos ------------------------------
build_in_ct() {  # Fallback: Binary im temporären Build-CT bauen
  fetch_sources
  msg_info "$(t 'Build-CT') ${BUILD_VMID} $(t 'vorbereiten (Fallback-Build)')"
  pct destroy "${BUILD_VMID}" --purge >/dev/null 2>&1 || true
  spin_run "Build-CT anlegen" pct create "${BUILD_VMID}" "$1" \
    --hostname rust-build --memory "${BUILD_RAM}" --cores 4 --swap 512 \
    --net0 "${NET0/${VMID}/${BUILD_VMID}}" \
    --rootfs "${BUILD_DISK}" --unprivileged 1 --start 1
  if ! wait_ct "${BUILD_VMID}"; then die "$(t 'Build-CT startet nicht')"; fi
  msg_ok "$(t 'Build-CT läuft')"
  spin_run "Rust-Toolchain installieren (kein OpenSSL/cmake/go)" \
    pct exec "${BUILD_VMID}" -- sh -c 'apk update -q && apk add -q rust cargo build-base pkgconf musl-dev ca-certificates'
  msg_info "$(t 'Quellen übertragen + bauen (12 Unit-Tests inklusive)')"
  pct exec "${BUILD_VMID}" -- mkdir -p /build/src
  pct push "${BUILD_VMID}" "$SRC_CARGO" /build/Cargo.toml
  pct push "${BUILD_VMID}" "$SRC_MAIN" /build/src/main.rs
  rm -f "$HOST_BIN"
  spin_run "cargo test --release (12 Tests)" pct exec "${BUILD_VMID}" -- sh -c 'cd /build && cargo test --release 2>&1 | grep "test result"'
  spin_run "cargo rustc --release (statisch, musl)" pct exec "${BUILD_VMID}" -- sh -c 'cd /build && cargo rustc --release -- -C target-feature=+crt-static' \
    || die "$(t 'Statischer Build fehlgeschlagen (Log:') ${LAST_LOG})"
  pct pull "${BUILD_VMID}" /build/target/release/lenovo-tracker "$HOST_BIN" \
    || die "$(t 'Binary nicht aus dem Build-CT ladbar – Build fehlgeschlagen?')"
  command -v strip >/dev/null 2>&1 && strip -s "$HOST_BIN" || true
  pct destroy "${BUILD_VMID}" --purge >/dev/null || msg_warn "$(t 'Build-CT') ${BUILD_VMID} $(t 'konnte nicht entfernt werden')"
  msg_ok "$(t 'Binary gebaut:') ${HOST_BIN} ($(stat -c %s "$HOST_BIN") B)"
}

cmd_setup() {
  prereq
  ensure_ui || true
  detect_installed_ct || true
  [[ -n "$VMID" ]] || VMID="${DETECTED_VMID:-}"
  detect_net_defaults
  detect_smtp
  apply_defaults   # Prefill für die Dialoge – ENV-Setzungen bleiben erhalten

  if ui_available; then
    local sel menu_text="$(t 'Option wählen (Pfeiltasten/TAB, ENTER bestätigt):')"
    local menu=()
    if [[ -n "$DETECTED_VMID" ]]; then
      menu_text+="\n\n$(t 'Erkannte Installation: CT') ${DETECTED_VMID}"
      menu+=("config" "Config bearbeiten (CT ${DETECTED_VMID})")
    fi
    menu+=("install"  "$(t 'Installation')" \
           "status"   "$(t 'Status & Log anzeigen')" \
           "exit"     "$(t 'Beenden')")
    sel="$(wt_menu "Lenovo-Tracker Setup" "$menu_text" "${menu[@]}")" || exit 0
    case "$sel" in
      config)    ui_vmid; cmd_edit_config "$VMID"; exit 0 ;;
      status)    ui_vmid; cmd_status "$VMID"; exit 0 ;;
      exit)      exit 0 ;;
      install)   ui_configure ;;
    esac
    ui_summary_confirm || die "$(t 'Abgebrochen.')"
    clear
    header_info
  else
    # Kein whiptail/TTY: Werte ausschließlich aus Env-Variablen
    header_info
    msg_warn "$(t 'Whiptail/TTY nicht verfügbar – NONINTERACTIVE: alle Werte müssen als Env-Variablen gesetzt sein.')"
  fi

  require_config
  prepare_net
  ensure_template
  local tpl="$TEMPLATE_TPL"

  # ---------- 1) Binary besorgen (GitHub-Release → vorhandenes Binary → Opt-in Build-CT) ----------
  local bin_ok=0
  if [[ "$BUILD_METHOD" != "build" ]]; then
    msg_info "$(t '1/6  Binary von GitHub Releases laden (kein Build-CT nötig)')"
    if spin_run "Release-Download" ct_download "$RELEASE_URL" "${HOST_BIN}.tmp" && [[ -s "${HOST_BIN}.tmp" ]]; then
      mv "${HOST_BIN}.tmp" "$HOST_BIN" && chmod +x "$HOST_BIN"
      msg_ok "$(t 'Binary aus Releases:') ${HOST_BIN} ($(stat -c %s "$HOST_BIN") B)"
      bin_ok=1
    else
      msg_warn "$(t 'Release-Download fehlgeschlagen')"
      rm -f "${HOST_BIN}.tmp"
    fi
  fi
  if [[ $bin_ok -eq 0 && "$SKIP_BUILD" == "1" && -f "$HOST_BIN" ]]; then
    msg_ok "SKIP_BUILD=1 – $(t 'nutze vorhandenes Binary') ${HOST_BIN}"
    bin_ok=1
  fi
  if [[ $bin_ok -eq 0 && "$BUILD_METHOD" == "build" ]]; then
    build_in_ct "$tpl"
    bin_ok=1
  fi
  if [[ $bin_ok -eq 0 ]]; then
    die "$(t 'Kein Binary verfügbar (Release-Download fehlgeschlagen). Später erneut versuchen – oder BUILD_METHOD=build BUILD_VMID=<id> setzen, um explizit einen temporären Build-CT zu nutzen.')"
  fi

  # ---------- 2) Produktions-CT ----------
  msg_info "$(t '2/6  Produktions-CT') ${VMID} $(t 'anlegen')"
  if pct status "${VMID}" >/dev/null 2>&1; then
    if [[ "$FORCE" == "1" ]]; then
      msg_warn "$(t 'CT') ${VMID} $(t 'existiert – FORCE=1: stoppe + lösche')"
      pct stop "${VMID}" >/dev/null 2>&1 || true
      pct destroy "${VMID}" --purge
    elif ui_available; then
      ui_ct_exists
      pct stop "${VMID}" >/dev/null 2>&1 || true
      pct destroy "${VMID}" --purge
    else
      die "$(t 'CT') ${VMID} $(t 'existiert bereits. FORCE=1 für Neuaufbau (State geht verloren!) oder VMID ändern.')"
    fi
  fi
  spin_run "CT ${VMID} erstellen" pct create "${VMID}" "$tpl" \
    --hostname "${CT_HOSTNAME}" --memory "${RAM}" --swap "${SWAP}" --cpulimit "${CPULIMIT}" \
    --net0 "$NET0" --rootfs "${DISK}" --unprivileged 1 --onboot 1 \
    --tags lenovo-tracker --start 1
  sleep 3
  if ! wait_ct "${VMID}"; then die "$(t 'CT') ${VMID} $(t 'startet nicht')"; fi
  msg_ok "$(t 'CT') ${VMID} $(t 'läuft')"

  # ---------- 3) Binary + curl-impersonate ----------
  msg_info "$(t '3/6  Binary + curl-impersonate installieren')"
  pct exec "${VMID}" -- mkdir -p "$APP_DIR" "$CONF_DIR" "$STATE_DIR"
  pct push "${VMID}" "$HOST_BIN" "${APP_DIR}/lenovo-tracker"
  pct exec "${VMID}" -- chmod +x "${APP_DIR}/lenovo-tracker"
  local ci_tar="/tmp/curl-impersonate-${CI_VERSION}.tar.gz"
  spin_run "curl-impersonate ${CI_VERSION} laden" ct_download "$CI_URL" "$ci_tar"
  rm -rf /tmp/ci-extract && mkdir -p /tmp/ci-extract
  tar xzf "$ci_tar" -C /tmp/ci-extract
  local ci_bin
  ci_bin="$(find /tmp/ci-extract -name 'curl-impersonate' -type f | head -1)"
  [[ -n "$ci_bin" ]] || die "$(t 'curl-impersonate-Binary nicht im Tarball gefunden')"
  pct push "${VMID}" "$ci_bin" "${APP_DIR}/curl-impersonate"
  pct exec "${VMID}" -- chmod +x "${APP_DIR}/curl-impersonate"
  rm -rf /tmp/ci-extract "$ci_tar"
  if pct exec "${VMID}" -- "${APP_DIR}/curl-impersonate" --version >/dev/null 2>&1; then
    msg_ok "$(t 'curl-impersonate einsatzbereit')"
  else
    msg_warn "$(t 'curl-impersonate --version schlug fehl – prüfen!')"
  fi

  # ---------- 4) Config ----------
  msg_info "$(t '4/6  Config generieren') (${CONF_DIR}/config.toml)"
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
smtp_user = "${SMTP_USER}"
smtp_pass = "${SMTP_PASS}"

[paths]
state_dir = "${STATE_DIR}"
EOF
  pct push "${VMID}" "$cfg" "${CONF_DIR}/config.toml"
  rm -f "$cfg"
  msg_ok "$(t 'Config installiert')"

  # ---------- 5) Cron + Logging ----------
  msg_info "$(t '5/6  Cron (*/30) + Logging einrichten')"
  pct exec "${VMID}" -- sh -c \
    "grep -q 'lenovo-tracker' /etc/crontabs/root 2>/dev/null || echo '*/30 * * * * ${APP_DIR}/lenovo-tracker --config ${CONF_DIR}/config.toml --once >> ${LOG_FILE} 2>&1' >> /etc/crontabs/root"
  pct exec "${VMID}" -- sh -c \
    "grep -q 'crond -f' /etc/inittab || echo '::respawn:/usr/sbin/crond -f -l 8 -c /etc/crontabs' >> /etc/inittab"
  pct exec "${VMID}" -- touch "$LOG_FILE"
  pct exec "${VMID}" -- sh -c 'kill -HUP 1 2>/dev/null || true' >/dev/null 2>&1 || true
  msg_ok "$(t 'crond läuft init-überwacht (::respawn), Intervall */30')"

  # ---------- 6) Verifikation ----------
  msg_info "$(t '6/6  Verifikation (--dry-run gegen die Live-API)')"
  local dry
  dry="$(pct exec "${VMID}" -- "${APP_DIR}/lenovo-tracker" --config "${CONF_DIR}/config.toml" --dry-run 2>&1 || true)"
  if echo "$dry" | grep -q "DRY-RUN"; then
    msg_ok "$dry"
  elif echo "$dry" | grep -q "Error relocating"; then
    die "$(t 'Binary läuft im CT nicht (dynamisch gelinkt?) – Build mit crt-static prüfen.')"
  else
    msg_warn "$(t 'Dry-Run lieferte keine Daten (Akamai 403? Netzwerk?):') $dry"
    msg_warn "$(t 'Der Cron versucht es alle 30 Min erneut – Log:') pct exec ${VMID} -- tail ${LOG_FILE}"
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

  # Test-Mail direkt anbieten – fängt falsche Empfänger/SMTP sofort ab
  if [[ "$NONINTERACTIVE" != "1" ]] && ui_available && wt_yesno "$(t 'Test-Mail')" \
    "Jetzt eine Test-Mail senden, um from/to/SMTP zu prüfen?"; then
    if cmd_testmail "$VMID"; then
      msg_ok "$(t 'Mailzustellung geprüft.')"
    else
      msg_warn "$(t 'Setup bleibt abgeschlossen – Mail erst bei korrigierter Config.')"
    fi
  fi
}

cmd_probe() {
  local id="${1:-$VMID}"
  require_ctid "$id" "probe"
  require_running_ct "$id" "$(t 'Probe braucht den laufenden Tracker-CT.')"
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
  local id="${1:-$VMID}"
  require_ctid "$id" "peak"
  pct status "$id" >/dev/null 2>&1 || die "$(t 'CT') ${id} $(t 'existiert nicht.')"
  local CG="/sys/fs/cgroup/lxc/${id}"
  [[ -f "${CG}/memory.peak" ]] || die "$(t 'Cgroup-Pfad') ${CG}/memory.peak $(t 'nicht gefunden')"
  msg_warn "$(t 'CT ${id} wird neu gestartet (nullt den Cgroup-Peak) – kurze Unterbrechung')"
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

cmd_status() {
  local id="${1:-$VMID}"
  require_ctid "$id" "status"
  echo "== CT ${id} =="
  if ! pct status "$id" >/dev/null 2>&1; then
    msg_warn "$(t 'CT') ${id} $(t 'existiert nicht (noch nicht installiert).')"
    return 0
  fi
  pct status "$id"
  echo "== Cron (CT ${id}) =="
  pct exec "$id" -- sh -c "grep lenovo-tracker /etc/crontabs/root 2>/dev/null || echo '(kein Cron-Eintrag)'"
  echo "== Letzte Log-Zeilen =="
  pct exec "$id" -- sh -c "tail -n 15 ${LOG_FILE} 2>/dev/null || echo '(kein Log)'"
  echo "== State =="
  pct exec "$id" -- sh -c "cat ${STATE_DIR}/state.json 2>/dev/null || echo '(kein State – Baseline noch nicht gesetzt)'"
  echo
  echo "Config: pct exec ${id} -- vi ${CONF_DIR}/config.toml"
}

# SMTP-Login einrichten (authentifizierte Submission über STARTTLS): fragt
# Benutzer + Token ab, trägt sie in die Config ein (mit Backup), setzt für
# Proton automatisch den Submission-Server/-Port und sendet sofort eine
# Test-Mail. Leerer Benutzer = zurück auf Plain-SMTP.
cmd_smtp_auth() {
  local id="${1:-$VMID}" cfg smtp_host smtp_port smtp_user smtp_pass v
  prereq
  require_ctid "$id" "smtp-auth"
  require_running_ct "$id" "bitte starten."
  ui_available || die "$(t 'SMTP-Login-Setup braucht einen Dialog (TTY) – oder NONINTERACTIVE-Setup mit SMTP_USER/SMTP_PASS per Env.')"
  cfg="/root/lenovotracker-smtp.toml"
  pct pull "$id" "${CONF_DIR}/config.toml" "$cfg" || die "$(t 'Config aus CT') ${id} $(t 'nicht ladbar.')"
  [[ -s "$cfg" ]] || die "Config in CT ${id} nicht lesbar."

  smtp_host="$(toml_value "$cfg" mail smtp_host | unquote)"
  smtp_port="$(toml_value "$cfg" mail smtp_port)"
  smtp_user="$(toml_value "$cfg" mail smtp_user | unquote)"
  smtp_pass="$(toml_value "$cfg" mail smtp_pass | unquote)"

  v="$(wt_input "$(t 'SMTP-Login')" "$(t 'SMTP-Benutzer (optional; Proton: deine Adresse – leer = plain):')" "$smtp_user")" || exit 0
  smtp_user="$v"
  v="$(whiptail --backtitle "$BACKTITLE" --title "SMTP-Token" --passwordbox \
    "SMTP-Passwort / Token (optional; Proton: SMTP-Token · Gmail: App-Passwort · Provider-abhängig):" \
    12 70 "$smtp_pass" 3>&1 1>&2 2>&3)" || exit 0
  smtp_pass="$v"

  if [[ -n "$smtp_user" ]]; then
    # Proton-MX-Hosts sind für Submission nicht gedacht → Submission-Server setzen
    if [[ -z "$smtp_host" || "$smtp_host" =~ ^mail(sec)?\.protonmail\.ch$ ]]; then
      smtp_host="smtp.protonmail.ch"
      msg_info "$(t 'SMTP-Host auf Submission-Server gesetzt:') ${smtp_host}"
    else
      v="$(wt_input "$(t 'SMTP-Host')" "$(t 'SMTP-Server für die Submission:')" "$smtp_host")" || exit 0
      smtp_host="$v"
    fi
    [[ -z "$smtp_port" || "$smtp_port" == "25" ]] && smtp_port="587"
    msg_info "$(t 'Port:') ${smtp_port} (STARTTLS)"
  else
    smtp_pass=""
  fi

  toml_set "$cfg" mail smtp_host "\"${smtp_host}\"" || die "smtp_host nicht gefunden."
  [[ -z "$smtp_port" ]] || toml_set "$cfg" mail smtp_port "$smtp_port" || die "smtp_port nicht gefunden."
  toml_set "$cfg" mail smtp_user "\"${smtp_user}\"" \
    || toml_insert_after "$cfg" mail smtp_port "smtp_user" "\"${smtp_user}\"" || die "smtp_user nicht setzbar."
  toml_set "$cfg" mail smtp_pass "\"${smtp_pass}\"" \
    || toml_insert_after "$cfg" mail smtp_port "smtp_pass" "\"${smtp_pass}\"" || die "smtp_pass nicht setzbar."

  push_ct_config "$id" "$cfg"
  rm -f "$cfg"
  if [[ -n "$smtp_user" ]]; then
    msg_ok "$(t 'SMTP-Login gesetzt:') ${smtp_user} -> ${smtp_host}:${smtp_port} (STARTTLS)"
  else
    msg_warn "$(t 'SMTP-Login geleert – Versand läuft wieder plain (Auth-Banner möglich).')"
  fi
  cmd_testmail "$id" || true
}

cmd_testmail() {
  local id="${1:-$VMID}"
  require_ctid "$id" "test-mail"
  require_running_ct "$id" "bitte starten."
  msg_info "$(t 'Sende Test-Mail (CT') ${id})"
  if pct exec "$id" -- "${APP_DIR}/lenovo-tracker" --config "${CONF_DIR}/config.toml" --test-mail; then
    return 0
  fi
  msg_warn "$(t 'Test-Mail fehlgeschlagen – Config korrigieren und erneut senden:')"
  msg_warn "  pct exec ${id} -- vi ${CONF_DIR}/config.toml   ($(t 'from/to/smtp_host prüfen'))"
  msg_warn "  pct exec ${id} -- ${APP_DIR}/lenovo-tracker --config ${CONF_DIR}/config.toml --test-mail"
  return 1
}

cmd_uninstall() {
  prereq
  [[ -n "${VMID:-}" ]] || die "$(t 'CT-ID (VMID) erforderlich.')"
  if [[ "$FORCE" != "1" ]]; then
    if ui_available; then
      wt_yesno "$(t 'Deinstallieren')" "$(t 'Löscht CT') ${VMID} $(t 'inkl. State/History und die Host-Binaries.

Wirklich deinstallieren?')" || { echo "Abbruch."; return; }
    else
      die "$(t 'Löschen braucht FORCE=1 im nicht-interaktiven Modus.')"
    fi
  fi
  if pct status "${VMID}" >/dev/null 2>&1; then
    pct stop "${VMID}" >/dev/null 2>&1 || true
    pct destroy "${VMID}" --purge && msg_ok "$(t 'CT') ${VMID} $(t 'entfernt')"
  else
    msg_warn "$(t 'CT') ${VMID} $(t 'existiert nicht – nichts zu löschen')"
  fi
  rm -f /root/lenovo-tracker-rs /root/lenovo-tracker-rs2 /root/lt-new \
        /root/curl-impersonate /root/curl-impersonate-check /root/lenovotracker-config.toml \
        /root/.lenovo-tracker-src
  msg_ok "$(t 'Host-Artefakte entfernt')"
}

cmd_help() {
  # Statischer Text: im curl-One-Liner (bash -c) gibt es kein BASH_SOURCE
  if [[ "$UI_LANG" == "en" ]]; then
    cat <<'EOF'
Lenovo Clearance-Sale Mainboard Tracker - Proxmox helper setup

The classic (on the Proxmox host):
  bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"

With a repo clone:
  bash lenovo-tracker-setup.sh                    # interactive menu
  NONINTERACTIVE=1 bash lenovo-tracker-setup.sh   # no dialogs - required values via env
  SKIP_BUILD=1 bash lenovo-tracker-setup.sh       # use existing binary
  FORCE=1 bash lenovo-tracker-setup.sh            # delete+recreate existing CT

Subcommands (also in the one-liner):
  ... --edit-config [VMID] edit an existing config
  ... --smtp-auth [VMID]   set SMTP login/token (optional, any provider) + test mail
  ... --status [VMID]      show CT status, cron, log and state
  ... --test-mail [VMID]   send a test e-mail
  ... --probe [VMID]       live API parameter matrix test
  ... --peak [VMID]        measure peak RAM (CT restart, 3 runs)
  ... --uninstall          remove CT + host artifacts

Required values (no defaults - asked interactively or set via env):
  MAIL_FROM MAIL_TO   (VMID/SMTP_HOST get sensible suggestions; SMTP login optional)

Defaults (pre-filled, overridable via env, editable in dialogs):
  CT_HOSTNAME=LenovoTracker  RAM=32  SWAP=32  CPULIMIT=0.25  DISK=local:0.25
  COMMODITY_KEYS=system boards  VMID=first free ID from 100 (pct+qm)
  BRIDGE: menu of host bridges - gateway/IP base follow the selection
  SMTP_HOST: auto-detected (host Postfix relay, else MX/A of sender domain)
  SMTP_USER/SMTP_PASS: empty = plain SMTP; set = STARTTLS submission (port 587),
  provider signs DKIM/SPF (Proton: smtp.protonmail.ch + SMTP token)

More optional: NET0 EXTRA_RECIPIENTS KEYWORDS FORCE SKIP_BUILD BUILD_METHOD CI_VERSION UI_LANG=en|de
  BUILD_METHOD=build BUILD_VMID=<id>  -> explicitly use a temporary build CT
  (default: binary is downloaded from GitHub releases, no build CT)

License: MIT - free to use (see LICENSE). Copyright (c) 2026 LMasterPiec3
EOF
  else
    cat <<'EOF'
Lenovo Clearance-Sale Mainboard-Tracker – Proxmox-Helper-Setup

Der Klassiker (auf dem Proxmox-Host):
  bash -c "$(curl -fsSL https://raw.githubusercontent.com/LMasterPiec3/LenovoTracker/main/lenovo-tracker-setup.sh)"

Mit Repo-Klon:
  bash lenovo-tracker-setup.sh                    # interaktives Menü
  NONINTERACTIVE=1 bash lenovo-tracker-setup.sh   # ohne Dialoge – Pflichtwerte per Env
  SKIP_BUILD=1 bash lenovo-tracker-setup.sh       # vorhandenes Binary nutzen
  FORCE=1 bash lenovo-tracker-setup.sh            # existierenden CT löschen+neu

Unterkommandos (auch im One-Liner):
  … --edit-config [VMID] bestehende Config bearbeiten
  … --smtp-auth [VMID]   SMTP-Login/Token einrichten (optional, alle Provider) + Test-Mail
  … --status [VMID]      CT-Status, Cron, Log und State anzeigen
  … --test-mail [VMID]   Test-Mail senden
  … --probe [VMID]       API-Parameter-Matrix live testen
  … --peak [VMID]        Peak-RAM messen (CT-Restart, 3 Läufe)
  … --uninstall          CT + Host-Artefakte entfernen

Pflichtwerte (interaktiv abgefragt oder per Env setzen):
  MAIL_FROM MAIL_TO   (VMID/SMTP_HOST bekommen sinnvolle Vorschläge; SMTP-Login optional)

Defaults (voreingestellt – per Env überschreibbar, im Dialog editierbar):
  CT_HOSTNAME=LenovoTracker  RAM=32  SWAP=32  CPULIMIT=0.25  DISK=local:0.25
  COMMODITY_KEYS=system boards  VMID=erste freie ID ab 100 (pct+qm)
  BRIDGE: Auswahl-Menü aller Host-Bridges – Gateway/IP-Basis passen sich je
  nach gewählter Bridge an; ohne Route gilt der Vorschlag <IP-Basis>.1
  SMTP_HOST: automatisch ermittelt (Postfix-Relayhost des Hosts, sonst MX/A
  der Absender-Domain)
  SMTP_USER/SMTP_PASS: leer = Plain-SMTP; gesetzt = Submission über STARTTLS
  (Port 587 automatisch), Provider signiert DKIM/SPF

Weitere Optionale: NET0 EXTRA_RECIPIENTS KEYWORDS FORCE SKIP_BUILD BUILD_METHOD CI_VERSION UI_LANG=en|de
  BUILD_METHOD=build BUILD_VMID=<id>  → explizit temporären Build-CT nutzen
  (Standard: Binary wird aus den GitHub-Releases geladen, kein Build-CT)

Lizenz: MIT – free to use (siehe LICENSE). Copyright (c) 2026 LMasterPiec3
EOF
  fi
}
# bash -c "$(curl …)" --probe legt das Argument in $0 (nicht $1) — beides akzeptieren:
ARGS=()
if [[ "${0:-}" == --* ]]; then ARGS+=("$0"); fi
ARGS+=("$@")
MODE="${ARGS[0]:-setup}"

case "$MODE" in
  setup)        cmd_setup ;;
  --edit-config) cmd_edit_config "${ARGS[1]:-$VMID}" ;;
  --status)     cmd_status "${ARGS[1]:-$VMID}" ;;
  --test-mail)  cmd_testmail "${ARGS[1]:-$VMID}" ;;
  --smtp-auth)  cmd_smtp_auth "${ARGS[1]:-$VMID}" ;;
  --probe)      cmd_probe "${ARGS[1]:-$VMID}" ;;
  --peak)       cmd_peak "${ARGS[1]:-$VMID}" ;;
  --uninstall)  cmd_uninstall ;;
  --help|-h)    cmd_help ;;
  *)            msg_error "$(t 'Unbekanntes Kommando:') $MODE ($(t 'siehe --help'))"; exit 1 ;;
esac
