//! Tests der Web-UI: Antwortform, NEU-Marke, Est-Wert, Live-Speicher,
//! Pause/Wecken, Config-Redaktion und Anmeldung.

use super::*;
use crate::{api::*, diff::*, guard::*, state::*};
use std::path::Path;
use std::time::{Duration, Instant};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lt-webui-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const CONFIG: &str = r#"
[api]
url = "https://support.lenovo.com/de/de/api/v4/lbom/clearanceParts/get"
curl_path = "/bin/true"

[tracker]
commodity_keys = ["system boards"]
name_keywords = []

[mail]
from = "a@example.de"
to = ["a@example.de"]
smtp_host = "smtp.example.de"
smtp_pass = "geheim"

[paths]
state_dir = "/tmp/lt-webui-test"
"#;

fn config_for(dir: &Path) -> Config {
    let mut cfg = parse_config(CONFIG).expect("Config");
    cfg.paths.state_dir = dir.display().to_string();
    cfg
}

fn request(method: &str, path: &str) -> Request {
    Request {
        method: method.to_string(),
        path: path.to_string(),
        query: String::new(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    }
}

#[test]
fn test_redact_keeps_password() {
    let text = redact(CONFIG);
    assert!(text.contains("smtp_pass = \"__KEEP__\""));
    assert!(!text.contains("geheim"));
    let back = unredact(&text, CONFIG);
    assert!(back.contains("smtp_pass = \"geheim\""));
    assert_eq!(parse_config(&back).unwrap().mail.smtp_pass, "geheim");
    // ohne Platzhalter bleibt der Text unverändert
    let plain = CONFIG.replace("geheim", "neu");
    assert_eq!(unredact(&plain, CONFIG), plain);
}

#[test]
fn test_state_json_lists_parts_and_specs() {
    let dir = temp_dir("state");
    let cfg = config_for(&dir);
    let (state_path, _) = state_paths(&cfg);
    std::fs::write(
        &state_path,
        r#"{"last_check":"2026-11-26T09:00:00Z","total_stock":7,"part_count":2,
                "filter":"c=system boards;k=","kw":{},
                "parts":{"A":{"s":5,"n":"Board A"},"B":{"s":2,"n":"Board B"}},
                "gone":{"C":{"s":4,"n":"Board C","t":"2026-11-26T08:00:00Z"}}}"#,
    )
    .unwrap();
    std::fs::write(
        specs_cache_path(&cfg),
        r#"{"A":"CPU: Intel Core i5-8250U | GPU: integriert"}"#,
    )
    .unwrap();

    let resp = route(&cfg, "/dev/null", &request("GET", "/api/state"));
    assert_eq!(resp.status, 200);
    let body = String::from_utf8(resp.body).unwrap();
    assert!(body.contains("\"total_stock\":7"));
    assert!(body.contains("\"id\":\"A\""));
    assert!(body.contains("Intel Core i5-8250U"));
    // Die Antwort trägt keine eigene `gone`-Liste mehr – die Seite nutzt sie nicht,
    // der Bestand zurückgekehrter Teile steckt weiter im `delta` (`parts_json`).
    assert!(!body.contains("\"gone\""));
}

#[test]
fn hintergrund_pausiert_ohne_seitenaufruf() {
    assert!(
        !aktiv(None, IDLE_AFTER),
        "ohne Aufruf kein Hintergrund-Abruf"
    );
    assert!(aktiv(Some(Instant::now()), IDLE_AFTER));
    assert!(
        aktiv(
            Some(Instant::now() - IDLE_AFTER + Duration::from_secs(30)),
            IDLE_AFTER
        ),
        "kurz vor Ablauf läuft er noch"
    );
    assert!(
        !aktiv(Some(Instant::now() - IDLE_AFTER), IDLE_AFTER),
        "nach IDLE_AFTER pausiert er"
    );
}

#[test]
fn stand_aus_der_pause_gilt_nicht_mehr_als_frisch() {
    let dir = temp_dir("snapshot-alter");
    let cfg = config_for(&dir);
    let body = br#"{"source":"live"}"#.to_vec();
    *lock(&SNAPSHOT) = Some(LiveSnapshot {
        key: snapshot_key(&cfg),
        body: body.clone(),
        at: Instant::now() - Duration::from_secs(90),
    });
    // Im Betrieb überbrückt ein 90 s alter Stand einen Aussetzer …
    assert_eq!(snapshot_live(&cfg, STALE_OK), Some(body));
    // … nach der Pause zählt er nicht mehr (dort gilt nur ein Takt).
    assert!(snapshot_live(&cfg, REFRESH_EVERY).is_none());
}

#[test]
fn live_antwort_kommt_aus_dem_speicher() {
    let dir = temp_dir("live-cache");
    let cfg = config_for(&dir);
    let body = br#"{"source":"live","part_count":7}"#.to_vec();
    store_snapshot(&cfg, body.clone());
    // Kein Lenovo-Abruf: die Antwort ist der gespeicherte Stand (curl_path
    // zeigt auf /bin/true und würde sonst scheitern).
    let resp = route(&cfg, "/dev/null", &request("POST", "/api/live"));
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, body);
    assert_eq!(resp.ctype, "application/json; charset=utf-8");
    // Ein anderer State-Ordner (andere Installation) nutzt den Stand nicht.
    let other = config_for(&temp_dir("live-cache-2"));
    assert!(snapshot_live(&other, STALE_OK).is_none());
}

#[test]
fn parts_json_markiert_nur_unbekannte_teile_als_neu() {
    let mut parts = BTreeMap::new();
    for (id, s) in [("NEU", 2), ("ALT", 5), ("ZURUECK", 1)] {
        parts.insert(id.to_string(), PartState { s, n: id.into() });
    }
    let mut base = BTreeMap::new();
    base.insert(
        "ALT".to_string(),
        PartState {
            s: 3,
            n: "ALT".into(),
        },
    );
    let mut gone = BTreeMap::new();
    gone.insert(
        "ZURUECK".to_string(),
        GonePart {
            s: 1,
            n: "ZURUECK".into(),
            t: "2026-11-26T08:00:00Z".into(),
        },
    );
    let specs: BTreeMap<String, String> = BTreeMap::new();
    let produkte: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let rows = parts_json(
        &parts,
        &specs,
        PartPrices::default(),
        Some(&base),
        &gone,
        &[],
        &produkte,
    );
    let neu = |id: &str| {
        rows.iter()
            .find(|r| r["id"] == id)
            .and_then(|r| r["new"].as_bool())
            .unwrap_or(false)
    };
    assert!(neu("NEU"), "unbekanntes Teil ist neu");
    assert!(!neu("ALT"), "bekanntes Teil ist nicht neu");
    assert!(!neu("ZURUECK"), "zurückgekehrtes Teil ist nicht neu");
    // Neue Teile haben kein Delta (es wäre der ganze Bestand).
    let delta = |id: &str| rows.iter().find(|r| r["id"] == id).unwrap()["delta"].clone();
    assert!(delta("NEU").is_null());
    assert_eq!(delta("ALT"), 2);
    assert_eq!(delta("ZURUECK"), 0);
    // Ohne Basis (State-Ansicht) kann nichts „neu“ sein.
    let ohne_basis = parts_json(
        &parts,
        &specs,
        PartPrices::default(),
        None,
        &gone,
        &[],
        &produkte,
    );
    assert!(ohne_basis.iter().all(|r| r["new"] == false));
}

#[test]
fn parts_json_schickt_est_wert_als_listenpreis_mit() {
    let mut parts = BTreeMap::new();
    parts.insert(
        "A".to_string(),
        PartState {
            s: 2,
            n: "Board A".into(),
        },
    );
    let mut preise = PriceMap::new();
    preise.insert("A".to_string(), 11585);
    let mut listen = PriceMap::new();
    listen.insert("A".to_string(), 189920);
    let specs: BTreeMap<String, String> = BTreeMap::new();
    let produkte: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let gone: BTreeMap<String, GonePart> = BTreeMap::new();
    let rows = parts_json(
        &parts,
        &specs,
        PartPrices {
            sale: Some(&preise),
            list: Some(&listen),
        },
        None,
        &gone,
        &[],
        &produkte,
    );
    assert_eq!(rows[0]["price"], 11585);
    assert_eq!(rows[0]["list_price"], 189920);
    // Ohne Est-Wert im Abruf bleibt das Feld null (Karte zeigt nur einen Preis).
    let ohne = parts_json(
        &parts,
        &specs,
        PartPrices {
            sale: Some(&preise),
            list: None,
        },
        None,
        &gone,
        &[],
        &produkte,
    );
    assert!(ohne[0]["list_price"].is_null());
}

#[test]
fn test_tracked_view_lists_all_parts_and_marks_mail() {
    let dir = temp_dir("tracked");
    let mut cfg = config_for(&dir);
    cfg.tracker.name_keywords = vec!["RTX".into()];
    let (tracked, hist) = tracked_paths(&cfg);
    std::fs::write(
        &tracked,
        r#"{"last_check":"2026-11-26T09:30:00Z","filter":"c=system boards",
                "total_stock":12,"part_count":3,
                "parts":{"A":{"s":5,"n":"Board RTX 3000"},"B":{"s":4,"n":"Board i5"},
                          "C":{"s":3,"n":"Board RTX"}},
                "prices":{"A":10000}}"#,
    )
    .unwrap();
    std::fs::write(
        &hist,
        "timestamp,total_stock,part_count\n2026-11-26T09:30:00Z,12,3\n",
    )
    .unwrap();
    std::fs::write(
        specs_cache_path(&cfg),
        r#"{"A":"CPU: Intel Core i5-8250U","B":"CPU: Intel Core i5-8250U"}"#,
    )
    .unwrap();

    let resp = route(&cfg, "/dev/null", &request("GET", "/api/state"));
    let body = String::from_utf8(resp.body).unwrap();
    assert!(body.contains("\"scope\":\"all\""));
    assert!(body.contains("\"part_count\":3"));
    assert!(
        body.contains("\"id\":\"B\""),
        "alle Teile, nicht nur Mail-Treffer"
    );
    assert!(body.contains("\"mail_count\":2"));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let marked = v["parts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["mail"].as_bool().unwrap())
        .count();
    assert_eq!(marked, 2);

    // Der Verlauf kommt aus tracked-history.csv (alle Teile).
    let h = route(&cfg, "/dev/null", &request("GET", "/api/history"));
    let hb = String::from_utf8(h.body).unwrap();
    assert!(hb.contains("\"total\":12"));
}

#[test]
fn test_pending_is_reported() {
    let dir = temp_dir("pending");
    let cfg = config_for(&dir);
    crate::guard::save_pending(
        &pending_path(&cfg),
        &crate::guard::Pending {
            parts: 20,
            total: 80,
            ts: "2026-11-26T08:10:00Z".into(),
        },
    )
    .unwrap();
    let resp = route(&cfg, "/dev/null", &request("GET", "/api/state"));
    let body = String::from_utf8(resp.body).unwrap();
    assert!(body.contains("\"pending\":{\"parts\":20"));
}

#[test]
fn test_config_post_writes_valid_toml() {
    let dir = temp_dir("config");
    let path = dir.join("config.toml");
    std::fs::write(&path, CONFIG).unwrap();
    let original = redact(CONFIG);
    let edited = original.replace("name_keywords = []", "name_keywords = [\"RTX\"]");
    let body = serde_json::to_vec(&json!({ "text": edited })).unwrap();
    let mut req = request("POST", "/api/config");
    req.body = body;
    let resp = route(&parse_config(CONFIG).unwrap(), path.to_str().unwrap(), &req);
    assert_eq!(resp.status, 200, "{}", String::from_utf8_lossy(&resp.body));
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("name_keywords = [\"RTX\"]"));
    assert!(saved.contains("smtp_pass = \"geheim\"")); // Passwort erhalten
    assert!(dir.join("config.toml.bak").exists());
}

#[test]
fn test_config_post_rejects_broken_toml() {
    let dir = temp_dir("broken");
    let path = dir.join("config.toml");
    std::fs::write(&path, CONFIG).unwrap();
    let mut req = request("POST", "/api/config");
    req.body = serde_json::to_vec(&json!({ "text": "kein toml [" })).unwrap();
    let resp = route(&parse_config(CONFIG).unwrap(), path.to_str().unwrap(), &req);
    assert_eq!(resp.status, 400);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CONFIG);
}

#[test]
fn test_history_and_unknown_route() {
    let dir = temp_dir("history");
    let cfg = config_for(&dir);
    let (_, history) = state_paths(&cfg);
    std::fs::write(
            &history,
            "timestamp,total_stock,part_count\n2026-11-26T08:00:00Z,254,50\n2026-11-26T08:10:00Z,166,38\n",
        )
        .unwrap();
    let resp = route(&cfg, "/dev/null", &request("GET", "/api/history"));
    let body = String::from_utf8(resp.body).unwrap();
    assert!(body.contains("\"total\":166"));
    assert_eq!(
        route(&cfg, "/dev/null", &request("GET", "/nope")).status,
        404
    );
}

#[test]
fn test_base64_and_token() {
    // "user:geheim" → dXNlcjpnZWhlaW0=
    assert_eq!(base64_decode("dXNlcjpnZWhlaW0=").unwrap(), b"user:geheim");
    assert_eq!(base64_decode("dXNl\ncjpn ZWhlaW0").unwrap(), b"user:geheim");
    assert!(base64_decode("###").is_none());
    let mut cfg = parse_config(CONFIG).unwrap();
    cfg.webui.token = "geheim".into();
    let mut headers = BTreeMap::new();
    assert!(!authorized(&cfg, &headers));
    headers.insert("authorization".into(), "Basic dXNlcjpnZWhlaW0=".into());
    assert!(authorized(&cfg, &headers));
    headers.insert("authorization".into(), "Basic dXNlcjpmYWxzY2g=".into());
    assert!(!authorized(&cfg, &headers));
}

#[test]
fn test_loopback_bind() {
    assert!(is_loopback_bind("127.0.0.1:8787"));
    assert!(is_loopback_bind("localhost:8787"));
    assert!(is_loopback_bind("[::1]:8787"));
    assert!(!is_loopback_bind("0.0.0.0:8787"));
    assert!(!is_loopback_bind("192.168.1.5:8787"));
}
