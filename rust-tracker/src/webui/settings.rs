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

pub(crate) fn redact(text: &str) -> String {
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

pub(crate) fn unredact(text: &str, old: &str) -> String {
    if !text.contains(REDACTED) {
        return text.to_string();
    }
    let secret = secret_value(old).unwrap_or_default();
    text.replace(&format!("\"{}\"", REDACTED), &format!("\"{}\"", secret))
}

/// Wert von `smtp_pass = "…"` aus der alten Config (nur die Anführungszeichen
/// werden entfernt; der Text ist ein einfaches TOML-String-Literal).
pub(crate) fn secret_value(text: &str) -> Option<String> {
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
