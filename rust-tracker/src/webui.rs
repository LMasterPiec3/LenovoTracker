//! Web-UI (`--serve`): Live-Bestand ohne Mail, Verlauf und Config-Bearbeitung.
//!
//! Nur std: ein `TcpListener`, ein Thread je Anfrage, eine eingebettete Seite.
//! Die Config wird pro Anfrage neu geladen – Änderungen aus der Oberfläche
//! gelten sofort für Live-Abrufe und ab dem nächsten Cron-Lauf für die Mails.
//! Live-Abrufe schreiben weder State noch Verlauf und versenden keine Mail.

use crate::api::{facets, fetch_clearance_parts, PriceMap};
use crate::config::*;
use crate::diff::{compute_totals, filter_label, keyword_matches, GonePart, PartState};
use crate::guard::{load_pending, pending_path};
use crate::state::*;
use crate::util::*;
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

const PAGE: &str = include_str!("webui.html");
const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;
/// Platzhalter, den die Oberfläche statt des SMTP-Passworts sieht.
const REDACTED: &str = "__KEEP__";

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

pub fn run(config_path: &str) -> i32 {
    let cfg = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            log_error!("Config-Fehler: {}", e);
            return 1;
        }
    };
    let bind = cfg.webui.bind.clone();
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            log_error!("Web-UI kann {} nicht binden: {}", bind, e);
            return 1;
        }
    };
    log_info!(
        "Web-UI auf http://{}{} – Config {}",
        bind,
        if cfg.webui.token.is_empty() {
            ""
        } else {
            " (Login aktiv)"
        },
        config_path
    );
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let path = config_path.to_string();
                std::thread::spawn(move || handle(stream, &path));
            }
            Err(e) => log_warn!("Web-UI: Verbindung fehlgeschlagen: {}", e),
        }
    }
    0
}

fn handle(mut stream: TcpStream, config_path: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(e) => {
            log_warn!("Web-UI: {}", e);
            return;
        }
    };
    let cfg = match load_config(config_path) {
        Ok(c) => c,
        Err(e) => {
            send(
                &mut stream,
                &Response::error(500, &format!("Config-Fehler: {}", e)),
            );
            return;
        }
    };
    if !authorized(&cfg, &req.headers) {
        let resp = Response {
            status: 401,
            ctype: "text/plain; charset=utf-8",
            body: b"Login erforderlich".to_vec(),
            extra: "WWW-Authenticate: Basic realm=\"Lenovo Tracker\"\r\nContent-Security-Policy: default-src 'none'\r\n".into(),
        };
        send(&mut stream, &resp);
        return;
    }
    // Schreibende Anfragen nur aus der eigenen Seite: der Custom-Header löst
    // cross-origin einen Preflight aus, den dieser Server nie erlaubt.
    if req.method == "POST"
        && req.headers.get("x-requested-with").map(String::as_str) != Some("lenovo-tracker")
    {
        send(
            &mut stream,
            &Response::error(403, "fehlender X-Requested-With-Header"),
        );
        return;
    }
    let resp = route(&cfg, config_path, &req);
    send(&mut stream, &resp);
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEADER {
            return Err(anyhow!("Header zu groß"));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(anyhow!("Verbindung ohne vollständigen Request"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..head_end])?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default().to_ascii_uppercase();
    let target = first.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return Err(anyhow!("Body zu groß ({} Bytes)", len));
    }
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(len);
    Ok(Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        headers,
        body,
    })
}

struct Response {
    status: u16,
    ctype: &'static str,
    body: Vec<u8>,
    extra: String,
}

impl Response {
    fn json(value: &serde_json::Value) -> Response {
        Response {
            status: 200,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec(value).unwrap_or_default(),
            extra: String::new(),
        }
    }

    fn error(status: u16, message: &str) -> Response {
        Response {
            status,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec(&json!({ "error": message })).unwrap_or_default(),
            extra: String::new(),
        }
    }
}

fn send(stream: &mut TcpStream, resp: &Response) {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n{}\r\n",
        resp.status,
        reason,
        resp.ctype,
        resp.body.len(),
        resp.extra
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
}

// ---------------------------------------------------------------------------
// Auth (optional HTTP Basic)
// ---------------------------------------------------------------------------

fn authorized(cfg: &Config, headers: &BTreeMap<String, String>) -> bool {
    if cfg.webui.token.is_empty() {
        return true;
    }
    let Some(value) = headers.get("authorization") else {
        return false;
    };
    let Some(encoded) = value
        .split_once(' ')
        .and_then(|(scheme, v)| scheme.eq_ignore_ascii_case("basic").then_some(v))
    else {
        return false;
    };
    let Some(raw) = base64_decode(encoded) else {
        return false;
    };
    let Ok(text) = String::from_utf8(raw) else {
        return false;
    };
    text.split_once(':')
        .is_some_and(|(_, pass)| ct_eq(pass.as_bytes(), cfg.webui.token.as_bytes()))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Routen
// ---------------------------------------------------------------------------

fn route(cfg: &Config, config_path: &str, req: &Request) -> Response {
    if req.method == "GET" {
        if let Some(rest) = req.path.strip_prefix("/api/thumb/") {
            return thumb_response(cfg, rest);
        }
    }
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => Response {
            status: 200,
            ctype: "text/html; charset=utf-8",
            body: PAGE.as_bytes().to_vec(),
            extra: "Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; \
                    script-src 'unsafe-inline'; connect-src 'self'; \
                    img-src 'self' https://download.lenovo.com data:; form-action 'none'; \
                    frame-ancestors 'none'\r\n"
                .into(),
        },
        ("GET", "/api/state") => state_json(cfg),
        ("GET", "/api/history") => history_json(cfg, query_limit(&req.query)),
        ("POST", "/api/live") => live_json(cfg),
        ("GET", "/api/config") => config_json(config_path),
        ("POST", "/api/config") => save_config_text(config_path, &req.body),
        _ => Response::error(404, "nicht gefunden"),
    }
}

/// Listen-Vorschau: liefert das gecachte 320-px-JPEG. Die Originale (2736×1824,
/// ~275 KB) gehen nur über die direkte Lenovo-URL in den Bild-Dialog.
fn thumb_response(cfg: &Config, raw: &str) -> Response {
    let id = raw.split('?').next().unwrap_or("");
    if !crate::api::safe_part_id(id) {
        return Response::error(400, "ungültige Teile-ID");
    }
    match crate::thumb::thumb(cfg, id, crate::thumb::THUMB_WIDTH) {
        Some(bytes) => Response {
            status: 200,
            ctype: "image/jpeg",
            body: bytes,
            extra: "Cache-Control: public, max-age=604800\r\n\
                    X-Content-Type-Options: nosniff\r\n"
                .into(),
        },
        None => Response::error(404, "kein Bild"),
    }
}

fn query_limit(query: &str) -> usize {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("limit="))
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
        .clamp(1, 2000)
}

#[derive(Deserialize, Default)]
struct StateView {
    #[serde(default)]
    last_check: String,
    #[serde(default)]
    total_stock: i64,
    #[serde(default)]
    part_count: usize,
    #[serde(default)]
    filter: String,
    #[serde(default)]
    parts: BTreeMap<String, PartState>,
    #[serde(default)]
    gone: BTreeMap<String, GonePart>,
    #[serde(default)]
    prices: BTreeMap<String, i64>,
}

fn load_state_view(cfg: &Config) -> StateView {
    let (path, _) = state_paths(cfg);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Kompletter Bestand (`tracked.json`) – alle Teile des Abrufs, unabhängig
/// vom Mail-Keyword-Filter. Fehlt die Datei (alte Installation), zeigt die
/// Oberfläche wie früher die Mail-Liste aus `state.json`.
#[derive(Deserialize, Default)]
struct TrackedView {
    #[serde(default)]
    last_check: String,
    #[serde(default)]
    filter: String,
    #[serde(default)]
    total_stock: i64,
    #[serde(default)]
    part_count: usize,
    #[serde(default)]
    parts: BTreeMap<String, PartState>,
    #[serde(default)]
    prices: BTreeMap<String, i64>,
}

fn load_tracked_view(cfg: &Config) -> TrackedView {
    let (path, _) = tracked_paths(cfg);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn state_json(cfg: &Config) -> Response {
    let view = load_state_view(cfg);
    let tracked = load_tracked_view(cfg);
    let specs = load_specs_cache(&specs_cache_path(cfg));
    let keywords = cfg.tracker.name_keywords.clone();
    let pending = load_pending(&pending_path(cfg))
        .map(|p| json!({ "parts": p.parts, "total": p.total, "ts": p.ts }));
    let empty_gone = BTreeMap::new();
    let mail_only = tracked.parts.is_empty();
    let (parts, prices, total_stock, part_count, checked_at, filter) = if mail_only {
        (
            &view.parts,
            &view.prices,
            view.total_stock,
            view.part_count,
            &view.last_check,
            &view.filter,
        )
    } else {
        (
            &tracked.parts,
            &tracked.prices,
            tracked.total_stock,
            tracked.part_count,
            &tracked.last_check,
            &tracked.filter,
        )
    };
    let gone_map = if mail_only { &view.gone } else { &empty_gone };
    let mail_parts = parts
        .iter()
        .filter(|(id, p)| {
            keyword_matches(
                id.as_str(),
                &p.n,
                specs.get(*id).map_or("", String::as_str),
                &keywords,
            )
        })
        .count();
    let gone: Vec<_> = gone_map
        .iter()
        .map(|(id, g)| json!({ "id": id, "name": g.n, "stock": g.s, "since": g.t }))
        .collect();
    // Maschinentyp aus dem Cache des letzten Laufs (die Liste selbst liefert
    // beim reinen State-Abruf keine Teile).
    let products = crate::state::load_products_cache(&crate::state::products_cache_path(cfg));
    Response::json(&json!({
        "source": "state",
        "scope": if mail_only { "mail" } else { "all" },
        "checked_at": checked_at,
        "total_stock": total_stock,
        "part_count": part_count,
        "filter": filter,
        "keywords": keywords,
        "mail_count": mail_parts,
        "currency": currency_symbol(&cfg.api.url),
        "parts": parts_json(
            parts,
            &specs,
            (!prices.is_empty()).then_some(prices),
            None,
            gone_map,
            &keywords,
            &products,
        ),
        "gone": gone,
        "pending": pending,
    }))
}

fn history_json(cfg: &Config, limit: usize) -> Response {
    // Der Verlauf der Web-UI umfasst alle Teile; ältere Installationen haben
    // nur den (auf Mail-Teile bezogenen) history.csv.
    let (_, mail_history) = state_paths(cfg);
    let (_, tracked_history) = tracked_paths(cfg);
    let path = if tracked_history.exists() {
        tracked_history
    } else {
        mail_history
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut rows: Vec<serde_json::Value> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split(',');
            let t = fields.next()?;
            let total: i64 = fields.next()?.trim().parse().ok()?;
            let count: usize = fields.next()?.trim().parse().ok()?;
            Some(json!({ "t": t, "total": total, "count": count }))
        })
        .collect();
    if rows.len() > limit {
        rows.drain(..rows.len() - limit);
    }
    Response::json(&json!({ "rows": rows }))
}

/// Frischer API-Abruf ohne Mail und ohne State-Änderung – genau der Blick,
/// den die Oberfläche zeigen soll. Preise kommen mit, der State kennt sie nicht.
fn live_json(cfg: &Config) -> Response {
    log_info!("Web-UI: Live-Abruf");
    let outcome = match fetch_clearance_parts(cfg, &cfg.tracker.commodity_keys) {
        Ok(o) => o,
        Err(e) => return Response::error(502, &format!("Abruf fehlgeschlagen: {}", e)),
    };
    // Maschinentyp für den nächsten State-Abruf mitschreiben.
    if let Err(e) = crate::state::save_products_cache(
        &crate::state::products_cache_path(cfg),
        &outcome.products,
    ) {
        log_warn!("Notebook-Typ-Cache nicht schreibbar: {}", e);
    }
    // Alles zeigen, was die API liefert – der Mail-Filter gilt nur für Mails.
    let map = outcome.boards;
    let specs = match crate::load_specs_for_all(
        cfg,
        &map,
        Some(load_specs_cache(&specs_cache_path(cfg))),
    ) {
        Ok(s) => s,
        Err(e) => return Response::error(502, &format!("Tech-Daten fehlgeschlagen: {}", e)),
    };
    let (total, count) = compute_totals(&map);
    let tracked = load_tracked_view(cfg);
    let view = load_state_view(cfg);
    let keywords = cfg.tracker.name_keywords.clone();
    let empty_gone = BTreeMap::new();
    let (base, base_gone) = if tracked.parts.is_empty() {
        (&view.parts, &view.gone)
    } else {
        (&tracked.parts, &empty_gone)
    };
    let mail_parts = map
        .iter()
        .filter(|(id, p)| {
            keyword_matches(
                id.as_str(),
                &p.n,
                specs.get(*id).map_or("", String::as_str),
                &keywords,
            )
        })
        .count();
    Response::json(&json!({
        "source": "live",
        "scope": "all",
        "checked_at": now_iso(),
        "total_stock": total,
        "part_count": count,
        "filter": filter_label(&cfg.tracker.commodity_keys, &[]),
        "keywords": keywords,
        "mail_count": mail_parts,
        "currency": currency_symbol(&cfg.api.url),
        "parts": parts_json(
            &map,
            &specs,
            Some(&outcome.prices),
            Some(base),
            base_gone,
            &keywords,
            &outcome.products,
        ),
        "gone": view.gone.len(),
        "pending": load_pending(&pending_path(cfg))
            .map(|p| json!({ "parts": p.parts, "total": p.total, "ts": p.ts })),
    }))
}

/// Teile als JSON, nach Bestand sortiert. `base` (State) liefert das Delta je
/// Teil; für zurückgekehrte Teile zählt der letzte bekannte Bestand als Basis.
fn parts_json(
    parts: &BTreeMap<String, PartState>,
    specs: &BTreeMap<String, String>,
    prices: Option<&PriceMap>,
    base: Option<&BTreeMap<String, PartState>>,
    gone: &BTreeMap<String, GonePart>,
    keywords: &[String],
    products: &BTreeMap<String, Vec<String>>,
) -> Vec<serde_json::Value> {
    let mut rows: Vec<(&String, &PartState)> = parts.iter().collect();
    rows.sort_unstable_by(|a, b| b.1.s.cmp(&a.1.s).then_with(|| a.0.cmp(b.0)));
    rows.into_iter()
        .map(|(id, p)| {
            let line = specs.get(id).map(String::as_str).unwrap_or("");
            let f = facets(line);
            // Maschinentyp („20KU") aus der Teileliste.
            let prod = products.get(id).cloned().unwrap_or_default();
            let delta = base.map(|b| match b.get(id) {
                Some(old) => json!(p.s - old.s),
                None => match gone.get(id) {
                    Some(g) => json!(p.s - g.s),
                    None => json!(p.s),
                },
            });
            json!({
                "id": id,
                "name": p.n,
                "stock": p.s,
                "specs": line,
                "price": prices.and_then(|m| m.get(id).copied()),
                "delta": delta,
                "back": gone.contains_key(id),
                // Erfüllt das Teil die Mail-Keywords? Die Oberfläche kann so
                // zwischen „allem“ und der Mail-Liste umschalten.
                "mail": keyword_matches(id, &p.n, line, keywords),
                // Liste: kleines, gecachtes Vorschaubild (das Original ist ~275 KB);
                // der Bild-Dialog baut die Original-URLs selbst aus der ID.
                "image": format!("/api/thumb/{id}"),
                // Maschinentyps („20KU") – sagt, zu welchem Notebook das Board gehört.
                "products": prod,
                "cpu": f.cpu,
                "cpu_brand": f.cpu_brand,
                "cpu_series": f.cpu_series,
                "cpu_gen": f.cpu_gen,
                "cpu_score": f.perf.1,
                "cpu_bench": f.cpu_bench.map(|(s, m)| [s, m]),
                "cpu_src": if f.cpu_bench.is_none() { "est" } else { f.cpu_bench_src },
                "gpu_score": f.gpu_bench,
                "gpu_src": f.gpu_bench_src,
                "gpu_igpu": f.gpu_bench_name,
                "gpu_igpu_short": crate::api::igpu_short(f.gpu_bench_name),
                "gpu_igpu_note": f.gpu_bench_note,
                "ram": f.ram,
                "gpu": f.gpu,
                "gpu_kind": f.gpu_kind,
                "gpu_family": f.gpu_family,
                "perf": [f.perf.0, f.perf.1, f.perf.2],
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Config lesen/schreiben (SMTP-Passwort bleibt verdeckt)
// ---------------------------------------------------------------------------

fn config_json(config_path: &str) -> Response {
    match std::fs::read_to_string(config_path) {
        Ok(text) => Response::json(&json!({
            "path": config_path,
            "text": redact(&text),
        })),
        Err(e) => Response::error(500, &format!("Config nicht lesbar: {}", e)),
    }
}

fn save_config_text(config_path: &str, body: &[u8]) -> Response {
    #[derive(Deserialize)]
    struct Input {
        text: String,
    }
    let input: Input = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::error(400, &format!("Ungültiges JSON: {}", e)),
    };
    let old = std::fs::read_to_string(config_path).unwrap_or_default();
    let text = unredact(&input.text, &old);
    if let Err(e) = parse_config(&text) {
        return Response::error(400, &format!("Config ungültig: {}", e));
    }
    let backup = format!("{}.bak", config_path);
    let _ = std::fs::copy(config_path, &backup);
    let perms = std::fs::metadata(config_path).ok().map(|m| m.permissions());
    match write_atomic(Path::new(config_path), text.as_bytes()) {
        Ok(()) => {
            if let Some(p) = perms {
                let _ = std::fs::set_permissions(config_path, p);
            }
            log_info!("Web-UI: Config gespeichert (Backup {})", backup);
            Response::json(&json!({ "ok": true, "backup": backup }))
        }
        Err(e) => Response::error(500, &format!("Schreiben fehlgeschlagen: {}", e)),
    }
}

fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.trim_start().starts_with("smtp_pass") {
            out.push_str(&format!("smtp_pass = \"{}\"\n", REDACTED));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn unredact(text: &str, old: &str) -> String {
    if !text.contains(REDACTED) {
        return text.to_string();
    }
    let secret = secret_value(old).unwrap_or_default();
    text.replace(&format!("\"{}\"", REDACTED), &format!("\"{}\"", secret))
}

/// Wert von `smtp_pass = "…"` aus der alten Config (nur die Anführungszeichen
/// werden entfernt; der Text ist ein einfaches TOML-String-Literal).
fn secret_value(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix("smtp_pass") else {
            continue;
        };
        if let Some(value) = rest.trim_start().strip_prefix('=') {
            return Some(value.trim().trim_matches('"').to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lt-webui-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const CONFIG: &str = r#"
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
curl_path = "/bin/true"

[tracker]
commodity_keys = ["system boards"]
name_keywords = []

[mail]
from = "a@example.de"
to = ["a@example.de"]
smtp_host = "smtp.example.de"
smtp_pass = "geheim"

[paths]
state_dir = "/tmp/lt-webui-test"
"#;

    fn config_for(dir: &Path) -> Config {
        let mut cfg = parse_config(CONFIG).expect("Config");
        cfg.paths.state_dir = dir.display().to_string();
        cfg
    }

    fn request(method: &str, path: &str) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            query: String::new(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        }
    }

    #[test]
    fn test_redact_keeps_password() {
        let text = redact(CONFIG);
        assert!(text.contains("smtp_pass = \"__KEEP__\""));
        assert!(!text.contains("geheim"));
        let back = unredact(&text, CONFIG);
        assert!(back.contains("smtp_pass = \"geheim\""));
        assert_eq!(parse_config(&back).unwrap().mail.smtp_pass, "geheim");
        // ohne Platzhalter bleibt der Text unverändert
        let plain = CONFIG.replace("geheim", "neu");
        assert_eq!(unredact(&plain, CONFIG), plain);
    }

    #[test]
    fn test_state_json_lists_parts_and_specs() {
        let dir = temp_dir("state");
        let cfg = config_for(&dir);
        let (state_path, _) = state_paths(&cfg);
        std::fs::write(
            &state_path,
            r#"{"last_check":"2026-11-26T09:00:00Z","total_stock":7,"part_count":2,
                "filter":"c=system boards;k=","kw":{},
                "parts":{"A":{"s":5,"n":"Board A"},"B":{"s":2,"n":"Board B"}},
                "gone":{"C":{"s":4,"n":"Board C","t":"2026-11-26T08:00:00Z"}}}"#,
        )
        .unwrap();
        std::fs::write(
            specs_cache_path(&cfg),
            r#"{"A":"CPU: Intel Core i5-8250U | GPU: integriert"}"#,
        )
        .unwrap();

        let resp = route(&cfg, "/dev/null", &request("GET", "/api/state"));
        assert_eq!(resp.status, 200);
        let body = String::from_utf8(resp.body).unwrap();
        assert!(body.contains("\"total_stock\":7"));
        assert!(body.contains("\"id\":\"A\""));
        assert!(body.contains("Intel Core i5-8250U"));
        assert!(body.contains("\"gone\":[{\"id\":\"C\""));
    }

    #[test]
    fn test_tracked_view_lists_all_parts_and_marks_mail() {
        let dir = temp_dir("tracked");
        let mut cfg = config_for(&dir);
        cfg.tracker.name_keywords = vec!["RTX".into()];
        let (tracked, hist) = tracked_paths(&cfg);
        std::fs::write(
            &tracked,
            r#"{"last_check":"2026-11-26T09:30:00Z","filter":"c=system boards",
                "total_stock":12,"part_count":3,
                "parts":{"A":{"s":5,"n":"Board RTX 3000"},"B":{"s":4,"n":"Board i5"},
                          "C":{"s":3,"n":"Board RTX"}},
                "prices":{"A":10000}}"#,
        )
        .unwrap();
        std::fs::write(
            &hist,
            "timestamp,total_stock,part_count\n2026-11-26T09:30:00Z,12,3\n",
        )
        .unwrap();
        std::fs::write(
            specs_cache_path(&cfg),
            r#"{"A":"CPU: Intel Core i5-8250U","B":"CPU: Intel Core i5-8250U"}"#,
        )
        .unwrap();

        let resp = route(&cfg, "/dev/null", &request("GET", "/api/state"));
        let body = String::from_utf8(resp.body).unwrap();
        assert!(body.contains("\"scope\":\"all\""));
        assert!(body.contains("\"part_count\":3"));
        assert!(
            body.contains("\"id\":\"B\""),
            "alle Teile, nicht nur Mail-Treffer"
        );
        assert!(body.contains("\"mail_count\":2"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let marked = v["parts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["mail"].as_bool().unwrap())
            .count();
        assert_eq!(marked, 2);

        // Der Verlauf kommt aus tracked-history.csv (alle Teile).
        let h = route(&cfg, "/dev/null", &request("GET", "/api/history"));
        let hb = String::from_utf8(h.body).unwrap();
        assert!(hb.contains("\"total\":12"));
    }

    #[test]
    fn test_pending_is_reported() {
        let dir = temp_dir("pending");
        let cfg = config_for(&dir);
        crate::guard::save_pending(
            &pending_path(&cfg),
            &crate::guard::Pending {
                parts: 20,
                total: 80,
                ts: "2026-11-26T08:10:00Z".into(),
            },
        )
        .unwrap();
        let resp = route(&cfg, "/dev/null", &request("GET", "/api/state"));
        let body = String::from_utf8(resp.body).unwrap();
        assert!(body.contains("\"pending\":{\"parts\":20"));
    }

    #[test]
    fn test_config_post_writes_valid_toml() {
        let dir = temp_dir("config");
        let path = dir.join("config.toml");
        std::fs::write(&path, CONFIG).unwrap();
        let original = redact(CONFIG);
        let edited = original.replace("name_keywords = []", "name_keywords = [\"RTX\"]");
        let body = serde_json::to_vec(&json!({ "text": edited })).unwrap();
        let mut req = request("POST", "/api/config");
        req.body = body;
        let resp = route(&parse_config(CONFIG).unwrap(), path.to_str().unwrap(), &req);
        assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("name_keywords = [\"RTX\"]"));
        assert!(saved.contains("smtp_pass = \"geheim\"")); // Passwort erhalten
        assert!(dir.join("config.toml.bak").exists());
    }

    #[test]
    fn test_config_post_rejects_broken_toml() {
        let dir = temp_dir("broken");
        let path = dir.join("config.toml");
        std::fs::write(&path, CONFIG).unwrap();
        let mut req = request("POST", "/api/config");
        req.body = serde_json::to_vec(&json!({ "text": "kein toml [" })).unwrap();
        let resp = route(&parse_config(CONFIG).unwrap(), path.to_str().unwrap(), &req);
        assert_eq!(resp.status, 400);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
    }

    #[test]
    fn test_history_and_unknown_route() {
        let dir = temp_dir("history");
        let cfg = config_for(&dir);
        let (_, history) = state_paths(&cfg);
        std::fs::write(
            &history,
            "timestamp,total_stock,part_count\n2026-11-26T08:00:00Z,254,50\n2026-11-26T08:10:00Z,166,38\n",
        )
        .unwrap();
        let resp = route(&cfg, "/dev/null", &request("GET", "/api/history"));
        let body = String::from_utf8(resp.body).unwrap();
        assert!(body.contains("\"total\":166"));
        assert_eq!(
            route(&cfg, "/dev/null", &request("GET", "/nope")).status,
            404
        );
    }

    #[test]
    fn test_base64_and_token() {
        // "user:geheim" → dXNlcjpnZWhlaW0=
        assert_eq!(base64_decode("dXNlcjpnZWhlaW0=").unwrap(), b"user:geheim");
        assert_eq!(base64_decode("dXNl\ncjpn ZWhlaW0").unwrap(), b"user:geheim");
        assert!(base64_decode("###").is_none());
        let mut cfg = parse_config(CONFIG).unwrap();
        cfg.webui.token = "geheim".into();
        let mut headers = BTreeMap::new();
        assert!(!authorized(&cfg, &headers));
        headers.insert("authorization".into(), "Basic dXNlcjpnZWhlaW0=".into());
        assert!(authorized(&cfg, &headers));
        headers.insert("authorization".into(), "Basic dXNlcjpmYWxzY2g=".into());
        assert!(!authorized(&cfg, &headers));
    }

    #[test]
    fn test_loopback_bind() {
        assert!(is_loopback_bind("127.0.0.1:8787"));
        assert!(is_loopback_bind("localhost:8787"));
        assert!(is_loopback_bind("[::1]:8787"));
        assert!(!is_loopback_bind("0.0.0.0:8787"));
        assert!(!is_loopback_bind("192.168.1.5:8787"));
    }
}
