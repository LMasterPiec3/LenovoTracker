//! Anmeldungen: optionales Site-Token (`[webui] token`, Benutzername egal) und
//! der Admin-Login für die Einstellungen (`[webui] admin_user`/`admin_pass`).
//!
//! Verglichen wird zeitkonstant, damit weder Token noch Passwort über die
//! Antwortzeit erratbar werden.

use super::*;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Site-Token: gilt für die ganze Seite, der Benutzername ist egal.
pub(crate) fn authorized(cfg: &Config, headers: &BTreeMap<String, String>) -> bool {
    cfg.webui.token.is_empty() || basic_ok(headers, None, &cfg.webui.token)
}

/// Einstellungen: eigener Benutzer, eigenes Passwort. Ohne `admin_pass` bleibt
/// der Bereich zu – es gibt keinen Standard-Admin.
pub(crate) fn admin_ok(cfg: &Config, headers: &BTreeMap<String, String>) -> bool {
    !cfg.webui.admin_pass.is_empty()
        && basic_ok(headers, Some(&cfg.webui.admin_user), &cfg.webui.admin_pass)
}

/// Login-Bremse im Speicher: Fehlversuche zählen, danach kurz sperren. Ein
/// Neustart hebt sie auf; die Werte stehen in `[webui]` (`admin_attempts`,
/// `admin_lockout_secs`, jeweils 0 = Bremse aus).
struct Brake {
    fails: u32,
    until: Option<Instant>,
}

static BRAKE: Mutex<Brake> = Mutex::new(Brake {
    fails: 0,
    until: None,
});

fn brake() -> std::sync::MutexGuard<'static, Brake> {
    BRAKE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Wie lange ist die Anmeldung noch gesperrt? `None` = frei.
pub(crate) fn login_wait() -> Option<Duration> {
    let mut b = brake();
    let until = b.until?;
    let now = Instant::now();
    if until > now {
        return Some(until - now);
    }
    // Sperre abgelaufen: Zähler zurück, sonst wäre der nächste Fehler sofort wieder gesperrt.
    b.fails = 0;
    b.until = None;
    None
}

/// Fehlversuch zählen – ab `admin_attempts` ist der Login `admin_lockout_secs` zu.
pub(crate) fn note_fail(cfg: &Config) {
    let attempts = cfg.webui.admin_attempts;
    let lockout = cfg.webui.admin_lockout_secs;
    if attempts == 0 || lockout == 0 {
        return;
    }
    let mut b = brake();
    b.fails += 1;
    if b.fails >= attempts {
        b.fails = 0;
        b.until = Some(Instant::now() + Duration::from_secs(lockout));
    }
}

/// Erfolgreiche Anmeldung setzt Zähler und Sperre zurück.
pub(crate) fn note_ok() {
    let mut b = brake();
    b.fails = 0;
    b.until = None;
}

/// Stimmen Benutzer (falls erwartet) und Passwort im `Authorization`-Kopf?
fn basic_ok(headers: &BTreeMap<String, String>, user: Option<&str>, pass: &str) -> bool {
    let Some(value) = headers.get("authorization") else {
        return false;
    };
    let Some(encoded) = value
        .split_once(' ')
        .and_then(|(scheme, v)| scheme.eq_ignore_ascii_case("basic").then_some(v))
    else {
        return false;
    };
    let Some(text) = base64_decode(encoded).and_then(|raw| String::from_utf8(raw).ok()) else {
        return false;
    };
    let Some((given_user, given_pass)) = text.split_once(':') else {
        return false;
    };
    // beide Vergleiche laufen immer, damit die Laufzeit nichts über den Benutzer sagt
    let same_user = match user {
        Some(user) => ct_eq(given_user.as_bytes(), user.as_bytes()),
        None => true,
    };
    same_user & ct_eq(given_pass.as_bytes(), pass.as_bytes())
}

pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) fn base64_decode(input: &str) -> Option<Vec<u8>> {
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
