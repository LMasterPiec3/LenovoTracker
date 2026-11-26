#!/bin/sh
# ============================================================================
#  Lenovo Clearance-API – Conditional-Request-Check (ETag / If-None-Match)
# ----------------------------------------------------------------------------
#  Prüft, ob die API auf If-None-Match bzw. If-Modified-Since mit
#  "304 Not Modified" antwortet – dann könnte der Tracker den ~3,3-MB-Download
#  bei unveränderten Daten sparen.
#
#  Läuft im Tracker-CT (busybox ash) und auf jedem Host mit curl-impersonate:
#
#      pct push 105 conditional-check.sh /tmp/conditional-check.sh
#      pct exec 105 -- sh /tmp/conditional-check.sh
#
#  Optionen: sh conditional-check.sh --help
#  Exit-Code: 0 = 304 unterstützt, 1 = nicht unterstützt/unklar, 2 = Fehler.
# ============================================================================
set -u

URL="${URL:-https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get}"
REFERER="${REFERER:-https://support.lenovo.com/de/de/parts-lookup/clearance-sale}"
IMPERSONATE="${IMPERSONATE:-chrome124}"
DELAY="${DELAY:-0}"
CURL="${CURL:-}"

usage() {
  cat <<'EOF'
Conditional-Request-Check für die Lenovo-Clearance-API

Aufruf:
  sh conditional-check.sh [Optionen]

Optionen:
  --url <URL>           zu prüfende URL (Default: DE-Clearance-API)
  --delay <Sekunden>    2. Abruf verzögern (Default: 0)
  --impersonate <Ziel>  curl-impersonate-Target (Default: chrome124)
  -h, --help            diese Hilfe

Env:
  CURL=/pfad/curl-impersonate   überschreibt die automatische Suche
  URL, DELAY, IMPERSONATE, REFERER

Exit-Code: 0 = 304 unterstützt, 1 = nicht unterstützt/unklar, 2 = Fehler.
EOF
}

die_usage() { echo "FEHLER: $1" >&2; exit 2; }

while [ $# -gt 0 ]; do
  case "$1" in
    --url)
      [ $# -ge 2 ] || die_usage "--url braucht einen Wert"
      URL="$2"; shift 2 ;;
    --impersonate)
      [ $# -ge 2 ] || die_usage "--impersonate braucht einen Wert"
      IMPERSONATE="$2"; shift 2 ;;
    --delay)
      [ $# -ge 2 ] || die_usage "--delay braucht eine Zahl"
      DELAY="$2"; shift 2 ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "FEHLER: unbekannte Option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$DELAY" in
  ''|*[!0-9]*) die_usage "--delay erwartet Sekunden als Zahl" ;;
esac

# ------------------------------- curl finden --------------------------------
PLAIN_CURL=0
if [ -n "$CURL" ]; then
  [ -x "$CURL" ] || die_usage "CURL=$CURL ist nicht ausführbar"
  CURL_BIN="$CURL"
elif [ -x /opt/lenovo-tracker/curl-impersonate ]; then
  CURL_BIN=/opt/lenovo-tracker/curl-impersonate
elif command -v curl-impersonate >/dev/null 2>&1; then
  CURL_BIN="$(command -v curl-impersonate)"
elif command -v curl >/dev/null 2>&1; then
  CURL_BIN="$(command -v curl)"
  PLAIN_CURL=1
else
  die_usage "weder curl-impersonate noch curl gefunden (CURL=/pfad setzen)"
fi

TMP="${TMPDIR:-/tmp}/lenovo-cond.$$"
(umask 077 && mkdir -p "$TMP") || die_usage "$TMP nicht anlegbar"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

say() { printf '%s\n' "$*"; }

# Nur den letzten Header-Block auswerten (bei Redirects/100-continue steht der
# finale Status ganz unten).
last_block() {
  tr -d '\r' < "$TMP/$1.hdr" |
    awk 'BEGIN { RS = "" } { block = $0 } END { if (block != "") printf "%s\n", block }'
}

hdr() {  # hdr <prefix> <Header-Name>
  last_block "$1" | grep -i "^$2:" | head -n1 | sed 's/^[^:]*:[[:space:]]*//'
}

body_sha() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    cksum "$1"
  fi
}

update_time() {
  grep -o '"updateTime":"[^"]*"' "$1" 2>/dev/null | head -n1 | cut -d'"' -f4
}

STATUS=000; SIZE=0; TIME=0

run_curl() {  # run_curl <prefix> [curl-Optionen …]
  prefix="$1"; shift
  STATUS=000; SIZE=0; TIME=0
  : > "$TMP/$prefix.hdr"
  : > "$TMP/$prefix.body"
  "$CURL_BIN" "$@" --compressed -sS --max-time 120 \
    -D "$TMP/$prefix.hdr" -o "$TMP/$prefix.body" \
    -w '%{http_code} %{size_download} %{time_total}' \
    -H 'Accept: application/json, text/plain, */*' \
    -H 'Accept-Language: de-DE,de;q=0.9,en;q=0.8' \
    -H "Referer: $REFERER" \
    "$URL" > "$TMP/$prefix.stat" 2> "$TMP/$prefix.err" || true
  read -r STATUS SIZE TIME < "$TMP/$prefix.stat" 2>/dev/null || true
  STATUS="${STATUS:-000}"; SIZE="${SIZE:-0}"; TIME="${TIME:-0}"
}

fetch() {  # fetch <prefix> [zusätzliche curl-Header …]
  prefix="$1"; shift
  if [ "$PLAIN_CURL" = 1 ]; then
    run_curl "$prefix" "$@"
  else
    run_curl "$prefix" --impersonate "$IMPERSONATE" "$@"
  fi
}

show_get() {  # show_get <prefix>
  say "HTTP $STATUS · $SIZE B · $TIME s"
  if [ "$STATUS" = 000 ]; then
    say "  curl-Fehler: $(head -n2 "$TMP/$1.err" 2>/dev/null | tr '\n' ' ')"
  fi
}

# conditional_test <prefix> <Header-Name> <Wert> – 0 bei 304 Not Modified.
conditional_test() {
  say "-- GET mit $2: $3 --"
  fetch "$1" -H "$2: $3"
  show_get "$1"
  case "$STATUS" in
    304)
      say "  -> 304 Not Modified: $2 wird als Cache-Validator akzeptiert."
      cc304="$(hdr "$1" cache-control)"
      age304="$(hdr "$1" age)"
      [ -n "$cc304" ] && say "     Cache-Control im 304: $cc304"
      [ -n "$age304" ] && say "     Age im 304: $age304"
      return 0 ;;
    200)
      say "  -> 200 mit Body: $2 wird ignoriert (oder gilt nicht für diese Repräsentation)."
      return 1 ;;
    *)
      say "  -> unerwarteter Status $STATUS (siehe oben)."
      return 1 ;;
  esac
}

say "== Conditional-Request-Check =="
say "URL:      $URL"
if [ "$PLAIN_CURL" = 1 ]; then
  say "Werkzeug: $CURL_BIN (klassisches curl)"
  say "Hinweis:  klassisches curl bekommt von Akamai oft HTTP 403 – belastbar ist nur curl-impersonate."
else
  say "Werkzeug: $CURL_BIN (--impersonate $IMPERSONATE)"
fi
say ""

# ------------------------- 1) Referenz-Abruf --------------------------------
say "-- 1) GET ohne Bedingung --"
fetch base1
show_get base1
if [ "$STATUS" != 200 ]; then
  say "FEHLER: Basis-Abruf lieferte HTTP $STATUS – der Check braucht HTTP 200."
  [ "$STATUS" = 403 ] &&
    say "  Akamai blockt: im CT curl-impersonate verwenden (/opt/lenovo-tracker/curl-impersonate)."
  exit 2
fi
ETAG1="$(hdr base1 etag)"
LM1="$(hdr base1 last-modified)"
say "ETag:          ${ETAG1:-(fehlt)}"
say "Last-Modified: ${LM1:-(fehlt)}"
say "Cache-Control: $(hdr base1 cache-control)"
say "Age:           $(hdr base1 age)"
say "Vary:          $(hdr base1 vary)"
say "Server:        $(hdr base1 server)"
say "X-Cache:       $(hdr base1 x-cache)"
say "Datenstand:    $(update_time "$TMP/base1.body")"
SHA1="$(body_sha "$TMP/base1.body")"
say "SHA256:        $SHA1"
say ""

# ------------------------- 2) zweiter Abruf ---------------------------------
if [ "$DELAY" -gt 0 ]; then
  say "-- 2) GET erneut nach ${DELAY}s --"
  sleep "$DELAY"
else
  say "-- 2) GET erneut (ohne Wartezeit) --"
fi
fetch base2
show_get base2
if [ "$STATUS" != 200 ]; then
  say "FEHLER: 2. Abruf lieferte HTTP $STATUS – der Check braucht zwei erfolgreiche Abrufe."
  exit 2
fi
ETAG2="$(hdr base2 etag)"
LM2="$(hdr base2 last-modified)"
SHA2="$(body_sha "$TMP/base2.body")"
ETAG_SAME=nein
[ -n "$ETAG1" ] && [ "$ETAG1" = "$ETAG2" ] && ETAG_SAME=ja
LM_SAME=nein
[ -n "$LM1" ] && [ "$LM1" = "$LM2" ] && LM_SAME=ja
BODY_SAME=nein
[ "$SHA1" = "$SHA2" ] && BODY_SAME=ja
say "ETag:          ${ETAG2:-(fehlt)}  (unverändert: $ETAG_SAME)"
say "Last-Modified: ${LM2:-(fehlt)}  (unverändert: $LM_SAME)"
say "Body:          identisch: $BODY_SAME"
say ""

# --------------------- 3) If-None-Match / If-Modified-Since -----------------
INM_OK=0
if [ -n "$ETAG2" ]; then
  if conditional_test inm If-None-Match "$ETAG2"; then INM_OK=1; fi
else
  say "-- If-None-Match: übersprungen (kein ETag in der Antwort) --"
fi
say ""

IMS_OK=0
if [ -n "$LM2" ]; then
  if conditional_test ims If-Modified-Since "$LM2"; then IMS_OK=1; fi
else
  say "-- If-Modified-Since: übersprungen (kein Last-Modified in der Antwort) --"
fi
say ""

say '----------------------------------------------------------------'
say "== Ergebnis =="
if [ "$INM_OK" = 1 ] || [ "$IMS_OK" = 1 ]; then
  say "JA: Die API antwortet auf Conditional Requests mit 304 Not Modified."
  [ "$INM_OK" = 1 ] && say "  · If-None-Match (ETag) funktioniert."
  [ "$IMS_OK" = 1 ] && say "  · If-Modified-Since (Last-Modified) funktioniert."
  say "  Nutzen: bei unverändertem Datenstand entfällt der Body-Download"
  say "  (~3,3 MB); der Tracker muss 304 wie \"keine Änderung\" behandeln."
  RC=0
else
  say "NEIN: keine 304-Antwort – Conditional Requests bringen hier nichts."
  say "  (ETag/Last-Modified fehlen oder werden ignoriert; Details oben.)"
  RC=1
fi

if [ "$BODY_SAME" = nein ]; then
  say ""
  say "Hinweis: die beiden Abrufe lieferten unterschiedliche Daten (Bestand hat"
  say "sich während des Checks geändert). Für ein sauberes Ergebnis noch einmal"
  say "ausführen; mit --delay 600 auch die Stabilität über 10 min."
fi
if [ "$ETAG_SAME" = nein ] && [ -n "$ETAG1" ]; then
  say ""
  say "Hinweis: der ETag änderte sich zwischen zwei Abrufen. Als Cache-Validator"
  say "taugt er dann nur, wenn sich auch die Daten geändert haben (siehe Body)."
fi
if [ "$STATUS" = 000 ]; then
  say ""
  say "Hinweis: der letzte Abruf ist fehlgeschlagen (HTTP 000) – Netzwerk/Proxy"
  say "prüfen; im CT liegt curl-impersonate unter /opt/lenovo-tracker/."
  RC=2
fi
exit "$RC"
