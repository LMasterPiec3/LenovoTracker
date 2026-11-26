//! Schutz gegen sprunghafte, unvollständige API-Antworten.
//!
//! Zwei Ebenen:
//! 1. Ein großer Rückgang (viele Teile verschwinden) wird erst übernommen, wenn
//!    der nächste Lauf ihn bestätigt. Eine einzelne unvollständige Antwort kann
//!    so nicht die Baseline – und damit alle künftigen Mails – verschieben.
//! 2. Verschwundene Teile bleiben mit ihrem letzten Bestand als `gone` im State.
//!    Kommen sie zurück, ist das kein Anstieg (keine Mail), sondern nur wieder
//!    derselbe Bestand.

use crate::config::*;
use crate::diff::*;
use crate::util::*;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Ab hier gilt ein Rückgang als verdächtig: mindestens 3 Teile und mindestens
/// 20 % des bisher bekannten Bestands sind aus der Antwort verschwunden.
const MIN_MISSING: usize = 3;
const MISSING_PERCENT: usize = 20;

pub fn suspicious_drop(old_len: usize, new_len: usize) -> bool {
    let missing = old_len.saturating_sub(new_len);
    missing >= MIN_MISSING && missing * 100 >= old_len * MISSING_PERCENT
}

/// Marker für einen noch unbestätigten Rückgang (state_dir/pending.json).
#[derive(Serialize, Deserialize, Default)]
pub struct Pending {
    pub parts: usize,
    pub total: i64,
    pub ts: String,
}

pub fn pending_path(cfg: &Config) -> PathBuf {
    PathBuf::from(&cfg.paths.state_dir).join("pending.json")
}

pub fn load_pending(path: &Path) -> Option<Pending> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

pub fn save_pending(path: &Path, p: &Pending) -> Result<()> {
    write_atomic(path, &serde_json::to_vec(p)?)?;
    Ok(())
}

pub fn clear_pending(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Neues `gone`-Set: gerade fehlende Teile mit ihrem letzten Bestand merken,
/// zurückgekehrte vergessen. Bereits gemerkte Teile behalten ihren Zeitstempel
/// (sie waren schon vorher weg).
pub fn next_gone(
    old: &BTreeMap<String, PartState>,
    new: &BTreeMap<String, PartState>,
    was: &BTreeMap<String, GonePart>,
    ts: &str,
) -> BTreeMap<String, GonePart> {
    let mut out: BTreeMap<String, GonePart> = BTreeMap::new();
    for (id, p) in old {
        if !new.contains_key(id) {
            out.insert(
                id.clone(),
                GonePart {
                    s: p.s,
                    n: p.n.clone(),
                    t: ts.to_string(),
                },
            );
        }
    }
    for (id, g) in was {
        if !new.contains_key(id) {
            out.entry(id.clone()).or_insert_with(|| g.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(items: &[(&str, i64)]) -> BTreeMap<String, PartState> {
        items
            .iter()
            .map(|(id, s)| {
                (
                    id.to_string(),
                    PartState {
                        s: *s,
                        n: format!("Board {}", id),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn test_suspicious_drop_threshold() {
        assert!(!suspicious_drop(0, 0));
        assert!(!suspicious_drop(50, 50));
        assert!(!suspicious_drop(50, 60)); // Wachstum ist nie verdächtig
        assert!(!suspicious_drop(50, 48)); // 2 fehlende Teile → normal
        assert!(!suspicious_drop(50, 41)); // 9 fehlen = 18 % → unter der Schwelle
        assert!(suspicious_drop(50, 38)); // 12 fehlen = 24 % → verdächtig
        assert!(suspicious_drop(4, 1)); // 3 von 4 fehlen
        assert!(suspicious_drop(10, 0)); // leere Antwort
    }

    #[test]
    fn test_pending_roundtrip() {
        let dir = std::env::temp_dir().join(format!("lt-guard-{}", std::process::id()));
        let path = dir.join("pending.json");
        assert!(load_pending(&path).is_none());
        save_pending(
            &path,
            &Pending {
                parts: 38,
                total: 166,
                ts: "2026-11-26T08:10:00Z".into(),
            },
        )
        .unwrap();
        let p = load_pending(&path).expect("pending");
        assert_eq!(
            (p.parts, p.total, p.ts.as_str()),
            (38, 166, "2026-11-26T08:10:00Z")
        );
        clear_pending(&path);
        assert!(load_pending(&path).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_next_gone_remembers_and_forgets() {
        let old = map(&[("A", 2), ("B", 5)]);
        let new = map(&[("A", 2)]);
        let first = next_gone(&old, &new, &BTreeMap::new(), "2026-11-26T08:10:00Z");
        assert_eq!(first.len(), 1);
        assert_eq!(first["B"].s, 5);
        assert_eq!(first["B"].n, "Board B");
        // bleibt weg → Zeitstempel bleibt der erste
        let second = next_gone(&new, &new, &first, "2026-11-26T08:20:00Z");
        assert_eq!(second["B"].t, "2026-11-26T08:10:00Z");
        // kommt zurück → vergessen
        let back = map(&[("A", 2), ("B", 5)]);
        assert!(next_gone(&new, &back, &second, "2026-11-26T08:30:00Z").is_empty());
    }

    #[test]
    fn test_returning_part_is_not_an_increase() {
        // Genau der gefährliche Fall: eine unvollständige Antwort hat B
        // verschwinden lassen, B kommt mit demselben Bestand zurück.
        let old = map(&[("A", 2)]);
        let mut gone = BTreeMap::new();
        gone.insert(
            "B".to_string(),
            GonePart {
                s: 5,
                n: "Board B".into(),
                t: "2026-11-26T08:10:00Z".into(),
            },
        );
        let back = map(&[("A", 2), ("B", 5)]);
        let diff = build_diff(&old, &back, &gone);
        assert!(
            diff.increased.is_empty(),
            "Rückkehr darf keine Anstiegs-Mail auslösen"
        );
        assert!(diff.new_parts.is_empty(), "Rückkehr ist kein neues Teil");
        assert!(diff.changed, "der State muss B wieder aufnehmen");
        assert_eq!(diff.delta(), 0);
        // Echter Aufbau über dem alten Stand bleibt ein Anstieg (Basis 5).
        let more = map(&[("A", 2), ("B", 8)]);
        let diff = build_diff(&old, &more, &gone);
        assert_eq!(diff.increased.len(), 1);
        assert_eq!((diff.increased[0].old, diff.increased[0].new), (5, 8));
    }
}
