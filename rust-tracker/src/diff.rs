//! Filter, Summen, Diff, Keyword-Statistik und die Mail-Trigger-Regel.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Filter, Summe, Diff
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartState {
    pub s: i64,
    pub n: String,
}

/// Teil, das aus der API verschwunden ist – mit letztem bekannten Bestand und
/// Zeitpunkt. Kommt es zurück, ist das kein „Neu" und kein Anstieg: es war nur
/// vorübergehend nicht gelistet (sold out oder unvollständige Antwort).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GonePart {
    pub s: i64,
    pub n: String,
    pub t: String,
}

/// Case-insensitiver Substring-Test ohne Allocation (ASCII-Folding; Nicht-ASCII
/// byte-genau). `memchr2` springt zum nächsten ersten Needle-Byte, erst dort wird
/// der Rest verglichen – schneller als ein Fenster-Vergleich pro Position.
pub fn ci_contains(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    let Some(last_start) = h.len().checked_sub(n.len()) else {
        return false;
    };
    let lower = n[0].to_ascii_lowercase();
    let upper = n[0].to_ascii_uppercase();
    let mut i = 0;
    while i <= last_start {
        // Nächstes Byte, das dem ersten Needle-Byte in einer Schreibweise
        // entspricht (bei Nicht-Buchstaben sind beide gleich).
        let Some(rel) = memchr::memchr2(lower, upper, &h[i..=last_start]) else {
            return false;
        };
        let start = i + rel;
        if h[start + 1..start + n.len()].eq_ignore_ascii_case(&n[1..]) {
            return true;
        }
        i = start + 1;
    }
    false
}

/// Commodity-Filter: case-insensitiv, getrimmt. Wird vom Deserializer-Seed
/// schon während des Parsens genutzt, um nicht-mathende Teile zu überspringen.
pub fn commodity_key_matches(commodity_key: &str, keys: &[String]) -> bool {
    keys.iter()
        .any(|k| commodity_key.trim().eq_ignore_ascii_case(k))
}

/// Keyword-Filter: ODER-verknüpft auf `name`, `id` und die technischen Daten
/// (`specs`). Leere Liste = kein Zusatzfilter.
pub fn keyword_matches(id: &str, name: &str, specs: &str, keywords: &[String]) -> bool {
    keywords.is_empty()
        || keywords
            .iter()
            .any(|kw| ci_contains(name, kw) || ci_contains(id, kw) || ci_contains(specs, kw))
}

/// Kanonischer Fingerprint der aktiven Filter (commodity_keys + name_keywords).
/// Landet im state.json – ändert er sich, wird die Baseline neu gesetzt
/// (sonst würde ein Filterwechsel als riesiger „Anstieg" alarmieren).
pub fn filter_fingerprint(keys: &[String], keywords: &[String]) -> String {
    let mut ks: Vec<String> = keys.iter().map(|s| s.trim().to_lowercase()).collect();
    ks.sort();
    let mut kw: Vec<String> = keywords
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    kw.sort();
    // Keywords matchen seit dem Specs-Umbau auch die technischen Daten. Der
    // Modus im Fingerprint setzt die Baseline bei aktiven Keywords einmalig
    // neu, statt die zusätzlichen Treffer als riesigen Anstieg zu mailen.
    let mode = if kw.is_empty() { "" } else { ";m=specs" };
    format!("c={};k={}{}", ks.join("|"), kw.join("|"), mode)
}

/// Menschlich lesbares Filter-Label für Mail-Footer ("system boards · RTX").
pub fn filter_label(keys: &[String], keywords: &[String]) -> String {
    keys.iter()
        .chain(keywords.iter())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct KwStat {
    pub parts: usize,
    pub stock: i64,
}

/// Pro Keyword: Anzahl passender Teile und Summe ihres Bestands
/// (Matching wie `keyword_matches`: case-insensitive Substring auf Name, ID
/// und technischen Daten; `specs` ist der Specs-Cache, id → Text).
/// Ein Pass über die Map (statt ein Pass pro Keyword) – die Keyword-Liste ist
/// klein und liegt cache-lokal, die Map (≈3.100 Einträge) wird nur einmal
/// durchlaufen. Die Ausgabe-Map wird vorab befüllt, damit der innere Loop
/// ohne String-Klon auskommt (get_mut statt entry(kw.clone())).
pub fn keyword_stats(
    map: &BTreeMap<String, PartState>,
    specs: &BTreeMap<String, String>,
    keywords: &[String],
) -> BTreeMap<String, KwStat> {
    let mut kws: Vec<String> = keywords
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    kws.sort_unstable();
    kws.dedup();
    let mut out: BTreeMap<String, KwStat> = kws
        .into_iter()
        .map(|kw| (kw, KwStat { parts: 0, stock: 0 }))
        .collect();
    if out.is_empty() {
        return out;
    }
    for (id, p) in map.iter() {
        let text = specs.get(id).map_or("", String::as_str);
        for (kw, st) in out.iter_mut() {
            if ci_contains(&p.n, kw) || ci_contains(id, kw) || ci_contains(text, kw) {
                st.parts += 1;
                st.stock += p.s;
            }
        }
    }
    out
}

/// Keyword-Bewegungen: Bestand oder Teilzahl gestiegen (NEU zählt als Anstieg).
pub fn keyword_changes(
    old: &BTreeMap<String, KwStat>,
    new: &BTreeMap<String, KwStat>,
) -> Vec<(String, KwStat, KwStat)> {
    let mut out = Vec::new();
    for (kw, ns) in new {
        let (os_parts, os_stock) = old.get(kw).map_or((0, 0), |s| (s.parts, s.stock));
        if ns.stock > os_stock || ns.parts > os_parts {
            out.push((
                kw.clone(),
                KwStat {
                    parts: os_parts,
                    stock: os_stock,
                },
                ns.clone(),
            ));
        }
    }
    out
}

pub fn compute_totals(map: &BTreeMap<String, PartState>) -> (i64, usize) {
    // Ein Durchlauf statt separater sum() + len()-Lookups über dieselbe Map.
    let mut total = 0i64;
    for v in map.values() {
        total += v.s;
    }
    (total, map.len())
}

#[derive(Debug)]
pub struct IncreasedPart {
    pub id: String,
    pub name: String,
    pub old: i64,
    pub new: i64,
    pub delta: i64,
}

#[derive(Debug)]
pub struct Diff {
    pub old_total: i64,
    pub new_total: i64,
    pub increased: Vec<IncreasedPart>,
    pub new_parts: Vec<(String, String, i64)>, // id, name, stock
    /// Irgendein Unterschied (Bestand, Name, neuer/entfernter Schlüssel)?
    /// Wird im selben Pass ermittelt und ersetzt den zweiten Map-Vergleich.
    pub changed: bool,
}

impl Diff {
    pub fn delta(&self) -> i64 {
        self.new_total - self.old_total
    }
}

pub fn build_diff(
    old_map: &BTreeMap<String, PartState>,
    new_map: &BTreeMap<String, PartState>,
    gone: &BTreeMap<String, GonePart>,
) -> Diff {
    // Vergleichsbasis sind nur die Teile, die jetzt noch gelistet sind – jedes
    // gegen seinen letzten bekannten Bestand (State oder `gone`). Dadurch hebt
    // die Rückkehr eines verschwundenen Teils das Delta nicht an; der „Vorher"-
    // Wert der Mail entspricht der Summe genau der gelisteten Teile.
    let mut old_total: i64 = 0;
    let mut new_total: i64 = 0;
    let mut increased = Vec::new();
    let mut new_parts = Vec::new();
    // Längenvergleich deckt entfernte Teile ab; im selben Pass werden
    // Bestands- und Namensänderungen erkannt (ersetzt `old_map == new_map`).
    let mut changed = old_map.len() != new_map.len();
    for (id, nv) in new_map {
        new_total += nv.s;
        match old_map.get(id) {
            None => match gone.get(id) {
                // Zurückgekehrtes Teil: gegen den letzten bekannten Bestand
                // vergleichen, nicht gegen 0 – sonst gäbe es eine falsche
                // Anstiegs-Mail, sobald eine unvollständige Antwort nachwirkt.
                Some(g) => {
                    changed = true;
                    old_total += g.s;
                    if nv.s > g.s {
                        increased.push(IncreasedPart {
                            id: id.clone(),
                            name: nv.n.clone(),
                            old: g.s,
                            new: nv.s,
                            delta: nv.s - g.s,
                        });
                    }
                }
                None => {
                    changed = true;
                    new_parts.push((id.clone(), nv.n.clone(), nv.s));
                }
            },
            Some(ov) => {
                old_total += ov.s;
                if nv.s != ov.s || nv.n != ov.n {
                    changed = true;
                }
                if nv.s > ov.s {
                    increased.push(IncreasedPart {
                        id: id.clone(),
                        name: nv.n.clone(),
                        old: ov.s,
                        new: nv.s,
                        delta: nv.s - ov.s,
                    });
                }
            }
        }
    }
    increased.sort_unstable_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.id.cmp(&b.id)));
    new_parts.sort_unstable_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    Diff {
        old_total,
        new_total,
        increased,
        new_parts,
        changed,
    }
}

/// Sortiert beide Mail-Listen nach Leistung – stärkste Teile zuerst (siehe
/// `api::performance_key`). Der Sort ist stabil: bei gleichem Schlüssel bleibt
/// die Reihenfolge aus `build_diff` (Anstiege nach Δ, neue Teile nach Bestand)
/// erhalten; Teile ohne Specs landen hinten.
pub fn sort_by_performance(diff: &mut Diff, specs: &BTreeMap<String, String>) {
    let key = |id: &str| {
        specs
            .get(id)
            .map_or((0, 0, 0), |line| crate::api::performance_key(line))
    };
    diff.increased
        .sort_by_cached_key(|p| std::cmp::Reverse(key(&p.id)));
    diff.new_parts
        .sort_by_cached_key(|p| std::cmp::Reverse(key(&p.0)));
}

/// Nimmt MX- und Quadro-T-Karten aus beiden Mail-Listen (Anstiege und Neu im
/// Sale). Teile ohne Specs bleiben drin – sie lassen sich nicht einordnen. Der
/// State bleibt vollständig; gefiltert wird nur die Mail. Liefert die Anzahl
/// der entfernten Teile.
pub fn drop_low_end_parts(diff: &mut Diff, specs: &BTreeMap<String, String>) -> usize {
    let excluded = |id: &str| {
        specs
            .get(id)
            .is_some_and(|line| crate::api::is_low_end_gpu(line))
    };
    let before = diff.increased.len() + diff.new_parts.len();
    diff.increased.retain(|p| !excluded(&p.id));
    diff.new_parts.retain(|p| !excluded(&p.0));
    before - diff.increased.len() - diff.new_parts.len()
}

pub fn should_mail(diff: &Diff, kw_moved: &[(String, KwStat, KwStat)]) -> bool {
    diff.delta() > 0 || !kw_moved.is_empty()
}
