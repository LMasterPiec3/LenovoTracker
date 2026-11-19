//! Lenovo Clearance-Sale Mainboard-Tracker (Rust-Variante, minimal RAM).
//!
//! Gleiche Logik wie die Python-Version: API abrufen (Chrome-TLS-Imitation),
//! Mainboards filtern (`commodityKey == "system boards"`), Summe `stock`
//! bilden, Diff gegen state.json, Mail bei Anstieg (Plain-SMTP direkt, kein
//! TLS-Stack im Binary, kein Mail-Daemon nötig). Wird per Cron alle 30 Minuten
//! mit `--once` gestartet.

use anyhow::{anyhow, bail, Context, Result};
use lettre::Transport;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::SmtpTransport;
use serde::{Deserialize, Serialize};
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

/// RFC3339 (UTC, Sekunden-Genauigkeit) ohne `time`-Crate: Hinnants
/// civil_from_days, damit keine Abhängigkeit für eine Log-Zeile nötig ist.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m as u32, d as u32)
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
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
    // Wird nur für Config-Kompatibilität mitgelesen (cron steuert das Intervall).
    #[serde(default = "default_interval")]
    #[allow(dead_code)]
    interval_minutes: u64,
    #[serde(default = "default_commodity_keys")]
    commodity_keys: Vec<String>,
    /// Optionaler Keyword-Filter INHALTB der Boards (ODER, case-insensitive,
    /// Substring auf `name` UND `id`). Leer = alle Mainboards (bisheriges Verhalten).
    #[serde(default = "default_name_keywords")]
    name_keywords: Vec<String>,
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
    // HINWEIS: smtp_starttls wird bewusst ignoriert (unbekannte Felder sind
    // serde egal) – der STARTTLS-Pfad von Proton hängt, TLS-Stack ist entfernt.
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
fn default_name_keywords() -> Vec<String> { Vec::new() }
fn default_mode() -> String { "smtp".into() }
fn default_subject_prefix() -> String { "[Lenovo Tracker]".into() }
fn default_smtp_port() -> u16 { 25 }
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
/// Direkte Visitor-Impl statt Value-Zwischenschritt: keine Allocation pro Teil.
#[derive(Debug, Clone, Copy, Default)]
struct StockField(i64);

impl<'de> Deserialize<'de> for StockField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StockVisitor;

        impl<'de> serde::de::Visitor<'de> for StockVisitor {
            type Value = i64;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Zahl oder zahlähnlicher Text")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<i64, E> {
                Ok(v.max(0))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<i64, E> {
                // u64 > i64::MAX: wie die alte f64-Saturierung → i64::MAX
                Ok(i64::try_from(v).unwrap_or(i64::MAX).max(0))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<i64, E> {
                Ok((v as i64).max(0)) // saturierender Cast, NaN → 0
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<i64, E> {
                Ok(v.trim().parse::<i64>().unwrap_or(0).max(0))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<i64, E> {
                Ok(0)
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<i64, E> {
                Ok(0)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<i64, A::Error> {
                // Elemente konsumieren, sonst beschweren sich Deserializer
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(0)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<i64, A::Error> {
                while map.next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?.is_some() {}
                Ok(0)
            }
        }

        Ok(StockField(d.deserialize_any(StockVisitor)?))
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

/// Case-insensitiver Substring-Test ohne Allocation (ASCII-Folding; Nicht-ASCII
/// wird byte-genau verglichen, für Modellcodes/Chipsätze ausreichend).
fn ci_contains(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if n.len() > h.len() {
        return false;
    }
    h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// Keyword-Filter: ODER-verknüpft auf `name` UND `id`. Leere Liste = kein Zusatzfilter.
fn keyword_matches(part: &ApiPart, keywords: &[String]) -> bool {
    keywords.is_empty()
        || keywords.iter().any(|kw| ci_contains(&part.name, kw) || ci_contains(&part.id, kw))
}

fn filter_mainboards<'a>(parts: &'a [ApiPart], keys: &[String], keywords: &[String]) -> Vec<&'a ApiPart> {
    // Allocation-frei: case-insensitiver Vergleich statt pro Teil lowercase zu allokieren.
    parts
        .iter()
        .filter(|p| {
            keys.iter()
                .any(|k| p.commodity_key.trim().eq_ignore_ascii_case(k))
                && keyword_matches(p, keywords)
        })
        .collect()
}

/// Kanonischer Fingerprint der aktiven Filter (commodity_keys + name_keywords).
/// Landet im state.json – ändert er sich, wird die Baseline neu gesetzt
/// (sonst würde ein Filterwechsel als riesiger „Anstieg" alarmieren).
fn filter_fingerprint(keys: &[String], keywords: &[String]) -> String {
    let mut ks: Vec<String> = keys.iter().map(|s| s.trim().to_lowercase()).collect();
    ks.sort();
    let mut kw: Vec<String> = keywords.iter().map(|s| s.trim().to_lowercase()).collect();
    kw.sort();
    format!("c={};k={}", ks.join("|"), kw.join("|"))
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

/// Typisiertes State-File: ein einziger Parse-Pass – kein Value-Baum,
/// kein Teilbaum-Clone mehr (bei ~3.100 Teilen einige MB Peak-RAM gespart).
/// `filter` trägt den Filter-Fingerprint; fehlt er (alter State) oder weicht
/// ab (Filterwechsel), wird die Baseline neu gesetzt.
#[derive(Deserialize)]
struct StateFileIn {
    parts: BTreeMap<String, PartState>,
    #[serde(default)]
    filter: Option<String>,
}

fn load_state(path: &Path, expected_filter: &str) -> Option<BTreeMap<String, PartState>> {
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<StateFileIn>(&text) {
            Ok(sf) => {
                if sf.filter.as_deref() != Some(expected_filter) {
                    log_info!(
                        "Filter hat sich geändert (state: {:?} → jetzt {:?}) – Baseline wird neu gesetzt",
                        sf.filter.unwrap_or_else(|| "(alt, ohne Filtermarkierung)".into()),
                        expected_filter
                    );
                    return None;
                }
                Some(sf.parts)
            }
            Err(e) => {
                log_error!("state.json ist beschädigt/unerwartetes Format ({}) – Baseline wird neu gesetzt", e);
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

/// Nur die State-Felder, direkt in die Datei serialisiert –
/// kein `json!`-Value-Baum und kein JSON-String als Zwischenkopie im RAM.
#[derive(Serialize)]
struct StateFileOut<'a> {
    last_check: &'a str,
    total_stock: i64,
    part_count: usize,
    filter: &'a str,
    parts: &'a BTreeMap<String, PartState>,
}

fn save_state(
    state_path: &Path,
    ts: &str,
    total: i64,
    count: usize,
    filter: &str,
    part_map: &BTreeMap<String, PartState>,
) -> Result<()> {
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", state_path.display()));
    {
        let f = fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(64 * 1024, f);
        serde_json::to_writer(
            &mut w,
            &StateFileOut { last_check: ts, total_stock: total, part_count: count, filter, parts: part_map },
        )?;
        w.flush()?;
    }
    fs::rename(&tmp, state_path)?;
    Ok(())
}

/// save_state + append_history als Einheit – der Aufruf ist an drei Stellen
/// identisch gekoppelt.
#[allow(clippy::too_many_arguments)]
fn persist(
    state_path: &Path,
    history_path: &Path,
    ts: &str,
    total: i64,
    count: usize,
    filter: &str,
    part_map: &BTreeMap<String, PartState>,
) -> Result<()> {
    save_state(state_path, ts, total, count, filter, part_map)?;
    append_history(history_path, ts, total, count)
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
    // Plain-SMTP (Proton :25): kein TLS-Stack im Binary, der STARTTLS-Pfad
    // hängt bei Proton ohnehin (siehe README "Produktivstand").
    let mailer = SmtpTransport::builder_dangerous(&cfg.mail.smtp_host)
        .port(cfg.mail.smtp_port)
        .timeout(Some(Duration::from_secs(60)))
        .build();
    mailer.send(msg).map(|_| ())?;
    Ok(())
}

fn send_test_mail(cfg: &Config) -> Result<()> {
    let parts = fetch_clearance_parts(cfg).map_err(|e| anyhow!("Abruf fehlgeschlagen: {}", e))?;
    let boards = filter_mainboards(&parts, &cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);
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
    let fp = filter_fingerprint(&cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);

    let parts = match fetch_clearance_parts(cfg) {
        Ok(p) => p,
        Err(e) => {
            log_error!("Abruf fehlgeschlagen – kein State-Update: {}", e);
            return 2;
        }
    };

    let boards = filter_mainboards(&parts, &cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);
    if boards.is_empty() {
        log_warn!(
            "Keine Mainboards in der API-Response (Filter: {:?}, Keywords: {:?})",
            cfg.tracker.commodity_keys, cfg.tracker.name_keywords
        );
    }
    let new_map = parts_to_map(&boards);
    let (total, count) = compute_totals(&new_map);

    if dry_run {
        log_info!("DRY-RUN: {} Mainboard-Teile, Summe {} Stück", count, fmt_de(total));
        return 0;
    }

    let old_map = load_state(&state_path, &fp);

    if old_map.is_none() {
        if let Err(e) = persist(&state_path, &history_path, &ts, total, count, &fp, &new_map) {
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
        if let Err(e) = persist(&state_path, &history_path, &ts, total, count, &fp, &new_map) {
            log_error!("State konnte nicht geschrieben werden: {}", e);
            return 2;
        }
        log_info!("Mail gesendet: {}", subject);
        return 0;
    }

    if let Err(e) = persist(&state_path, &history_path, &ts, total, count, &fp, &new_map) {
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
        let boards = filter_mainboards(&data, &["system boards".to_string()], &[]);
        assert_eq!(boards.len(), 3);
        let map = parts_to_map(&boards);
        let (total, count) = compute_totals(&map);
        assert_eq!((total, count), (100, 3));
    }

    #[test]
    fn test_ci_contains() {
        assert!(ci_contains("AMD B650M Board", "b650"));
        assert!(ci_contains("AMD B650M Board", "B650M"));
        assert!(!ci_contains("AMD B550M Board", "b650"));
        assert!(ci_contains("irgendwas", ""));
        assert!(!ci_contains("kurz", "länger-als-haystack"));
        // Nicht-ASCII: byte-genau (kein Folding) — dokumentiertes Verhalten
        assert!(ci_contains("TÜV-Board", "TÜV"));
    }

    #[test]
    fn test_keyword_filter() {
        let mk = |id: &str, name: &str, stock: i64| -> ApiPart {
            serde_json::from_value(serde_json::json!({"id": id, "commodityKey": "system boards", "stock": stock, "name": name})).unwrap()
        };
        let data = vec![
            mk("P1", "AMD B650M Board", 5),
            mk("P2", "Intel H610 Board", 7),
            mk("P3", "Lenovo AM5 Board", 9),
        ];
        let keys = vec!["system boards".to_string()];
        // leer → alle
        assert_eq!(filter_mainboards(&data, &keys, &[]).len(), 3);
        // ODER auf name, case-insensitiv
        let b = filter_mainboards(&data, &keys, &["b650".to_string(), "am5".to_string()]);
        assert_eq!(b.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["P1", "P3"]);
        // Match auf id statt name
        let b = filter_mainboards(&data, &keys, &["P2".to_string()]);
        assert_eq!(b.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["P2"]);
        // nichts passt
        assert!(filter_mainboards(&data, &keys, &["x999".to_string()]).is_empty());
    }

    #[test]
    fn test_filter_fingerprint() {
        let keys = vec!["system boards".to_string()];
        let empty: Vec<String> = vec![];
        let kw = vec!["B650".to_string(), "AM5".to_string()];
        let f0 = filter_fingerprint(&keys, &empty);
        let f1 = filter_fingerprint(&keys, &kw);
        // Reihenfolge der Keywords ist egal (sortiert)
        let f1_rev = filter_fingerprint(&keys, &["am5".to_string(), "b650".to_string()]);
        assert_eq!(f1, f1_rev);
        assert_ne!(f0, f1);
        assert!(f0.starts_with("c=system boards;k="));
        assert_eq!(filter_fingerprint(&["SYSTEM BOARDS".to_string()], &empty), f0); // case/trim-normalisiert
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
        assert_eq!(sf(serde_json::json!(true)), 0);
        assert_eq!(sf(serde_json::json!([1])), 0);
        // u64 > i64::MAX → saturiert wie die alte f64-Variante
        assert_eq!(sf(serde_json::json!(18446744073709551615u64)), i64::MAX);
        // fehlendes Feld → Default
        let part: ApiPart = serde_json::from_value(serde_json::json!({"id": "X"})).unwrap();
        assert_eq!(part.stock.0, 0);
    }

    #[test]
    fn test_civil_from_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 kein Schaltjahr
        assert_eq!(civil_from_days(365), (1971, 1, 1));
        // 2024 Schaltjahr: 31.12.2024 = 20088 Tage nach Epoch
        assert_eq!(civil_from_days(20_088), (2024, 12, 31));
        assert_eq!(civil_from_days(20_089), (2025, 1, 1));
        // Datum aus der Planung: 2026-11-18 = 20775 Tage (verifiziert via datetime.date)
        assert_eq!(civil_from_days(20_775), (2026, 11, 18));
    }

    #[test]
    fn test_state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("lenovotracker-test-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut map = BTreeMap::new();
        map.insert("5A10W86303".to_string(), PartState { s: 98, n: "Board A".into() });
        map.insert("X".to_string(), PartState { s: 2, n: "Board \"B\" <ö>".to_string() });
        save_state(&path, "2026-11-19T00:00:00Z", 100, 2, "c=system boards;k=", &map).expect("save");
        // gleicher Filter → geladen
        let loaded = load_state(&path, "c=system boards;k=").expect("roundtrip");
        assert_eq!(loaded, map);
        // anderer Filter → Baseline-Reset
        assert!(load_state(&path, "c=system boards;k=b650").is_none());
        // alter State ohne Filter-Feld → Baseline-Reset
        std::fs::write(&path, "{\"last_check\": \"x\", \"total_stock\": 1, \"part_count\": 1, \"parts\": {}}").unwrap();
        assert!(load_state(&path, "c=system boards;k=").is_none());
        // kaputte Datei → None, keine Panic
        std::fs::write(&path, "{\"parts\": 42}").unwrap();
        assert!(load_state(&path, "c=system boards;k=").is_none());
        std::fs::write(&path, "{\"last_check\": \"x\"}").unwrap(); // parts fehlt
        assert!(load_state(&path, "c=system boards;k=").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
