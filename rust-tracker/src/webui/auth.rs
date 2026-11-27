//! Optionale HTTP-Basic-Anmeldung (`[webui] token`, Benutzername egal).
//!
//! Der Vergleich läuft zeitkonstant, damit das Token nicht über die
//! Antwortzeit erratbar wird.

use super::*;
use std::collections::BTreeMap;

pub(crate) fn authorized(cfg: &Config, headers: &BTreeMap<String, String>) -> bool {
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
