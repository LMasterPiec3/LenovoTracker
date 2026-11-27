//! Lenovo Clearance-Sale Mainboard-Tracker (Rust-Variante, minimal RAM).
//!
//! Aufteilung der Quellen (jede Datei eine Aufgabe):
//!   util      Helfer, Makros (log_*), Zeit/Geld-Format, atomares Schreiben
//!   config    Config-Struktur, TOML-Parser, Pfade
//!   api       Abruf über curl-impersonate + Streaming-Parser der Antwort
//!   state     state.json, tracked.json, Verlauf, Specs-/Produkt-Cache
//!   diff      Keywords/Filter, Summen, Diff gegen den State
//!   guard     Rückgangs-Sperre (pending) und Fehlerzähler
//!   mail      SMTP-Versand, Text-/HTML-Mail, Testmail
//!   report    --report und --unknowns
//!   thumb     Vorschaubilder (Cache, Download, Verkleinern)
//!   cycle     ein Lauf: `run_once` und seine Helfer (--once/--dry-run)
//!   cli       Argumente, Usage, Moduswahl (Einstieg: `cli::real_main`)
//!   bench     --bench-parse (Parse-Zeit, Peak-RAM, Prüfsumme)
//!   webui     Web-UI: HTTP-Server, Bestand/Verlauf, Live-Stand, Config
//!   tests     Querschnitts-Tests (nur `cargo test`)
//!
//! Cron ruft alle 10 Minuten `--once` auf; `--serve` startet die Web-UI.

#[macro_use]
mod util;
mod api;
mod bench;
mod cli;
mod config;
mod cycle;
mod diff;
mod guard;
mod mail;
mod report;
mod state;
mod thumb;
mod webui;

// Die Module hängen über `crate::…` voneinander ab; die Re-Exports machen ihre
// Items im Wurzelmodul verfügbar (kein zusätzlicher Glob-Import nötig).
pub use api::*;
pub(crate) use cli::*;
pub use config::*;
pub(crate) use cycle::*;
pub use diff::*;
pub use guard::*;
pub use mail::*;
pub use report::*;
pub use state::*;
pub use util::*;

fn main() {
    std::process::exit(real_main());
}

#[cfg(test)]
mod tests;
