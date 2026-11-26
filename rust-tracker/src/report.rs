//! Bestands-Report aus dem State (ohne Netz).

use crate::api::facets;
use crate::config::*;
use crate::diff::*;
use crate::state::*;
use crate::util::*;
use std::collections::BTreeMap;
use std::fmt::Write;

pub fn format_report(map: &BTreeMap<String, PartState>, kw: &BTreeMap<String, KwStat>) -> String {
    let total: i64 = map.values().map(|p| p.s).sum();
    let count = map.len();
    // Direkt in den Ausgabe-String schreiben: kein Zwischen-String pro Zeile.
    let mut out = String::with_capacity(64 + count * 48);
    let _ = write!(
        out,
        "Bestand: {} Stück über {} Teile\n\n",
        fmt_de(total),
        count
    );
    if !kw.is_empty() {
        out.push_str("Keyword-Bestand:\n");
        for (k, st) in kw.iter() {
            let _ = writeln!(
                out,
                "  {:<10} {:>6} Stück / {:>3} Teile",
                k,
                fmt_de(st.stock),
                st.parts
            );
        }
        out.push('\n');
    }
    out.push_str("Teile (nach Bestand sortiert):\n");
    let mut v: Vec<(&String, &PartState)> = map.iter().collect();
    // Tie-Break auf der ID → Reihenfolge ist auch unstabil eindeutig.
    v.sort_unstable_by(|a, b| b.1.s.cmp(&a.1.s).then(a.0.cmp(b.0)));
    for (id, p) in v {
        let _ = writeln!(out, "  {:>6}× {}  {}", fmt_de(p.s), id, p.n);
    }
    out
}

pub fn cmd_report_local(cfg: &Config) -> i32 {
    let fp = filter_fingerprint(&cfg.tracker.commodity_keys, &cfg.tracker.name_keywords);
    let Some(LoadedState { parts, kw, .. }) = load_state(&state_paths(cfg).0, &fp) else {
        log_error!("Keine Baseline vorhanden – erst einen Cron-Lauf oder --once ausführen");
        return 1;
    };
    print!("{}", format_report(&parts, &kw));
    0
}

/// Modelle, die in keiner Tabelle stehen – je Modell die Zahl der Teile.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Unknowns {
    pub cpu: BTreeMap<String, usize>,
    pub gpu: BTreeMap<String, usize>,
    pub igpu: BTreeMap<String, usize>,
}

impl Unknowns {
    pub fn is_empty(&self) -> bool {
        self.cpu.is_empty() && self.gpu.is_empty() && self.igpu.is_empty()
    }
}

/// Fehlende Modelle aus den Spec-Zeilen sammeln (ohne Ausgabe, damit testbar).
pub fn collect_unknowns(specs: &BTreeMap<String, String>) -> Unknowns {
    let mut u = Unknowns::default();
    for line in specs.values() {
        if line.trim().is_empty() {
            continue;
        }
        let f = facets(line);
        if !f.cpu.is_empty() && f.cpu_bench.is_none() {
            *u.cpu.entry(f.cpu.clone()).or_default() += 1;
        }
        // „diskret"/„integriert" ohne Modellnamen sind Platzhalter, keine
        // unbekannten Karten.
        let gpu = f.gpu.trim();
        let platzhalter = gpu.is_empty()
            || gpu.eq_ignore_ascii_case("diskret")
            || gpu.eq_ignore_ascii_case("integriert");
        if f.gpu_bench.is_none() && !platzhalter {
            *u.gpu.entry(f.gpu.clone()).or_default() += 1;
        } else if f.gpu_kind == "integriert" && f.gpu_bench.is_none() && !f.cpu.is_empty() {
            *u.igpu.entry(f.cpu.clone()).or_default() += 1;
        }
    }
    u
}

/// Was den Tabellen fehlt: CPUs ohne Benchmark-Zeile, GPUs ohne Score-Zeile
/// und UMA-Teile ohne iGPU-Zuordnung. Grundlage ist der Specs-Cache
/// (`--once` oder die Web-UI füllt ihn).
pub fn cmd_unknowns(cfg: &Config) -> i32 {
    let specs = load_specs_cache(&specs_cache_path(cfg));
    if specs.is_empty() {
        log_error!("Kein Specs-Cache – erst `--once` oder die Web-UI laden lassen");
        return 1;
    }
    let u = collect_unknowns(&specs);
    println!("Unbekannte Hardware in {} spec-Zeilen\n", specs.len());
    for (titel, gruppe) in [
        ("CPU ohne Benchmark-Zeile (cpu-scores.tsv)", &u.cpu),
        ("GPU ohne Score-Zeile (gpu-scores.tsv)", &u.gpu),
        ("iGPU ohne Zuordnung (cpu-igpu.tsv)", &u.igpu),
    ] {
        println!("{}: {}", titel, gruppe.len());
        for (name, n) in gruppe {
            println!("  {:>4}× {}", n, name);
        }
        println!();
    }
    if u.is_empty() {
        println!("Alles abgedeckt – kein Handlungsbedarf.");
    } else {
        println!("Ergänzen in src/:");
        println!("  cpu-scores.tsv  Serie:Modell<TAB>Name<TAB>Single<TAB>Multi<TAB>Quelle");
        println!("  gpu-scores.tsv  Schlüssel<TAB>Name<TAB>TimeSpy<TAB>ts|pm3d");
        println!("  cpu-igpu.tsv    CPU-Schlüssel<TAB>iGPU<TAB>TimeSpy<TAB>ts|pm3d|est<TAB>GPU-Schlüssel");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(lines: &[&str]) -> BTreeMap<String, String> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| (format!("P{}", i), l.to_string()))
            .collect()
    }

    #[test]
    fn unknowns_trennt_abgedeckt_von_fehlend() {
        let u = collect_unknowns(&specs(&[
            "CPU: Intel Core i7-12800H | GPU: integriert",
            "CPU: Intel Core i7-14700HX | GPU: integriert",
            "CPU: Intel Core Ultra 7 258V | GPU: integriert",
            "CPU: Intel N100 | GPU: integriert",
            "CPU: AMD Ryzen AI 9 HX 370 | GPU: integriert",
            "CPU: QualcommSnapdragonSC7280 | GPU: integriert",
            "CPU: Intel Core i5-1135G7 | GPU: NVIDIA GeForce RTX 9090 (diskret)",
            "CPU: Intel Core i7-8565U | GPU: diskret",
        ]));
        // Abgedeckte Modelle tauchen nicht auf.
        for name in [
            "Intel Core i7-12800H",
            "Intel Core i7-14700HX",
            "Intel Core Ultra 7 258V",
            "Intel N100",
            "AMD Ryzen AI 9 HX 370",
        ] {
            assert!(!u.cpu.contains_key(name), "{name} sollte abgedeckt sein");
            assert!(!u.igpu.contains_key(name), "{name} sollte eine iGPU haben");
        }
        // Fehlende Modelle werden genau einmal gezählt.
        assert_eq!(u.cpu.get("QualcommSnapdragonSC7280"), Some(&1));
        assert_eq!(u.igpu.get("QualcommSnapdragonSC7280"), Some(&1));
        assert_eq!(u.gpu.get("NVIDIA GeForce RTX 9090 (diskret)"), Some(&1));
        // „diskret" ohne Modell ist ein Platzhalter, keine unbekannte Karte.
        assert!(!u.gpu.keys().any(|g| g.eq_ignore_ascii_case("diskret")));
        assert_eq!(u.cpu.len(), 1);
        assert_eq!(u.gpu.len(), 1);
        assert_eq!(u.igpu.len(), 1);
    }

    #[test]
    fn unknowns_leer_wenn_alles_bekannt_ist() {
        let u = collect_unknowns(&specs(&[
            "CPU: Intel Core i5-1135G7 | GPU: integriert",
            "CPU: AMD Ryzen 7 4800U | GPU: integriert",
        ]));
        assert!(u.is_empty(), "{u:?}");
    }
}
