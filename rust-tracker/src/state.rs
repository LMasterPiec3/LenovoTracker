//! state.json/Verlauf + FailState (Störungszähler) – atomares Schreiben.

use crate::config::*;
use crate::diff::*;
use crate::util::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    kw: Option<BTreeMap<String, KwStat>>,
    #[serde(default)]
    filter: Option<String>,
    /// Verschwundene Teile mit letztem Bestand (siehe `guard::next_gone`).
    #[serde(default)]
    gone: BTreeMap<String, GonePart>,
    /// Stückpreise in Cent (nur Anzeige; ein Preiswechsel schreibt den State
    /// neu, löst aber keine Mail aus).
    #[serde(default)]
    prices: BTreeMap<String, i64>,
}

/// Geladener State als benannte Felder (statt eines 4er-Tupels).
pub struct LoadedState {
    pub parts: BTreeMap<String, PartState>,
    pub kw: BTreeMap<String, KwStat>,
    pub gone: BTreeMap<String, GonePart>,
    pub prices: BTreeMap<String, i64>,
}

/// Ein Stand, der geschrieben werden soll – bündelt die neun Werte, damit
/// `save_state`/`persist` lesbar bleiben.
pub struct Snapshot<'a> {
    pub ts: &'a str,
    pub total: i64,
    pub count: usize,
    pub filter: &'a str,
    pub kw: &'a BTreeMap<String, KwStat>,
    pub parts: &'a BTreeMap<String, PartState>,
    pub gone: &'a BTreeMap<String, GonePart>,
    pub prices: &'a BTreeMap<String, i64>,
}

pub fn load_state(path: &Path, expected_filter: &str) -> Option<LoadedState> {
    // Ganze Datei in den Speicher + `from_str`: derselbe Messbefund wie beim
    // API-Body (≈3× schneller als `from_reader`, ein einmaliger UTF-8-Check
    // statt Validierung pro String/Token). state.json ist ~300 KB.
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            log_error!(
                "state.json nicht lesbar ({}) – Baseline wird neu gesetzt",
                e
            );
            return None;
        }
    };
    match serde_json::from_str::<StateFileIn>(&text) {
        Ok(sf) => {
            if sf.filter.as_deref() != Some(expected_filter) {
                log_info!(
                    "Filter hat sich geändert (state: {:?} → jetzt {:?}) – Baseline wird neu gesetzt",
                    sf.filter.unwrap_or_else(|| "(alt, ohne Filtermarkierung)".into()),
                    expected_filter
                );
                return None;
            }
            let kw = match sf.kw {
                Some(k) => k,
                None => {
                    log_info!(
                        "State ohne Keyword-Daten (altes Format) – Baseline wird neu gesetzt"
                    );
                    return None;
                }
            };
            Some(LoadedState {
                parts: sf.parts,
                kw,
                gone: sf.gone,
                prices: sf.prices,
            })
        }
        Err(e) => {
            log_error!(
                "state.json ist beschädigt/unerwartetes Format ({}) – Baseline wird neu gesetzt",
                e
            );
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
    kw: &'a BTreeMap<String, KwStat>,
    parts: &'a BTreeMap<String, PartState>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    gone: &'a BTreeMap<String, GonePart>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    prices: &'a BTreeMap<String, i64>,
}

/// Schreibt JSON atomar: erst `<path>.tmp`, dann `rename` – ein Abbruch
/// hinterlässt nie eine halbe Datei. Alle State-Dateien laufen hier durch.
fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    {
        let f = fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::with_capacity(64 * 1024, f);
        serde_json::to_writer(&mut w, value)?;
        w.flush()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_state(state_path: &Path, snap: &Snapshot<'_>) -> Result<()> {
    write_json_atomic(
        state_path,
        &StateFileOut {
            last_check: snap.ts,
            total_stock: snap.total,
            part_count: snap.count,
            filter: snap.filter,
            kw: snap.kw,
            parts: snap.parts,
            gone: snap.gone,
            prices: snap.prices,
        },
    )
}

/// save_state + append_history als Einheit – der Aufruf ist an drei Stellen
/// identisch gekoppelt.
pub fn persist(state_path: &Path, history_path: &Path, snap: &Snapshot<'_>) -> Result<()> {
    save_state(state_path, snap)?;
    append_history(history_path, snap.ts, snap.total, snap.count)
}

// ---------------------------------------------------------------------------
// Kompletter Bestand für die Web-UI
// ---------------------------------------------------------------------------

/// `tracked.json` + `tracked-history.csv`: **alle** Teile des Abrufs, ohne den
/// Mail-Keyword-Filter. Getrennt vom Mail-State, damit die Oberfläche alles
/// zeigt, während die Mails weiter nur die abonnierten Teile melden.
pub fn tracked_paths(cfg: &Config) -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(&cfg.paths.state_dir);
    (dir.join("tracked.json"), dir.join("tracked-history.csv"))
}

#[derive(Serialize)]
struct TrackedFileOut<'a> {
    last_check: &'a str,
    filter: &'a str,
    total_stock: i64,
    part_count: usize,
    parts: &'a BTreeMap<String, PartState>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    prices: &'a BTreeMap<String, i64>,
}

pub fn save_tracked(
    path: &Path,
    ts: &str,
    filter: &str,
    total: i64,
    count: usize,
    parts: &BTreeMap<String, PartState>,
    prices: &BTreeMap<String, i64>,
) -> Result<()> {
    write_json_atomic(
        path,
        &TrackedFileOut {
            last_check: ts,
            filter,
            total_stock: total,
            part_count: count,
            parts,
            prices,
        },
    )
}

/// Eine Zeile Verlauf fortschreiben (`history.csv`, `tracked-history.csv`).
pub fn append_history(history_path: &Path, ts: &str, total: i64, count: usize) -> Result<()> {
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let is_new = !history_path.exists();
    let mut fh = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path)?;
    if is_new {
        writeln!(fh, "timestamp,total_stock,part_count")?;
    }
    writeln!(fh, "{},{},{}", ts, total, count)?;
    Ok(())
}

pub fn state_paths(cfg: &Config) -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(&cfg.paths.state_dir);
    (dir.join("state.json"), dir.join("history.csv"))
}

/// Specs-Cache (id → aufbereitete Specs-Zeile), getrennt vom State: ein
/// Filterwechsel setzt die Baseline neu, die Hardware-Daten bleiben aber
/// erhalten und müssen nicht erneut per Batch-POST geladen werden.
/// Maschinentyps je Teil aus der Teileliste („20KU"). Wird je Lauf
/// fortgeschrieben, damit die Startseite den Notebook-Typ ohne Live-Abruf zeigt.
pub fn products_cache_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.paths.state_dir).join("part-products.json")
}

pub fn load_products_cache(path: &Path) -> BTreeMap<String, Vec<String>> {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Nur neue/geänderte Zuordnungen schreiben – die Datei wächst langsam.
pub fn save_products_cache(path: &Path, fresh: &BTreeMap<String, Vec<String>>) -> Result<()> {
    if fresh.is_empty() {
        return Ok(());
    }
    let mut all = load_products_cache(path);
    let mut changed = false;
    for (id, types) in fresh {
        if all.get(id) != Some(types) {
            all.insert(id.clone(), types.clone());
            changed = true;
        }
    }
    if changed {
        write_json_atomic(path, &all)?;
    }
    Ok(())
}

pub fn specs_cache_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.paths.state_dir).join("specs-cache.json")
}

pub fn load_specs_cache(path: &Path) -> BTreeMap<String, String> {
    // Wie beim State: erst in den Speicher, dann `from_str` (schnellerer Pfad).
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return BTreeMap::new(),
    };
    serde_json::from_str(&text).unwrap_or_else(|e| {
        log_warn!("Specs-Cache unlesbar ({}) – wird neu aufgebaut", e);
        BTreeMap::new()
    })
}

pub fn save_specs_cache(path: &Path, specs: &BTreeMap<String, String>) -> Result<()> {
    write_json_atomic(path, specs)
}

/// Rückgabe: (exit_code)
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug)]
pub struct FailState {
    pub failures: u32,
    pub alerted: bool,
}

/// Konsekutive API-Fehlschlaege ueber Laeufe hinweg zaehlen (State-File).
/// 3 -> Stoerungsmail (einmal pro Strich), Erholung nach gemeldeter Stoerung
/// -> Recovery-Mail. Dry-Runs und Mail-Sendefehler zaehlen nicht.
pub fn fail_state_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.paths.state_dir).join("failstate.json")
}

pub fn load_fail_state(path: &Path) -> FailState {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return FailState::default(),
    };
    serde_json::from_reader(std::io::BufReader::with_capacity(8 * 1024, file)).unwrap_or_default()
}

pub fn save_fail_state(path: &Path, st: &FailState) -> Result<()> {
    write_json_atomic(path, st)
}
