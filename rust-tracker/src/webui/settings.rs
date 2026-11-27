//! Einstellungen: `config.toml` an die Oberfläche geben und zurückschreiben.
//!
//! Das SMTP-Passwort verlässt den Server nie – die Seite sieht einen
//! Platzhalter, der beim Speichern durch den alten Wert ersetzt wird.

use super::*;
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

pub(crate) fn config_json(config_path: &str) -> Response {
    match std::fs::read_to_string(config_path) {
        Ok(text) => Response::json(&json!({
            "path": config_path,
            "text": redact(&text),
        })),
        Err(e) => Response::error(500, &format!("Config nicht lesbar: {}", e)),
    }
}

pub(crate) fn save_config_text(config_path: &str, body: &[u8]) -> Response {
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
    write_config(config_path, &text)
}

/// Config schreiben: Sicherung als `.bak`, Rechte übernehmen, atomar ersetzen.
fn write_config(config_path: &str, text: &str) -> Response {
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

/// Felder, die die Seite nur als Platzhalter sieht: Passwörter bleiben auf dem
/// Server, auch wenn die Einstellungen offen sind.
const SECRET_KEYS: [&str; 2] = ["smtp_pass", "admin_pass"];

/// Geheimzeile (`key = "…"`): Schlüssel und Wert ohne Anführungszeichen.
fn secret_line(line: &str) -> Option<(&'static str, String)> {
    let trimmed = line.trim_start();
    let key = SECRET_KEYS.into_iter().find(|key| {
        trimmed
            .strip_prefix(key)
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    })?;
    let value = trimmed.split_once('=')?.1.trim().trim_matches('"');
    Some((key, value.to_string()))
}

pub(crate) fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match secret_line(line) {
            Some((key, _)) => out.push_str(&format!("{} = \"{}\"\n", key, REDACTED)),
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Platzhalter je Zeile durch den alten Wert *desselben* Schlüssels ersetzen –
/// sonst landete beim Speichern das SMTP-Passwort im Admin-Feld.
pub(crate) fn unredact(text: &str, old: &str) -> String {
    if !text.contains(REDACTED) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        match secret_line(line) {
            Some((key, _)) if line.contains(REDACTED) => {
                let secret = secret_value(old, key).unwrap_or_default();
                out.push_str(&line.replace(REDACTED, &secret));
                out.push('\n');
            }
            _ => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Wert von `key = "…"` aus der alten Config.
fn secret_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| match secret_line(line)? {
        (found, value) if found == key => Some(value),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Einstellungen als Formular
// ---------------------------------------------------------------------------

/// Art eines Feldes – steuert die Eingabe in der Seite und die Schreibweise.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Number,
    List,
    Secret,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Number => "number",
            Kind::List => "list",
            Kind::Secret => "secret",
        }
    }
}

struct Field {
    section: &'static str,
    key: &'static str,
    kind: Kind,
    label: &'static str,
    help: &'static str,
}

impl Field {
    const fn new(
        section: &'static str,
        key: &'static str,
        kind: Kind,
        label: &'static str,
    ) -> Field {
        Field {
            section,
            key,
            kind,
            label,
            help: "",
        }
    }

    /// Zusatz unter dem Feld.
    const fn help(self, help: &'static str) -> Field {
        Field {
            section: self.section,
            key: self.key,
            kind: self.kind,
            label: self.label,
            help,
        }
    }
}

/// Felder des Formulars, in Anzeigereihenfolge.
const FIELDS: &[Field] = &[
    Field::new("api", "url", Kind::Text, "API-Adresse").help("Lenovo-Clearance-API"),
    Field::new("api", "referer", Kind::Text, "Referer").help("muss zum API-Aufruf passen"),
    Field::new("api", "timeout_seconds", Kind::Number, "Timeout (Sekunden)"),
    Field::new("api", "retries", Kind::Number, "Wiederholungen"),
    Field::new("api", "curl_path", Kind::Text, "curl (Impersonation)")
        .help("Pfad zur curl-Impersonate-Binary"),
    Field::new(
        "api",
        "impersonate_target",
        Kind::Text,
        "Impersonation-Ziel",
    )
    .help("z. B. chrome124"),
    Field::new("api", "image_curl_path", Kind::Text, "curl für Bilder")
        .help("normales curl genügt"),
    Field::new(
        "api",
        "specs_gap_ms",
        Kind::Number,
        "Abstand Specs-Batches (ms)",
    ),
    Field::new("api", "specs_batch", Kind::Number, "IDs pro Specs-Batch"),
    Field::new("tracker", "commodity_keys", Kind::List, "Warengruppen")
        .help("durch Komma getrennt"),
    Field::new("tracker", "name_keywords", Kind::List, "Namens-Filter")
        .help("leer = alle Teile; Teiltreffer, ODER"),
    Field::new("mail", "mode", Kind::Text, "Modus").help("smtp"),
    Field::new("mail", "from", Kind::Text, "Absender"),
    Field::new("mail", "to", Kind::List, "Empfänger").help("durch Komma getrennt"),
    Field::new("mail", "extra_recipients", Kind::List, "Weitere Empfänger")
        .help("durch Komma getrennt"),
    Field::new("mail", "subject_prefix", Kind::Text, "Betreff-Präfix"),
    Field::new("mail", "smtp_host", Kind::Text, "SMTP-Server"),
    Field::new("mail", "smtp_port", Kind::Number, "SMTP-Port"),
    Field::new("mail", "smtp_user", Kind::Text, "SMTP-Benutzer"),
    Field::new("mail", "smtp_pass", Kind::Secret, "SMTP-Passwort").help("leer = unverändert"),
    Field::new("paths", "state_dir", Kind::Text, "Datenverzeichnis"),
    Field::new("webui", "bind", Kind::Text, "Adresse").help("127.0.0.1:8787 = nur lokal"),
    Field::new("webui", "token", Kind::Secret, "Site-Token")
        .help("schützt die ganze Seite; leer = unverändert"),
    Field::new("webui", "admin_user", Kind::Text, "Admin-Benutzer"),
    Field::new("webui", "admin_pass", Kind::Secret, "Admin-Passwort").help("leer = unverändert"),
    Field::new(
        "webui",
        "admin_attempts",
        Kind::Number,
        "Fehlversuche bis Sperre",
    )
    .help("0 = keine Bremse"),
    Field::new(
        "webui",
        "admin_lockout_secs",
        Kind::Number,
        "Sperrdauer (Sekunden)",
    ),
];

/// Wirkt erst nach einem Neustart des Dienstes.
fn needs_restart(f: &Field) -> bool {
    (f.section, f.key) == ("webui", "bind")
}

/// 400 mit Feldnamen und Klartext-Label; die Seite markiert die Zeile.
fn field_error(name: &str, label: &str, message: &str) -> Response {
    Response::error_field(400, &format!("{}: {}", label, message), Some(name))
}

/// Formularfelder samt aktuellen Werten (Geheimnisse nur als Platzhalter).
pub(crate) fn settings_json(config_path: &str) -> Response {
    let text = match std::fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(e) => return Response::error(500, &format!("Config nicht lesbar: {}", e)),
    };
    let cfg = match parse_config(&text) {
        Ok(cfg) => cfg,
        Err(e) => return Response::error(500, &format!("Config ungültig: {}", e)),
    };
    let values = serde_json::to_value(&cfg).unwrap_or_default();
    let fields: Vec<serde_json::Value> = FIELDS
        .iter()
        .map(|f| {
            json!({
                "section": f.section,
                "key": f.key,
                "kind": f.kind.as_str(),
                "label": f.label,
                "help": f.help,
                "restart": needs_restart(f),
                "value": current_value(&values, f),
            })
        })
        .collect();
    Response::json(&json!({
        "path": config_path,
        "text": redact(&text),
        "fields": fields,
    }))
}

/// Anzeigewert eines Feldes; Geheimnisse erscheinen nur als `__KEEP__`.
fn current_value(cfg: &serde_json::Value, f: &Field) -> String {
    let value = &cfg[f.section][f.key];
    match f.kind {
        Kind::Secret => {
            let set = value.as_str().is_some_and(|s| !s.is_empty());
            if set {
                REDACTED.to_string()
            } else {
                String::new()
            }
        }
        Kind::List => value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        Kind::Text | Kind::Number => match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        },
    }
}

/// Formular abschicken: jedes Feld einzeln in die Datei schreiben, dann prüfen.
pub(crate) fn save_settings(config_path: &str, body: &[u8]) -> Response {
    #[derive(Deserialize)]
    struct Input {
        values: std::collections::BTreeMap<String, String>,
    }
    let input: Input = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::error(400, &format!("Ungültiges JSON: {}", e)),
    };
    let mut text = std::fs::read_to_string(config_path).unwrap_or_default();
    for (name, raw) in &input.values {
        let Some(field) = FIELDS
            .iter()
            .find(|f| format!("{}.{}", f.section, f.key) == *name)
        else {
            return field_error(name, name, "unbekanntes Feld");
        };
        // leeres Geheimfeld = alten Wert behalten
        if field.kind == Kind::Secret && raw.is_empty() {
            continue;
        }
        let value = match toml_value(field.kind, raw) {
            Ok(value) => value,
            Err(e) => return field_error(name, field.label, &e.to_string()),
        };
        text = set_key(&text, field.section, field.key, &value);
    }
    if let Err(e) = parse_config(&text) {
        return Response::error(400, &format!("Config ungültig: {}", e));
    }
    write_config(config_path, &text)
}

/// Formularwert als TOML-Literal.
fn toml_value(kind: Kind, raw: &str) -> anyhow::Result<String> {
    match kind {
        Kind::Number => {
            if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
                anyhow::bail!("muss eine Zahl sein ({:?})", raw);
            }
            Ok(raw.to_string())
        }
        Kind::List => {
            let items: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| format!("\"{}\"", escape(item)))
                .collect();
            Ok(format!("[{}]", items.join(", ")))
        }
        Kind::Text | Kind::Secret => {
            if raw.contains('\n') || raw.contains('\r') {
                anyhow::bail!("darf keinen Zeilenumbruch enthalten");
            }
            Ok(format!("\"{}\"", escape(raw)))
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `key = wert` ersetzen oder im richtigen Abschnitt ergänzen – Kommentare und
/// unbekannte Schlüssel bleiben stehen.
fn set_key(text: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{}]", section);
    let line = format!("{} = {}", key, value);
    let mut out = String::with_capacity(text.len() + line.len() + 16);
    let mut inside = false;
    let mut done = false;
    for existing in text.lines() {
        let trimmed = existing.trim();
        if trimmed.starts_with('[') {
            // Ende des Ziel-Abschnitts: fehlenden Schlüssel davor einschieben
            if inside && !done {
                out.push_str(&line);
                out.push('\n');
                done = true;
            }
            inside = trimmed == header;
        } else if inside
            && !done
            && existing
                .split_once('=')
                .is_some_and(|(k, _)| k.trim() == key)
        {
            out.push_str(&line);
            out.push('\n');
            done = true;
            continue;
        }
        out.push_str(existing);
        out.push('\n');
    }
    if !done {
        if !out.contains(&header) {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(&header);
            out.push('\n');
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}
