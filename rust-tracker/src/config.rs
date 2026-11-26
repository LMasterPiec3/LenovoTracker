//! Config-Laden (TOML) mit Defaults – keine Werte fest verdrahtet.

use crate::util::*;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Config {
    pub api: ApiCfg,
    #[serde(default)]
    pub tracker: TrackerCfg,
    pub mail: MailCfg,
    #[serde(default)]
    pub paths: PathsCfg,
    #[serde(default)]
    pub webui: WebuiCfg,
}

#[derive(Deserialize)]
pub struct ApiCfg {
    pub url: String,
    #[serde(default = "default_referer")]
    pub referer: String,
    #[serde(default = "default_u60")]
    pub timeout_seconds: u64,
    #[serde(default = "default_retries")]
    pub retries: u64,
    #[serde(default = "default_curl_path")]
    pub curl_path: String,
    #[serde(default = "default_impersonate")]
    pub impersonate_target: String,
    /// Startabstand zwischen Specs-Batches in ms (Rate-Limit-Schutz).
    #[serde(default = "default_specs_gap_ms")]
    pub specs_gap_ms: u64,
    /// IDs pro Specs-Batch. 200 sind an der API verifiziert; lehnt der Server
    /// einen Batch mit 4xx ab, halbiert der Client ihn automatisch.
    #[serde(default = "default_specs_batch")]
    pub specs_batch: usize,
}

#[derive(Deserialize, Default)]
pub struct TrackerCfg {
    // Wird nur für Config-Kompatibilität mitgelesen (cron steuert das Intervall).
    #[serde(default = "default_interval")]
    #[allow(dead_code)]
    pub interval_minutes: u64,
    #[serde(default = "default_commodity_keys")]
    pub commodity_keys: Vec<String>,
    /// Optionaler Keyword-Filter INHALTB der Boards (ODER, case-insensitive,
    /// Substring auf `name` UND `id`). Leer = alle Mainboards (bisheriges Verhalten).
    #[serde(default = "default_name_keywords")]
    pub name_keywords: Vec<String>,
}

#[derive(Deserialize)]
pub struct MailCfg {
    #[serde(default = "default_mode")]
    pub mode: String,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default)]
    pub extra_recipients: Vec<String>,
    #[serde(default = "default_subject_prefix")]
    pub subject_prefix: String,
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_user: String,
    #[serde(default)]
    pub smtp_pass: String,
    // HINWEIS: smtp_starttls wird bewusst ignoriert (unbekannte Felder sind
    // serde egal) – TLS/STARTTLS übernimmt curl, sobald smtp_user gesetzt ist.
}

/// Web-UI (`--serve`). Standard ist Loopback, weil die Oberfläche die Config
/// schreiben darf; eine andere Adresse nur zusammen mit `token`.
#[derive(Deserialize)]
pub struct WebuiCfg {
    #[serde(default = "default_webui_bind")]
    pub bind: String,
    /// HTTP-Basic-Token (Benutzername egal). Leer = ohne Login, nur Loopback.
    #[serde(default)]
    pub token: String,
}

// `#[serde(default)]` auf dem Feld nutzt `Default::default()` – hier muss der
// Loopback-Bind auch ohne `[webui]`-Abschnitt greifen.
impl Default for WebuiCfg {
    fn default() -> Self {
        WebuiCfg {
            bind: default_webui_bind(),
            token: String::new(),
        }
    }
}

#[derive(Deserialize, Default)]
pub struct PathsCfg {
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
}

fn default_referer() -> String {
    PAGE_URL.into()
}
fn default_u60() -> u64 {
    60
}
fn default_retries() -> u64 {
    3
}
fn default_curl_path() -> String {
    "/opt/lenovo-tracker/curl-impersonate".into()
}
fn default_impersonate() -> String {
    "chrome124".into()
}
fn default_specs_gap_ms() -> u64 {
    250
}
fn default_specs_batch() -> usize {
    200
}
fn default_interval() -> u64 {
    30
}
fn default_commodity_keys() -> Vec<String> {
    vec!["system boards".into()]
}
fn default_name_keywords() -> Vec<String> {
    Vec::new()
}
fn default_webui_bind() -> String {
    "127.0.0.1:8787".into()
}
fn default_mode() -> String {
    "smtp".into()
}
fn default_subject_prefix() -> String {
    "[Lenovo Tracker]".into()
}
fn default_smtp_port() -> u16 {
    25
}
fn default_state_dir() -> String {
    "/var/lib/lenovotracker".into()
}

pub fn load_config(path: &str) -> Result<Config> {
    let text =
        fs::read_to_string(path).with_context(|| format!("Config-Datei nicht lesbar: {}", path))?;
    parse_config(&text)
}

/// Wie `load_config`, aber aus einem String – die Web-UI prüft damit, bevor
/// sie eine bearbeitete Config auf die Platte schreibt.
pub fn parse_config(text: &str) -> Result<Config> {
    let cfg: Config =
        basic_toml::from_str(text).context("Config ist kein gültiges TOML / unvollständig")?;
    if cfg.api.url.is_empty() {
        bail!("api.url fehlt in der Config");
    }
    if !is_loopback_bind(&cfg.webui.bind) && cfg.webui.token.is_empty() {
        bail!(
            "webui.bind = {:?} ist nicht Loopback – setze webui.token oder lasse bind bei 127.0.0.1",
            cfg.webui.bind
        );
    }
    if cfg.mail.from.is_empty() || cfg.mail.to.is_empty() {
        bail!("mail.from und mail.to müssen gesetzt sein");
    }
    if cfg.mail.mode != "smtp" {
        bail!(
            "Rust-Version unterstützt nur mail.mode = \"smtp\" (kein sendmail-Daemon im Container)"
        );
    }
    if cfg.mail.smtp_host.is_empty() {
        bail!("mail.smtp_host fehlt");
    }
    Ok(cfg)
}

/// Loopback-Erkennung für `webui.bind` (IPv4/IPv6 mit Port oder „localhost").
pub fn is_loopback_bind(bind: &str) -> bool {
    let host = bind
        .rsplit_once(':')
        .map_or(bind, |(h, _)| h)
        .trim_start_matches('[')
        .trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}
