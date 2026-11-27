//! Querschnitts-Tests des Trackers: Ablauf, Parser, Diff, Mail, State.
//!
//! Sie laufen über `crate::*`, die Module liegen in eigene Dateien.

use std::collections::BTreeMap;

use super::*;
use std::net::IpAddr;

fn filter_ids(
    map: &BTreeMap<String, PartState>,
    specs: &BTreeMap<String, String>,
    kws: &[&str],
) -> Vec<String> {
    let kws: Vec<String> = kws.iter().map(|s| s.to_string()).collect();
    map.iter()
        .filter(|(id, p)| {
            keyword_matches(
                id,
                &p.n,
                specs.get(id.as_str()).map_or("", String::as_str),
                &kws,
            )
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// Reader, der pro `read` höchstens `max` Bytes liefert – wie eine Pipe,
/// die in kleinen Häppchen gefüllt wird (partielle Reads).
struct DripReader {
    data: Vec<u8>,
    pos: usize,
    max: usize,
}

impl std::io::Read for DripReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.data.len() - self.pos).min(buf.len()).min(self.max);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[test]
fn test_fmt_de() {
    assert_eq!(fmt_de(16489), "16.489");
    assert_eq!(fmt_de(999), "999");
    assert_eq!(fmt_de(-1234), "-1.234");
}

#[test]
fn test_esc() {
    assert_eq!(esc("a<b>&\"c\""), "a&lt;b&gt;&amp;&quot;c&quot;");
}

#[test]
fn test_filter_seed_and_totals() {
    let body = r#"{"code":0,"msg":{"desc":"Success"},"data":[
            {"id":"A","country":"DE","commodityKey":"system boards","name":"Board A","stock":2},
            {"id":"B","country":"DE","commodityKey":"heat sinks","name":"Sink B","stock":500},
            {"id":"C","country":"DE","commodityKey":"SYSTEM BOARDS","name":"Board C","stock":98},
            {"id":"D","country":"DE","commodityKey":"system boards","name":"Board D"},
            {"id":"E","country":"DE","commodityKey":"system boards","name":"AMD RTX Board","stock":7},
            {"id":"","country":"DE","commodityKey":"system boards","name":"Ohne ID","stock":3}
        ]}"#;
    let keys = vec!["system boards".to_string()];
    // Nur Commodity-Filter: A, C, D, E matchen; B nicht; leere ID wird übersprungen
    let out = parse_clearance(body, &keys);
    assert_eq!(out.code, 0);
    assert_eq!(out.total_parts, 6);
    assert_eq!(out.boards.len(), 4);
    let (total, count) = compute_totals(&out.boards);
    assert_eq!((total, count), (107, 4)); // 2 + 98 + 0(null) + 7
                                          // Keywords greifen nach dem Parse auf Name, ID und Specs
    let no_specs = BTreeMap::new();
    assert_eq!(filter_ids(&out.boards, &no_specs, &["rtx"]), vec!["E"]);
    assert_eq!(filter_ids(&out.boards, &no_specs, &["amd"]), vec!["E"]);
    // Keyword nur in den technischen Daten (Name "Board A" enthält kein RTX)
    let mut specs = BTreeMap::new();
    specs.insert(
        "A".to_string(),
        "GPU: NVIDIA RTX 3000 (diskret)".to_string(),
    );
    assert_eq!(filter_ids(&out.boards, &specs, &["rtx"]), vec!["A", "E"]);
    assert_eq!(filter_ids(&out.boards, &specs, &[]).len(), 4);
}

#[test]
fn test_parse_clearance_from_matches_str() {
    let body = r#"{"code":0,"data":[
            {"id":"A","commodityKey":"system boards","name":"Board A","stock":2},
            {"id":"B","commodityKey":"heat sinks","name":"Sink B","stock":9}
        ]}"#;
    let keys = vec!["system boards".to_string()];
    let from_str = parse_clearance(body, &keys);
    let from_reader = parse_clearance_from(body.as_bytes(), &keys).expect("Reader-Parse");
    assert_eq!(from_reader.code, from_str.code);
    assert_eq!(from_reader.total_parts, from_str.total_parts);
    assert_eq!(from_reader.boards, from_str.boards);
    // kaputtes JSON → Err statt stillschweigend leerer Baseline
    assert!(parse_clearance_from("kaputt".as_bytes(), &keys).is_err());
    // Streaming-Fallback: Limit kleiner als der Body → Cursor-Präfix +
    // Rest aus dem Reader, Ergebnis identisch zum gepufferten Pfad
    let capped =
        crate::api::parse_clearance_capped(body.as_bytes(), 16, &keys).expect("Fallback-Parse");
    assert_eq!(capped.boards, from_str.boards);
    assert_eq!(capped.total_parts, from_str.total_parts);
    // Produktionspfad: Body kommt aus einer OS-Pipe (wie curl-stdout)
    let (r, mut w) = std::io::pipe().expect("Pipe");
    let writer = std::thread::spawn(move || {
        std::io::Write::write_all(&mut w, body.as_bytes()).expect("Pipe-Write")
    });
    let piped = parse_clearance_from(r, &keys).expect("Pipe-Parse");
    writer.join().expect("Writer-Thread");
    assert_eq!(piped.boards, from_str.boards);
    assert_eq!(piped.total_parts, from_str.total_parts);
}

#[test]
fn test_read_capped_partial_reads() {
    let body = r#"{"code":0,"data":[
            {"id":"A","commodityKey":"system boards","name":"Board A","stock":2},
            {"id":"B","commodityKey":"system boards","name":"Board B","stock":5}
        ]}"#;
    let keys = vec!["system boards".to_string()];
    let expected = parse_clearance(body, &keys);
    // Gepufferter Pfad: viele 3-Byte-Reads, cap groß genug.
    let drip = DripReader {
        data: body.as_bytes().to_vec(),
        pos: 0,
        max: 3,
    };
    let got = crate::api::parse_clearance_capped(drip, 4096, &keys).expect("Drip-Parse");
    assert_eq!(got.boards, expected.boards);
    // Overflow-Fallback: cap kleiner als der Body, der Rest kommt weiter
    // in 3-Byte-Stücken aus dem Reader.
    let drip = DripReader {
        data: body.as_bytes().to_vec(),
        pos: 0,
        max: 3,
    };
    let got = crate::api::parse_clearance_capped(drip, 16, &keys).expect("Drip-Fallback");
    assert_eq!(got.boards, expected.boards);
    assert_eq!(got.total_parts, expected.total_parts);
}

#[test]
fn test_filter_seed_id_match() {
    let body = r#"{"code":0,"data":[
            {"id":"P1","commodityKey":"system boards","name":"AMD B650M Board","stock":5},
            {"id":"P2","commodityKey":"system boards","name":"Intel H610 Board","stock":7},
            {"id":"P3","commodityKey":"system boards","name":"Lenovo AM5 Board","stock":9}
        ]}"#;
    let keys = vec!["system boards".to_string()];
    let all = parse_clearance(body, &keys);
    // leer → alle
    assert_eq!(all.boards.len(), 3);
    // ODER auf name, case-insensitiv
    let no_specs = BTreeMap::new();
    assert_eq!(
        filter_ids(&all.boards, &no_specs, &["b650", "am5"]),
        vec!["P1", "P3"]
    );
    // Match auf id statt name
    assert_eq!(filter_ids(&all.boards, &no_specs, &["P2"]), vec!["P2"]);
    // nichts passt
    assert!(filter_ids(&all.boards, &no_specs, &["x999"]).is_empty());
    // leere Datenliste
    assert_eq!(
        parse_clearance(r#"{"code":0,"data":[]}"#, &keys).total_parts,
        0
    );
    // data fehlt komplett
    assert_eq!(parse_clearance(r#"{"code":0}"#, &keys).total_parts, 0);
    // Code-Fehler
    assert_eq!(parse_clearance(r#"{"code":7,"data":[]}"#, &keys).code, 7);
}

#[test]
fn test_ci_contains() {
    assert!(ci_contains("AMD B650M Board", "b650"));
    assert!(ci_contains("AMD B650M Board", "B650M"));
    assert!(!ci_contains("AMD B550M Board", "b650"));
    assert!(ci_contains("irgendwas", ""));
    assert!(!ci_contains("kurz", "länger-als-haystack"));
    // Nicht-ASCII: byte-genau (kein Folding) — dokumentiertes Verhalten
    assert!(ci_contains("TÜV-Board", "TÜV"));
    // memchr-Sprungpfad: Treffer am Ende, Kandidat ohne vollen Match
    assert!(ci_contains("abcdEFG", "efg"));
    assert!(!ci_contains("abcabcabc", "abd"));
}

#[test]
fn test_filter_fingerprint() {
    let keys = vec!["system boards".to_string()];
    let empty: Vec<String> = vec![];
    let kw = vec!["B650".to_string(), "AM5".to_string()];
    let f0 = filter_fingerprint(&keys, &empty);
    let f1 = filter_fingerprint(&keys, &kw);
    // Reihenfolge der Keywords ist egal (sortiert)
    let f1_rev = filter_fingerprint(&keys, &["am5".to_string(), "b650".to_string()]);
    assert_eq!(f1, f1_rev);
    assert_ne!(f0, f1);
    assert!(f0.starts_with("c=system boards;k="));
    // Aktive Keywords kennzeichnen den Specs-Modus → Baseline-Reset
    assert!(f1.ends_with(";m=specs"));
    assert!(!f0.contains("m=specs"));
    assert_eq!(
        filter_fingerprint(&["SYSTEM BOARDS".to_string()], &empty),
        f0
    ); // case/trim-normalisiert
}

#[test]
fn test_build_diff() {
    let mut old = BTreeMap::new();
    old.insert(
        "A".to_string(),
        PartState {
            s: 5,
            n: "A".into(),
        },
    );
    old.insert(
        "B".to_string(),
        PartState {
            s: 90,
            n: "B".into(),
        },
    );
    let mut new = BTreeMap::new();
    new.insert(
        "A".to_string(),
        PartState {
            s: 25,
            n: "A".into(),
        },
    );
    new.insert(
        "B".to_string(),
        PartState {
            s: 95,
            n: "B".into(),
        },
    );
    new.insert(
        "C".to_string(),
        PartState {
            s: 3,
            n: "C".into(),
        },
    );
    let diff = build_diff(&old, &new, &BTreeMap::new());
    assert_eq!(diff.delta(), 28);
    assert_eq!(
        diff.increased
            .iter()
            .map(|i| i.id.clone())
            .collect::<Vec<_>>(),
        vec!["A", "B"]
    );
    assert_eq!(
        diff.new_parts
            .iter()
            .map(|p| p.0.clone())
            .collect::<Vec<_>>(),
        vec!["C"]
    );
    assert!(diff.changed);
    // identische Maps → kein zweiter Vergleich nötig, changed bleibt false
    assert!(!build_diff(&old, &old, &BTreeMap::new()).changed);
    // reine Bestands-Senkung zählt ebenfalls als Änderung (State-Update)
    let mut down = old.clone();
    down.insert(
        "A".to_string(),
        PartState {
            s: 1,
            n: "A".into(),
        },
    );
    assert!(build_diff(&old, &down, &BTreeMap::new()).changed);
    assert!(build_diff(&old, &down, &BTreeMap::new())
        .increased
        .is_empty());
    // reine Namensänderung bei gleichem Bestand
    let mut renamed = old.clone();
    renamed.insert(
        "A".to_string(),
        PartState {
            s: 5,
            n: "A neu".into(),
        },
    );
    assert!(build_diff(&old, &renamed, &BTreeMap::new()).changed);
}

#[test]
fn test_stock_garbage() {
    let sf = |v: serde_json::Value| -> i64 { serde_json::from_value::<StockField>(v).unwrap().0 };
    assert_eq!(sf(serde_json::json!(null)), 0);
    assert_eq!(sf(serde_json::json!("7")), 7);
    assert_eq!(sf(serde_json::json!(-5)), 0);
    assert_eq!(sf(serde_json::json!("kaputt")), 0);
    assert_eq!(sf(serde_json::json!(7.9)), 7);
    assert_eq!(sf(serde_json::json!(true)), 0);
    assert_eq!(sf(serde_json::json!([1])), 0);
    // u64 > i64::MAX → saturiert wie die alte f64-Variante
    assert_eq!(sf(serde_json::json!(18446744073709551615u64)), i64::MAX);
    // fehlendes Feld → Default (StockField hat serde-Default)
    let part = parse_clearance(
        r#"{"code":0,"data":[{"id":"X","commodityKey":"system boards"}]}"#,
        &["system boards".to_string()],
    );
    assert_eq!(part.boards["X"].s, 0);
}

#[test]
fn test_long_id_uses_heap_fallback() {
    // IDs > 32 Bytes (ID_INLINE) müssen unverändert in der Map landen
    let long_id = "A".repeat(40);
    let body = format!(
        r#"{{"code":0,"data":[{{"id":"{}","commodityKey":"system boards","name":"Board X","stock":3}}]}}"#,
        long_id
    );
    let keys = vec!["system boards".to_string()];
    let out = parse_clearance(&body, &keys);
    assert_eq!(out.total_parts, 1);
    assert_eq!(out.boards.get(&long_id).map(|p| p.s), Some(3));
    // Inline-Pfad (kurze ID) liefert dasselbe Ergebnis
    let out = parse_clearance(
        r#"{"code":0,"data":[{"id":"5B20S42095","commodityKey":"system boards","name":"Board Y","stock":4}]}"#,
        &keys,
    );
    assert_eq!(out.boards.get("5B20S42095").map(|p| p.s), Some(4));
}

#[test]
fn test_civil_from_days() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(1), (1970, 1, 2));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 kein Schaltjahr
    assert_eq!(civil_from_days(365), (1971, 1, 1));
    // 2024 Schaltjahr: 31.12.2024 = 20088 Tage nach Epoch
    assert_eq!(civil_from_days(20_088), (2024, 12, 31));
    assert_eq!(civil_from_days(20_089), (2025, 1, 1));
    // Datum aus der Planung: 2026-11-18 = 20775 Tage (verifiziert via datetime.date)
    assert_eq!(civil_from_days(20_775), (2026, 11, 18));
}

#[test]
fn test_merge_specs_caches_hits_and_misses() {
    let mut cache = BTreeMap::new();
    cache.insert("OLD".to_string(), "CPU: alt".to_string());
    let mut fetched = BTreeMap::new();
    fetched.insert("HIT".to_string(), "CPU: neu".to_string());
    let mut out = BTreeMap::new();
    // "MISS" wurde geprüft, hat aber keine Specs → leer im Cache, nicht in out
    merge_specs(&["HIT", "MISS"], fetched, &mut out, &mut cache);
    assert_eq!(out.get("HIT").map(String::as_str), Some("CPU: neu"));
    assert!(!out.contains_key("MISS"));
    assert_eq!(cache.get("MISS").map(String::as_str), Some(""));
    assert_eq!(cache.get("OLD").map(String::as_str), Some("CPU: alt"));
    assert_eq!(cache.len(), 3);
}

#[test]
fn test_specs_pipeline() {
    // beautify_cpu: kompakte Codes → lesbare Namen; Unbekanntes bleibt unverändert
    assert_eq!(beautify_cpu("intelI79750H"), "Intel Core i7-9750H");
    assert_eq!(beautify_cpu("intelI36006U"), "Intel Core i3-6006U");
    assert_eq!(beautify_cpu("intelI58265U"), "Intel Core i5-8265U");
    assert_eq!(beautify_cpu("AMDR76850H-PRO"), "AMD Ryzen 7 6850H PRO");
    assert_eq!(beautify_cpu("AMDR72700U"), "AMD Ryzen 7 2700U");
    assert_eq!(beautify_cpu("KAPUTT"), "KAPUTT");
    // ram_from_name: explizit GB, NG-Token, VRAM-Ausschluss
    assert_eq!(
        ram_from_name("WIN, i5-8250U, 8 GB, UMA", None).as_deref(),
        Some("8 GB")
    );
    assert_eq!(
        ram_from_name("MB C81NH NOK UMA R7 4G FP", None).as_deref(),
        Some("4 GB")
    );
    assert_eq!(
        ram_from_name("FRUR7P5850U16G WINY-DYdT2L3Y-A", None).as_deref(),
        Some("16 GB")
    );
    // DIS + GPU-Token davor → VRAM, kein RAM-Chip
    assert_eq!(
        ram_from_name(
            "i7-9750H RTX3000 6G NOKNAYT dT2",
            Some("NVIDIA Quadro RTX 3000")
        ),
        None
    );
    assert_eq!(
        ram_from_name(
            "FL R7 P T1200 4G WIN yD nAB yDT",
            Some("NVIDIA Quadro T1200")
        ),
        None
    );
    // mehrdeutig → None
    assert_eq!(ram_from_name("8G 16G UMA", None), None);
    // format_specs (Fallback ohne Attribute)
    assert_eq!(
        format_specs("DPK: WIN;;CPU: AMDR72700U;;GPU Type: UMA;;"),
        "DPK: WIN | CPU: AMDR72700U | GPU Type: UMA"
    );
    // detail_target: Locale aus api.url ableiten
    let (url, country, lang) =
        detail_target("https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get").unwrap();
    assert_eq!(
        url,
        "https://support.lenovo.com/de/de/api/v4/lbom/parts/get"
    );
    assert_eq!((country.as_str(), lang.as_str()), ("DE", "DE"));
    assert!(detail_target("https://example.com").is_none());
    // safe_part_id
    assert!(safe_part_id("5B20S42095"));
    assert!(!safe_part_id("\"; rm -rf /"));
    // parse_detail: geordnete, beautifizierte Zeile (CPU → RAM → GPU → DPK/AMT)
    let map = parse_detail(
        r#"{"code":0,"data":[
                {"id":"02DC237","name":"AMD R7 2700U mit Vega10 WIN YT","specs":{"specsInfo":"DPK: WIN;;CPU: AMDR72700U;;GPU Type: UMA;;","specsAttributes":[
                    {"displayTitle":"DPK","value":"WIN","transKey":"dpk"},
                    {"displayTitle":"CPU","value":"AMDR72700U","transKey":"cpu"},
                    {"displayTitle":"GPU Type","value":"UMA","transKey":"gpu type"}]}},
                {"id":"RTXBOARD","name":"i7-9750H RTX3000 6G NOKNAYT dT2","specs":{"specsAttributes":[
                    {"transKey":"cpu","value":"intelI79750H"},
                    {"transKey":"gpu type","value":"DIS"},
                    {"transKey":"gpu name","value":"NVIDIA Quadro RTX 3000"},
                    {"transKey":"amt","value":"N-AMT"},
                    {"transKey":"dpk","value":"NOK"}]}},
                {"id":"DRAMBOARD","name":"FL R7 P T1200 4G WIN yD nAB yDT","specs":{"specsAttributes":[
                    {"transKey":"cpu","value":"AMDR76850H-PRO"},
                    {"transKey":"dram","value":"16 GB"},
                    {"transKey":"gpu type","value":"DIS"},
                    {"transKey":"gpu name","value":"NVIDIA Quadro T1200"}]}},
                {"id":"NO_SPECS"},
                {"id":"EMPTY_SPECS","specs":{"specsInfo":""}}
            ]}"#,
    );
    assert_eq!(map.len(), 3);
    assert_eq!(
        map.get("02DC237").unwrap(),
        "CPU: AMD Ryzen 7 2700U | GPU: integriert | DPK: WIN"
    );
    assert_eq!(
        map.get("RTXBOARD").unwrap(),
        "CPU: Intel Core i7-9750H | GPU: NVIDIA Quadro RTX 3000 (diskret) | DPK: NOK | AMT: N-AMT"
    );
    assert_eq!(
        map.get("DRAMBOARD").unwrap(),
        "CPU: AMD Ryzen 7 6850H PRO | RAM: 16 GB | GPU: NVIDIA Quadro T1200 (diskret)"
    );
    // garbage → leere Map, keine Panic
    assert!(parse_detail("kaputt").is_empty());
}

#[test]
fn test_mail_contains_specs() {
    let mut old = BTreeMap::new();
    old.insert(
        "A".to_string(),
        PartState {
            s: 5,
            n: "A".into(),
        },
    );
    let mut new = BTreeMap::new();
    new.insert(
        "A".to_string(),
        PartState {
            s: 25,
            n: "AMD R7 2700U Board".into(),
        },
    );
    let diff = build_diff(&old, &new, &BTreeMap::new());
    let mut specs = BTreeMap::new();
    specs.insert(
        "A".to_string(),
        "CPU: AMD Ryzen 7 2700U | RAM: 16 GB | GPU: integriert | DPK: WIN".to_string(),
    );
    let label = filter_label(&["system boards".to_string()], &["RTX".to_string()]);
    assert_eq!(label, "system boards · RTX");
    let mut prices: PriceMap = BTreeMap::new();
    prices.insert("A".to_string(), 5989);
    let view = MailView {
        diff: &diff,
        page_url: "https://example.com",
        checked_at: "2026-11-19T00:00:00Z",
        specs: &specs,
        prices: &prices,
        currency: "€",
        filter_label: &label,
        kw_moved: &[],
    };
    let (text, html) = build_mail(&view);
    assert!(text.contains("CPU: AMD Ryzen 7 2700U | RAM: 16 GB | GPU: integriert | DPK: WIN"));
    assert!(text.contains("Filter: system boards · RTX"));
    // Kosten je Teil (59,89 € × 25) – ohne früheren Preis, ohne Summenzeile
    assert!(text.contains(" · 1.497,25 €"));
    assert!(!text.contains("Kosten aller gelisteten Teile"));
    assert!(!text.contains("statt"));
    assert!(html.contains("LENOVO CLEARANCE-TRACKER"));
    assert!(html.contains("display:inline-block;background:#f1f3f5")); // Chip
    assert!(html.contains("RAM: 16 GB"));
    assert!(html.contains("system boards · RTX"));
    assert!(html.contains("+20")); // Delta-Badge
    assert!(html.contains("2026-11-19 00:00:00 UTC")); // pretty_ts
    assert!(html.contains("Clearance-Sale ansehen"));
    assert!(html.contains(">Kosten</th>"));
    assert!(html.contains("<b>1.497,25 €</b>"));
    assert!(!html.contains("Kosten aller gelisteten Teile")); // keine Summenbox
    assert!(!html.contains("line-through")); // kein Streichpreis mehr
                                             // ohne Specs/Preise: Mail normal, keine Chips, Preis-Spalte mit "–"
    let leer_specs = BTreeMap::new();
    let leer_prices = BTreeMap::new();
    let view_ohne = MailView {
        diff: &diff,
        page_url: "https://example.com",
        checked_at: "2026-11-19T00:00:00Z",
        specs: &leer_specs,
        prices: &leer_prices,
        currency: "€",
        filter_label: &label,
        kw_moved: &[],
    };
    let (text_ohne, html_ohne) = build_mail(&view_ohne);
    assert!(!text_ohne.contains("CPU:"));
    assert!(!html_ohne.contains("background:#f1f3f5"));
    assert!(html_ohne.contains(">Kosten</th>"));
}

#[test]
fn test_mail_lists_all_parts() {
    // Mehr als 20 Teile je Liste: früher wurden nur 20 gezeigt
    // ("+ N weitere Teile") – jetzt muss jedes Teil in der Mail stehen.
    let old = BTreeMap::new();
    let mut new: BTreeMap<String, PartState> = BTreeMap::new();
    for i in 0..25 {
        new.insert(
            format!("NEW{:02}", i),
            PartState {
                s: 1,
                n: format!("Board {}", i),
            },
        );
    }
    let mut old_up = BTreeMap::new();
    let mut new_up = BTreeMap::new();
    for i in 0..25 {
        let id = format!("UP{:02}", i);
        old_up.insert(
            id.clone(),
            PartState {
                s: 1,
                n: "x".into(),
            },
        );
        new_up.insert(
            id,
            PartState {
                s: 3,
                n: "x".into(),
            },
        );
    }
    let diff_new = build_diff(&old, &new, &BTreeMap::new());
    let diff_up = build_diff(&old_up, &new_up, &BTreeMap::new());
    let leer_specs = BTreeMap::new();
    let leer_prices = BTreeMap::new();
    let view_neu = MailView {
        diff: &diff_new,
        page_url: "https://example.com",
        checked_at: "2026-11-26T00:00:00Z",
        specs: &leer_specs,
        prices: &leer_prices,
        currency: "€",
        filter_label: "system boards",
        kw_moved: &[],
    };
    let (text, _) = build_mail(&view_neu);
    assert!(text.contains("NEW24"));
    assert!(!text.contains("weitere Teile"));
    let view_up = MailView {
        diff: &diff_up,
        page_url: "https://example.com",
        checked_at: "2026-11-26T00:00:00Z",
        specs: &leer_specs,
        prices: &leer_prices,
        currency: "€",
        filter_label: "system boards",
        kw_moved: &[],
    };
    let (_, html) = build_mail(&view_up);
    assert!(html.contains("UP24"));
    assert!(html.contains("Bestandsanstiege (25)"));
    assert!(!html.contains("weitere Teile"));
}

#[test]
fn test_price_pipeline() {
    // Preise kommen aus der Clearance-Liste (price/promotionPrice): Zahl oder null.
    // Klammerung passiert in to_cents (negativ/0/NaN → 0).
    assert_eq!(to_cents(59.89), 5989);
    assert_eq!(to_cents(0.0), 0);
    assert_eq!(to_cents(-3.0), 0);
    assert_eq!(to_cents(f64::NAN), 0);
    assert_eq!(fmt_money(5989, "€"), "59,89 €");
    assert_eq!(fmt_money(85563, "€"), "855,63 €");
    assert_eq!(fmt_money(123456789, "€"), "1.234.567,89 €");
    assert_eq!(fmt_money(5, ""), "0,05");
    assert_eq!(
        currency_symbol("https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"),
        "€"
    );
    assert_eq!(
        currency_symbol("https://support.lenovo.com/fr/fr/api/v4/lbom/clearanceParts/get"),
        "€"
    );
    assert_eq!(
        currency_symbol("https://support.lenovo.com/gb/en/api/v4/lbom/clearanceParts/get"),
        "£"
    );
    assert_eq!(
        currency_symbol("https://support.lenovo.com/us/en/api/v4/lbom/clearanceParts/get"),
        "$"
    );
    assert_eq!(currency_symbol("https://example.com"), "");
    // Liste → Map: Aktionspreis, sonst Listenpreis; ganze Zahlen und null
    // müssen ebenfalls durchlaufen.
    let body = r#"{"code":0,"data":[
            {"id":"A","commodityKey":"system boards","name":"Board A","price":855.63,"promotionPrice":59.89,"stock":2},
            {"id":"B","commodityKey":"system boards","name":"Board B","price":null,"stock":1},
            {"id":"D","commodityKey":"system boards","name":"Board D","price":12,"promotionPrice":3,"stock":1},
            {"id":"E","commodityKey":"system boards","name":"Board E","price":50,"promotionPrice":0,"stock":1},
            {"id":"C","commodityKey":"heat sinks","name":"Sink C","price":10.0,"promotionPrice":5.0,"stock":9}
        ]}"#;
    let out = parse_clearance(body, &["system boards".to_string()]);
    assert_eq!(out.prices.get("A"), Some(&5989)); // Aktionspreis gewinnt
    assert_eq!(out.prices.get("D"), Some(&300));
    assert_eq!(out.prices.get("E"), Some(&5000)); // ohne Aktion: Listenpreis
    assert!(!out.prices.contains_key("B")); // ohne Preis kein Eintrag
    assert!(!out.prices.contains_key("C")); // Commodity-Filter greift auch hier
                                            // Est-Wert (Feld `price`) zusätzlich und unabhängig von der Aktion.
    assert_eq!(out.list_prices.get("A"), Some(&85563));
    assert_eq!(out.list_prices.get("D"), Some(&1200));
    assert_eq!(out.list_prices.get("E"), Some(&5000));
    assert!(!out.list_prices.contains_key("B")); // null → kein Eintrag
    assert!(!out.list_prices.contains_key("C"));
}

#[test]
fn test_sort_by_performance() {
    // Stärkste GPU zuerst, dann CPU-Klasse; integrierte und Teile ohne Specs hinten.
    let mut old = BTreeMap::new();
    let mut new = BTreeMap::new();
    for id in ["OLD0", "OLD1", "OLD2", "OLD3"] {
        old.insert(id.to_string(), PartState { s: 1, n: id.into() });
        new.insert(id.to_string(), PartState { s: 2, n: id.into() });
    }
    let mut diff = build_diff(&old, &new, &BTreeMap::new());
    let mut specs = BTreeMap::new();
    specs.insert(
        "OLD1".to_string(),
        "CPU: Intel Core i5-8250U | RAM: 8 GB | GPU: integriert | DPK: WIN".to_string(),
    );
    specs.insert(
        "OLD2".to_string(),
        "CPU: Intel Core i9-9880H | GPU: NVIDIA Quadro RTX 4000 (diskret) | DPK: WIN".to_string(),
    );
    specs.insert("OLD3".to_string(), "CPU: Intel Core i7-13705H | RAM: 16 GB | GPU: NVIDIA GeForce RTX 4060 (diskret) | DPK: WIN".to_string());
    assert!(performance_key(&specs["OLD3"]) > performance_key(&specs["OLD2"]));
    assert!(performance_key(&specs["OLD2"]) > performance_key(&specs["OLD1"]));
    assert_eq!(performance_key(""), (0, 0, 0));
    assert_eq!(performance_key("GPU: integriert | DPK: WIN"), (0, 0, 0));
    // Echte 3DMark-Time-Spy-Werte: „GeForce RTX 4000" und „Quadro RTX 4000"
    // sind dieselbe Karte (7.284), RTX 3070 (9.619) und 4060 (10.338) liegen
    // darüber, die Quadro RTX 6000 (9.339) dazwischen.
    let rt = |gpu: &str| performance_key(&format!("GPU: {} (diskret)", gpu));
    let quadro_4000 = rt("NVIDIA Quadro RTX 4000");
    assert_eq!(rt("NVIDIA GeForce RTX 4000"), quadro_4000);
    assert_eq!(rt("NVIDIA RTX 4000"), quadro_4000);
    assert!(rt("NVIDIA GeForce RTX 3070") > quadro_4000);
    assert!(rt("NVIDIA GeForce RTX 4060") > rt("NVIDIA GeForce RTX 3070"));
    assert!(rt("NVIDIA Quadro RTX 6000") > quadro_4000);
    // Gleiche Karte in anderer Schreibweise trifft denselben Eintrag.
    assert_eq!(rt("NVIDIA RTX 3070"), rt("NVIDIA GeForce RTX 3070"));
    assert_eq!(rt("NVIDIA GTX1650"), rt("NVIDIA GeForce GTX 1650"));
    assert_eq!(rt("NVIDIA RTX 3000"), rt("NVIDIA Quadro RTX 3000"));
    assert_eq!(rt("NVIDIA RTX-A4000"), rt("NVIDIA RTX A4000"));
    assert_eq!(rt("NVIDIA RTXA2000"), rt("NVIDIA RTX A2000"));
    // RTX A4000 (9.021) schlägt die alte Quadro RTX 4000 (7.284),
    // RTX A2000 (5.016) liegt unter der Quadro RTX 3000 (5.589).
    assert!(rt("NVIDIA RTX A4000") > quadro_4000);
    assert!(rt("NVIDIA Quadro RTX 3000") > rt("NVIDIA RTX A2000"));
    // Einstiegskarten: MX110 450 < T500 1.788 < MX450 1.900 < T1000 3.879;
    // 940MX (806) bleibt unter der T1200 (3.595).
    assert!(rt("NVIDIA GeForce MX110") < rt("NVIDIA Quadro T500"));
    assert!(rt("NVIDIA GeForce MX450") > rt("NVIDIA Quadro T500"));
    assert!(rt("NVIDIA Quadro T500") < rt("NVIDIA Quadro T1000"));
    assert!(rt("NVIDIA GeForce 940MX") < rt("NVIDIA Quadro T1200"));
    // Radeon: RX 6600M (7.840) vor Radeon Pro W6600 (6.846).
    assert!(rt("AMD Radeon RX 6600M") > rt("AMD Radeon Pro W6600"));
    // UMA-Boards („integriert") bekommen eine iGPU aus der CPU abgeleitet, klar
    // als Quelle „igpu" markiert: 12. Gen → Iris Xe 96 EU, Tiger Lake → 80 EU,
    // Ryzen 6000 → Radeon 680M.
    let uma = |cpu: &str| facets(&format!("CPU: {} | GPU: integriert", cpu));
    let f_uma = uma("Intel Core i7-12800H");
    assert_eq!(f_uma.gpu_bench_src, "igpu");
    assert_eq!(f_uma.gpu_bench, Some(1_560));
    assert_eq!(f_uma.gpu_bench_name, "Intel Iris Xe Graphics G7 96EUs");
    assert_eq!(uma("Intel Core i5-1135G7").gpu_bench, Some(1_181));
    assert_eq!(uma("AMD Ryzen 7 6850H PRO").gpu_bench, Some(2_298));
    // 7530U ist Zen 3 mit Vega 7 (nicht Radeon 760M), 7735U Rembrandt (680M),
    // 7840U Phoenix (780M).
    assert_eq!(
        uma("AMD Ryzen 5 7530U").gpu_bench_name,
        "AMD Radeon RX Vega 7 (Ryzen 4000/5000)"
    );
    assert_eq!(uma("AMD Ryzen 5 7530U").gpu_bench, Some(1_017));
    assert_eq!(uma("AMD Ryzen 7 7735U").gpu_bench_name, "AMD Radeon 680M");
    assert_eq!(uma("AMD Ryzen 7 7840U").gpu_bench_name, "AMD Radeon 780M");
    assert_eq!(uma("Intel Core i5-6300U").gpu_bench, Some(476));
    // Exakte SKU-Zuordnung statt Klassen-Regel: jede CPU bekommt genau ihre
    // iGPU (Intel ARK/AMD), nicht die ihrer Reihe.
    assert_eq!(uma("Intel Core i7-1165G7").gpu_bench, Some(1_560));
    assert_eq!(
        uma("Intel Core i5-1135G7").gpu_bench_name,
        "Intel Iris Xe Graphics G7 80EUs"
    );
    // Tiger Lake-H hat nur 32 EU, nicht 96.
    assert_eq!(uma("Intel Core i7-11800H").gpu_bench, Some(658));
    assert_eq!(
        uma("Intel Core i7-11800H").gpu_bench_name,
        "Intel UHD Graphics Xe 32EUs (Tiger Lake-H)"
    );
    assert_eq!(uma("Intel Core i9-11950H").gpu_bench, Some(658));
    // Alder Lake: i5-12450H nur 48 EU (Schätzwert, als solcher markiert),
    // i3-1215U 64 EU, i7-12800H 96 EU.
    assert_eq!(uma("Intel Core i5-12450H").gpu_bench, Some(832));
    assert_eq!(uma("Intel Core i5-12450H").gpu_bench_note, "est");
    assert_eq!(uma("Intel Core i3-1215U").gpu_bench, Some(1_110));
    assert_eq!(uma("Intel Core i3-1215U").gpu_bench_note, "ts");
    // Ice Lake: G1 (32 EU) statt pauschal Iris Xe.
    assert_eq!(uma("Intel Core i5-1035G1").gpu_bench, Some(450));
    // AMD: 7320U Mendocino mit Radeon 610M, 4300U Vega 5, 4700U Vega 7,
    // 4800U Vega 8.
    assert_eq!(uma("AMD Ryzen 3 7320U").gpu_bench_name, "AMD Radeon 610M");
    assert_eq!(uma("AMD Ryzen 3 4300U").gpu_bench, Some(733));
    assert_eq!(uma("AMD Ryzen 7 4700U").gpu_bench, Some(1_017));
    assert_eq!(uma("AMD Ryzen 7 4800U").gpu_bench, Some(1_163));
    // Schreibweisen treffen denselben Eintrag.
    assert_eq!(
        uma("AMD Athlon3050U").gpu_bench,
        uma("AMD ATHLON3050U").gpu_bench
    );
    // Für die Anzeige zählt der Kurzname (Tests dazu in api.rs), die
    // Kategorie bleibt „integriert".
    assert_eq!(f_uma.gpu_kind, "integriert");
    assert_eq!(igpu_short(f_uma.gpu_bench_name), "Iris Xe G7 96EU");
    // Ryzen 2000/3000 und 4000/5000 haben beide Vega 8 (851 vs. 1.163) –
    // am Chip steht die Generation.
    assert_eq!(uma("AMD Ryzen 5 3500U").gpu_bench, Some(851));
    assert_eq!(
        uma("AMD Ryzen 5 3500U").gpu_bench_name,
        "Radeon Vega 8 (Ryzen 2000/3000)"
    );
    assert_eq!(uma("AMD Ryzen 7 4800U").gpu_bench, Some(1_163));
    assert_eq!(
        uma("AMD Ryzen 7 4800U").gpu_bench_name,
        "AMD Radeon RX Vega 8 (Ryzen 4000/5000)"
    );
    // Neue Hardware: Ryzen AI (Zen 5) hat Tier und Modell getrennt – ohne
    // eigenen Parser landete „AI 7 350" auf der Zeile des alten AMD E-350.
    let ai = uma("AMD Ryzen AI 9 HX 370");
    assert_eq!(ai.cpu_series, "Ryzen AI");
    assert_eq!(ai.cpu_gen, "Ryzen AI 300");
    assert_eq!(ai.cpu_bench, Some((2_032, 22_906)));
    assert_eq!(ai.gpu_bench, Some(3_421)); // Radeon 890M
    assert_eq!(uma("AMD Ryzen AI 9 365").gpu_bench, Some(3_335)); // 880M
    assert_eq!(uma("AMD Ryzen AI 7 350").gpu_bench, Some(2_565)); // 860M
                                                                  // Inzwischen in der Tabelle nachgetragen (vorher Lücke).
    assert!(uma("AMD Ryzen AI 7 350").cpu_bench.is_some());
    // Intel 13./14. Gen: HX hat nur UHD 770, „…20H" weniger EUs.
    assert_eq!(uma("Intel Core i7-14700HX").gpu_bench, Some(2_655));
    assert_eq!(uma("Intel Core i9-14900HX").gpu_bench, Some(2_655));
    assert_eq!(uma("Intel Core i5-13420H").gpu_bench, Some(832)); // 48 EU
    assert_eq!(uma("Intel Core i7-13620H").gpu_bench, Some(1_110)); // 64 EU
    assert_eq!(uma("Intel N100").gpu_bench, Some(321)); // UHD 24EU
                                                        // i5-HX hat nur 16 EU, i7/i9-HX die vollen 32 EU (UHD 770).
    assert_eq!(uma("Intel Core i5-13500HX").gpu_bench, Some(348));
    assert_eq!(uma("Intel Core i9-14900HX").gpu_bench, Some(2_655));
    // Arrow Lake-S (K) und -HX haben 4 Xe-Kerne, kein UHD 770.
    assert_eq!(uma("Intel Core Ultra 9 285K").gpu_bench, Some(1_902));
    // Core Ultra 200: Lunar Lake und Arrow Lake statt Meteor-Lake-Bucket.
    assert_eq!(uma("Intel Core Ultra 7 258V").gpu_bench, Some(4_044));
    assert_eq!(uma("Intel Core Ultra 9 285H").gpu_bench, Some(3_843));
    assert_eq!(uma("Intel Core Ultra 7 155H").gpu_bench, Some(3_271));
    // Auch der Fallback ohne exakte SKU-Zeile trifft die richtige
    // Generation: 4000/5000 → RX Vega, 2000/3000 → Vega.
    assert_eq!(
        uma("AMD Ryzen 5 4600H").gpu_bench_name,
        "AMD Radeon RX Vega 6 (Ryzen 4000/5000)"
    );
    assert_eq!(uma("AMD Ryzen 5 4600H").gpu_bench, Some(852));
    assert_eq!(
        uma("AMD Ryzen 7 2800H").gpu_bench_name,
        "Radeon Vega 10 (Ryzen 2000/3000)"
    );
    assert_eq!(uma("AMD Ryzen 7 2800H").gpu_bench, Some(833));
    // Abgeleitet bleibt deutlich unter einer echten dedizierten Karte.
    assert!(f_uma.perf.0 < rt("NVIDIA GeForce MX110").0 + 2_000);
    assert!(f_uma.perf.0 < rt("NVIDIA GeForce RTX 3060").0);
    sort_by_performance(&mut diff, &specs);
    assert_eq!(
        diff.increased
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["OLD3", "OLD2", "OLD1", "OLD0"]
    );
    // Stabile Sortierung: gleiche Punktzahl behält die Δ-/ID-Reihenfolge
    let mut same = build_diff(&old, &new, &BTreeMap::new());
    let mut equal_specs = BTreeMap::new();
    equal_specs.insert(
        "OLD1".to_string(),
        "CPU: Intel Core i5-8250U | GPU: integriert".to_string(),
    );
    equal_specs.insert(
        "OLD3".to_string(),
        "CPU: Intel Core i5-8250U | GPU: integriert".to_string(),
    );
    sort_by_performance(&mut same, &equal_specs);
    assert_eq!(
        same.increased
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["OLD1", "OLD3", "OLD0", "OLD2"]
    );
}

#[test]
fn test_low_end_gpu_filter_from_real_specs_cache() {
    // Echte GPU-Namen aus der specs-cache.json (CT 120): MX-Serie und
    // Quadro-T fliegen raus, Quadro P/M/RTX, GTX/RTX und Radeon bleiben.
    let excluded = [
        "NVIDIA GEFORCE MX130 (diskret)",
        "NVIDIA GEFORCE MX330 (diskret)",
        "NVIDIA GEFORCE MX450 (diskret)",
        "NVIDIA GEFORCE MX550 (diskret)",
        "NVIDIA GeForce 940MX (diskret)",
        "NVIDIA GeForce MX110 (diskret)",
        "NVIDIA GeForce MX130 (diskret)",
        "NVIDIA GeForce MX150 (diskret)",
        "NVIDIA GeForce MX230 (diskret)",
        "NVIDIA GeForce MX250 (diskret)",
        "NVIDIA GeForce MX330 (diskret)",
        "NVIDIA GeForce MX350 (diskret)",
        "NVIDIA GeForce MX450 (diskret)",
        "NVIDIA GeForce MX450B (diskret)",
        "NVIDIA GeForce MX550 (diskret)",
        "NVIDIA® GeForce MX330 (diskret)",
        "NVIDIA QUADRO T1200 (diskret)",
        "NVIDIA Quadro T1000 (diskret)",
        "NVIDIA Quadro T1200 (diskret)",
        "NVIDIA Quadro T2000 (diskret)",
        "NVIDIA Quadro T500 (diskret)",
        "NVIDIA Quadro T600 (diskret)",
        "NVIDIA T500 (diskret)",
    ];
    for name in excluded {
        assert!(
            is_low_end_gpu(&format!("GPU: {}", name)),
            "soll raus: {}",
            name
        );
    }
    let kept = [
        "NVIDIA GeForce GTX 1650 (diskret)",
        "NVIDIA GeForce RTX3070 (diskret)",
        "GeForce RTX 2070 Super (diskret)",
        "NVIDIA Quadro P620 (diskret)",
        "NVIDIA Quadro M1200M (diskret)",
        "NVIDIA Quadro RTX 4000 (diskret)",
        "AMD Radeon RX 640 (diskret)",
        "AMD Radeon 530 (diskret)",
        "integriert",
    ];
    for name in kept {
        assert!(
            !is_low_end_gpu(&format!("GPU: {}", name)),
            "soll bleiben: {}",
            name
        );
    }
    // Nur das GPU-Feld zählt: „MX" in der CPU oder fehlende Specs filtern nichts.
    assert!(!is_low_end_gpu(
        "CPU: Intel Core i5-8350U | GPU: NVIDIA GeForce RTX 4060 (diskret)"
    ));
    assert!(!is_low_end_gpu("CPU: Intel Core i5-8350U"));
}

#[test]
fn test_drop_low_end_parts() {
    let mut old = BTreeMap::new();
    old.insert(
        "A".to_string(),
        PartState {
            s: 1,
            n: "stark".into(),
        },
    );
    let mut new = BTreeMap::new();
    new.insert(
        "A".to_string(),
        PartState {
            s: 3,
            n: "stark".into(),
        },
    );
    new.insert(
        "MX".to_string(),
        PartState {
            s: 2,
            n: "MX".into(),
        },
    );
    new.insert(
        "T1".to_string(),
        PartState {
            s: 5,
            n: "T".into(),
        },
    );
    new.insert(
        "Q".to_string(),
        PartState {
            s: 1,
            n: "P620".into(),
        },
    );
    new.insert(
        "N".to_string(),
        PartState {
            s: 1,
            n: "ohne Specs".into(),
        },
    );
    let mut diff = build_diff(&old, &new, &BTreeMap::new());
    let mut specs = BTreeMap::new();
    specs.insert(
        "A".to_string(),
        "GPU: NVIDIA GeForce RTX 3070 (diskret)".to_string(),
    );
    specs.insert(
        "MX".to_string(),
        "GPU: NVIDIA GeForce MX450 (diskret)".to_string(),
    );
    specs.insert(
        "T1".to_string(),
        "GPU: NVIDIA Quadro T1200 (diskret)".to_string(),
    );
    specs.insert(
        "Q".to_string(),
        "GPU: NVIDIA Quadro P620 (diskret)".to_string(),
    );
    let dropped = drop_low_end_parts(&mut diff, &specs);
    assert_eq!(dropped, 2);
    assert_eq!(
        diff.increased
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec!["A"]
    );
    let mut new_ids: Vec<&str> = diff.new_parts.iter().map(|p| p.0.as_str()).collect();
    new_ids.sort();
    assert_eq!(new_ids, vec!["N", "Q"]);
}

#[test]
fn test_nur_gefilterte_karte_keine_mail_aber_state() {
    // Nur eine MX-Karte steigt: Mail fällt weg, State wird trotzdem
    // geschrieben – sonst meldet der nächste Lauf den Anstieg erneut.
    let dir = std::env::temp_dir().join(format!("lenovotracker-emptymail-{}", std::process::id()));
    let state_path = dir.join("state.json");
    let history_path = dir.join("state-history.csv");
    let mut old = BTreeMap::new();
    old.insert(
        "MX".to_string(),
        PartState {
            s: 1,
            n: "Board MX".into(),
        },
    );
    old.insert(
        "A".to_string(),
        PartState {
            s: 2,
            n: "Board A".into(),
        },
    );
    let mut new_map = old.clone();
    new_map.insert(
        "MX".to_string(),
        PartState {
            s: 3,
            n: "Board MX".into(),
        },
    );
    let mut diff = build_diff(&old, &new_map, &BTreeMap::new());
    assert!(should_mail(&diff, &[])); // Bestand steigt → ohne Filter Mail
    let mut specs = BTreeMap::new();
    specs.insert(
        "MX".to_string(),
        "CPU: Intel Core i5-1135G7 | GPU: NVIDIA GeForce MX450 (diskret) | DPK: WIN".to_string(),
    );
    assert!(!filter_mail_parts(&mut diff, &specs, &[]));
    assert!(diff.increased.is_empty() && diff.new_parts.is_empty());
    // Der neue Stand (3 statt 1) wird übernommen, beide Teile bleiben drin.
    persist(
        &state_path,
        &history_path,
        &Snapshot {
            ts: "2026-11-26T00:00:00Z",
            total: diff.new_total,
            count: new_map.len(),
            filter: "c=system boards;k=",
            kw: &BTreeMap::new(),
            parts: &new_map,
            gone: &BTreeMap::new(),
            prices: &BTreeMap::new(),
        },
    )
    .expect("persist");
    let parts = load_state(&state_path, "c=system boards;k=")
        .expect("state")
        .parts;
    assert_eq!(parts.len(), 2);
    assert_eq!(parts["MX"].s, 3);
    assert!(std::fs::read_to_string(&history_path)
        .unwrap()
        .contains("2026-11-26T00:00:00Z"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_facets_for_ui_filters() {
    use crate::api::facets;
    let f = facets(
            "CPU: Intel Core i5-1135G7 | RAM: 16 GB | GPU: NVIDIA GeForce RTX 3060 (diskret) | DPK: WIN",
        );
    assert_eq!(f.cpu, "Intel Core i5-1135G7");
    assert_eq!(f.cpu_brand, "Intel");
    assert_eq!(f.cpu_series, "Core i5");
    assert_eq!(f.ram, 16);
    assert_eq!(f.gpu_family, "RTX 30");
    assert_eq!(f.gpu_kind, "diskret");
    assert_eq!(f.perf, performance_key(
            "CPU: Intel Core i5-1135G7 | RAM: 16 GB | GPU: NVIDIA GeForce RTX 3060 (diskret) | DPK: WIN"
        ));
    // Generationen, Workstation, Einstieg, integriert
    let fam = |gpu: &str| facets(&format!("GPU: {}", gpu)).gpu_family;
    assert_eq!(fam("NVIDIA GeForce RTX 4060 (diskret)"), "RTX 40");
    assert_eq!(fam("NVIDIA GeForce RTX 2070 (diskret)"), "RTX 20");
    assert_eq!(fam("NVIDIA GeForce GTX 1660 Ti (diskret)"), "GTX 16");
    assert_eq!(fam("NVIDIA GeForce GTX 1050 (diskret)"), "GTX 10");
    assert_eq!(fam("NVIDIA Quadro RTX 4000 (diskret)"), "Quadro");
    assert_eq!(fam("NVIDIA RTX A2000 (diskret)"), "Quadro");
    assert_eq!(fam("NVIDIA Quadro T1200 (diskret)"), "Quadro T");
    assert_eq!(fam("NVIDIA GeForce MX450 (diskret)"), "MX");
    assert_eq!(fam("AMD Radeon RX 6600M (diskret)"), "Radeon");
    assert_eq!(fam("AMD Radeon Pro W6600 (diskret)"), "Radeon Pro");
    assert_eq!(fam("integriert"), "integriert");
    // CPU-Reihen und leere Zeile
    let series = |cpu: &str| facets(&format!("CPU: {}", cpu)).cpu_series;
    assert_eq!(series("AMD Ryzen 7 6850H PRO"), "Ryzen 7");
    assert_eq!(series("INTELCELERON5205U"), "Celeron");
    assert_eq!(series("Intel Pentium N5030"), "Pentium");
    assert_eq!(series("AMD Athlon 300U"), "Athlon");
    // Jede CPU mit Text landet in einer Reihe – keine Lücke im Filter.
    assert_eq!(series("KAPUTT"), "sonstige");
    assert_eq!(series(""), "");
    assert_eq!(series("Intel Xeon E-2276M"), "Xeon");
    assert_eq!(series("INTELE2276M"), "Xeon");
    assert_eq!(series("INTELW10855M"), "Xeon");
    assert_eq!(series("INTELM38100Y"), "Core m");
    assert_eq!(series("INTELU300"), "Intel U-Serie");
    // Dieselben Codes ohne Vendor-Präfix, wie sie die echte API liefert.
    assert_eq!(series("E2276M"), "Xeon");
    assert_eq!(series("Intel E2276M"), "Xeon");
    assert_eq!(series("Intel m38100Y"), "Core m");
    assert_eq!(series("Intel U300"), "Intel U-Serie");
    assert_eq!(series("E31505M"), "Xeon");
    assert_eq!(series("W10855M"), "Xeon");
    assert_eq!(series("M38100Y"), "Core m");
    assert_eq!(series("U300"), "Intel U-Serie");
    assert_eq!(series("AMD 3020E"), "Athlon");
    // Generation: Intel nach Modellnummer, Ryzen in Tausenderschritten.
    let gen = |cpu: &str| facets(&format!("CPU: {}", cpu)).cpu_gen;
    assert_eq!(gen("INTELI712800H"), "12. Gen");
    assert_eq!(gen("Intel Core i5-1335U"), "13. Gen");
    assert_eq!(gen("Intel Core i3-8145U"), "8. Gen");
    assert_eq!(gen("AMDR75825U"), "Ryzen 5000");
    assert_eq!(gen("AMD Ryzen 5 7530U"), "Ryzen 7000");
    assert_eq!(gen("INTELCELERON5205U"), "");
    assert_eq!(gen(""), "");
    // Rohe Codes ohne Leerzeichen: Anzeige aufbereitet, Reihe erkannt
    let raw = facets("CPU: INTELI712800H");
    assert_eq!(raw.cpu, "Intel Core i7-12800H");
    assert_eq!(raw.cpu_series, "Core i7");
    let raw_amd = facets("CPU: AMDR72700U");
    assert_eq!(raw_amd.cpu, "AMD Ryzen 7 2700U");
    assert_eq!(raw_amd.cpu_series, "Ryzen 7");
    assert_eq!(facets("").ram, 0);
    assert_eq!(facets("").gpu_family, "");
    assert_eq!(facets("RAM: 8 GB").ram, 8);
}

#[test]
fn test_gpu_bonus_from_real_specs_cache() {
    // Alle 62 unterschiedlichen GPU-Namen aus der echten specs-cache.json
    // (CT 120, 750 Teile, Stand 2026-11-26). Bonus nur für RTX/GTX – MX,
    // GeForce 9xx, Radeon 5xx und Quadro T/P bleiben ohne.
    let bonus = [
        "GeForce RTX 2060 (diskret)",
        "GeForce RTX 2070 (diskret)",
        "GeForce RTX 2070 Super (diskret)",
        "NVIDIA GEFORCE GTX 1650 (diskret)",
        "NVIDIA GEFORCE RTX4060 (diskret)",
        "NVIDIA GeForce GTX 1050 (diskret)",
        "NVIDIA GeForce GTX 1050 Ti (diskret)",
        "NVIDIA GeForce GTX 1060 (diskret)",
        "NVIDIA GeForce GTX 1650 (diskret)",
        "NVIDIA GeForce GTX 1650 Ti (diskret)",
        "NVIDIA GeForce GTX 1660 Ti (diskret)",
        "NVIDIA GeForce RTX3050 (diskret)",
        "NVIDIA GeForce RTX3060 (diskret)",
        "NVIDIA GeForce RTX3070 (diskret)",
    ];
    let no_bonus = [
        // AMD-Einstieg
        "AMD RADEON 530 (diskret)",
        "AMD Radeon 530 (diskret)",
        "AMD Radeon 535 (diskret)",
        "AMD Radeon 540 (diskret)",
        "AMD Radeon 620 (diskret)",
        "AMD Radeon R5 M230 (diskret)",
        "AMD Radeon R5 M430 (diskret)",
        "AMD Radeon RX 550x (diskret)",
        "AMD Radeon RX 640 (diskret)",
        // MX-Serie und 940MX
        "NVIDIA GEFORCE MX130 (diskret)",
        "NVIDIA GEFORCE MX330 (diskret)",
        "NVIDIA GEFORCE MX450 (diskret)",
        "NVIDIA GEFORCE MX550 (diskret)",
        "NVIDIA GeForce 940MX (diskret)",
        "NVIDIA GeForce MX110 (diskret)",
        "NVIDIA GeForce MX130 (diskret)",
        "NVIDIA GeForce MX150 (diskret)",
        "NVIDIA GeForce MX230 (diskret)",
        "NVIDIA GeForce MX250 (diskret)",
        "NVIDIA GeForce MX330 (diskret)",
        "NVIDIA GeForce MX350 (diskret)",
        "NVIDIA GeForce MX450 (diskret)",
        "NVIDIA GeForce MX450B (diskret)",
        "NVIDIA GeForce MX550 (diskret)",
        "NVIDIA® GeForce MX330 (diskret)",
        // Quadro inkl. Großschreibung
        "NVIDIA QUADRO P620 (diskret)",
        "NVIDIA QUADRO T1200 (diskret)",
        "NVIDIA Quadro M1200M (diskret)",
        "NVIDIA Quadro M2200M (diskret)",
        "NVIDIA Quadro M520M (diskret)",
        "NVIDIA Quadro P1000 (diskret)",
        "NVIDIA Quadro P2000 (diskret)",
        "NVIDIA Quadro P4200 (diskret)",
        "NVIDIA Quadro P500 (diskret)",
        "NVIDIA Quadro P520 (diskret)",
        "NVIDIA Quadro P600 (diskret)",
        "NVIDIA Quadro P620 (diskret)",
        "NVIDIA Quadro RTX 3000 (diskret)",
        "NVIDIA Quadro RTX 4000 (diskret)",
        "NVIDIA Quadro RTX 5000 (diskret)",
        "NVIDIA Quadro T1000 (diskret)",
        "NVIDIA Quadro T1200 (diskret)",
        "NVIDIA Quadro T2000 (diskret)",
        "NVIDIA Quadro T500 (diskret)",
        "NVIDIA Quadro T600 (diskret)",
        // ohne Modellname bzw. ohne Familie
        "NVIDIA T500 (diskret)",
        "diskret",
        "integriert",
    ];
    assert_eq!(bonus.len() + no_bonus.len(), 62);
    for name in bonus {
        assert!(crate::api::has_gpu_bonus(name), "Bonus erwartet: {}", name);
    }
    for name in no_bonus.iter() {
        assert!(
            !crate::api::has_gpu_bonus(name),
            "kein Bonus erwartet: {}",
            name
        );
    }
}

#[test]
fn test_performance_key_cpu_und_ram() {
    // GPU zuerst, dann CPU (Klasse + Generation), dann RAM.
    let k = performance_key;
    assert!(
        k("GPU: NVIDIA GeForce RTX 3070 (diskret) | CPU: Intel Core i7-12700H | RAM: 8 GB")
            > k("GPU: NVIDIA GeForce RTX 3070 (diskret) | CPU: Intel Core i5-8250U | RAM: 64 GB")
    );
    assert!(k("CPU: Intel Core i7-13705H") > k("CPU: Intel Core i7-8550U"));
    assert!(k("CPU: AMD Ryzen 9 5900HX") > k("CPU: AMD Ryzen 7 5800H"));
    // Gleiche Generation: die SKU entscheidet (12 800H > 12 700H > 12 650H).
    assert!(k("CPU: Intel Core i7-12800H") > k("CPU: Intel Core i7-12700H"));
    assert!(k("CPU: Intel Core i7-12700H") > k("CPU: Intel Core i7-12650H"));
    // Vierstellige Modelle: Generation aus den ersten zwei Ziffern
    // (12 55U = 12. Gen) bzw. der ersten (8 250U = 8. Gen).
    assert!(k("CPU: Intel Core i5-1255U") > k("CPU: Intel Core i5-1135G7"));
    assert!(k("CPU: Intel Core i5-1135G7") > k("CPU: Intel Core i5-8250U"));
    assert!(k("CPU: AMD Ryzen 7 5825U") > k("CPU: AMD Ryzen 7 5700U"));
    // Cinebench R23: 12 800H (18 130 + 15 712) schlägt i9-9880H deutlich
    // (11 360 + 8 673) – das Tier allein sagt hier nichts.
    assert_eq!(k("CPU: Intel Core i7-12800H").1, 1_813 * 10 + 15_712);
    assert!(k("CPU: Intel Core i7-12800H") > k("CPU: Intel Core i9-9880H"));
    assert!(k("CPU: Intel Core i7-12800H") > k("CPU: Intel Core i9-11950H"));
    assert!(k("CPU: Intel Core i7-12700H") > k("CPU: Intel Core i7-8550U"));
    // Rohe Codes (INTELI712800H, AMDR75825U) zählen genauso – vorher
    // fiel dort die Modellnummer weg und alle Teile waren gleich schnell.
    assert!(k("CPU: INTELI712800H") > k("CPU: INTELI712700H"));
    assert!(k("CPU: INTELI712700H") > k("CPU: INTELI511300H"));
    assert!(k("CPU: AMDR75825U") > k("CPU: AMDR75700U"));
    assert_eq!(k("CPU: INTELI712800H"), k("CPU: Intel Core i7-12800H"));
    // Ohne Tabellentreffer (i5-7300U): Schätzung aus Serie/Generation,
    // damit unbekannte CPUs im selben Wertebereich bleiben.
    let est = k("CPU: Intel Core i5-7300U").1;
    assert!(
        est > 5_000 && est < 30_000,
        "Schätzung unplausibel: {}",
        est
    );
    assert!(
        k("GPU: NVIDIA GeForce RTX 3070 (diskret) | CPU: Intel Core i7-12700H | RAM: 32 GB")
            > k("GPU: NVIDIA GeForce RTX 3070 (diskret) | CPU: Intel Core i7-12700H | RAM: 8 GB")
    );
    // GPU schlägt CPU: starke GPU mit i5 vor schwacher GPU mit i9
    assert!(
        k("GPU: NVIDIA GeForce RTX 4060 (diskret) | CPU: Intel Core i5-8250U")
            > k("GPU: NVIDIA Quadro T500 (diskret) | CPU: Intel Core i9-9880H")
    );
    assert_eq!(k(""), (0, 0, 0));
}

/// Erzeugt eine Beispiel-Mail mit erfundenen Teilen (kein Versand) für die
/// Browser-Vorschau: `cargo test write_preview_mail -- --ignored --nocapture`.
#[test]
#[ignore]
fn write_preview_mail() {
    let mut old = BTreeMap::new();
    old.insert(
        "5B21L40810".to_string(),
        PartState {
            s: 1,
            n: "L83BY I713705H RTX406016GWINRM".into(),
        },
    );
    old.insert(
        "5B21H80248".to_string(),
        PartState {
            s: 1,
            n: "MBL82TFWINI712700H30708GB8GRM".into(),
        },
    );
    old.insert(
        "02DM481".to_string(),
        PartState {
            s: 1,
            n: "Lenovo BDPLANAR i9-9880H VP RTX4000 8G WINYAYdT".into(),
        },
    );
    let mut new = old.clone();
    new.insert(
        "5B21L40810".to_string(),
        PartState {
            s: 2,
            n: "L83BY I713705H RTX406016GWINRM".into(),
        },
    );
    new.insert(
        "5B21H80248".to_string(),
        PartState {
            s: 3,
            n: "MBL82TFWINI712700H30708GB8GRM".into(),
        },
    );
    new.insert(
        "02DM481".to_string(),
        PartState {
            s: 2,
            n: "Lenovo BDPLANAR i9-9880H VP RTX4000 8G WINYAYdT".into(),
        },
    );
    new.insert(
        "02DM024".to_string(),
        PartState {
            s: 1,
            n: "Lenovo BDPLANAR FRU FLBL R7 WIN NAMT YdTPM2.0".into(),
        },
    );
    new.insert(
        "5B21J77190".to_string(),
        PartState {
            s: 1,
            n: "L82DM I58265U 8GDDR4 NOOS".into(),
        },
    );
    let mut diff = build_diff(&old, &new, &BTreeMap::new());
    let mut specs = BTreeMap::new();
    specs.insert("5B21L40810".to_string(), "CPU: Intel Core i7-13705H | RAM: 16 GB | GPU: NVIDIA GeForce RTX 4060 (diskret) | DPK: WIN".to_string());
    specs.insert(
        "5B21H80248".to_string(),
        "CPU: Intel Core i7-12700H | RAM: 8 GB | GPU: NVIDIA GeForce RTX 3070 (diskret) | DPK: WIN"
            .to_string(),
    );
    specs.insert(
        "02DM024".to_string(),
        "CPU: AMD Ryzen 7 3700U | GPU: integriert | DPK: WIN | AMT: N-AMT".to_string(),
    );
    specs.insert(
        "02DM481".to_string(),
        "CPU: Intel Core i9-9880H | GPU: NVIDIA Quadro RTX 4000 (diskret) | DPK: WIN | AMT: Y-AMT"
            .to_string(),
    );
    let mut prices: PriceMap = BTreeMap::new();
    prices.insert("5B21L40810".to_string(), 59900);
    prices.insert("5B21H80248".to_string(), 42900);
    prices.insert("02DM024".to_string(), 4677);
    prices.insert("02DM481".to_string(), 32294);
    sort_by_performance(&mut diff, &specs);
    let view = MailView {
        diff: &diff,
        page_url: PAGE_URL,
        checked_at: "2026-11-26T15:20:00Z",
        specs: &specs,
        prices: &prices,
        currency: "€",
        filter_label: "system boards · RTX · R7 · I713",
        kw_moved: &[],
    };
    let (_, html) = build_mail(&view);
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../preview/beispiel-mail.html");
    std::fs::create_dir_all(path.parent().expect("preview-Ordner")).expect("preview/ anlegen");
    std::fs::write(&path, html).expect("Beispiel-Mail schreiben");
    eprintln!("Beispiel-Mail: {}", path.display());
}

#[test]
fn test_format_report() {
    let mut m = BTreeMap::new();
    m.insert(
        "B2".to_string(),
        PartState {
            s: 3,
            n: "rtx planar".into(),
        },
    );
    m.insert(
        "A1".to_string(),
        PartState {
            s: 12,
            n: "i7-12800H board".into(),
        },
    );
    let mut kw = BTreeMap::new();
    kw.insert("rtx".to_string(), KwStat { parts: 1, stock: 3 });
    let r = format_report(&m, &kw);
    assert!(r.contains("15 Stück über 2 Teile"));
    assert!(r.contains("rtx"));
    // A1 (12 Stück) vor B2 (3 Stück)
    assert!(r.find("A1").unwrap() < r.find("B2").unwrap());
    let r2 = format_report(&BTreeMap::new(), &BTreeMap::new());
    assert!(r2.contains("0 Stück über 0 Teile"));
}

#[test]
fn test_fail_state_roundtrip_and_reset() {
    let dir = std::env::temp_dir().join(format!("lenovotracker-fail-{}", std::process::id()));
    let p = dir.join("failstate.json");
    let mut st = load_fail_state(&p); // fehlt -> Default
    assert_eq!(st.failures, 0);
    assert!(!st.alerted);
    st.failures = 3;
    st.alerted = true;
    save_fail_state(&p, &st).expect("save");
    let loaded = load_fail_state(&p);
    assert_eq!(loaded.failures, 3);
    assert!(loaded.alerted);
    save_fail_state(&p, &FailState::default()).expect("reset");
    assert_eq!(load_fail_state(&p).failures, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_should_mail() {
    let mut m = BTreeMap::new();
    m.insert(
        "A".to_string(),
        PartState {
            s: 5,
            n: "x".into(),
        },
    );
    // unverändert -> keine Mail
    let flat = build_diff(&m, &m, &BTreeMap::new());
    assert!(!should_mail(&flat, &[]));
    // Gesamtbestand steigt -> Mail
    let mut m2 = m.clone();
    m2.insert(
        "A".to_string(),
        PartState {
            s: 7,
            n: "x".into(),
        },
    );
    let up = build_diff(&m, &m2, &BTreeMap::new());
    assert!(should_mail(&up, &[]));
    // Gesamt flach, aber Keyword-NEU -> Mail
    let kw = vec![(
        "r7".to_string(),
        KwStat { parts: 0, stock: 0 },
        KwStat { parts: 1, stock: 5 },
    )];
    assert!(should_mail(&flat, &kw));
}

#[test]
fn test_keyword_stats_and_changes() {
    let mut m = BTreeMap::new();
    m.insert(
        "A1".to_string(),
        PartState {
            s: 5,
            n: "AMD R7 5700U board".into(),
        },
    );
    m.insert(
        "B2".to_string(),
        PartState {
            s: 3,
            n: "rtx a920 planar".into(),
        },
    );
    // C3: Keyword steht nur in den technischen Daten
    m.insert(
        "C3".to_string(),
        PartState {
            s: 4,
            n: "planar board".into(),
        },
    );
    let mut specs = BTreeMap::new();
    specs.insert("C3".to_string(), "GPU: RTX 3000".to_string());
    let kws = vec!["R7".to_string(), "RTX".to_string()];
    let st = keyword_stats(&m, &specs, &kws);
    assert_eq!(st["r7"].parts, 1);
    assert_eq!(st["r7"].stock, 5);
    assert_eq!(st["rtx"].parts, 2);
    assert_eq!(st["rtx"].stock, 7);
    // Bewegung: nur RTX steigt (NEU), R7 unverändert
    let mut old = BTreeMap::new();
    old.insert("r7".to_string(), KwStat { parts: 1, stock: 5 });
    let moved = keyword_changes(&old, &st);
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].0, "rtx");
    assert_eq!(moved[0].1.stock, 0);
    assert_eq!(moved[0].2.stock, 7);
}

#[test]
fn test_smtp_target_ips() {
    // localhost liefert mindestens 127.0.0.1 (dedupliziert, <= 4 Einträge)
    let ips = smtp_target_ips("localhost");
    assert!(ips.contains(&IpAddr::from([127, 0, 0, 1])));
    assert!(ips.len() <= 4);
    let mut uniq = ips.clone();
    uniq.dedup();
    assert_eq!(ips, uniq);
    // nicht auflösbar -> leer (kein Panic)
    assert!(smtp_target_ips("nonexistent.invalid").is_empty());
}

#[test]
fn test_smtp_auth_config_parse() {
    // ohne smtp_user/smtp_pass: Defaults leer (Legacy Plain-SMTP aktiv)
    let base = "[api]\nurl=\"https://example.com\"\n[tracker]\n\
                    [mail]\nfrom=\"a@b.de\"\nto=[\"c@d.de\"]\nsmtp_host=\"mail.example.de\"\nsmtp_port=25\n\
                    [paths]\n";
    let cfg: Config = basic_toml::from_str(base).expect("Basis-Config muss parsen");
    assert_eq!(cfg.api.specs_gap_ms, 250);
    assert_eq!(cfg.api.specs_batch, 200);
    assert!(cfg.mail.smtp_user.is_empty());
    assert!(cfg.mail.smtp_pass.is_empty());
    assert_eq!(cfg.mail.smtp_port, 25);
    // mit Login: Felder landen in der Config (Submission über STARTTLS)
    let mail_part = base.trim_end_matches("[paths]\n");
    let with_auth = format!(
        "{}smtp_user=\"user@b.de\"\nsmtp_pass=\"secret\"\n[paths]\n",
        mail_part
    );
    let cfg2: Config = basic_toml::from_str(&with_auth).expect("Config mit Auth muss parsen");
    assert_eq!(cfg2.mail.smtp_user, "user@b.de");
    assert_eq!(cfg2.mail.smtp_pass, "secret");
    // Specs-Pause überschreibbar (CT-Test mit 100 ms)
    let with_gap = base.replace("[tracker]", "specs_gap_ms=100\nspecs_batch=60\n[tracker]");
    let cfg3: Config = basic_toml::from_str(&with_gap).expect("Config mit Pause muss parsen");
    assert_eq!(cfg3.api.specs_gap_ms, 100);
    assert_eq!(cfg3.api.specs_batch, 60);
}

#[test]
fn test_test_mail_design() {
    let text = build_test_mail_text(
        63,
        18,
        "SMTP mail.example.de (plain)",
        "2026-11-21T10:25:03Z",
    );
    assert!(text.contains("Installation fertig"));
    assert!(text.contains("63 Stück über 18 Teile"));
    assert!(text.contains("SMTP mail.example.de (plain)"));
    let html = build_test_mail_html(
        63,
        18,
        "system boards · RTX",
        "SMTP smtp.example.de (STARTTLS+AUTH)",
        "2026-11-21T10:25:03Z",
    );
    assert!(html.contains("LENOVO CLEARANCE-TRACKER"));
    assert!(html.contains("Installation fertig – der Tracker läuft ✓"));
    assert!(html.contains("63 Stück</b> über 18 Teile"));
    assert!(html.contains("system boards · RTX"));
    assert!(html.contains("STARTTLS+AUTH"));
    assert!(html.contains("Clearance-Sale ansehen"));
    assert!(html.contains("2026-11-21 10:25:03 UTC")); // pretty_ts
    assert!(html.contains("background:#e2231a"));
}

#[test]
fn test_state_roundtrip() {
    let dir = std::env::temp_dir().join(format!("lenovotracker-test-{}", std::process::id()));
    let path = dir.join("state.json");
    let mut map = BTreeMap::new();
    map.insert(
        "5A10W86303".to_string(),
        PartState {
            s: 98,
            n: "Board A".into(),
        },
    );
    map.insert(
        "X".to_string(),
        PartState {
            s: 2,
            n: "Board \"B\" <ö>".to_string(),
        },
    );
    let kw = BTreeMap::new();
    save_state(
        &path,
        &Snapshot {
            ts: "2026-11-19T00:00:00Z",
            total: 100,
            count: 2,
            filter: "c=system boards;k=",
            kw: &kw,
            parts: &map,
            gone: &BTreeMap::new(),
            prices: &BTreeMap::new(),
        },
    )
    .expect("save");
    // gleicher Filter → geladen
    let loaded = load_state(&path, "c=system boards;k=").expect("roundtrip");
    assert_eq!(loaded.parts, map);
    // anderer Filter → Baseline-Reset
    assert!(load_state(&path, "c=system boards;k=b650").is_none());
    // alter State ohne Filter-Feld → Baseline-Reset
    std::fs::write(
        &path,
        "{\"last_check\": \"x\", \"total_stock\": 1, \"part_count\": 1, \"parts\": {}}",
    )
    .unwrap();
    assert!(load_state(&path, "c=system boards;k=").is_none());
    // kaputte Datei → None, keine Panic
    std::fs::write(&path, "{\"parts\": 42}").unwrap();
    assert!(load_state(&path, "c=system boards;k=").is_none());
    std::fs::write(&path, "{\"last_check\": \"x\"}").unwrap(); // parts fehlt
    assert!(load_state(&path, "c=system boards;k=").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_specs_cache_roundtrip() {
    let dir = std::env::temp_dir().join(format!("lenovotracker-specs-{}", std::process::id()));
    let path = dir.join("specs-cache.json");
    assert!(load_specs_cache(&path).is_empty()); // Datei fehlt → leer
    let mut cache = BTreeMap::new();
    cache.insert("A".to_string(), "CPU: Intel Core i7-9750H".to_string());
    cache.insert("B".to_string(), String::new()); // geprüft, aber keine Specs
    save_specs_cache(&path, &cache).expect("save");
    assert_eq!(load_specs_cache(&path), cache);
    // kaputte Datei → leer statt Panic (Cache wird neu aufgebaut)
    std::fs::write(&path, "kaputt").unwrap();
    assert!(load_specs_cache(&path).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
