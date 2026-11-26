//! API-Abruf via curl-impersonate (GET Liste, POST Detail-Specs).

use crate::config::*;
use crate::diff::{ci_contains, commodity_key_matches, PartState};
use crate::util::*;
use anyhow::{anyhow, Result};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// API-Abruf
// ---------------------------------------------------------------------------

/// Robust gegen fehlende/garbage/Text-Zahlen – wie die Python-Version.
/// Direkte Visitor-Impl statt Value-Zwischenschritt: keine Allocation pro Teil.
#[derive(Debug, Clone, Copy, Default)]
pub struct StockField(pub i64);

impl<'de> Deserialize<'de> for StockField {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct StockVisitor;

        impl<'de> serde::de::Visitor<'de> for StockVisitor {
            type Value = i64;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Zahl oder zahlähnlicher Text")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<i64, E> {
                Ok(v.max(0))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<i64, E> {
                // u64 > i64::MAX: wie die alte f64-Saturierung → i64::MAX
                Ok(i64::try_from(v).unwrap_or(i64::MAX).max(0))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<i64, E> {
                Ok((v as i64).max(0)) // saturierender Cast, NaN → 0
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<i64, E> {
                Ok(v.trim().parse::<i64>().unwrap_or(0).max(0))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<i64, E> {
                Ok(0)
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<i64, E> {
                Ok(0)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<i64, A::Error> {
                // Elemente konsumieren, sonst beschweren sich Deserializer
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(0)
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<i64, A::Error> {
                while map
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(0)
            }
        }

        Ok(StockField(d.deserialize_any(StockVisitor)?))
    }
}

/// Stückpreis je Teil in Cent (Aktionspreis, sonst Listenpreis). Die Währung
/// steht nicht im Schema, sie folgt dem Locale der API-URL (Lenovo-API.md §9).
/// Getrennt vom State: Preise sind kein Diff-Kriterium, nur für die Mail.
pub type PriceMap = BTreeMap<String, i64>;

/// f64 → Cent; negative/NaN/Infinity-Werte werden zu 0 (Rust-Casts sättigen).
pub fn to_cents(value: f64) -> i64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    (value * 100.0).round() as i64
}

/// Ergebnis des gefilterten Parse-Vorgangs: der Envelope-Code (Fehlerprüfung),
/// die Gesamtzahl der Teile in `data` (Empty-Check) und die Teile, die den
/// Commodity-Filter bestanden haben – direkt als fertige Map. Keyword-Filter
/// und Specs-Anreicherung laufen danach (die Liste enthält keine Specs).
pub struct FetchOutcome {
    pub code: i64,
    pub total_parts: usize,
    pub boards: BTreeMap<String, PartState>,
    /// Aktions-/Listenpreis je Teil (nur Teile mit Preisangabe).
    pub prices: PriceMap,
}

/// Seed für den Envelope `{code, msg, data: [...]}`. Filtert während des
/// Parsens auf `commodityKey`: nicht-mathende Teile werden übersprungen, ohne
/// `name`/`stock` zu materialisieren (commodityKey kommt im echten JSON vor
/// name) – die ~6.000 Nicht-Boards allokieren so fast nichts. Keywords werden
/// erst nach dem Specs-Abruf angewendet: die Listen-API liefert keine
/// technischen Daten, und die stehen im `name` nicht immer.
#[derive(Clone, Copy)]
pub struct FilterSeed<'a> {
    keys: &'a [String],
}

impl<'de, 'a> DeserializeSeed<'de> for FilterSeed<'a> {
    type Value = FetchOutcome;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(EnvelopeVisitor { seed: self })
    }
}

/// Bis zu dieser Größe wird der Body im RAM gepuffert und über den
/// Slice-Pfad geparst – serde_json ist dort rund 3× schneller als im
/// IoRead-Pfad (gemessen am 3,3-MB-Body: 14,5 ms → 4,6 ms inkl. Puffern).
/// Größere Bodies streamen weiter, damit der Speicherbedarf gedeckelt bleibt.
const BODY_RAM_MAX: usize = 8 * 1024 * 1024;

/// Lese-Chunk für den Body. 256 KiB statt 64 KiB: bei 3,3 MB sind das ~13
/// statt ~50 read-Syscalls (auf Windows messbar). Chunk- und Body-Puffer sind
/// die einzigen großen Blöcke; bei kleinem `cap` (Test/Overflow) schrumpft
/// der Chunk mit.
const READ_CHUNK: usize = 256 * 1024;

/// Obergrenze der Vorab-Reservierung: die realen ~3,3 MB passen in einen
/// Block, statt beim Wachsen mehrfach umkopiert zu werden (~4 MB Kopien je
/// Lauf). Reserviert wird nur virtueller Speicher – RSS wächst mit den
/// tatsächlich gelesenen Bytes.
const READ_RESERVE: usize = 4 * 1024 * 1024;

/// Liest bis zu `cap` Bytes. `true` im zweiten Feld heißt: der Body war
/// größer, der Rest steckt noch im Reader.
fn read_capped<R: Read>(mut reader: R, cap: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let chunk_len = READ_CHUNK.min(cap);
    let mut buf: Vec<u8> = Vec::with_capacity(cap.min(READ_RESERVE));
    let mut chunk = vec![0u8; chunk_len];
    loop {
        // Nie über das Limit hinaus lesen – sonst wären die Bytes dahinter
        // konsumiert und für den Streaming-Fallback verloren.
        let room = cap - buf.len();
        if room == 0 {
            return Ok((buf, true));
        }
        let want = room.min(chunk.len());
        let n = reader.read(&mut chunk[..want])?;
        if n == 0 {
            return Ok((buf, false));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// Parst einen API-Body aus einem Reader – der Pfad aus `fetch_once` und
/// Grundlage von `--bench-parse`. Bis `BODY_RAM_MAX` läuft der Parse über
/// `from_str` (einmaliger UTF-8-Check statt Validierung pro String, kein
/// IoRead-Overhead pro Token); darüber wird der bereits gelesene Präfix an
/// den Rest gestreamt. Der Reader wird in jedem Fall bis EOF geleert, damit
/// curl nicht beim Schreiben blockiert.
pub fn parse_clearance_from<R: Read>(reader: R, keys: &[String]) -> Result<FetchOutcome> {
    parse_clearance_capped(reader, BODY_RAM_MAX, keys)
}

/// Wie `parse_clearance_from`, aber mit explizitem Puffer-Limit – so lässt sich
/// der Streaming-Fallback im Test mit einem winzigen Limit erzwingen.
pub(crate) fn parse_clearance_capped<R: Read>(
    mut reader: R,
    cap: usize,
    keys: &[String],
) -> Result<FetchOutcome> {
    let (buf, overflow) =
        read_capped(&mut reader, cap).map_err(|e| anyhow!("Body nicht lesbar: {}", e))?;
    if !overflow {
        let text = std::str::from_utf8(&buf).map_err(|e| anyhow!("Body kein UTF-8: {}", e))?;
        let mut de = serde_json::Deserializer::from_str(text);
        return FilterSeed { keys }
            .deserialize(&mut de)
            .map_err(|e| anyhow!("JSON nicht parsebar: {}", e));
    }
    let mut reader =
        std::io::BufReader::with_capacity(64 * 1024, std::io::Cursor::new(buf).chain(reader));
    let mut de = serde_json::Deserializer::from_reader(&mut reader);
    let result = FilterSeed { keys }.deserialize(&mut de);
    // Rest bis EOF leeren (Parse-Abbruch darf curl nicht blockieren).
    let _ = std::io::copy(&mut reader, &mut std::io::sink());
    result.map_err(|e| anyhow!("JSON nicht parsebar: {}", e))
}

/// Parst einen Envelope-String mit aktivem Filter (ohne Netz) – Test-/Diagnose-Helfer.
/// Die Produktion nutzt `fetch_once` mit Streaming-Reader direkt aus der Pipe.
pub fn parse_clearance(text: &str, keys: &[String]) -> FetchOutcome {
    let mut de = serde_json::Deserializer::from_str(text);
    FilterSeed { keys }
        .deserialize(&mut de)
        .unwrap_or(FetchOutcome {
            code: -1,
            total_parts: 0,
            boards: BTreeMap::new(),
            prices: BTreeMap::new(),
        })
}

struct EnvelopeVisitor<'a> {
    seed: FilterSeed<'a>,
}

impl<'de, 'a> Visitor<'de> for EnvelopeVisitor<'a> {
    type Value = FetchOutcome;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lenovo-API-Envelope {code, data: [..]}")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut code: i64 = 0;
        let mut total_parts: usize = 0;
        let mut boards: BTreeMap<String, PartState> = BTreeMap::new();
        let mut prices: PriceMap = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "code" => code = map.next_value()?,
                "data" => {
                    map.next_value_seed(ListSeed {
                        seed: self.seed,
                        total: &mut total_parts,
                        boards: &mut boards,
                        prices: &mut prices,
                    })?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(FetchOutcome {
            code,
            total_parts,
            boards,
            prices,
        })
    }
}

struct ListSeed<'a> {
    seed: FilterSeed<'a>,
    total: &'a mut usize,
    boards: &'a mut BTreeMap<String, PartState>,
    prices: &'a mut PriceMap,
}

impl<'de, 'a> DeserializeSeed<'de> for ListSeed<'a> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(ListVisitor { seed: self })
    }
}

struct ListVisitor<'a> {
    seed: ListSeed<'a>,
}

impl<'de, 'a> Visitor<'de> for ListVisitor<'a> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Array von Teilen")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let ListVisitor { seed } = self;
        let ListSeed {
            seed,
            total,
            boards,
            prices,
        } = seed;
        while seq
            .next_element_seed(PartSeed {
                seed,
                total: &mut *total,
                boards: &mut *boards,
                prices: &mut *prices,
            })?
            .is_some()
        {}
        Ok(())
    }
}

struct PartSeed<'a> {
    seed: FilterSeed<'a>,
    total: &'a mut usize,
    boards: &'a mut BTreeMap<String, PartState>,
    prices: &'a mut PriceMap,
}

impl<'de, 'a> DeserializeSeed<'de> for PartSeed<'a> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(PartVisitor { seed: self })
    }
}

/// Feldnamen als enum ohne String-Allokation pro Key (14 Felder × ~9.255 Teile).
enum PartField {
    Id,
    Name,
    CommodityKey,
    Stock,
    Price,
    PromotionPrice,
    Other,
}

struct PartFieldSeed;

impl<'de> DeserializeSeed<'de> for PartFieldSeed {
    type Value = PartField;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        struct FieldVisitor;

        impl<'de> Visitor<'de> for FieldVisitor {
            type Value = PartField;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Feldname")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<PartField, E> {
                Ok(match v {
                    "id" => PartField::Id,
                    "name" => PartField::Name,
                    "commodityKey" => PartField::CommodityKey,
                    "stock" => PartField::Stock,
                    "price" => PartField::Price,
                    "promotionPrice" => PartField::PromotionPrice,
                    _ => PartField::Other,
                })
            }
        }

        d.deserialize_identifier(FieldVisitor)
    }
}

/// Vergleicht den `commodityKey` bereits im Visitor – Nicht-Treffer
/// allokieren dadurch keinen String.
struct CommodityKey<'a> {
    keys: &'a [String],
}

impl<'de, 'a> DeserializeSeed<'de> for CommodityKey<'a> {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de, 'a> Visitor<'de> for CommodityKey<'a> {
    type Value = bool;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("commodityKey als String")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<bool, E> {
        Ok(commodity_key_matches(v, self.keys))
    }
}

/// Teilenummer beim Parsen: kurze IDs liegen inline im Stack, damit Nicht-Treffer
/// keinen String allokieren; nur längere fallen auf den Heap zurück.
const ID_INLINE: usize = 32;

#[derive(Default)]
struct PartId {
    inline: [u8; ID_INLINE],
    len: usize,
    heap: Option<String>,
}

impl PartId {
    fn from_str(v: &str) -> Self {
        if v.len() <= ID_INLINE {
            let mut id = PartId::default();
            id.inline[..v.len()].copy_from_slice(v.as_bytes());
            id.len = v.len();
            id
        } else {
            PartId {
                heap: Some(v.to_string()),
                ..PartId::default()
            }
        }
    }

    fn as_str(&self) -> &str {
        match &self.heap {
            Some(h) => h.as_str(),
            // Bytes wurden aus einem &str kopiert – immer gültiges UTF-8.
            None => std::str::from_utf8(&self.inline[..self.len]).unwrap_or(""),
        }
    }
}

impl<'de> Deserialize<'de> for PartId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct IdVisitor;

        impl<'de> Visitor<'de> for IdVisitor {
            type Value = PartId;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("Teilenummer als String")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<PartId, E> {
                Ok(PartId::from_str(v))
            }
        }

        d.deserialize_str(IdVisitor)
    }
}

struct PartVisitor<'a> {
    seed: PartSeed<'a>,
}

impl<'de, 'a> Visitor<'de> for PartVisitor<'a> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Teil-Objekt")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let PartVisitor { seed } = self;
        let PartSeed {
            seed,
            total,
            boards,
            prices,
        } = seed;
        let mut id: Option<PartId> = None;
        let mut name: Option<String> = None;
        let mut has_commodity = false;
        let mut stock: i64 = 0;
        let mut price: f64 = 0.0;
        let mut promo: f64 = 0.0;
        let mut skip = false;
        while let Some(field) = map.next_key_seed(PartFieldSeed)? {
            if skip {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            match field {
                PartField::Id => id = Some(map.next_value()?),
                PartField::Name => name = Some(map.next_value()?),
                PartField::CommodityKey => {
                    has_commodity = true;
                    // commodityKey steht im echten JSON vor `name`: ab hier können
                    // nicht-mathende Teile komplett ohne String-Allokation übersprungen
                    // werden (Vergleich im Visitor).
                    if !map.next_value_seed(CommodityKey { keys: seed.keys })? {
                        skip = true;
                    }
                }
                PartField::Stock => stock = map.next_value::<StockField>()?.0,
                // Preise sind laut Schema immer Zahl oder null (Lenovo-API.md §6).
                PartField::Price => price = map.next_value::<Option<f64>>()?.unwrap_or(0.0),
                PartField::PromotionPrice => {
                    promo = map.next_value::<Option<f64>>()?.unwrap_or(0.0)
                }
                PartField::Other => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        *total += 1;
        if skip || !has_commodity {
            return Ok(());
        }
        let Some(id) = id else { return Ok(()) };
        let id = id.as_str();
        if id.is_empty() {
            return Ok(());
        }
        let name = name.unwrap_or_default();
        boards.insert(id.to_string(), PartState { s: stock, n: name });
        // Aktionspreis, sonst Listenpreis; 0 heißt „kein Preis bekannt".
        let unit = match to_cents(promo) {
            0 => to_cents(price),
            sale => sale,
        };
        if unit > 0 {
            prices.insert(id.to_string(), unit);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct FetchError {
    pub message: String,
    pub status: Option<u32>,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Startet curl-impersonate: Body → stdout-Pipe, HTTP-Status als
/// "HTTP:<code>"-Zeile → stderr. Der Body wird direkt aus der Pipe geparst –
/// kein Temp-File, kein Disk-I/O, kein ENOSPC-Risiko im Transfer.
/// Der Caller muss stdout bis EOF lesen, bevor er `finish_curl` aufruft.
pub fn spawn_curl(
    cfg: &Config,
    method: Option<&str>,
    data: Option<&str>,
    headers: &[(&str, &str)],
    url: &str,
) -> Result<(
    std::process::Child,
    std::process::ChildStdout,
    std::thread::JoinHandle<String>,
)> {
    let mut cmd = std::process::Command::new(&cfg.api.curl_path);
    cmd.arg("--impersonate")
        .arg(&cfg.api.impersonate_target)
        .arg("--compressed")
        .arg("-s")
        .arg("--max-time")
        .arg(cfg.api.timeout_seconds.to_string());
    if let Some(m) = method {
        cmd.arg("-X").arg(m);
    }
    for (k, v) in headers {
        cmd.arg("-H").arg(format!("{}: {}", k, v));
    }
    if let Some(d) = data {
        cmd.arg("-d").arg(d);
    }
    // -o - : Body auf stdout; %{stderr}… : Statuszeile auf stderr, damit stdout
    // nur den JSON-Body enthält und direkt geparst werden kann.
    cmd.arg("-o")
        .arg("-")
        .arg("-w")
        .arg("%{stderr}HTTP:%{http_code}\n")
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("curl konnte nicht gestartet werden: {}", e))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("curl-stdout nicht verfügbar"))?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("curl-stderr nicht verfügbar"))?;
    // stderr parallel drainen (eigener Thread), damit der Pipe-Buffer nie
    // volläuft und curl beim Schreiben blockieren kann.
    let stderr_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stderr_pipe, &mut s);
        s
    });
    Ok((child, stdout, stderr_thread))
}

/// curl beenden und den HTTP-Status aus der stderr-Markierung lesen.
/// Liefert (status, stderr ohne Markierungszeile).
pub fn finish_curl(
    child: &mut std::process::Child,
    stderr_thread: std::thread::JoinHandle<String>,
) -> (u32, String) {
    let _ = child.wait();
    let text = stderr_thread.join().unwrap_or_default();
    let status = text
        .lines()
        .find_map(|l| l.strip_prefix("HTTP:"))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let cleaned: String = text
        .lines()
        .filter(|l| !l.starts_with("HTTP:"))
        .collect::<Vec<_>>()
        .join("\n");
    (status, cleaned.trim().to_string())
}

pub fn fetch_clearance_parts(cfg: &Config, keys: &[String]) -> Result<FetchOutcome, FetchError> {
    let attempts = (cfg.api.retries + 1).max(1) as usize;
    let mut last: Option<FetchError> = None;
    for attempt in 0..attempts {
        if attempt > 0 {
            let backoff = BACKOFFS[(attempt - 1).min(BACKOFFS.len() - 1)];
            sleep(Duration::from_secs(backoff));
        }
        match fetch_once(cfg, keys) {
            Ok(outcome) => return Ok(outcome),
            Err(e) => {
                if e.status == Some(403) {
                    log_warn!("HTTP 403 – Akamai blockiert den Abruf (kein Retry bei Bot-Block)");
                    return Err(e);
                }
                log_warn!(
                    "API-Abruf fehlgeschlagen (Versuch {}/{}): {}",
                    attempt + 1,
                    attempts,
                    e
                );
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| FetchError {
        message: "unbekannter Abruffehler".into(),
        status: None,
    }))
}

/// Holt die API-Response über das offizielle curl-impersonate-Binary
/// (Chrome-TLS-Fingerprint, gleiche Technik wie curl_cffi – ohne Python).
/// Der Body kommt aus der stdout-Pipe (kein Temp-File, kein Disk-I/O) und
/// wird bis `BODY_RAM_MAX` gepuffert geparst – bei den realen ~3,3 MB also
/// über den schnellen Slice-Pfad. Der Commodity-Filter greift bereits beim
/// Parsen (DeserializeSeed); Keywords folgen nach dem Specs-Abruf.
pub fn fetch_once(cfg: &Config, keys: &[String]) -> Result<FetchOutcome, FetchError> {
    fn fe(e: impl std::fmt::Display) -> FetchError {
        FetchError {
            message: format!("Abruf fehlgeschlagen: {}", e),
            status: None,
        }
    }

    let (mut child, stdout, stderr_thread) = spawn_curl(
        cfg,
        None,
        None,
        &[
            ("Accept", "application/json, text/plain, */*"),
            ("Accept-Language", "de-DE,de;q=0.9,en;q=0.8"),
            ("Referer", &cfg.api.referer),
        ],
        &cfg.api.url,
    )
    .map_err(fe)?;

    // Body einlesen und parsen; der Reader wird intern bis EOF geleert.
    let parse_result = parse_clearance_from(stdout, keys);
    let (status, stderr) = finish_curl(&mut child, stderr_thread);

    if status == 403 {
        return Err(FetchError {
            message:
                "HTTP 403 – Akamai blockiert den Abruf (Fallback: Headless-Chromium, siehe README)"
                    .into(),
            status: Some(403),
        });
    }
    if status == 0 || status >= 400 {
        return Err(FetchError {
            message: format!("HTTP {} von der Lenovo-API {}", status, stderr),
            status: Some(status),
        });
    }

    let outcome = parse_result.map_err(|_| FetchError {
        message: "Antwort ist kein gültiges JSON (vermutlich XML ohne Accept-Header oder HTML-Blockseite)".into(),
        status: Some(status),
    })?;

    if outcome.code != 0 {
        return Err(FetchError {
            message: format!("API-Fehlercode: {}", outcome.code),
            status: Some(status),
        });
    }
    if outcome.total_parts == 0 {
        return Err(FetchError {
            message: "API lieferte keine/leere Datenliste – Abbruch, State bleibt unangetastet"
                .into(),
            status: Some(status),
        });
    }
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Detail-/Specs-Abruf (POST api/v4/lbom/parts/get, Batch je Teilenummer)
// ---------------------------------------------------------------------------

/// Typisierte Teile des Detail-Response – nur was wir brauchen:
/// id + name + specs (Attribute + specsInfo-Fallback).
#[derive(Deserialize)]
struct DetailResponse {
    #[serde(default)]
    data: Option<Vec<DetailPart>>,
}

#[derive(Deserialize)]
pub struct DetailPart {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    specs: Option<Specs>,
}

#[derive(Deserialize)]
struct Specs {
    #[serde(rename = "specsInfo", default)]
    info: Option<String>,
    #[serde(rename = "specsAttributes", default)]
    attributes: Vec<SpecAttr>,
}

#[derive(Deserialize)]
struct SpecAttr {
    #[serde(rename = "transKey", default)]
    trans_key: String,
    #[serde(rename = "displayTitle", default)]
    display_title: String,
    #[serde(default)]
    value: String,
}

/// Kompakte CPU-Codes lesbar machen:
/// "intelI79750H" → "Intel Core i7-9750H", "AMDR76850H-PRO" → "AMD Ryzen 7 6850H PRO".
/// Unbekannte Muster bleiben unverändert (Prinzip: niemals falsche Infos erfinden).
pub fn beautify_cpu(raw: &str) -> String {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix("intel").or_else(|| t.strip_prefix("Intel")) {
        let r = rest.trim();
        // I79750H → i7-9750H / I36006U → i3-6006U
        if r.len() > 2 {
            let gen = r.as_bytes()[1] as char;
            if matches!(r.as_bytes()[0], b'I' | b'i') && matches!(gen, '3' | '5' | '7' | '9') {
                let model = &r[2..];
                if !model.is_empty() && model.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return format!("Intel Core i{}-{}", gen, model);
                }
            }
        }
        return format!("Intel {}", r);
    }
    if let Some(rest) = t.strip_prefix("AMD").or_else(|| t.strip_prefix("amd")) {
        let r = rest.trim();
        // R76850H-PRO → Ryzen 7 6850H PRO
        if r.len() > 2 {
            let n = r.as_bytes()[1] as char;
            if matches!(r.as_bytes()[0], b'R' | b'r') && matches!(n, '3' | '5' | '7' | '9') {
                let model = r[2..].trim().replace("-PRO", " PRO");
                if !model.is_empty() {
                    return format!("AMD Ryzen {} {}", n, model);
                }
            }
        }
        return format!("AMD {}", r);
    }
    t.to_string()
}

pub fn gpu_type_de(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "DIS" => Some("diskret"),
        "UMA" => Some("integriert"),
        _ => None,
    }
}

/// RAM aus dem Namen ableiten (die Specs haben nicht immer ein dram-Attribut):
///   1) explizit "8 GB" / "16GB" im Namen
///   2) "8G"-Tokens – mit VRAM-Ausschluss: bei diskreter GPU zählt ein NG-Token,
///      der direkt hinter dem GPU-Modell steht (z. B. "T1200 4G", "RTX3000 6G"),
///      als Grafikspeicher, nicht als RAM. Mehrdeutig → None (lieber nichts sagen).
pub fn ram_from_name(name: &str, gpu_name: Option<&str>) -> Option<String> {
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let n = bytes.len();
    // GPU-Modell-Token (letztes Wort des GPU-Namens, z. B. "t1200", "3000")
    let gpu_token: Option<String> = gpu_name
        .and_then(|g| g.split_whitespace().last())
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty());
    let mut candidates: Vec<i64> = Vec::new();
    let mut i = 0;
    while i < n {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
        }
        // Varianten nach der Zahl: "8 GB", "8GB", "8G" (optional Leerzeichen, mit Wortgrenze)
        let mut j = i;
        if j < n && bytes[j] == b' ' {
            j += 1;
        }
        let mut consumed_end = None;
        if j + 1 < n && bytes[j] == b'g' && bytes[j + 1] == b'b' {
            consumed_end = Some(j + 2);
        } else if j < n && bytes[j] == b'g' {
            consumed_end = Some(j + 1);
        }
        let Some(end) = consumed_end else { continue };
        // Wortgrenze danach: kein Buchstabe (Komma/Klammer/Ende ok)
        if end < n && bytes[end].is_ascii_alphabetic() {
            continue;
        }
        // Ziffer direkt vor dem Wert? ("Vega10", "T1200" → keine RAM-Kandidaten)
        let prev_ok = start == 0
            || !bytes[start - 1].is_ascii_digit()
                && (bytes[start - 1] == b'u'
                    || bytes[start - 1] == b'h'
                    || !bytes[start - 1].is_ascii_alphanumeric());
        if !prev_ok {
            continue;
        }
        let num: i64 = lower[start..i].parse().unwrap_or(0);
        if !(2..=64).contains(&num) {
            continue;
        }
        // VRAM-Ausschluss: Token direkt davor ist das GPU-Modell
        if let Some(gpu_tok) = &gpu_token {
            let before = lower[..start].trim_end();
            let tok_before = before.rsplit([' ', ',']).next().unwrap_or("");
            if !tok_before.is_empty() && tok_before.ends_with(gpu_tok.as_str()) {
                continue;
            }
        }
        candidates.push(num);
    }
    let uniq: std::collections::BTreeSet<i64> = candidates.into_iter().collect();
    if uniq.len() == 1 {
        uniq.into_iter().next().map(|v| format!("{} GB", v))
    } else {
        None
    }
}

/// Geordnete Spec-Zeile: CPU → RAM → GPU → DPK → AMT → WLAN → Rest.
/// Fallback: rohes specsInfo (falls die Attribute fehlen).
pub fn build_spec_line(part: &DetailPart) -> Option<String> {
    let specs = part.specs.as_ref()?;
    let mut cpu: Option<String> = None;
    let mut ram: Option<String> = None;
    let mut gpu_name: Option<String> = None;
    let mut gpu_type: Option<String> = None;
    let mut rest: Vec<(String, String)> = Vec::new();
    for a in &specs.attributes {
        let v = a.value.trim();
        if v.is_empty() {
            continue;
        }
        match a.trans_key.as_str() {
            "cpu" => cpu = Some(beautify_cpu(v)),
            "dram" => ram = Some(v.to_string()),
            "gpu name" => gpu_name = Some(v.to_string()),
            "gpu type" => gpu_type = Some(v.to_string()),
            "dpk" => rest.push(("DPK".into(), v.to_string())),
            "amt" => rest.push(("AMT".into(), v.to_string())),
            "wlan" => rest.push(("WLAN".into(), v.to_string())),
            other => {
                let label = if a.display_title.is_empty() {
                    other.to_string()
                } else {
                    a.display_title.clone()
                };
                rest.push((label, v.to_string()))
            }
        }
    }
    // RAM-Fallback aus dem Namen (VRAM-Ausschluss via GPU-Name)
    if ram.is_none() {
        let gpu_ref = gpu_name
            .as_deref()
            .filter(|_| gpu_type.as_deref() == Some("DIS"));
        ram = ram_from_name(&part.name, gpu_ref);
    }
    let gpu = match (&gpu_name, &gpu_type) {
        (Some(nm), Some(ty)) => gpu_type_de(ty).map(|de| format!("GPU: {} ({})", nm, de)),
        (Some(nm), None) => Some(format!("GPU: {}", nm)),
        (None, Some(ty)) => gpu_type_de(ty).map(|de| format!("GPU: {}", de)),
        (None, None) => None,
    };
    // Feste Reihenfolge im Rest: DPK → AMT → WLAN → alles andere
    let prio = |label: &str| match label {
        "DPK" => 0,
        "AMT" => 1,
        "WLAN" => 2,
        _ => 3,
    };
    rest.sort_by_key(|(label, _)| prio(label));
    let mut out: Vec<String> = Vec::new();
    if let Some(c) = cpu {
        out.push(format!("CPU: {}", c));
    }
    if let Some(r) = ram {
        out.push(format!("RAM: {}", r));
    }
    if let Some(g) = gpu {
        out.push(g);
    }
    for (label, value) in &rest {
        out.push(format!("{}: {}", label, value));
    }
    if out.is_empty() {
        return specs.info.as_deref().map(format_specs);
    }
    Some(out.join(" | "))
}

/// Größte Modellnummer mit 3–5 Ziffern im Text (RTX 4060 → 4060, i7-13705H →
/// 13705), sonst 0. Kürzere Zahlen (z. B. „Vega10") zählen nicht.
fn model_number(value: &str) -> i64 {
    value
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| (3..=5).contains(&run.len()))
        .filter_map(|run| run.parse().ok())
        .max()
        .unwrap_or(0)
}

/// Vergleichsform für GPU-Namen: nur Buchstaben/Ziffern, lowercase – damit
/// „RTX A2000", „RTXA2000" und „RTX-A2000" gleich behandelt werden.
fn gpu_norm(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// RTX-A-Serie (Workstation) in allen Schreibweisen: normiert „rtxa2000".
/// Nach dem „rtxa" muss eine Ziffer folgen, damit nicht jedes „RTX A…"-Wort
/// (z. B. „RTX Ada") als Workstation zählt.
fn is_rtx_a(norm: &str) -> bool {
    norm.match_indices("rtxa")
        .any(|(i, _)| norm[i + 4..].starts_with(|c: char| c.is_ascii_digit()))
}

/// Weitere Workstation-Marker (in `gpu_norm`-Form): Quadro-Serie, NVS/Tesla,
/// Radeon Pro/FirePro/Instinct.
const WORKSTATION_GPU: [&str; 6] = ["quadro", "nvs", "tesla", "radeonpro", "firepro", "instinct"];

/// Gaming-Linien, die den Bonus bekommen: RTX und GTX. Einstiegskarten (MX,
/// GeForce 9xx, Radeon 5xx) bleiben bewusst draußen – sie sollen nicht über
/// stärkere Quadro T/P rutschen.
const GAMING_GPU: [&str; 2] = ["rtx", "gtx"];

/// Bonus einer Gaming-Karte auf die Modellnummer: eine Modellgeneration, damit
/// eine RTX 3070 (4070) über einer Quadro RTX 4000 liegt, eine Quadro RTX 6000
/// aber weiter über einer RTX 4060 (5060).
const GAMING_BONUS: i64 = 1_000;

/// Bekommt der GPU-Name den Gaming-Bonus? Workstation (Quadro, RTX A…, Radeon
/// Pro, …) und Einstiegskarten (MX, Radeon 5xx) nie – nur RTX/GTX.
pub(crate) fn has_gpu_bonus(name: &str) -> bool {
    let n = gpu_norm(name);
    model_number(name) > 0
        && !is_rtx_a(&n)
        && !WORKSTATION_GPU.iter().any(|m| n.contains(m))
        && GAMING_GPU.iter().any(|m| n.contains(m))
}

/// Einstiegskarten, die nicht in die Mail sollen: GeForce-MX-Serie (MX110 …
/// MX570, 940MX) und die Quadro-T-Serie (T500–T2000). Wertet nur das GPU-Feld
/// der Specs-Zeile aus – Teile ohne GPU-Angabe bleiben unangetastet.
pub fn is_low_end_gpu(spec_line: &str) -> bool {
    spec_line
        .split(" | ")
        .filter_map(|field| field.strip_prefix("GPU: "))
        .any(low_end_gpu_name)
}

/// Steht hinter `prefix` eine Ziffer? („mx4" in mx450, „quadrot1" in quadrot1200)
fn followed_by_digit(norm: &str, prefix: &str) -> bool {
    norm.match_indices(prefix)
        .any(|(i, _)| norm[i + prefix.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// „mx" mit Ziffer (MX450) oder am Modellende (940MX) = MX-Serie; „quadro" +
/// „t" + Ziffer (T1200) oder nur T + Nummer („NVIDIA T500") = Quadro-T-Serie.
fn low_end_gpu_name(value: &str) -> bool {
    // Vendor-Präfix und Typ-Zusatz weg: „NVIDIA GeForce 940MX (diskret)" →
    // „geforce940mx", „NVIDIA QUADRO T1200 (diskret)" → „quadrot1200".
    let norm = gpu_norm(value);
    let bare = norm
        .strip_prefix("nvidia")
        .unwrap_or(&norm)
        .trim_end_matches("diskret")
        .trim_end_matches("integriert");
    followed_by_digit(bare, "mx")
        || followed_by_digit(bare, "quadrot")
        || bare.ends_with("mx")
        || matches!(bare, "t500" | "t600" | "t1000" | "t1200" | "t2000")
}

// ---------------------------------------------------------------------------
// GPU-Benchmarks (3DMark Time Spy Graphics, eingebettet)
// ---------------------------------------------------------------------------

/// Datenzeilen einer eingebetteten TSV-Tabelle: ohne `#`-Kopf und Leerzeilen.
/// Alle Tabellen (GPU, iGPU, CPU-Bench, Fallback) nutzen dasselbe Format und
/// dieselbe Prüfung – hier an einer Stelle.
fn tsv_lines(raw: &'static str) -> impl Iterator<Item = &'static str> {
    raw.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
}

/// GPU-Leistung je Modell: `Schluessel\tName\tScore\tQuelle`. Quelle `ts` =
/// gemessen (3DMark Time Spy Graphics, inkl. iGPUs), `pm3d` = aus dem
/// PassMark-G3Dmark über eine Regression auf die Time-Spy-Skala umgerechnet.
const GPU_SCORES_TSV: &str = include_str!("gpu-scores.tsv");

/// Wert, Quelle und Anzeigename je GPU-Schlüssel.
fn gpu_table() -> &'static BTreeMap<String, (i64, &'static str, &'static str)> {
    static TABLE: OnceLock<BTreeMap<String, (i64, &'static str, &'static str)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = BTreeMap::new();
        for line in tsv_lines(GPU_SCORES_TSV) {
            let mut f = line.split('\t');
            let (Some(key), Some(name), Some(score)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            let src = if f.next() == Some("ts") { "ts" } else { "pm3d" };
            if let Ok(v) = score.parse::<i64>() {
                out.insert(key.to_string(), (v, src, name));
            }
        }
        out
    })
}

/// Klammerzusätze entfernen und klein schreiben („RTX 3060 (Laptop)" →
/// „rtx 3060 "). GPU- und CPU-Schlüssel brauchen dieselbe Vorbehandlung.
fn ohne_klammern(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut depth = 0usize;
    for ch in value.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    out
}

/// Suchschlüssel einer GPU: Marken- und Füllwörter weg, nur Buchstaben und
/// Ziffern – damit „NVIDIA GeForce RTX 3060 Laptop GPU" und „NVIDIA GeForce
/// RTX3060" denselben Eintrag treffen.
fn gpu_key(value: &str) -> String {
    let s = ohne_klammern(value);
    // „Max-Q" ist ein eigenes Wortbild und wird vor dem Zerlegen entfernt.
    let s = s
        .replace("max-q", "")
        .replace("max q", "")
        .replace("maxq", "");
    const FILLER: [&str; 15] = [
        "nvidia", "amd", "intel", "geforce", "quadro", "radeon", "pro", "firepro", "graphics",
        "laptop", "mobile", "gpu", "notebook", "with", "design",
    ];
    let mut out = String::new();
    for token in s.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token.is_empty() || FILLER.contains(&token) || token == "generation" {
            continue;
        }
        out.push_str(token);
    }
    out
}

/// Gemessener GPU-Score (3DMark Time Spy Graphics) mit Quelle – `None`, wenn
/// die Karte unbekannt ist. Varianten wie „M2200M"/„MX450B" fallen auf die
/// Basisbezeichnung zurück.
fn gpu_bench(value: &str) -> Option<(i64, &'static str)> {
    let table = gpu_table();
    let key = gpu_key(value);
    if key.is_empty() {
        return None;
    }
    if let Some((score, src, _)) = table.get(&key) {
        return Some((*score, *src));
    }
    for suffix in ['m', 'b'] {
        if let Some(base) = key.strip_suffix(suffix) {
            if !base.is_empty() {
                if let Some((score, src, _)) = table.get(base) {
                    return Some((*score, *src));
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// iGPU je CPU-Modell (exakt)
// ---------------------------------------------------------------------------

/// iGPU-Zuordnung je CPU-Modell: `CPU-Schluessel\tName\tTimeSpy\tQuelle\tGPU-Schluessel`.
/// Schlüssel ist der CPU-Name klein und nur aus Buchstaben/Ziffern. Quelle `ts`
/// = iGPU in 3DMark Time Spy gemessen, `pm3d` = umgerechnet, `est` = aus dem
/// EU-/CU-Ausbau geschätzt (dann zeigt der Tooltip „≈“).
const CPU_IGPU_TSV: &str = include_str!("cpu-igpu.tsv");

/// iGPU-Name, Time-Spy-Wert und Wertquelle je normalisiertem CPU-Namen.
fn igpu_table() -> &'static BTreeMap<String, (&'static str, i64, &'static str)> {
    static TABLE: OnceLock<BTreeMap<String, (&'static str, i64, &'static str)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = BTreeMap::new();
        for line in tsv_lines(CPU_IGPU_TSV) {
            let mut f = line.split('\t');
            let (Some(key), Some(name), Some(score), Some(src)) =
                (f.next(), f.next(), f.next(), f.next())
            else {
                continue;
            };
            let src = match src {
                "ts" => "ts",
                "est" => "est",
                _ => "pm3d",
            };
            if let Ok(v) = score.parse::<i64>() {
                out.insert(key.to_string(), (name, v, src));
            }
        }
        out
    })
}

/// CPU-Name auf den Tabellenschlüssel bringen (klein, nur alphanumerisch), damit
/// „AMD ATHLON3050U“, „AMD Athlon3050U“ und „AMD Athlon 3050U“ gleich treffen.
fn igpu_table_key(cpu: &str) -> String {
    cpu.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Exakte iGPU eines CPU-Modells (Name, Time-Spy-Wert, Wertquelle).
fn igpu_exact(cpu: &str) -> Option<(&'static str, i64, &'static str)> {
    igpu_table().get(&igpu_table_key(cpu)).copied()
}

/// „(Ryzen 4000/5000)" → „(4000/5000)": die Generationen aus der Klammer,
/// damit Vega 8 aus Ryzen 2000/3000 und Vega 8 aus Ryzen 4000/5000 (312 Punkte
/// Unterschied) am Chip unterscheidbar bleiben.
fn series_suffix(inside: &str) -> Option<String> {
    let rest = &inside[inside.find("Ryzen ")? + "Ryzen ".len()..];
    let gens: Vec<&str> = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() == 4)
        .collect();
    (!gens.is_empty()).then(|| format!("({})", gens.join("/")))
}

/// „32 EU, Ice Lake" → „32EU" (nichts, wenn in der Klammer keine EU-Zahl steht).
fn eu_token(inside: &str) -> Option<String> {
    let num = inside[..inside.find("EU")?].trim_end();
    let digits = &num[num.trim_end_matches(|c: char| c.is_ascii_digit()).len()..];
    (!digits.is_empty()).then(|| format!("{digits}EU"))
}

/// Kurzname für den Chip, abgeleitet aus dem vollen iGPU-Namen der Tabelle –
/// so steht jeder Name nur einmal in `cpu-igpu.tsv`:
/// „Intel Iris Xe Graphics G7 96EUs" → „Iris Xe G7 96EU",
/// „Intel UHD Graphics (Jasper Lake 32 EU)" → „UHD 32EU",
/// „AMD Radeon RX Vega 8 (Ryzen 4000/5000)" → „Vega 8",
/// „Intel Graphics 4-Core iGPU (Meteor / Arrow Lake)" → „4-Core iGPU",
/// „Qualcomm Adreno 618 GPU" → „Adreno 618".
pub fn igpu_short(name: &str) -> String {
    let (base, paren) = name.split_once('(').unwrap_or((name, ""));
    let short = base
        .trim()
        .trim_start_matches("Intel ")
        .trim_start_matches("AMD ")
        .trim_start_matches("Qualcomm ")
        .replace(" Graphics", "")
        .replace(" Mobile", "")
        .replace(" GPU", "")
        .replace("Radeon RX Vega", "Vega")
        .replace("Radeon Vega", "Vega")
        .replace("EUs", "EU");
    // „Intel Graphics 4-Core iGPU" → „4-Core iGPU".
    let short = short.trim().trim_start_matches("Graphics ").to_string();
    // EU-Zahl aus der Klammer nur anhängen, wenn der Name selbst keine hat.
    if short.is_empty() {
        return short;
    }
    if let Some(suffix) = series_suffix(paren) {
        return format!("{short} {suffix}");
    }
    if short.contains("EU") {
        return short;
    }
    match eu_token(paren) {
        Some(eu) => format!("{short} {eu}"),
        None => short,
    }
}

/// „Core i5-9400F", „Core Ultra 7 265KF": Intel lässt bei diesen Desktop-
/// Modellen die iGPU weg – ein UMA-Board gibt es damit nicht.
fn intel_ohne_igpu(cpu: &str) -> bool {
    let low = cpu.to_ascii_lowercase();
    let modell = low.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    modell.ends_with('f')
}

/// iGPU-Schlüssel zu einer CPU (Fallback für Modelle ohne exakten Eintrag).
/// Lenovo schreibt bei UMA-Boards nur „integriert"
/// bzw. „UMA" – welcher Grafikchip drinsteckt, folgt aber der CPU-Reihe und
/// -Generation (12. Gen → Iris Xe, Tiger Lake → Xe 96/80 EU, Ryzen 6000 →
/// Radeon 680M/660M, Ryzen 7000 → 780M/760M/740M, ältere → HD/UHD/Vega).
fn igpu_key(cpu: &str) -> Option<&'static str> {
    let series = cpu_series(cpu);
    let (gen, sku) = cpu_gen_sku(cpu_model_number(cpu));
    // AMD 7000/8000: die letzten zwei Stellen verraten die iGPU-Generation –
    // 7x20/7x30 = Vega (Mendocino/Barcelo), 7x35 = Rembrandt (680M/660M),
    // 7x40/7040/8040 = Phoenix/Hawk Point (780M/760M/740M).
    let tail = sku % 100;
    let tier = match series.as_str() {
        "Core i3" | "Ryzen 3" => 3,
        "Core i5" | "Ryzen 5" => 5,
        "Core i7" | "Ryzen 7" => 7,
        "Core i9" | "Ryzen 9" => 9,
        _ => 0,
    };
    match series.as_str() {
        // Grob: Arc 8-Core. Genauere Modelle (Lunar/Arrow Lake) stehen als
        // Zeilen in cpu-igpu.tsv.
        "Core Ultra" => (!intel_ohne_igpu(cpu)).then_some("arc8coreigpu"),
        "Core i3" | "Core i5" | "Core i7" | "Core i9" | "Core m" | "Xeon" => match gen {
            0 => None,
            _ if intel_ohne_igpu(cpu) => None,
            1..=3 => Some("hd4000"),
            4 => Some("hd4600"),
            5 => Some("hd5500"),
            6 => Some("hd520"),
            7 => Some("hd620"),
            8 => Some("uhd620"),
            9 => Some("uhd630"),
            10 => Some("uhd620"),
            11 => Some(if tier >= 7 {
                "irisxeg796eus"
            } else {
                "irisxeg780eus"
            }),
            12..=14 => Some(match tier {
                9 | 7 => "irisxeg796eus",
                5 => "irisxeg780eus",
                _ => "uhd64eus",
            }),
            _ => None,
        },
        "Ryzen 3" | "Ryzen 5" | "Ryzen 7" | "Ryzen 9" => match gen {
            0 => None,
            // Vega 8/6/3 gibt es in zwei Ryzen-Generationen mit eigenen
            // Messwerten – die Regel greift deshalb auf die passende Zeile zu,
            // nicht auf eine gemeinsame.
            2 | 3 => Some(match tier {
                9 | 7 => "vega10",
                5 => "vega8",
                _ => "vega3",
            }),
            4 => Some(match tier {
                9 | 7 => "rxvega7",
                5 => "rxvega6",
                _ => "rxvega5",
            }),
            5 => Some(match tier {
                9 | 7 => "rxvega8",
                5 => "rxvega7",
                _ => "rxvega6",
            }),
            6 => Some(match tier {
                9 | 7 => "680m",
                _ => "660m",
            }),
            _ => Some(match (tail, tier) {
                (35, 9 | 7) => "680m",
                (35, _) => "660m",
                (20 | 30, 9 | 7 | 5) => "vega8",
                (20 | 30, _) => "vega6",
                (_, 9 | 7) => "780m",
                (_, 5) => "760m",
                (_, _) => "740m",
            }),
        },
        "Celeron" | "Pentium" => Some("uhd620"),
        "Athlon" | "A-Serie" => Some("vega3"),
        _ => None,
    }
}

/// GPU-Klasse: gemessener 3DMark-Time-Spy-Wert, sonst die alte Heuristik
/// (Modellnummer, für Gaming-Karten um den Bonus angehoben; ohne Modellnummer
/// wie „integriert" 0).
fn gpu_class(value: &str) -> i64 {
    if let Some((score, _)) = gpu_bench(value) {
        return score;
    }
    let model = model_number(value);
    if model > 0 && has_gpu_bonus(value) {
        model + GAMING_BONUS
    } else {
        model
    }
}

/// RAM in GB aus „16 GB"/„16GB"; ohne Zahl 0.
fn ram_gb(value: &str) -> i64 {
    value
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Generation und SKU-Anteil aus der CPU-Modellnummer. Fünf- und Vierstellern
/// gemeinsam ist die Generation, dahinter folgt die SKU – erst die macht
/// „12800H" schneller als „12700H":
/// „12700" → (12, 700), „10510" → (10, 510), „1255" → (12, 55),
/// „1135" → (11, 35), „8550" → (8, 550), „3700" → (3, 700).
fn cpu_gen_sku(model: i64) -> (i64, i64) {
    if model <= 0 {
        return (0, 0);
    }
    if model >= 10_000 {
        return (model / 1_000, model % 1_000);
    }
    if model >= 1_000 {
        // Vierstellig: „1255U"/„1165G7" meinen mit den ersten zwei Ziffern
        // die Generation, „8250U"/„8550U" mit nur der ersten.
        let two = model / 100;
        if (10..=14).contains(&two) {
            return (two, model % 100);
        }
        return (model / 1_000, model % 1_000);
    }
    (model / 100, model % 100)
}

/// Modellnummer einer CPU – auch aus rohen Codes („INTELI712800H" → 12800,
/// „AMDR75825U" → 5825), wo Tier und Modell ohne Trenner zusammenkleben und
/// `model_number` (3–5 Ziffern) nichts findet. Gesucht wird die Zahl direkt
/// hinter der Familienangabe; ohne Treffer gilt die allgemeine Heuristik.
fn cpu_model_number(cpu: &str) -> i64 {
    let lower = cpu.to_ascii_lowercase();
    // „Ryzen AI 9 HX 370": Tier (9) und Modell (370) stehen getrennt, die
    // erste Zahl wäre der Tier – gesucht ist die letzte dreistellige Gruppe.
    if let Some(pos) = lower.find("ryzen ai") {
        if let Some(n) = last_three_digits(&lower[pos..]) {
            return n;
        }
    }
    // „Core Ultra 7 258V": nach „Core Ultra" kommt erst der Tier (7), dann das
    // Modell (258) – also ebenfalls die letzte dreistellige Gruppe.
    if let Some(pos) = lower.find("core ultra") {
        if let Some(n) = last_three_digits(&lower[pos..]) {
            return n;
        }
    }
    for prefix in [
        "core i9",
        "core i7",
        "core i5",
        "core i3",
        "core ultra",
        "i9",
        "i7",
        "i5",
        "i3",
        "ryzen 9",
        "ryzen 7",
        "ryzen 5",
        "ryzen 3",
        "ryzen",
        "r9",
        "r7",
        "r5",
        "r3",
    ] {
        let Some(pos) = lower.find(prefix) else {
            continue;
        };
        let rest = &lower[pos + prefix.len()..];
        let digits: String = rest
            .trim_start_matches(|c: char| !c.is_ascii_digit())
            .chars()
            .take_while(char::is_ascii_digit)
            .take(5)
            .collect();
        if let Ok(n) = digits.parse::<i64>() {
            if n > 0 {
                return n;
            }
        }
    }
    model_number(cpu)
}

// ---------------------------------------------------------------------------
// CPU-Benchmarks (Cinebench R23, eingebettet)
// ---------------------------------------------------------------------------

/// Werte je Modell: `Serie:Modell\tName\tSingle\tMulti\tQuelle`. Das
/// Serienpräfix („i7", „r5") verhindert Verwechslungen wie Intel Core i7-6600U
/// und AMD Ryzen 5 6600U; das Modell ist die Nummer mit Suffix („12800h").
/// Quelle `r23` = direkt gemessen (Cinebench R23), `gb6` = aus Geekbench 6
/// auf die R23-Skala umgerechnet.
const CPU_SCORES_TSV: &str = include_str!("cpu-scores.tsv");
/// Rückfall ohne Benchmark: `Serie\tGeneration\tScore` (Median der Serie).
const CPU_FALLBACK_TSV: &str = include_str!("cpu-fallback.tsv");

fn bench_table() -> &'static BTreeMap<String, (i64, i64, &'static str)> {
    static TABLE: OnceLock<BTreeMap<String, (i64, i64, &'static str)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = BTreeMap::new();
        for line in tsv_lines(CPU_SCORES_TSV) {
            let mut f = line.split('\t');
            let (Some(key), Some(_name), Some(single), Some(multi)) =
                (f.next(), f.next(), f.next(), f.next())
            else {
                continue;
            };
            let src = match f.next() {
                Some("r23") => "r23",
                Some("cb24") => "cb24",
                Some("r20") => "r20",
                Some("r15") => "r15",
                Some("cb15") => "cb15",
                Some("cb20") => "cb20",
                _ => "gb6",
            };
            if let (Ok(s), Ok(m)) = (single.parse(), multi.parse()) {
                out.insert(key.to_string(), (s, m, src));
            }
        }
        out
    })
}

fn fallback_table() -> &'static BTreeMap<String, i64> {
    static TABLE: OnceLock<BTreeMap<String, i64>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut out = BTreeMap::new();
        for line in tsv_lines(CPU_FALLBACK_TSV) {
            let mut f = line.split('\t');
            let (Some(series), Some(gen), Some(score)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            if let (Ok(g), Ok(s)) = (gen.parse::<i64>(), score.parse::<i64>()) {
                out.insert(format!("{}|{}", series, g), s);
            }
        }
        out
    })
}

/// Modellschlüssel einer CPU: die Nummer direkt hinter der Familienangabe
/// („INTELI712800H" → „12800h", „AMDR75825U" → „5825u", „Celeron 5205U" →
/// „5205u"). Klammerzusätze wie „(N)" werden ignoriert.
/// Letzte dreistellige Zahlengruppe („ryzen ai 9 hx 370" → 370).
fn last_three_digits(s: &str) -> Option<i64> {
    let mut best = None;
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_ascii_digit() {
            cur.push(ch);
            continue;
        }
        if cur.len() == 3 {
            best = cur.parse().ok();
        }
        cur.clear();
    }
    if cur.len() == 3 {
        best = cur.parse().ok();
    }
    best
}

fn cpu_model_key(cpu: &str) -> String {
    let s = ohne_klammern(cpu);
    if let Some(pos) = s.find("ryzen ai") {
        if let Some(n) = last_three_digits(&s[pos..]) {
            return n.to_string();
        }
    }
    const PREFIX: [&str; 18] = [
        "core ultra",
        "core i9",
        "core i7",
        "core i5",
        "core i3",
        "i9",
        "i7",
        "i5",
        "i3",
        "ryzen 9",
        "ryzen 7",
        "ryzen 5",
        "ryzen 3",
        "ryzen",
        "r9",
        "r7",
        "r5",
        "r3",
    ];
    for prefix in PREFIX {
        let Some(pos) = s.find(prefix) else {
            continue;
        };
        let rest = &s[pos + prefix.len()..];
        let rest = rest.trim_start_matches(|c: char| !c.is_ascii_digit());
        let digits: String = rest
            .chars()
            .take_while(char::is_ascii_digit)
            .take(5)
            .collect();
        if digits.len() < 3 {
            continue;
        }
        let letters: String = rest[digits.len()..]
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect();
        return format!("{}{}", digits, letters);
    }
    // Ohne Familienangabe (Celeron, Athlon, Xeon-Codes): erster Ziffernblock.
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start >= 3 {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                return s[start..j].to_string();
            }
        } else {
            i += 1;
        }
    }
    // Kurzcodes wie „7Y54" oder Lenovos „M76Y75" haben keinen dreistelligen
    // Ziffernblock – dann das letzte Token nehmen, das überhaupt Ziffern hat.
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .rev()
        .find(|t| t.len() >= 3 && t.chars().any(|c| c.is_ascii_digit()))
        .unwrap_or("")
        .to_string()
}

/// Werte (Single, Multi, Quelle) zu einer CPU – `None`, wenn unbekannt.
/// Das „P" in AMD-Codes (4750UP = PRO) wird beim Abgleich toleriert.
fn cpu_bench(cpu: &str) -> Option<(i64, i64, &'static str)> {
    let table = bench_table();
    let prefix = cpu_series_prefix(&cpu_series(cpu));
    let roh = cpu_model_key(cpu);
    let mut key = roh.clone();
    while !key.is_empty() {
        if let Some(v) = table.get(&format!("{}:{}", prefix, key)) {
            return Some(*v);
        }
        let trimmed = key.trim_end_matches('p').to_string();
        if trimmed.len() == key.len() || trimmed.is_empty() {
            break;
        }
        key = trimmed;
    }
    bench_key_suffix(&roh, prefix)
}

/// Lenovo hängt Marke und Serie ohne Trennzeichen an die Modellnummer
/// („INTELM76Y75" = Core m7-6Y75, „AMD A129700P" = A12-9700P,
/// „Intel E31505M" = Xeon E3-1505M v6). Statt solche Codes als zusätzliche
/// Tabellenzeilen zu pflegen, wird der Schlüssel genommen, dessen Modellnummer
/// am Tokenende steht – mindestens vier Zeichen und nur, wenn er eindeutig ist.
fn bench_key_suffix(token: &str, prefix: &str) -> Option<(i64, i64, &'static str)> {
    if token.len() < 4 {
        return None;
    }
    let mut treffer = None;
    for (key, value) in bench_table() {
        let Some((key_prefix, model)) = key.split_once(':') else {
            continue;
        };
        if key_prefix != prefix || model.len() < 4 || !token.ends_with(model) {
            continue;
        }
        if treffer.is_some() {
            return None; // mehrdeutig – lieber schätzen als raten
        }
        treffer = Some(*value);
    }
    treffer
}

/// Kürzel der Serie für den Tabellenschlüssel: „Core i7" → „i7",
/// „Ryzen 5" → „r5", alles andere ohne Präfix.
fn cpu_series_prefix(series: &str) -> &'static str {
    match series {
        "Core i3" => "i3",
        "Core i5" => "i5",
        "Core i7" => "i7",
        "Core i9" => "i9",
        "Ryzen 3" => "r3",
        "Ryzen 5" => "r5",
        "Ryzen 7" => "r7",
        "Ryzen 9" => "r9",
        "Ryzen AI" => "rai",
        _ => "",
    }
}

/// Geschätzter Score ohne Benchmark: Median der Serie/Generation aus der
/// Tabelle (z. B. „Core i7", 12. Gen). Ist die Generation nicht ableitbar
/// (Codes wie 7Y54), zählt der Mindestwert der Serie; zuletzt der Median über
/// alle Modelle.
fn cpu_score_estimate(cpu: &str) -> i64 {
    let table = fallback_table();
    let series = cpu_series(cpu);
    let (gen, _) = cpu_gen_sku(cpu_model_number(cpu));
    let core_or_ryzen = series.starts_with("Core i") || series.starts_with("Ryzen");
    let gen = if core_or_ryzen { gen } else { 0 };
    let mut keys = vec![format!("{}|{}", series, gen)];
    if gen == 0 && core_or_ryzen {
        // Generation unbekannt (Codes wie 7Y54): konservativ der Mindestwert.
        keys.insert(0, format!("{}|-1", series));
    }
    keys.push(format!("{}|0", series));
    keys.push("|0".to_string());
    keys.iter()
        .find_map(|k| table.get(k))
        .copied()
        .unwrap_or(8_000)
}

/// CPU-Klasse = Verbundscore aus Cinebench R23 (Single × 10 + Multi), damit
/// sowohl „schneller Kern" als auch „viele Kerne" zählen. Ohne Tabellentreffer
/// schätzt `cpu_score_estimate` aus Serie und Generation.
fn cpu_class(value: &str) -> i64 {
    match cpu_bench(value) {
        Some((single, multi, _)) => single * 10 + multi,
        None => cpu_score_estimate(value),
    }
}

/// Leistungs-Schlüssel eines Teils aus seiner Specs-Zeile: (GPU-Klasse, CPU,
/// RAM). Das Tupel vergleicht lexikografisch – stärkste GPU zuerst, bei
/// gleicher GPU die stärkere CPU, bei Gleichstand mehr RAM. Ohne Specs/Angabe
/// bleibt er (0, 0, 0). Bewusst eine Heuristik, keine Benchmarks.
pub fn performance_key(spec_line: &str) -> (i64, i64, i64) {
    let (mut gpu, mut cpu, mut ram) = (0, 0, 0);
    for field in spec_line.split(" | ") {
        match field.split_once(": ") {
            Some(("GPU", value)) => gpu = gpu_class(value),
            Some(("CPU", value)) => cpu = cpu_class(value),
            Some(("RAM", value)) => ram = ram_gb(value),
            _ => {}
        }
    }
    (gpu, cpu, ram)
}

// ---------------------------------------------------------------------------
// Filter-Facetten (Web-UI)
// ---------------------------------------------------------------------------

/// Aufbereitete Merkmale eines Teils für Filter in der Web-UI. Alle Felder
/// kommen aus derselben Specs-Zeile wie `performance_key` – keine zusätzlichen
/// API-Aufrufe. Leere Strings/0 heißen „keine Angabe".
pub struct Facets {
    pub cpu: String,
    pub cpu_brand: &'static str,
    pub cpu_series: String,
    /// „13. Gen" / „Ryzen 5000" – leer, wenn nicht ableitbar.
    pub cpu_gen: String,
    /// Gemessene Werte (Single, Multi) bzw. `None` ohne Tabellentreffer.
    pub cpu_bench: Option<(i64, i64)>,
    /// Quelle der Werte: „r23", „cb24", „r20", „r15", „gb6" – leer ohne Treffer.
    pub cpu_bench_src: &'static str,
    pub ram: i64,
    pub gpu: String,
    pub gpu_kind: &'static str,
    pub gpu_family: String,
    /// 3DMark-Time-Spy-Wert bzw. `None` ohne Tabellentreffer.
    pub gpu_bench: Option<i64>,
    /// Quelle des GPU-Werts: „ts", „pm3d", „igpu" (aus der CPU abgeleitet).
    pub gpu_bench_src: &'static str,
    /// Name des abgeleiteten iGPUs (nur bei Quelle „igpu").
    pub gpu_bench_name: &'static str,
    /// Woher der iGPU-Wert stammt: „ts", „pm3d" oder „est" (geschätzt).
    pub gpu_bench_note: &'static str,
    pub perf: (i64, i64, i64),
}

pub fn facets(spec_line: &str) -> Facets {
    let mut f = Facets {
        cpu: String::new(),
        cpu_brand: "",
        cpu_series: String::new(),
        cpu_gen: String::new(),
        cpu_bench: None,
        cpu_bench_src: "",
        ram: 0,
        gpu: String::new(),
        gpu_kind: "",
        gpu_family: String::new(),
        gpu_bench: None,
        gpu_bench_src: "",
        gpu_bench_name: "",
        gpu_bench_note: "",
        perf: (0, 0, 0),
    };
    for field in spec_line.split(" | ") {
        match field.split_once(": ") {
            Some(("CPU", value)) => {
                // Rohe Codes („INTELI712800H") lesbar machen – die Mail nutzt
                // weiter die Specs-Zeile, hier geht es nur um die Anzeige.
                f.cpu = cpu_display(value);
                f.cpu_brand = if ci_contains(value, "intel") {
                    "Intel"
                } else if ci_contains(value, "amd") {
                    "AMD"
                } else {
                    ""
                };
                f.cpu_series = cpu_series(value);
                f.cpu_gen = cpu_generation(value);
                if let Some((single, multi, src)) = cpu_bench(value) {
                    f.cpu_bench = Some((single, multi));
                    f.cpu_bench_src = src;
                }
            }
            Some(("RAM", value)) => f.ram = ram_gb(value),
            Some(("GPU", value)) => {
                f.gpu = value.trim().to_string();
                f.gpu_kind = if ci_contains(value, "diskret") {
                    "diskret"
                } else if ci_contains(value, "integriert") {
                    "integriert"
                } else {
                    ""
                };
                f.gpu_family = gpu_family(value);
            }
            _ => {}
        }
    }
    // Leistungsschlüssel aus den aufbereiteten Feldern – so zählt auch eine
    // rohe CPU („INTELI712800H") richtig, deren Modellnummer der Parser sonst
    // nicht erkennt.
    if !f.gpu.is_empty() {
        if let Some((score, src)) = gpu_bench(&f.gpu) {
            f.gpu_bench = Some(score);
            f.gpu_bench_src = src;
        }
    }
    // „integriert"/„UMA": Lenovo nennt die iGPU nicht – aus der CPU ableiten
    // und als abgeleitet kennzeichnen.
    if f.gpu_bench.is_none() && f.gpu_kind == "integriert" && !f.cpu.is_empty() {
        // 1) exakte Zuordnung CPU-Modell → iGPU (Intel ARK/AMD-Daten), sonst
        // 2) Ableitung aus Reihe + Generation.
        if let Some((name, score, note)) = igpu_exact(&f.cpu) {
            f.gpu_bench = Some(score);
            f.gpu_bench_src = "igpu";
            f.gpu_bench_name = name;
            f.gpu_bench_note = note;
        } else if let Some(key) = igpu_key(&f.cpu) {
            if let Some((score, src, name)) = gpu_table().get(key) {
                f.gpu_bench = Some(*score);
                f.gpu_bench_src = "igpu";
                f.gpu_bench_name = name;
                f.gpu_bench_note = src;
            }
        }
    }
    f.perf = (
        match f.gpu_bench {
            Some(score) => score,
            None if f.gpu.is_empty() => 0,
            None => gpu_class(&f.gpu),
        },
        if f.cpu.is_empty() {
            0
        } else {
            cpu_class(&f.cpu)
        },
        f.ram,
    );
    f
}

/// CPU-Anzeige: Vendor-Schreibweise normalisieren, dann `beautify_cpu`.
fn cpu_display(raw: &str) -> String {
    let t = raw.trim();
    let text = match t.strip_prefix("INTEL").or_else(|| t.strip_prefix("Intel")) {
        Some(rest) => format!("Intel{}", rest),
        None => t.to_string(),
    };
    beautify_cpu(&text)
        .replace("CELERON", "Celeron")
        .replace("PENTIUM", "Pentium")
}

/// CPU-Reihe für den Filter („Core i7", „Ryzen 5", „Celeron", …). Jede CPU
/// landet in genau einer Reihe – was sich nicht zuordnen lässt, wird
/// „sonstige", damit der Filter keine Lücke hat.
fn cpu_series(cpu: &str) -> String {
    if cpu.trim().is_empty() {
        return String::new();
    }
    for (needle, label) in [
        ("core ultra", "Core Ultra"),
        ("ryzen ai", "Ryzen AI"),
        ("ryzen 9", "Ryzen 9"),
        ("ryzen 7", "Ryzen 7"),
        ("ryzen 5", "Ryzen 5"),
        ("ryzen 3", "Ryzen 3"),
        ("core i9", "Core i9"),
        ("core i7", "Core i7"),
        ("core i5", "Core i5"),
        ("core i3", "Core i3"),
        ("celeron", "Celeron"),
        ("pentium", "Pentium"),
        ("xeon", "Xeon"),
        ("athlon", "Athlon"),
        ("snapdragon", "Snapdragon"),
        ("amd a", "A-Serie"),
        ("core m", "Core m"),
        // Rohe Code-Schreibweise ohne Modellnamen: INTELE2276M (Xeon E),
        // INTELW10855M (Xeon W), INTELM38100Y (Core m3), INTELU300.
        ("intele", "Xeon"),
        ("intelw", "Xeon"),
        ("intelm", "Core m"),
        ("intelu", "Intel U-Serie"),
        ("amd 3", "Athlon"),
        // Rohe Code-Schreibweise ohne „Core"/„Ryzen" (INTELI712800H, AMDR72700U)
        ("i9", "Core i9"),
        ("i7", "Core i7"),
        ("i5", "Core i5"),
        ("i3", "Core i3"),
        ("r9", "Ryzen 9"),
        ("r7", "Ryzen 7"),
        ("r5", "Ryzen 5"),
        ("r3", "Ryzen 3"),
    ] {
        if ci_contains(cpu, needle) {
            return label.to_string();
        }
    }
    // Rohe Codes ohne Vendor davor („E2276M", „M38100Y", „U300", „W10855M")
    let bare = cpu
        .trim()
        .strip_prefix("INTEL")
        .or_else(|| cpu.trim().strip_prefix("Intel"))
        .unwrap_or_else(|| cpu.trim())
        .trim();
    let mut chars = bare.chars();
    if let (Some(first), Some(second)) = (chars.next(), chars.next()) {
        if second.is_ascii_digit() {
            match first.to_ascii_uppercase() {
                'E' | 'W' => return "Xeon".to_string(),
                'M' => return "Core m".to_string(),
                'U' => return "Intel U-Serie".to_string(),
                _ => {}
            }
        }
    }
    "sonstige".to_string()
}

/// CPU-Generation für den Filter: „13. Gen" bei Core iX, „Ryzen 5000" bei
/// AMD. Leer, wo sich keine ableiten lässt (Celeron, Pentium, Xeon, …).
fn cpu_generation(cpu: &str) -> String {
    if cpu.trim().is_empty() {
        return String::new();
    }
    let (gen, _) = cpu_gen_sku(cpu_model_number(cpu));
    if gen <= 0 {
        return String::new();
    }
    let series = cpu_series(cpu);
    if series == "Core Ultra" {
        // „Core Ultra 7 258V" → „Core Ultra Series 2" (125H → Series 1).
        return format!("Core Ultra Series {}", cpu_model_number(cpu) / 100);
    }
    if series == "Ryzen AI" {
        // „Ryzen AI 9 HX 370" → „Ryzen AI 300" (Modellnummer/100 ist die Serie).
        return format!("Ryzen AI {}00", cpu_model_number(cpu) / 100);
    }
    if series.starts_with("Ryzen") {
        return format!("Ryzen {}", gen * 1000);
    }
    if series.starts_with("Core i") || series == "Core Ultra" {
        return format!("{}. Gen", gen);
    }
    String::new()
}

/// GPU-Familie für den Filter: RTX nach Generation, GTX nach Serie, dazu
/// Quadro (inkl. T-Serie), MX, Radeon, Radeon Pro und integrierte Grafik.
fn gpu_family(value: &str) -> String {
    let norm = gpu_norm(value);
    if norm.is_empty() {
        return String::new();
    }
    if norm.ends_with("integriert") {
        return "integriert".into();
    }
    let bare = norm
        .strip_prefix("nvidia")
        .unwrap_or(&norm)
        .trim_end_matches("diskret")
        .trim_end_matches("integriert");
    if followed_by_digit(bare, "quadrot")
        || matches!(bare, "t500" | "t600" | "t1000" | "t1200" | "t2000")
    {
        return "Quadro T".into();
    }
    if followed_by_digit(bare, "mx") || bare.ends_with("mx") {
        // MX-Serie (MX110 … MX570, 940MX)
        return "MX".into();
    }
    if norm.contains("quadro") || is_rtx_a(&norm) {
        return "Quadro".into();
    }
    if norm.contains("radeonpro") || norm.contains("firepro") || norm.contains("instinct") {
        return "Radeon Pro".into();
    }
    let model = model_number(value);
    if norm.contains("rtx") {
        return match model {
            5000.. => "RTX 50".into(),
            4000..=4999 => "RTX 40".into(),
            3000..=3999 => "RTX 30".into(),
            2000..=2999 => "RTX 20".into(),
            _ => "RTX".into(),
        };
    }
    if norm.contains("gtx") {
        return match model {
            1600..=1699 => "GTX 16".into(),
            1000..=1099 => "GTX 10".into(),
            _ => "GTX".into(),
        };
    }
    if norm.contains("radeon") {
        return "Radeon".into();
    }
    "sonstige".into()
}

fn detail_map_from_response(r: DetailResponse) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(parts) = r.data {
        for p in parts {
            if let Some(line) = build_spec_line(&p) {
                if !line.is_empty() {
                    out.insert(p.id, line);
                }
            }
        }
    }
    out
}

pub fn parse_detail(text: &str) -> BTreeMap<String, String> {
    serde_json::from_str::<DetailResponse>(text)
        .map(detail_map_from_response)
        .unwrap_or_default()
}

/// "…/de/de/api/v4/lbom/clearanceParts/get" → (detail-url, country, language)
pub fn detail_target(api_url: &str) -> Option<(String, String, String)> {
    let idx = api_url.rfind("/api/v4/")?;
    let base = &api_url[..idx];
    // Die letzten beiden nicht-leeren Segmente sind country/language.
    let mut segs = base.rsplit('/').filter(|s| !s.is_empty());
    let language = segs.next()?.to_uppercase();
    let country = segs.next()?.to_uppercase();
    Some((format!("{}/api/v4/lbom/parts/get", base), country, language))
}

/// IDs kommen aus der eigenen API-Response – trotzdem nur streng validiert in
/// den POST-Body (alphanumerisch + Bindestrich), damit da nichts injizierbar ist.
pub fn safe_part_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 24 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// specsInfo "DPK: WIN;;CPU: AMDR72700U;;GPU Type: UMA;;" → "DPK: WIN | CPU: AMDR72700U | GPU Type: UMA"
pub fn format_specs(info: &str) -> String {
    info.split(";;")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// ID-Obergrenze pro Specs-POST. 60 war der alte, konservative Wert; an der
/// echten API sind 200 IDs verifiziert (HTTP 200, alle Teile zurück). Wird ein
/// Batch trotzdem als zu groß abgelehnt (4xx), halbiert `fetch_chunk` ihn bis
/// auf diesen sicheren Wert.
pub const SPECS_BATCH_SAFE: usize = 60;

/// Fehler eines Specs-Abrufs. `status` trägt den HTTP-Code, wenn der Server
/// geantwortet hat – daran erkennt der Batch-Aufrufer eine zu große Anfrage.
#[derive(Debug)]
pub struct SpecsError {
    pub status: Option<u16>,
    message: String,
}

impl SpecsError {
    fn new(status: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SpecsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SpecsError {}

/// Lädt die "Technischen Daten" (specsInfo) für eine Liste von Teilenummern.
/// Ein Batch-POST; Fehler sind nicht fatal (Mail geht auch ohne Specs raus).
pub fn fetch_specs(
    cfg: &Config,
    ids: &[&str],
) -> std::result::Result<BTreeMap<String, String>, SpecsError> {
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let (url, country, language) = detail_target(&cfg.api.url).ok_or_else(|| {
        SpecsError::new(
            None,
            "api.url passt nicht zum erwarteten Muster – kein Detail-Abruf möglich",
        )
    })?;
    let valid: Vec<&str> = ids.iter().copied().filter(|i| safe_part_id(i)).collect();
    if valid.is_empty() {
        return Ok(BTreeMap::new());
    }
    // Body direkt in einen String schreiben: kein Vec plus ein Zwischen-String
    // pro Teilenummer (60 pro Batch). `safe_part_id` garantiert alphanumerisch
    // bzw. Bindestrich – Escaping ist nicht nötig.
    let mut body = String::with_capacity(48 + valid.iter().map(|i| i.len() + 3).sum::<usize>());
    body.push_str("{\"country\":\"");
    body.push_str(&country);
    body.push_str("\",\"language\":\"");
    body.push_str(&language);
    body.push_str("\",\"ids\":[");
    for (i, id) in valid.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        body.push('"');
        body.push_str(id);
        body.push('"');
    }
    body.push_str("]}");

    let mut last_err: Option<SpecsError> = None;
    for attempt in 0..2 {
        if attempt > 0 {
            sleep(Duration::from_secs(3));
        }
        let (mut child, stdout, stderr_thread) = spawn_curl(
            cfg,
            Some("POST"),
            Some(&body),
            &[
                ("Accept", "application/json"),
                ("Content-Type", "application/json"),
                ("Referer", &cfg.api.referer),
            ],
            &url,
        )
        .map_err(|e| SpecsError::new(None, format!("Detail-Abruf fehlgeschlagen: {}", e)))?;
        // Streaming-Parse direkt aus der Pipe – der Body landet nie komplett
        // im RAM und nie auf Platte. 64-KB-Puffer: weniger read-Syscalls.
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, stdout);
        let parse_result: Result<DetailResponse, _> = serde_json::from_reader(&mut reader);
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
        let (status, stderr) = finish_curl(&mut child, stderr_thread);
        if status != 200 {
            let err = SpecsError::new(
                Some(status as u16),
                format!("HTTP {} vom Detail-Endpunkt ({})", status, stderr),
            );
            // 4xx (außer Timeout/Rate-Limit) ist endgültig: ein Retry nach 3 s
            // ändert nichts, der Aufrufer kann den Batch stattdessen halbieren.
            if (400..500).contains(&status) && status != 408 && status != 429 {
                return Err(err);
            }
            last_err = Some(err);
            continue;
        }
        match parse_result {
            Ok(r) => {
                let map = detail_map_from_response(r);
                log_info!("Tech-Daten geladen: {}/{} Teile", map.len(), valid.len());
                return Ok(map);
            }
            Err(e) => {
                last_err = Some(SpecsError::new(
                    Some(status as u16),
                    format!("Detail-Response unparsebar: {}", e),
                ));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| SpecsError::new(None, "Detail-Abruf fehlgeschlagen")))
}

/// Ergebnis eines Specs-Batches (id → Specs-Zeile).
type SpecBatch = std::result::Result<BTreeMap<String, String>, SpecsError>;

/// Ein Batch; lehnt der Server ihn als zu groß ab (4xx), wird er halbiert und
/// erneut versucht – die Batch-Größe sinkt so automatisch bis
/// `SPECS_BATCH_SAFE`. Der Fallback greift nur im Fehlerfall und ist selten.
fn fetch_chunk(cfg: &Config, chunk: &[&str]) -> SpecBatch {
    match fetch_specs(cfg, chunk) {
        Ok(m) => Ok(m),
        Err(e)
            if chunk.len() > SPECS_BATCH_SAFE
                && matches!(e.status, Some(400 | 413 | 414 | 422)) =>
        {
            let mid = chunk.len() / 2;
            let mut out = fetch_chunk(cfg, &chunk[..mid])?;
            out.extend(fetch_chunk(cfg, &chunk[mid..])?);
            Ok(out)
        }
        Err(e) => Err(e),
    }
}

/// Lädt Specs für viele IDs in Batches (Default 200, `api.specs_batch`). Wird
/// nur beim ersten Lauf mit Keywords bzw. für neue Teile gebraucht. Bis zu
/// `WORKERS` Batches laufen gleichzeitig; der Startabstand bleibt global
/// erhalten, damit die Anfragenrate nicht steigt – überlappt wird nur die
/// Antwortlatenz.
pub fn fetch_specs_batched(cfg: &Config, ids: &[&str]) -> Result<BTreeMap<String, String>> {
    const WORKERS: usize = 4;
    // 200 IDs pro POST sind an der echten API verifiziert; konfigurierbar,
    // damit sich im CT auch kleinere Werte testen lassen.
    let batch = cfg.api.specs_batch.clamp(1, 200);
    let gap = Duration::from_millis(cfg.api.specs_gap_ms);

    let batches: Vec<&[&str]> = ids.chunks(batch).collect();
    if batches.len() <= 1 {
        let mut out = BTreeMap::new();
        for chunk in batches {
            out.extend(fetch_chunk(cfg, chunk)?);
        }
        return Ok(out);
    }

    let next_batch = AtomicUsize::new(0);
    // Nächster erlaubter Start; Worker reservieren sich ihren Slot.
    let gate = Mutex::new(Instant::now());
    let results: Mutex<Vec<Option<SpecBatch>>> =
        Mutex::new((0..batches.len()).map(|_| None).collect());

    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(batches.len()) {
            scope.spawn(|| loop {
                let i = next_batch.fetch_add(1, Ordering::Relaxed);
                let Some(&chunk) = batches.get(i) else { break };
                let start = {
                    let mut next = gate.lock().expect("Gate vergiftet");
                    let at = (*next).max(Instant::now());
                    *next = at + gap;
                    at
                };
                let wait = start.saturating_duration_since(Instant::now());
                if !wait.is_zero() {
                    sleep(wait);
                }
                let r = fetch_chunk(cfg, chunk);
                results.lock().expect("Ergebnisse vergiftet")[i] = Some(r);
            });
        }
    });

    let mut out = BTreeMap::new();
    for slot in results.into_inner().expect("Ergebnisse vergiftet") {
        out.extend(slot.expect("Worker ohne Ergebnis")?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nachgetragene CPUs ohne R23-Messung: R15/R20 sind auf R23-Skala
    /// umgerechnet, Kurzcodes wie „7Y54" und Lenovos „INTELM76Y75" müssen
    /// trotzdem einen Tabellentreffer haben.
    #[test]
    fn bench_nachzuegler() {
        for (cpu, single, multi) in [
            ("Intel Core i5-6300U", 777, 1834),
            ("Intel Core i5-7300U", 865, 2011),
            ("Intel Core i5-7Y54", 685, 1294),
            ("Intel Core i5-7Y57", 778, 1486),
            ("Intel Core i7-6600U", 836, 1744),
            ("Intel Core i7-7600U", 963, 2225),
            ("INTELM76Y75", 597, 1176),
            ("Intel Core m3-8100Y", 651, 1009),
            ("Intel Pentium5405U", 539, 1390),
            ("Intel E31505M", 1095, 5218),
            ("AMD A69220", 399, 634),
            ("AMD A129700P", 459, 1239),
            ("AMD Ryzen 3 3250C", 785, 1989),
            ("AMD Ryzen 5 3500C", 765, 2752),
        ] {
            assert_eq!(
                cpu_bench(cpu).map(|(s, m, _)| (s, m)),
                Some((single, multi)),
                "{cpu}"
            );
        }
        // Der Fit darf die 5-W-CPU nicht über die Kaby-Lake-Y heben.
        let m3 = cpu_bench("Intel Core m3-8100Y").unwrap();
        let y54 = cpu_bench("Intel Core i5-7Y54").unwrap();
        assert!(m3.0 < y54.0 && m3.1 < y54.1, "{m3:?} vs {y54:?}");
    }

    /// R15-Zeilen tragen die Quelle `cb15` – sie darf nicht als `gb6`
    /// im Tooltip landen.
    #[test]
    fn bench_quelle_bleibt_erhalten() {
        assert_eq!(
            cpu_bench("Intel Core i5-7Y54").map(|(_, _, s)| s),
            Some("cb15")
        );
        assert_eq!(
            cpu_bench("Intel Core m3-8100Y").map(|(_, _, s)| s),
            Some("cb20")
        );
        assert_eq!(
            cpu_bench("Intel Core i5-6300U").map(|(_, _, s)| s),
            Some("r23")
        );
    }

    /// Jeder CPU-Name steht genau einmal in `cpu-scores.tsv`; die kaputten
    /// Lenovo-Schreibweisen werden im Code aufgelöst, nicht über Kopien.
    #[test]
    fn cpu_tabelle_ohne_doppelte_namen() {
        let mut namen: Vec<&str> = bench_table().values().map(|_| "").collect();
        namen.clear();
        for line in tsv_lines(CPU_SCORES_TSV) {
            namen.push(line.split('\t').nth(1).unwrap_or(""));
        }
        let mut sortiert = namen.clone();
        sortiert.sort_unstable();
        let doppelt: Vec<&str> = sortiert
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0])
            .collect();
        assert!(doppelt.is_empty(), "doppelte Namen: {doppelt:?}");
        assert!(namen.len() > 1000, "Tabelle zu klein: {}", namen.len());
    }

    #[test]
    fn igpu_ohne_f_modelle() {
        // „F"/„KF" = Desktop-CPU ohne iGPU, auch wenn die Reihe sonst eine hat.
        for cpu in [
            "Intel Core i5-9400F",
            "Intel Core i9-9900KF",
            "Intel Core Ultra 7 265KF",
        ] {
            assert_eq!(igpu_key(cpu), None, "{cpu}");
        }
        assert_eq!(igpu_key("Intel Core Ultra 7 265K"), Some("arc8coreigpu"));
        assert_eq!(igpu_key("Intel Core i7-14700HX"), Some("irisxeg796eus"));
    }

    #[test]
    fn igpu_short_randfaelle() {
        assert_eq!(
            igpu_short("Intel Graphics 4-Core iGPU (Meteor / Arrow Lake)"),
            "4-Core iGPU"
        );
        assert_eq!(igpu_short("Qualcomm Adreno 618 GPU"), "Adreno 618");
        assert_eq!(igpu_short(""), "");
        assert_eq!(igpu_short("   "), "");
        // Klammer ohne EU-Zahl: nur der Zusatz fällt weg.
        assert_eq!(igpu_short("Radeon R7 (Bristol Ridge)"), "Radeon R7");
        // Nur Klammer, kein Name davor: da ist nichts zu kürzen.
        assert_eq!(igpu_short("(32 EU)"), "");
        assert_eq!(
            igpu_short("Intel UHD Graphics (Jasper Lake 32 EU)"),
            "UHD 32EU"
        );
        assert_eq!(
            igpu_short("Intel UHD Graphics G1 (32 EU, Ice Lake)"),
            "UHD G1 32EU"
        );
        // Klammer nennt nur die Einheit, keine Zahl.
        assert_eq!(igpu_short("Intel UHD Graphics (EU)"), "UHD");
        // Kurzformen, wie sie in der Tabelle stehen.
        assert_eq!(
            igpu_short("Intel Iris Xe Graphics G7 96EUs"),
            "Iris Xe G7 96EU"
        );
        assert_eq!(
            igpu_short("Intel UHD Graphics 64EUs (Alder Lake 12th Gen)"),
            "UHD 64EU"
        );
        // Vega 8 gibt es in zwei Ryzen-Generationen mit 312 Punkten Abstand –
        // die Generation steht deshalb im Kurznamen.
        assert_eq!(
            igpu_short("Radeon Vega 8 (Ryzen 2000/3000)"),
            "Vega 8 (2000/3000)"
        );
        assert_eq!(
            igpu_short("AMD Radeon RX Vega 8 (Ryzen 4000/5000)"),
            "Vega 8 (4000/5000)"
        );
        assert_ne!(
            igpu_short("Radeon Vega 8 (Ryzen 2000/3000)"),
            igpu_short("AMD Radeon RX Vega 8 (Ryzen 4000/5000)")
        );
        // Kurznamen bleiben Kurznamen (idempotent).
        for name in ["Iris Xe G7 96EU", "Vega 8", "UHD 620", "Radeon 610M"] {
            assert_eq!(igpu_short(name), name);
        }
    }

    /// `gpu-scores.tsv` enthält jede Karte genau einmal – gleicher Schlüssel
    /// und gleicher Anzeigename (nur andere Schreibweise) sind verboten.
    /// Verschiedene Modelle wie „GTX 960M" und „GTX 960" bleiben erlaubt.
    #[test]
    fn gpu_tabelle_ist_eindeutig() {
        let mut keys = std::collections::BTreeSet::new();
        let mut namen = std::collections::BTreeSet::new();
        let mut zeilen = 0;
        for line in GPU_SCORES_TSV.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            zeilen += 1;
            let mut f = line.split('\t');
            let (Some(key), Some(name), Some(score)) = (f.next(), f.next(), f.next()) else {
                panic!("kaputte Zeile: {line}");
            };
            assert!(!key.is_empty() && !name.is_empty(), "leeres Feld: {line}");
            assert!(score.parse::<i64>().is_ok(), "Score kaputt: {line}");
            assert!(keys.insert(key), "Schlüssel doppelt: {key}");
            let norm: String = name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .map(|c| c.to_ascii_lowercase())
                .collect();
            assert!(namen.insert(norm), "Name doppelt: {name}");
        }
        assert_eq!(gpu_table().len(), zeilen, "Kollision nach dem Einlesen");
    }

    /// Zwei verschiedene iGPUs dürfen nie denselben Kurznamen tragen (sonst
    /// stehen z. B. Vega 8 mit 851 und mit 1.163 Punkten identisch am Chip).
    #[test]
    fn igpu_short_bleibt_eindeutig() {
        let mut seen: BTreeMap<String, i64> = BTreeMap::new();
        for (name, score, _) in igpu_table().values() {
            let short = igpu_short(name);
            if let Some(prev) = seen.insert(short.clone(), *score) {
                assert_eq!(
                    prev, *score,
                    "„{short}\" steht für {prev} und {score} Punkte"
                );
            }
        }
    }

    /// Jeder CPU-Name steht genau einmal in `cpu-igpu.tsv` – auch Varianten
    /// wie „AMD 3020E" und „AMD 3020e" landen im selben normalisierten Schlüssel.
    #[test]
    fn igpu_tabelle_hat_jede_cpu_genau_einmal() {
        let mut keys = std::collections::BTreeSet::new();
        let mut zeilen = 0;
        for line in CPU_IGPU_TSV.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            zeilen += 1;
            let key = line.split('\t').next().unwrap_or("");
            assert!(!key.is_empty(), "leerer Schlüssel: {line}");
            assert!(
                key.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "Schlüssel nicht normalisiert: {key}"
            );
            assert!(keys.insert(key), "Schlüssel doppelt: {key}");
        }
        assert_eq!(igpu_table().len(), zeilen, "Kollision nach Normalisierung");
    }

    /// Jede Zeile aus `cpu-igpu.tsv` muss einen brauchbaren Chip-Namen ergeben –
    /// so fällt eine schlecht gepflegte Namenzeile sofort auf.
    #[test]
    fn igpu_short_fuer_jede_tabellenzeile() {
        for (key, (name, _, _)) in igpu_table().iter() {
            let short = igpu_short(name);
            assert!(!short.is_empty(), "leer für {key}");
            // Klammern sind nur noch als Generations-Zusatz erlaubt, deshalb
            // hier auf liegen gebliebene Klammer-Inhalte prüfen.
            for bad in ["Ryzen", "EUs", "Graphics", "  "] {
                assert!(!short.contains(bad), "„{bad}\" in {key}: {short}");
            }
        }
    }
}
