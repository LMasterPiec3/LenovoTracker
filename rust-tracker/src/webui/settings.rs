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
