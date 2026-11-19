//! Lenovo Clearance-Sale Mainboard-Tracker (Rust-Variante, minimal RAM).
//!
//! Gleiche Logik wie die Python-Version: API abrufen (Chrome-TLS-Imitation),
//! Mainboards filtern (`commodityKey == "system boards"`), Summe `stock`
//! bilden, Diff gegen state.json, Mail bei Anstieg (SMTP/STARTTLS direkt,
//! kein Mail-Daemon nötig). Wird per Cron alle 30 Minuten mit `--once`
//! gestartet.

use anyhow::{anyhow, bail, Context, Result};
use lettre::Transport;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::SmtpTransport;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

const PAGE_URL: &str = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale";
const DEFAULT_CONFIG_PATH: &str = "/etc/lenovotracker/config.toml";
const BACKOFFS: [u64; 3] = [5, 15, 45];

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn now_iso() -> String {
    use time::format_description::well_known::Rfc3339;
    let now = time::OffsetDateTime::now_utc();
    now.format(&Rfc3339).unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

macro_rules! log_info {
    ($($arg:tt)*) => { println!("[{} INFO] {}", now_iso(), format_args!($($arg)*)) };
}
macro_rules! log_warn {
    ($($arg:tt)*) => { println!("[{} WARN] {}", now_iso(), format_args!($($arg)*)) };
}
macro_rules! log_error {
    ($($arg:tt)*) => { println!("[{} ERROR] {}", now_iso(), format_args!($($arg)*)) };
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Config {
    api: ApiCfg,
    #[serde(default)]
    tracker: TrackerCfg,
    mail: MailCfg,
    #[serde(default)]
    paths: PathsCfg,
}

#[derive(Deserialize)]
struct ApiCfg {
    url: String,
    #[serde(default = "default_referer")]
    referer: String,
    #[serde(default = "default_u60")]
    timeout_seconds: u64,
    #[serde(default = "default_retries")]
    retries: u64,
    #[serde(default = "default_curl_path")]
    curl_path: String,
    #[serde(default = "default_impersonate")]
    impersonate_target: String,
}

#[derive(Deserialize, Default)]
struct TrackerCfg {
    #[serde(default = "default_interval")]
    interval_minutes: u64,
    #[serde(default = "default_commodity_keys")]
    commodity_keys: Vec<String>,
}

#[derive(Deserialize)]
struct MailCfg {
    #[serde(default = "default_mode")]
    mode: String,
    from: String,
    to: Vec<String>,
    #[serde(default)]
    extra_recipients: Vec<String>,
    #[serde(default = "default_subject_prefix")]
    subject_prefix: String,
    #[serde(default)]
    smtp_host: String,
    #[serde(default = "default_smtp_port")]
    smtp_port: u16,
    #[serde(default = "default_true")]
    smtp_starttls: bool,
}

#[derive(Deserialize, Default)]
struct PathsCfg {
    #[serde(default = "default_state_dir")]
    state_dir: String,
}

fn default_referer() -> String { PAGE_URL.into() }
fn default_u60() -> u64 { 60 }
fn default_retries() -> u64 { 3 }
fn default_curl_path() -> String { "/opt/lenovo-tracker/curl-impersonate".into() }
fn default_impersonate() -> String { "chrome124".into() }
fn default_interval() -> u64 { 30 }
fn default_commodity_keys() -> Vec<String> { vec!["system boards".into()] }
fn default_mode() -> String { "smtp".into() }
fn default_subject_prefix() -> String { "[Lenovo Tracker]".into() }
fn default_smtp_port() -> u16 { 25 }
fn default_true() -> bool { true }
fn default_state_dir() -> String { "/var/lib/lenovotracker".into() }

fn load_config(path: &str) -> Result<Config> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("Config-Datei nicht lesbar: {}", path))?;
    let cfg: Config = toml::from_str(&text).context("Config ist kein gültiges TOML / unvollständig")?;
    if cfg.api.url.is_empty() {
        bail!("api.url fehlt in der Config");
    }
    if cfg.mail.from.is_empty() || cfg.mail.to.is_empty() {
        bail!("mail.from und mail.to müssen gesetzt sein");
    }
    if cfg.mail.mode != "smtp" {
        bail!("Rust-Version unterstützt nur mail.mode = \"smtp\" (kein sendmail-Daemon im Container)");
    }
    if cfg.mail.smtp_host.is_empty() {
        bail!("mail.smtp_host fehlt");
    }
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// API-Abruf
// ---------------------------------------------------------------------------

/// Typisierte API-Teile (statt Value-Baum): ~3x weniger Peak-RAM beim Parsen.
#[derive(Debug, Clone, Deserialize, Default)]
struct ApiPart {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "commodityKey", default)]
    commodity_key: String,
    #[serde(default)]
    stock: StockField,
}

/// Robust gegen fehlende/garbage/Text-Zahlen – wie die Python-Version.
#[derive(Debug, Clone, Default)]
struct StockField(i64);

impl<'de> Deserialize<'de> for StockField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(StockField(match v {
            Value::Number(n) => {
                let f = n.as_f64().unwrap_or(0.0);
                (f as i64).max(0)
            }
            Value::String(s) => s.trim().parse::<i64>().unwrap_or(0).max(0),
            _ => 0,
        }))
    }
}

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    data: Option<Vec<ApiPart>>,
}

#[derive(Debug)]
struct FetchError {
    message: String,
    status: Option<u32>,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn fetch_clearance_parts(cfg: &Config) -> Result<Vec<ApiPart>, FetchError> {
    let attempts = (cfg.api.retries + 1).max(1) as usize;
    let mut last: Option<FetchError> = None;
    for attempt in 0..attempts {
        if attempt > 0 {
            let backoff = BACKOFFS[(attempt - 1).min(BACKOFFS.len() - 1)];
            sleep(Duration::from_secs(backoff));
        }
        match fetch_once(cfg) {
            Ok(parts) => return Ok(parts),
            Err(e) => {
                if e.status == Some(403) {
                    log_warn!("HTTP 403 – Akamai blockiert den Abruf (kein Retry bei Bot-Block)");
                    return Err(e);
                }
                log_warn!(
                    "API-Abruf fehlgeschlagen (Versuch {}/{}): {}",
                    attempt + 1, attempts, e
                );
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| FetchError { message: "unbekannter Abruffehler".into(), status: None }))
}

/// Holt die API-Response über das offizielle curl-impersonate-Binary
/// (Chrome-TLS-Fingerprint, gleiche Technik wie curl_cffi – ohne Python).
fn fetch_once(cfg: &Config) -> Result<Vec<ApiPart>, FetchError> {
    fn fe(e: impl std::fmt::Display) -> FetchError {
        FetchError { message: format!("Abruf fehlgeschlagen: {}", e), status: None }
    }

    const BODY_FILE: &str = "/tmp/lenovo-tracker-body.json";

    let output = std::process::Command::new(&cfg.api.curl_path)
        .arg("--impersonate")
        .arg(&cfg.api.impersonate_target)
        .arg("--compressed")
        .arg("-s")
        .arg("--max-time")
        .arg(cfg.api.timeout_seconds.to_string())
        .arg("-o")
        .arg(BODY_FILE)
        .arg("-w")
        .arg("%{http_code}")
        .arg("-H")
        .arg("Accept: application/json, text/plain, */*")
        .arg("-H")
        .arg("Accept-Language: de-DE,de;q=0.9,en;q=0.8")
        .arg("-H")
        .arg(format!("Referer: {}", cfg.api.referer))
        .arg(&cfg.api.url)
        .output()
        .map_err(fe)?;

    // stdout enthält nur den HTTP-Status (Body ging in die Datei)
    let status_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let status: u32 = status_str.parse().unwrap_or(0);

    if status == 403 {
        return Err(FetchError {
            message: "HTTP 403 – Akamai blockiert den Abruf (Fallback: Headless-Chromium, siehe README)".into(),
            status: Some(403),
        });
    }
    if status == 0 || status >= 400 {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(FetchError {
            message: format!("HTTP {} von der Lenovo-API {}", status, stderr),
            status: Some(status),
        });
    }

    // Streaming-Parse aus der Datei – der Body landet nie komplett im RAM
    let parse_result: Result<ApiResponse, _> = {
        let file = fs::File::open(BODY_FILE).map_err(fe)?;
        let reader = std::io::BufReader::new(file);
        serde_json::from_reader(reader)
    };
    let _ = fs::remove_file(BODY_FILE); // Temp-Datei in jedem Fall aufräumen

    let payload = parse_result.map_err(|_| FetchError {
        message: "Antwort ist kein gültiges JSON (vermutlich XML ohne Accept-Header oder HTML-Blockseite)".into(),
        status: Some(status),
    })?;

    if payload.code != 0 {
        return Err(FetchError { message: format!("API-Fehlercode: {}", payload.code), status: Some(status) });
    }
    let data = payload.data.unwrap_or_default();
    if data.is_empty() {
        return Err(FetchError {
            message: "API lieferte keine/leere Datenliste – Abbruch, State bleibt unangetastet".into(),
            status: Some(status),
        });
    }
    Ok(data)
}

// ---------------------------------------------------------------------------
// Filter, Summe, Diff
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PartState {
    s: i64,
    n: String,
}

fn stock_of(part: &ApiPart) -> i64 {
    part.stock.0
}

fn filter_mainboards<'a>(parts: &'a [ApiPart], keys: &[String]) -> Vec<&'a ApiPart> {
    // Allocation-frei: case-insensitiver Vergleich statt pro Teil lowercase zu allokieren.
    parts
        .iter()
        .filter(|p| {
            keys.iter()
                .any(|k| p.commodity_key.trim().eq_ignore_ascii_case(k))
        })
        .collect()
}

fn parts_to_map(boards: &[&ApiPart]) -> BTreeMap<String, PartState> {
    let mut map = BTreeMap::new();
    for p in boards {
        if p.id.is_empty() {
            continue;
        }
        map.insert(p.id.clone(), PartState { s: stock_of(p), n: p.name.clone() });
    }
    map
}

fn compute_totals(map: &BTreeMap<String, PartState>) -> (i64, usize) {
    (map.values().map(|v| v.s).sum(), map.len())
}

#[derive(Debug)]
struct IncreasedPart {
    id: String,
    name: String,
    old: i64,
    new: i64,
    delta: i64,
}

#[derive(Debug)]
struct Diff {
    old_total: i64,
    new_total: i64,
    increased: Vec<IncreasedPart>,
    new_parts: Vec<(String, String, i64)>, // id, name, stock
}

impl Diff {
    fn delta(&self) -> i64 {
        self.new_total - self.old_total
    }
}

fn build_diff(old_map: &BTreeMap<String, PartState>, new_map: &BTreeMap<String, PartState>) -> Diff {
    let old_total: i64 = old_map.values().map(|v| v.s).sum();
    let new_total: i64 = new_map.values().map(|v| v.s).sum();
    let mut increased = Vec::new();
    let mut new_parts = Vec::new();
    for (id, nv) in new_map {
        match old_map.get(id) {
            None => new_parts.push((id.clone(), nv.n.clone(), nv.s)),
            Some(ov) if nv.s > ov.s => increased.push(IncreasedPart {
                id: id.clone(),
                name: nv.n.clone(),
                old: ov.s,
                new: nv.s,
                delta: nv.s - ov.s,
            }),
            _ => {}
        }
    }
    increased.sort_by(|a, b| b.delta.cmp(&a.delta).then(a.id.cmp(&b.id)));
    new_parts.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    Diff { old_total, new_total, increased, new_parts }
}

// ---------------------------------------------------------------------------
// State & Verlauf
// ---------------------------------------------------------------------------

fn load_state(path: &Path) -> Option<BTreeMap<String, PartState>> {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => {
                match serde_json::from_value::<BTreeMap<String, PartState>>(v.get("parts").cloned().unwrap_or(Value::Null)) {
                    Ok(map) => Some(map),
                    Err(_) => {
                        log_error!("state.json hat ein unerwartetes Format – Baseline wird neu gesetzt");
                        None
                    }
                }
            }
            Err(e) => {
                log_error!("state.json ist beschädigt ({}) – Baseline wird neu gesetzt", e);
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            log_error!("state.json nicht lesbar ({}) – Baseline wird neu gesetzt", e);
            None
        }
    }
}

fn save_state(state_path: &Path, ts: &str, total: i64, count: usize, part_map: &BTreeMap<String, PartState>) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::json!({
        "last_check": ts,
        "total_stock": total,
        "part_count": count,
        "parts": part_map,
    });
    let tmp = PathBuf::from(format!("{}.tmp", state_path.display()));
    fs::write(&tmp, serde_json::to_string(&payload)?)?;
    fs::rename(&tmp, state_path)?;
    Ok(())
}

fn append_history(history_path: &Path, ts: &str, total: i64, count: usize) -> Result<()> {
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let is_new = !history_path.exists();
    let mut fh = fs::OpenOptions::new().create(true).append(true).open(history_path)?;
    if is_new {
        writeln!(fh, "timestamp,total_stock,part_count")?;
    }
    writeln!(fh, "{},{},{}", ts, total, count)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Mail
// ---------------------------------------------------------------------------

fn fmt_de(number: i64) -> String {
    let negative = number < 0;
    let digits = number.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('.');
        }
        out.push(ch);
    }
    if negative {
        format!("-{}", out)
    } else {
        out
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn build_subject(prefix: &str, diff: &Diff) -> String {
    format!(
        "{} Mainboard-Bestand gestiegen: {} → {} (+{})",
        prefix,
        fmt_de(diff.old_total),
        fmt_de(diff.new_total),
        fmt_de(diff.delta())
    )
}

fn build_mail_text(diff: &Diff, page_url: &str, checked_at: &str) -> String {
    let mut lines = vec![
        "Mainboard-Bestand gestiegen!".to_string(),
        String::new(),
        format!("Vorher: {} Stück", fmt_de(diff.old_total)),
        format!("Jetzt:  {} Stück", fmt_de(diff.new_total)),
        format!("Änderung: +{}", fmt_de(diff.delta())),
        String::new(),
    ];
    if !diff.increased.is_empty() {
        lines.push(format!("Top-Neuzugänge ({} Teile mit mehr Bestand):", diff.increased.len()));
        for item in diff.increased.iter().take(20) {
            lines.push(format!(
                "  +{}  {}  {}  ({} → {})",
                item.delta, item.id, item.name, item.old, item.new
            ));
        }
        let rest: Vec<&IncreasedPart> = diff.increased.iter().skip(20).collect();
        if !rest.is_empty() {
            let rest_delta: i64 = rest.iter().map(|i| i.delta).sum();
            lines.push(format!("  (+ {} weitere Teile: +{})", rest.len(), fmt_de(rest_delta)));
        }
        lines.push(String::new());
    }
    if !diff.new_parts.is_empty() {
        lines.push(format!("Neu im Sale: {} Teile", diff.new_parts.len()));
        for (id, name, stock) in diff.new_parts.iter().take(20) {
            lines.push(format!("  {}  {}  ({} Stück)", id, name, stock));
        }
        lines.push(String::new());
    }
    lines.push(format!("Seite: {}", page_url));
    lines.push(format!("Geprüft: {}", checked_at));
    lines.join("\n")
}

fn build_mail_html(diff: &Diff, page_url: &str, checked_at: &str) -> String {
    let mut parts = vec![
        "<html><body style='font-family:sans-serif'>".to_string(),
        "<h2>Mainboard-Bestand gestiegen!</h2>".to_string(),
        format!(
            "<p><b>{} → {} Stück (+{})</b></p>",
            fmt_de(diff.old_total),
            fmt_de(diff.new_total),
            fmt_de(diff.delta())
        ),
    ];
    if !diff.increased.is_empty() {
        parts.push("<h3>Top-Neuzugänge</h3>".to_string());
        parts.push("<table border='1' cellpadding='4' cellspacing='0'>".to_string());
        parts.push("<tr><th>Δ</th><th>Teilenummer</th><th>Bezeichnung</th><th>Bestand</th></tr>".to_string());
        for item in diff.increased.iter().take(20) {
            parts.push(format!(
                "<tr><td>+{}</td><td>{}</td><td>{}</td><td>{} → {}</td></tr>",
                item.delta, esc(&item.id), esc(&item.name), item.old, item.new
            ));
        }
        parts.push("</table>".to_string());
    }
    if !diff.new_parts.is_empty() {
        parts.push(format!("<h3>Neu im Sale ({} Teile)</h3>", diff.new_parts.len()));
        parts.push("<table border='1' cellpadding='4' cellspacing='0'>".to_string());
        parts.push("<tr><th>Teilenummer</th><th>Bezeichnung</th><th>Bestand</th></tr>".to_string());
        for (id, name, stock) in diff.new_parts.iter().take(20) {
            parts.push(format!(
                "<tr><td>{}</td><td>{}</td><td>{} Stück</td></tr>",
                esc(id), esc(name), stock
            ));
        }
        parts.push("</table>".to_string());
    }
    parts.push(format!(
        "<p><a href='{}'>{}</a><br>Geprüft: {}</p>",
        esc(page_url), esc(page_url), esc(checked_at)
    ));
    parts.push("</body></html>".to_string());
    parts.join("\n")
}

fn compose_mail(cfg: &Config, subject: &str, text: &str, html: &str) -> Result<lettre::Message> {
    let from: Mailbox = cfg.mail.from.parse().context("mail.from ist keine gültige Adresse")?;
    let mut builder = lettre::Message::builder().from(from).subject(subject.to_string());
    for recipient in cfg.mail.to.iter().chain(cfg.mail.extra_recipients.iter()) {
        let mb: Mailbox = recipient
            .parse()
            .with_context(|| format!("Ungültige Empfängeradresse: {}", recipient))?;
        builder = builder.to(mb);
    }
    builder
        .multipart(MultiPart::alternative_plain_html(text.to_string(), html.to_string()))
        .context("Mail konnte nicht zusammengesetzt werden")
}

fn deliver_mail(cfg: &Config, msg: &lettre::Message) -> Result<()> {
    let mailer = if cfg.mail.smtp_starttls {
        SmtpTransport::starttls_relay(&cfg.mail.smtp_host)
            .with_context(|| format!("SMTP-Relay ungültig: {}", cfg.mail.smtp_host))?
            .timeout(Some(Duration::from_secs(60)))
            .build()
    } else {
        SmtpTransport::builder_dangerous(&cfg.mail.smtp_host)
            .port(cfg.mail.smtp_port)
            .timeout(Some(Duration::from_secs(60)))
            .build()
    };
    mailer.send(msg).map(|_| ())?;
    Ok(())
}

fn send_test_mail(cfg: &Config) -> Result<()> {
    let parts = fetch_clearance_parts(cfg).map_err(|e| anyhow!("Abruf fehlgeschlagen: {}", e))?;
    let boards = filter_mainboards(&parts, &cfg.tracker.commodity_keys);
    let part_map = parts_to_map(&boards);
    let (total, count) = compute_totals(&part_map);
    let ts = now_iso();
    let subject = format!("{} Testmail – Tracker aktiv (Rust)", cfg.mail.subject_prefix);
    let text = format!(
        "Testmail des Lenovo-Mainboard-Trackers (Rust-Version).\n\nAktueller Mainboard-Bestand: {} Stück über {} Teile.\nVersandweg: SMTP {}\nGeprüft: {}\nSeite: {}\n",
        fmt_de(total), count, cfg.mail.smtp_host, ts, PAGE_URL
    );
    let html = format!(
        "<html><body><h2>Testmail – Tracker aktiv (Rust-Version)</h2><p>Aktueller Mainboard-Bestand: <b>{} Stück</b> über {} Teile.</p><p>Versandweg: SMTP {}<br>Geprüft: {}<br><a href='{}'>{}</a></p></body></html>",
        fmt_de(total), count, esc(&cfg.mail.smtp_host), esc(&ts), PAGE_URL, PAGE_URL
    );
    let msg = compose_mail(cfg, &subject, &text, &html)?;
    deliver_mail(cfg, &msg)?;
    log_info!("Mail via SMTP {} gesendet: {}", cfg.mail.smtp_host, cfg.mail.from);
    Ok(())
}

// ---------------------------------------------------------------------------
// Ablauf
// ---------------------------------------------------------------------------

fn state_paths(cfg: &Config) -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(&cfg.paths.state_dir);
    (dir.join("state.json"), dir.join("history.csv"))
}

/// Rückgabe: (exit_code)
fn run_once(cfg: &Config, dry_run: bool) -> i32 {
    let ts = now_iso();
    let (state_path, history_path) = state_paths(cfg);

    let parts = match fetch_clearance_parts(cfg) {
        Ok(p) => p,
        Err(e) => {
            log_error!("Abruf fehlgeschlagen – kein State-Update: {}", e);
            return 2;
        }
    };

    let boards = filter_mainboards(&parts, &cfg.tracker.commodity_keys);
    if boards.is_empty() {
        log_warn!(
            "Keine Mainboards in der API-Response (Filter: {:?})",
            cfg.tracker.commodity_keys
        );
    }
    let new_map = parts_to_map(&boards);
    let (total, count) = compute_totals(&new_map);

    if dry_run {
        log_info!("DRY-RUN: {} Mainboard-Teile, Summe {} Stück", count, fmt_de(total));
        return 0;
    }

    let old_map = load_state(&state_path);

    if old_map.is_none() {
        if let Err(e) = save_state(&state_path, &ts, total, count, &new_map)
            .and_then(|_| append_history(&history_path, &ts, total, count))
        {
            log_error!("State konnte nicht geschrieben werden: {}", e);
            return 2;
        }
        log_info!("Baseline gesetzt: {} Stück über {} Mainboard-Teile", fmt_de(total), count);
        return 0;
    }
    let old_map = old_map.unwrap();

    let diff = build_diff(&old_map, &new_map);
    if diff.delta() > 0 {
        let subject = build_subject(&cfg.mail.subject_prefix, &diff);
        let text = build_mail_text(&diff, PAGE_URL, &ts);
        let html = build_mail_html(&diff, PAGE_URL, &ts);
        let msg = match compose_mail(cfg, &subject, &text, &html) {
            Ok(m) => m,
            Err(e) => {
                log_error!("Mail konnte nicht erstellt werden – Anstieg wird erneut gemeldet: {}", e);
                return 2;
            }
        };
        if let Err(e) = deliver_mail(cfg, &msg) {
            // State NICHT aktualisieren: der Anstieg wird beim nächsten Lauf
            // erneut gemeldet (Duplikat-Alarm schlägt verlorenen Alarm).
            log_error!("Mailversand fehlgeschlagen – Anstieg wird erneut gemeldet: {}", e);
            return 2;
        }
        if let Err(e) = save_state(&state_path, &ts, total, count, &new_map)
            .and_then(|_| append_history(&history_path, &ts, total, count))
        {
            log_error!("State konnte nicht geschrieben werden: {}", e);
            return 2;
        }
        log_info!("Mail gesendet: {}", subject);
        return 0;
    }

    if let Err(e) = save_state(&state_path, &ts, total, count, &new_map)
        .and_then(|_| append_history(&history_path, &ts, total, count))
    {
        log_error!("State konnte nicht geschrieben werden: {}", e);
        return 2;
    }
    log_info!("Kein Anstieg (vorher {} → jetzt {})", fmt_de(diff.old_total), fmt_de(diff.new_total));
    0
}

fn print_usage() {
    println!("Lenovo Clearance-Sale Mainboard-Tracker (Rust)");
    println!("  --config <pfad>   Config-Pfad (Standard: {})", DEFAULT_CONFIG_PATH);
    println!("  --once            einmal prüfen und beenden");
    println!("  --dry-run         einmal abrufen und zusammenfassen (kein State, keine Mail)");
    println!("  --test-mail       Testmail mit aktuellem Bestand senden");
}

fn real_main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path = DEFAULT_CONFIG_PATH.to_string();
    let mut once = false;
    let mut dry_run = false;
    let mut test_mail = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => match iter.next() {
                Some(v) => config_path = v.clone(),
                None => {
                    print_usage();
                    return 1;
                }
            },
            "--once" => once = true,
            "--dry-run" => dry_run = true,
            "--test-mail" => test_mail = true,
            other => {
                println!("Unbekanntes Argument: {}", other);
                print_usage();
                return 1;
            }
        }
    }

    let cfg = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            log_error!("Config-Fehler: {}", e);
            return 1;
        }
    };

    if dry_run {
        return run_once(&cfg, true);
    }
    if test_mail {
        return match send_test_mail(&cfg) {
            Ok(_) => 0,
            Err(e) => {
                log_error!("Testmail fehlgeschlagen: {}", e);
                2
            }
        };
    }
    if once {
        return run_once(&cfg, false);
    }
    println!("Kein Modus angegeben – die Rust-Version läuft per cron (siehe README).");
    print_usage();
    1
}

fn main() {
    std::process::exit(real_main());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fmt_de() {
        assert_eq!(fmt_de(16489), "16.489");
        assert_eq!(fmt_de(999), "999");
        assert_eq!(fmt_de(-1234), "-1.234");
    }

    #[test]
    fn test_esc() {
        assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
    }

    #[test]
    fn test_filter_and_totals() {
        let data: Vec<ApiPart> = vec![
            serde_json::from_value(serde_json::json!({"id": "A", "commodityKey": "system boards", "stock": 2, "name": "A"})).unwrap(),
            serde_json::from_value(serde_json::json!({"id": "B", "commodityKey": "heat sinks", "stock": 500, "name": "B"})).unwrap(),
            serde_json::from_value(serde_json::json!({"id": "C", "commodityKey": "SYSTEM BOARDS", "stock": 98, "name": "C"})).unwrap(),
            serde_json::from_value(serde_json::json!({"id": "D", "commodityKey": "system boards", "name": "D"})).unwrap(),
        ];
        let boards = filter_mainboards(&data, &["system boards".to_string()]);
        assert_eq!(boards.len(), 3);
        let map = parts_to_map(&boards);
        let (total, count) = compute_totals(&map);
        assert_eq!((total, count), (100, 3));
    }

    #[test]
    fn test_build_diff() {
        let mut old = BTreeMap::new();
        old.insert("A".to_string(), PartState { s: 5, n: "A".into() });
        old.insert("B".to_string(), PartState { s: 90, n: "B".into() });
        let mut new = BTreeMap::new();
        new.insert("A".to_string(), PartState { s: 25, n: "A".into() });
        new.insert("B".to_string(), PartState { s: 95, n: "B".into() });
        new.insert("C".to_string(), PartState { s: 3, n: "C".into() });
        let diff = build_diff(&old, &new);
        assert_eq!(diff.delta(), 28);
        assert_eq!(diff.increased.iter().map(|i| i.id.clone()).collect::<Vec<_>>(), vec!["A", "B"]);
        assert_eq!(diff.new_parts.iter().map(|p| p.0.clone()).collect::<Vec<_>>(), vec!["C"]);
    }

    #[test]
    fn test_stock_garbage() {
        let sf = |v: serde_json::Value| -> i64 { serde_json::from_value::<StockField>(v).unwrap().0 };
        assert_eq!(sf(serde_json::json!(null)), 0);
        assert_eq!(sf(serde_json::json!("7")), 7);
        assert_eq!(sf(serde_json::json!(-5)), 0);
        assert_eq!(sf(serde_json::json!("kaputt")), 0);
        assert_eq!(sf(serde_json::json!(7.9)), 7);
        // fehlendes Feld → Default
        let part: ApiPart = serde_json::from_value(serde_json::json!({"id": "X"})).unwrap();
        assert_eq!(part.stock.0, 0);
    }
}
