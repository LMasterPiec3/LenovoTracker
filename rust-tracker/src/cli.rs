//! Kommandozeile: Argumente lesen, Usage-Text, Moduswahl.
//!
//! `cli::real_main` ist der Einstieg; `main` in `main.rs` gibt nur den
//! Exit-Code weiter.

use crate::{bench::*, config::*, cycle::*, mail::*, report::*, util::*, webui};

pub(crate) fn print_usage() {
    println!("Lenovo Clearance-Sale Mainboard-Tracker (Rust)");
    println!(
        "  --config <pfad>   Config-Pfad (Standard: {})",
        DEFAULT_CONFIG_PATH
    );
    println!("  --once            einmal prüfen und beenden");
    println!("  --dry-run         einmal abrufen und zusammenfassen (kein State, keine Mail)");
    println!("  --test-mail       Testmail mit aktuellem Bestand senden");
    println!("  --report          Bestands-Report aus dem State anzeigen (ohne Netz)");
    println!("  --unknowns        CPUs/GPUs/iGPUs listen, die in den Tabellen fehlen");
    println!("  --serve           Web-UI starten (Bestand live, Verlauf, Config)");
    println!(
        "  --bench-parse <body>  gespeicherten API-Body parsen (Parse-Zeit/Peak-RAM, ohne Netz)"
    );
    println!("  --iterations <n>  Läufe für --bench-parse (Standard: 5, erster Lauf = Warmup)");
}

/// Menschenlesbarer Bestands-Report aus dem State (kein Netz-Zugriff).
pub(crate) fn real_main() -> i32 {
    let mut config_path = DEFAULT_CONFIG_PATH.to_string();
    let mut once = false;
    let mut dry_run = false;
    let mut test_mail = false;
    let mut report = false;
    let mut unknowns = false;
    let mut serve = false;
    let mut bench_path: Option<String> = None;
    let mut iterations: usize = 5;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => match iter.next() {
                Some(v) => config_path = v,
                None => {
                    print_usage();
                    return 1;
                }
            },
            "--once" => once = true,
            "--dry-run" => dry_run = true,
            "--test-mail" => test_mail = true,
            "--report" => report = true,
            "--unknowns" => unknowns = true,
            "--serve" => serve = true,
            "--bench-parse" => match iter.next() {
                Some(v) => bench_path = Some(v),
                None => {
                    print_usage();
                    return 1;
                }
            },
            "--iterations" => match iter.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(n) if n >= 1 => iterations = n,
                _ => {
                    log_error!("--iterations erwartet eine Zahl ≥ 1");
                    return 1;
                }
            },
            other => {
                println!("Unbekanntes Argument: {}", other);
                print_usage();
                return 1;
            }
        }
    }

    let cfg = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            log_error!("Config-Fehler: {}", e);
            return 1;
        }
    };

    if serve {
        return webui::run(&config_path);
    }
    if let Some(path) = bench_path {
        return cmd_bench_parse(&cfg, &path, iterations);
    }
    if dry_run {
        return run_once(&cfg, true);
    }
    if report {
        return cmd_report_local(&cfg);
    }
    if unknowns {
        return crate::report::cmd_unknowns(&cfg);
    }
    if test_mail {
        return match send_test_mail(&cfg) {
            Ok(_) => 0,
            Err(e) => {
                log_error!("Testmail fehlgeschlagen: {}", e);
                2
            }
        };
    }
    if once {
        return run_once(&cfg, false);
    }
    println!("Kein Modus angegeben – die Rust-Version läuft per cron (siehe README).");
    print_usage();
    1
}
