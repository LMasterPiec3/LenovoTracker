//! Live-Stand im Speicher: ein Lenovo-Abruf, den die Seite in Millisekunden
//! liest statt bei jedem Aufruf zu warten.
//!
//! Der Hintergrund-Thread erneuert ihn im Minutentakt, solange eine Seite ihn
//! abruft, und pausiert nach `IDLE_AFTER` ohne Aufruf; der erste Aufruf weckt
//! ihn und holt bei Bedarf selbst. „Laden“ (`?fresh=1`) holt immer frisch.

use super::*;
use crate::{api::*, diff::*, state::*};
use anyhow::{anyhow, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Der Live-Abruf geht zu Lenovo und dauert ~0,5 s. Deshalb hält der Server
/// das fertige Ergebnis im Speicher und erneuert es im Hintergrund jede
/// Minute: die Seite liest es dann in wenigen Millisekunden aus. „Laden“
/// (`?fresh=1`) geht weiterhin sofort zu Lenovo.
pub(crate) struct LiveSnapshot {
    /// Wozu der Stand gehört (State-Ordner, API, Filter). Nach einer
    /// Config-Änderung wird er neu gebaut, statt weiter ausgeliefert.
    pub(crate) key: String,
    pub(crate) body: Vec<u8>,
    pub(crate) at: Instant,
}

pub(crate) static SNAPSHOT: Mutex<Option<LiveSnapshot>> = Mutex::new(None);
/// Nur ein Lenovo-Abruf gleichzeitig – sonst gingen Hintergrund und
/// Seitenaufruf parallel raus.
pub(crate) static FETCH_LOCK: Mutex<()> = Mutex::new(());
/// Wann zuletzt eine Seite den Stand geholt hat.
pub(crate) static LAST_HIT: Mutex<Option<Instant>> = Mutex::new(None);

/// Ein vergifteter Mutex ist hier harmlos: der Inhalt bleibt gültig.
pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn snapshot_key(cfg: &Config) -> String {
    format!(
        "{}|{}|{}|{}",
        cfg.paths.state_dir,
        cfg.api.url,
        cfg.tracker.commodity_keys.join(","),
        cfg.tracker.name_keywords.join(",")
    )
}

/// Der Stand aus dem Speicher – nur, wenn er zu dieser Config gehört und
/// höchstens `max_age` alt ist.
pub(crate) fn snapshot_live(cfg: &Config, max_age: Duration) -> Option<Vec<u8>> {
    let key = snapshot_key(cfg);
    let guard = lock(&SNAPSHOT);
    guard
        .as_ref()
        .filter(|s| s.key == key && s.at.elapsed() < max_age)
        .map(|s| s.body.clone())
}

/// Ist der Stand gefragt? Nach `idle` ohne Seitenaufruf nicht mehr.
pub(crate) fn aktiv(hit: Option<Instant>, idle: Duration) -> bool {
    hit.is_some_and(|t| t.elapsed() < idle)
}

pub(crate) fn note_hit() {
    *lock(&LAST_HIT) = Some(Instant::now());
}

pub(crate) fn watched() -> bool {
    aktiv(*lock(&LAST_HIT), IDLE_AFTER)
}

pub(crate) fn store_snapshot(cfg: &Config, body: Vec<u8>) {
    *lock(&SNAPSHOT) = Some(LiveSnapshot {
        key: snapshot_key(cfg),
        body,
        at: Instant::now(),
    });
}

pub(crate) fn refresh_snapshot(cfg: &Config) -> Result<Vec<u8>> {
    let body = fetch_live(cfg)?;
    store_snapshot(cfg, body.clone());
    Ok(body)
}

/// Hält den Stand frisch, solange eine Seite ihn abruft – ohne offene Seite
/// pausiert der Thread und geht nicht zu Lenovo. Der erste Aufruf weckt ihn
/// binnen einer Sekunde; ob dieser Aufruf selbst holen muss, entscheidet
/// `live_json` am Alter des Stands. Fehler bleiben im Log, der letzte gute
/// Stand wird weiter ausgeliefert.
pub(crate) fn spawn_live_refresher(config_path: String) {
    std::thread::spawn(move || loop {
        while !watched() {
            std::thread::sleep(IDLE_POLL);
        }
        match load_config(&config_path) {
            Ok(cfg) => {
                let _ticket = lock(&FETCH_LOCK);
                // Ein Aufruf der Seite kann gerade selbst geholt haben.
                if snapshot_live(&cfg, REFRESH_EVERY).is_none() {
                    match refresh_snapshot(&cfg) {
                        Ok(_) => log_info!("Web-UI: Live-Stand erneuert"),
                        Err(e) => log_warn!("Web-UI: Live-Stand nicht erneuert: {}", e),
                    }
                }
            }
            Err(e) => log_warn!("Web-UI: Live-Stand nicht erneuert: {}", e),
        }
        std::thread::sleep(REFRESH_EVERY);
    });
}

/// Die Seite bekommt den Stand aus dem Speicher (Millisekunden). `?fresh=1`
/// erneuert ihn sofort – das ist der „Laden“-Knopf.
pub(crate) fn live_json(cfg: &Config, req: &Request) -> Response {
    let fresh = req.query.split('&').any(|p| p == "fresh=1");
    // Vor dem Aufruf fragen: danach gilt die Seite ja als aktiv.
    let resume = !watched();
    // Jeder Abruf der Seite hält den Hintergrund-Abruf wach.
    note_hit();
    // Während die Seite läuft, überbrückt `STALE_OK` einen Lenovo-Aussetzer.
    // Nach der Pause gilt nur ein Stand aus dem letzten Takt – ein minutenalter
    // Bestand ist keine Antwort auf „was ist jetzt da?“, den holt der Aufruf
    // selbst (mit der Sperre unten, falls der Hintergrund schon dabei ist).
    let max_age = if resume { REFRESH_EVERY } else { STALE_OK };
    // „Laden“ wartet nicht auf einen hängenden Hintergrund-Abruf (Lenovo
    // antwortet gelegentlich erst nach Sekunden); ist der Platz frei, wird er
    // genommen, sonst läuft der Abruf parallel und der letzte Stand gewinnt.
    let _ticket = if fresh {
        FETCH_LOCK.try_lock().ok()
    } else {
        None
    };
    let result = if fresh {
        log_info!("Web-UI: Live-Abruf");
        refresh_snapshot(cfg)
    } else {
        cached_live_json(cfg, max_age)
    };
    match result {
        Ok(body) => Response::raw_json(body),
        Err(e) => Response::error(502, &e.to_string()),
    }
}

/// Stand aus dem Speicher; fehlt er oder ist er älter als `max_age`, wird er
/// neu geholt – mit nur einem Abruf gleichzeitig (während des Wartens kann ein
/// anderer Thread den Stand schon erneuert haben). Scheitert der Abruf, bleibt
/// der letzte gute Stand stehen, statt die Seite hängen zu lassen.
pub(crate) fn cached_live_json(cfg: &Config, max_age: Duration) -> Result<Vec<u8>> {
    if let Some(body) = snapshot_live(cfg, max_age) {
        return Ok(body);
    }
    let _ticket = lock(&FETCH_LOCK);
    if let Some(body) = snapshot_live(cfg, max_age) {
        return Ok(body);
    }
    log_info!("Web-UI: Live-Abruf");
    match refresh_snapshot(cfg) {
        Ok(body) => Ok(body),
        Err(e) => match snapshot_live(cfg, Duration::MAX) {
            Some(body) => {
                log_warn!("Web-UI: Live-Stand bleibt alt: {}", e);
                Ok(body)
            }
            None => Err(e),
        },
    }
}

/// Frischer API-Abruf ohne Mail und ohne State-Änderung – genau der Blick,
/// den die Oberfläche zeigen soll. Preise kommen mit, der State kennt sie nicht.
/// Ergebnis ist die fertige JSON-Antwort, damit sie im Speicher liegen kann.
pub(crate) fn fetch_live(cfg: &Config) -> Result<Vec<u8>> {
    let outcome = fetch_clearance_parts(cfg, &cfg.tracker.commodity_keys)
        .map_err(|e| anyhow!("Abruf fehlgeschlagen: {}", e))?;
    // Maschinentyp für den nächsten State-Abruf mitschreiben.
    if let Err(e) = save_products_cache(&products_cache_path(cfg), &outcome.products) {
        log_warn!("Notebook-Typ-Cache nicht schreibbar: {}", e);
    }
    // Alles zeigen, was die API liefert – der Mail-Filter gilt nur für Mails.
    let map = outcome.boards;
    let specs =
        crate::load_specs_for_all(cfg, &map, Some(load_specs_cache(&specs_cache_path(cfg))))
            .map_err(|e| anyhow!("Tech-Daten fehlgeschlagen: {}", e))?;
    let (total, count) = compute_totals(&map);
    // Vergleichsbasis für Delta/NEU: der komplette Bestand, sonst die
    // Mail-Liste aus `state.json` (alte Installation) – die wird nur im
    // Ausnahmefall gelesen.
    let tracked = tracked_view(cfg);
    let state = tracked.parts.is_empty().then(|| state_view(cfg));
    let base = state.as_ref().unwrap_or(&tracked);
    let keywords = cfg.tracker.name_keywords.clone();
    let mail_parts = map
        .iter()
        .filter(|(id, p)| part_matches(id, p, &specs, &keywords))
        .count();
    Ok(serde_json::to_vec(&json!({
        "source": "live",
        "scope": "all",
        "checked_at": now_iso(),
        "total_stock": total,
        "part_count": count,
        "filter": filter_label(&cfg.tracker.commodity_keys, &[]),
        "keywords": keywords,
        "mail_count": mail_parts,
        "currency": currency_symbol(&cfg.api.url),
        "parts": parts_json(
            &map,
            &specs,
            PartPrices {
                sale: Some(&outcome.prices),
                list: Some(&outcome.list_prices),
            },
            Some(&base.parts),
            &base.gone,
            &keywords,
            &outcome.products,
        ),
        "pending": pending_json(cfg),
    }))?)
}

/// Beide Preise je Teil in Cent: `sale` ist der Aktionspreis (bzw. der
/// Listenpreis, wenn es keine Aktion gibt), `list` der Est-Wert laut Lenovo.
#[derive(Default)]
pub(crate) struct PartPrices<'a> {
    pub(crate) sale: Option<&'a PriceMap>,
    pub(crate) list: Option<&'a PriceMap>,
}

/// Teile als JSON, nach Bestand sortiert. `base` (State) liefert das Delta je
/// Teil; für zurückgekehrte Teile zählt der letzte bekannte Bestand als Basis.
pub(crate) fn parts_json(
    parts: &BTreeMap<String, PartState>,
    specs: &BTreeMap<String, String>,
    prices: PartPrices<'_>,
    base: Option<&BTreeMap<String, PartState>>,
    gone: &BTreeMap<String, GonePart>,
    keywords: &[String],
    products: &BTreeMap<String, Vec<String>>,
) -> Vec<serde_json::Value> {
    let mut rows: Vec<(&String, &PartState)> = parts.iter().collect();
    rows.sort_unstable_by(|a, b| b.1.s.cmp(&a.1.s).then_with(|| a.0.cmp(b.0)));
    rows.into_iter()
        .map(|(id, p)| {
            let line = specs.get(id).map(String::as_str).unwrap_or("");
            let f = facets(line);
            // Maschinentyp („20KU") aus der Teileliste.
            let prod = products.get(id).cloned().unwrap_or_default();
            // Ohne Vergleichsbasis (State-Ansicht) und für neue Teile gibt es
            // kein Delta – dort sagt `new` alles.
            let delta = base.and_then(|b| match b.get(id) {
                Some(old) => Some(p.s - old.s),
                None => gone.get(id).map(|g| p.s - g.s),
            });
            // Weder im gespeicherten Bestand noch als „weg“ vermerkt: das Teil
            // ist wirklich neu in der Liste – nicht nur eine Bestandsänderung.
            let new = base.is_some_and(|b| !b.contains_key(id) && !gone.contains_key(id));
            json!({
                "id": id,
                "name": p.n,
                "stock": p.s,
                "specs": line,
                "price": prices.sale.and_then(|m| m.get(id).copied()),
                // Est-Wert laut Lenovo (Cent, Ganzzahl) – die Karte zeigt ihn
                // durchgestrichen vor dem Aktionspreis.
                "list_price": prices.list.and_then(|m| m.get(id).copied()),
                "delta": delta,
                "new": new,
                "back": gone.contains_key(id),
                // Erfüllt das Teil die Mail-Keywords? Die Oberfläche kann so
                // zwischen „allem“ und der Mail-Liste umschalten.
                "mail": part_matches(id, p, specs, keywords),
                // Liste: kleines, gecachtes Vorschaubild (das Original ist ~275 KB);
                // der Bild-Dialog baut die Original-URLs selbst aus der ID.
                // `?v=2` verwirft die alten 404-Antworten im Browser-Cache
                // (Platzhalter blieben sonst bis zu einer Stunde stehen).
                "image": format!("/api/thumb/{id}?v=2"),
                // Maschinentyps („20KU") – sagt, zu welchem Notebook das Board gehört.
                "products": prod,
                "cpu": f.cpu,
                "cpu_brand": f.cpu_brand,
                "cpu_series": f.cpu_series,
                "cpu_gen": f.cpu_gen,
                "cpu_score": f.perf.1,
                "cpu_bench": f.cpu_bench.map(|(s, m)| [s, m]),
                "cpu_src": if f.cpu_bench.is_none() { "est" } else { f.cpu_bench_src },
                "gpu_score": f.gpu_bench,
                "gpu_src": f.gpu_bench_src,
                "gpu_igpu": f.gpu_bench_name,
                "gpu_igpu_short": crate::api::igpu_short(f.gpu_bench_name),
                "gpu_igpu_note": f.gpu_bench_note,
                "ram": f.ram,
                "gpu": f.gpu,
                "gpu_kind": f.gpu_kind,
                "gpu_family": f.gpu_family,
                "perf": [f.perf.0, f.perf.1, f.perf.2],
            })
        })
        .collect()
}
