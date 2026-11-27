//! Web-UI (`--serve`): Live-Bestand ohne Mail, Verlauf und Config-Bearbeitung.
//!
//! Nur std: ein `TcpListener`, ein Thread je Anfrage, eine eingebettete Seite.
//! Die Config wird pro Anfrage neu geladen – Änderungen aus der Oberfläche
//! gelten sofort für Live-Abrufe und ab dem nächsten Cron-Lauf für die Mails.
//! Live-Abrufe schreiben weder State noch Verlauf und versenden keine Mail.

use crate::config::*;
use crate::util::*;
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

mod auth;
mod live;
mod settings;
mod view;

// Die Untermodule teilen sich die Anfrage-/Antwort-Typen aus diesem Modul; die
// Re-Exports halten die Routen unten (`state_json`, `live_json`, …) kurz.
pub(crate) use auth::*;
pub(crate) use live::*;
pub(crate) use settings::*;
pub(crate) use view::*;

#[cfg(test)]
mod tests;

const PAGE: &str = include_str!("../webui.html");
const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;

// Drei Zeiten, drei Rollen: `REFRESH_EVERY` ist der Takt des Hintergrunds und
// zugleich „frisch genug“, `STALE_OK` überbrückt einen Aussetzer im Betrieb,
// `IDLE_AFTER` beendet den Betrieb ohne offene Seite.
/// Takt, in dem der Hintergrund-Thread den Stand erneuert – und wie alt er nach
/// einer Pause höchstens sein darf, um noch zu gelten.
const REFRESH_EVERY: Duration = Duration::from_secs(60);
/// So alt darf der Stand werden, während die Seite ihn abruft: ein
/// Lenovo-Aussetzer soll den Seitenaufbau nicht auf die Retry-Kette (bis ~60 s)
/// warten lassen – solange wird der letzte gute Stand gezeigt, mit sichtbarem
/// Zeitstempel.
const STALE_OK: Duration = Duration::from_secs(300);
/// So lange nach dem letzten Seitenaufruf läuft der Hintergrund-Abruf weiter.
/// Danach pausiert er: ohne offene Seite braucht der Server nicht zu Lenovo zu
/// gehen. Der erste Aufruf danach weckt ihn binnen einer Sekunde.
const IDLE_AFTER: Duration = Duration::from_secs(300);
/// Abtastrate der Pause – bestimmt, wie schnell ein Aufruf ihn wieder weckt.
const IDLE_POLL: Duration = Duration::from_secs(1);
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
    spawn_live_refresher(config_path.to_string());
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
            cache: None,
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

pub(crate) struct Request {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query: String,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: Vec<u8>,
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

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) ctype: &'static str,
    pub(crate) body: Vec<u8>,
    pub(crate) extra: String,
    /// Cache-Control der Antwort; `None` = no-store (Standard für alles, was
    /// Daten oder Einstellungen zeigt).
    pub(crate) cache: Option<&'static str>,
}

impl Response {
    fn json(value: &serde_json::Value) -> Response {
        Response {
            status: 200,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec(value).unwrap_or_default(),
            extra: String::new(),
            cache: None,
        }
    }

    fn error(status: u16, message: &str) -> Response {
        Response {
            status,
            ctype: "application/json; charset=utf-8",
            body: serde_json::to_vec(&json!({ "error": message })).unwrap_or_default(),
            extra: String::new(),
            cache: None,
        }
    }

    /// Cache-Control der Antwort; ohne Angabe antwortet `send` mit `no-store`.
    fn with_cache(mut self, value: &'static str) -> Self {
        self.cache = Some(value);
        self
    }

    /// Fertig serialisiertes JSON (aus dem Live-Speicher) – kein erneutes
    /// Bauen der Antwort.
    fn raw_json(body: Vec<u8>) -> Response {
        Response {
            status: 200,
            ctype: "application/json; charset=utf-8",
            body,
            extra: String::new(),
            // Der Stand ist bis zu eine Minute alt; der Browser soll ihn
            // trotzdem nie aus seinem eigenen Cache nehmen.
            cache: None,
        }
    }

    /// ETag plus `no-cache`: der Browser fragt nach, bekommt bei gleichem
    /// Stand aber nur einen leeren 304 statt des ganzen JSON.
    fn with_etag(mut self, etag: &str) -> Self {
        self.extra = format!("ETag: {}\r\n", etag);
        self.cache = Some("no-cache");
        self
    }

    /// 304 ohne Body – der Client hat den Stand schon.
    fn not_modified(etag: &str) -> Response {
        Response {
            status: 304,
            ctype: "application/json; charset=utf-8",
            body: Vec::new(),
            extra: String::new(),
            cache: None,
        }
        .with_etag(etag)
    }
}

fn send(stream: &mut TcpStream, resp: &Response) {
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        304 => "Not Modified",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: {}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n{}\r\n",
        resp.status,
        reason,
        resp.ctype,
        resp.body.len(),
        resp.cache.unwrap_or("no-store"),
        resp.extra
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
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
            cache: None,
        },
        ("GET", "/api/state") => state_json(cfg, req),
        ("GET", "/api/history") => history_json(cfg, query_limit(&req.query)),
        ("POST", "/api/live") => live_json(cfg, req),
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
        // Vorschaubilder ändern sich praktisch nie: der Browser nimmt sie aus
        // dem eigenen Cache, statt 96 Bilder bei jedem Reload neu zu holen.
        Some(body) => Response {
            status: 200,
            ctype: "image/jpeg",
            body,
            extra: String::new(),
            cache: Some("public, max-age=604800"),
        },
        // Auch das „kein Bild“ kurz cachen – sonst fragt der Browser die
        // fehlenden Teile bei jedem Seitenaufbau erneut an.
        None => Response::error(404, "kein Bild").with_cache("public, max-age=3600"),
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
