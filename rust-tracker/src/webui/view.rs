//! JSON für Bestand und Verlauf: `state.json`/`tracked.json` einlesen,
//! Teile mit Specs, Preisen, Delta und NEU-Marke aufbereiten.
//!
//! Die Seite bekommt fertige Felder (CPU-Klasse, Benchmark, Preise in Cent),
//! damit im Browser keine Fachlogik nötig ist.

use super::*;
use crate::{diff::*, guard::*, state::*};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

/// Ein gespeicherter Bestand – `state.json` (Mail-Liste) und `tracked.json`
/// (alle Teile) haben dieselbe Form. `tracked.json` führt keine `gone`-Liste;
/// dort bleibt das Feld leer.
#[derive(Deserialize, Default)]
pub(crate) struct StoredView {
    #[serde(default)]
    pub(crate) last_check: String,
    #[serde(default)]
    pub(crate) total_stock: i64,
    #[serde(default)]
    pub(crate) part_count: usize,
    #[serde(default)]
    pub(crate) filter: String,
    #[serde(default)]
    pub(crate) parts: BTreeMap<String, PartState>,
    #[serde(default)]
    pub(crate) gone: BTreeMap<String, GonePart>,
    #[serde(default)]
    pub(crate) prices: BTreeMap<String, i64>,
}

fn load_view(path: std::path::PathBuf) -> StoredView {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub(crate) fn state_view(cfg: &Config) -> StoredView {
    load_view(state_paths(cfg).0)
}

/// Alle Teile des letzten Laufs; fehlt die Datei (alte Installation), ist die
/// Struktur leer und die Oberfläche zeigt die Mail-Liste aus `state.json`.
pub(crate) fn tracked_view(cfg: &Config) -> StoredView {
    load_view(tracked_paths(cfg).0)
}

/// Unbestätigter Rückgang (`pending.json`) für die Anzeige.
pub(crate) fn pending_json(cfg: &Config) -> Option<serde_json::Value> {
    load_pending(&pending_path(cfg))
        .map(|p| json!({ "parts": p.parts, "total": p.total, "ts": p.ts }))
}

/// Änderungsmarke für `/api/state`: Pfad, Größe und mtime der Dateien, aus
/// denen die Antwort gebaut wird. Gleiche Marke = 304 statt 464 KB JSON.
pub(crate) fn state_etag(cfg: &Config) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in [
        state_paths(cfg).0,
        tracked_paths(cfg).0,
        specs_cache_path(cfg),
        pending_path(cfg),
    ] {
        path.hash(&mut h);
        if let Ok(meta) = std::fs::metadata(&path) {
            meta.len().hash(&mut h);
            meta.modified().ok().hash(&mut h);
        }
    }
    format!("\"{:016x}\"", h.finish())
}

pub(crate) fn state_json(cfg: &Config, req: &Request) -> Response {
    let etag = state_etag(cfg);
    if req.headers.get("if-none-match").is_some_and(|v| v == &etag) {
        return Response::not_modified(&etag);
    }
    // `tracked.json` ist der komplette Bestand; fehlt sie (alte Installation),
    // zeigt die Seite wie früher die Mail-Liste aus `state.json` (mit `gone`).
    let tracked = tracked_view(cfg);
    let mail_only = tracked.parts.is_empty();
    let view = if mail_only { state_view(cfg) } else { tracked };
    let specs = load_specs_cache(&specs_cache_path(cfg));
    let keywords = cfg.tracker.name_keywords.clone();
    let mail_parts = view
        .parts
        .iter()
        .filter(|(id, p)| part_matches(id, p, &specs, &keywords))
        .count();
    // Maschinentyp aus dem Cache des letzten Laufs (die Liste selbst liefert
    // beim reinen State-Abruf keine Teile).
    let products = load_products_cache(&products_cache_path(cfg));
    Response::json(&json!({
        "source": "state",
        "scope": if mail_only { "mail" } else { "all" },
        "checked_at": &view.last_check,
        "total_stock": view.total_stock,
        "part_count": view.part_count,
        "filter": &view.filter,
        "keywords": keywords,
        "mail_count": mail_parts,
        "currency": currency_symbol(&cfg.api.url),
        "parts": parts_json(
            &view.parts,
            &specs,
            PartPrices {
                sale: (!view.prices.is_empty()).then_some(&view.prices),
                list: None,
            },
            None,
            &view.gone,
            &keywords,
            &products,
        ),
        "pending": pending_json(cfg),
    }))
    .with_etag(&etag)
}

pub(crate) fn history_json(cfg: &Config, limit: usize) -> Response {
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
