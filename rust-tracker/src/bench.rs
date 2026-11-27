//! `--bench-parse`: einen gespeicherten API-Body mehrfach über denselben
//! Streaming-Pfad parsen und Parse-Zeit, Peak-RAM und eine Prüfsumme melden
//! (Prüfsumme muss bei Optimierungen gleich bleiben).

use crate::{api::*, config::*, diff::*, util::*};
use std::collections::BTreeMap;

/// FNV-1a über id/name/stock – erkennt, wenn eine Optimierung das Ergebnis ändert.
pub(crate) fn boards_checksum(map: &BTreeMap<String, PartState>) -> u64 {
    fn feed(hash: &mut u64, bytes: &[u8]) {
        for &b in bytes {
            *hash ^= b as u64;
            *hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (id, p) in map {
        feed(&mut h, id.as_bytes());
        feed(&mut h, p.n.as_bytes());
        feed(&mut h, &p.s.to_le_bytes());
    }
    h
}

/// Parst einen gespeicherten API-Body mehrfach über denselben Streaming-Pfad
/// wie `fetch_once` (64-KB-Buffer, FilterSeed) und meldet Parse-Zeit, Peak-RSS
/// und eine Prüfsumme des Ergebnisses (Optimierungen müssen sie erhalten).
pub(crate) fn cmd_bench_parse(cfg: &Config, path: &str, iterations: usize) -> i32 {
    let bytes = match std::fs::metadata(path) {
        Ok(m) => m.len() as usize,
        Err(e) => {
            log_error!("Body nicht lesbar: {} ({})", path, e);
            log_info!(
                "Fixture erzeugen: python3 rust-tracker/testdata/gen_body.py {}",
                path
            );
            return 1;
        }
    };
    let keys = &cfg.tracker.commodity_keys;
    let keywords = &cfg.tracker.name_keywords;
    let (rss_before, _) = rss_kib();
    let mut times: Vec<std::time::Duration> = Vec::with_capacity(iterations);
    let mut signature: Option<(usize, i64, u64)> = None;
    let mut total_in_body = 0usize;
    for _ in 0..iterations.max(1) {
        // Datei je Lauf neu öffnen: der Body liegt im Page-Cache, nicht im Heap.
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) => {
                log_error!("Body nicht lesbar: {} ({})", path, e);
                return 1;
            }
        };
        let started = std::time::Instant::now();
        let outcome = match parse_clearance_from(file, keys) {
            Ok(o) => o,
            Err(e) => {
                log_error!("Parse fehlgeschlagen: {}", e);
                return 2;
            }
        };
        times.push(started.elapsed());
        total_in_body = outcome.total_parts;
        let (total, count) = compute_totals(&outcome.boards);
        let sig = (count, total, boards_checksum(&outcome.boards));
        match signature {
            None => signature = Some(sig),
            Some(s) if s != sig => {
                log_error!("Ergebnis weicht zwischen den Läufen ab – Benchmark unbrauchbar");
                return 2;
            }
            _ => {}
        }
    }
    if times.len() > 1 {
        times.remove(0); // erster Lauf: Page-Cache/Allocator-Warmup
    }
    let (_, rss_peak) = rss_kib();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    let min = sorted[0].as_secs_f64();
    let median = sorted[sorted.len() / 2].as_secs_f64();
    let mean = times.iter().map(|d| d.as_secs_f64()).sum::<f64>() / times.len() as f64;
    let (count, total, checksum) = signature.unwrap_or((0, 0, 0));
    log_info!(
        "BENCH {} – {} Bytes, {} Teile in data, {} nach Filter",
        path,
        bytes,
        total_in_body,
        count
    );
    log_info!(
        "  Filter: {:?} · Keywords (nach Specs): {:?}",
        keys,
        keywords
    );
    log_info!(
        "  Parse ({} Läufe nach Warmup): min {:.2} ms · median {:.2} ms · mittel {:.2} ms",
        times.len(),
        min * 1000.0,
        median * 1000.0,
        mean * 1000.0
    );
    log_info!(
        "  Ergebnis: {} Teile, {} Stück, Prüfsumme {:016x}",
        count,
        fmt_de(total),
        checksum
    );
    match (rss_before, rss_peak) {
        (Some(before), Some(peak)) => log_info!(
            "  RAM: {:.1} MiB vor dem Parse · {:.1} MiB Peak (High-Water-Mark)",
            before as f64 / 1024.0,
            peak as f64 / 1024.0
        ),
        _ => log_info!("  RAM: auf dieser Plattform nicht messbar (/proc bzw. psapi fehlt)"),
    }
    0
}
