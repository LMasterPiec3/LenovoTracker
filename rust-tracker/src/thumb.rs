//! Vorschaubilder für Liste und Tabelle.
//!
//! Lenovo liefert je Teil ein JPEG mit 2736×1824 Pixeln und ~275 KB. Für Karten
//! und Tabellenzeilen wird daraus einmalig ein kleines JPEG gebaut (320 px) und
//! im State-Verzeichnis gecacht – die Startseite lädt also nur ein paar KB je
//! Teil. Die Originale werden ausschließlich im Bild-Dialog geladen.

use crate::config::*;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Kantenlänge der Vorschau: quadratisch eingepasst, Seitenverhältnis bleibt.
pub const THUMB_WIDTH: u32 = 320;

fn cache_path(state_dir: &str, id: &str, width: u32) -> PathBuf {
    PathBuf::from(state_dir)
        .join("thumbs")
        .join(format!("{}_{}w.jpg", id, width))
}

/// Mögliche Bild-URLs eines Teils. Neuere Teile heißen `<ID>_A.jpg`, ältere
/// (z. B. 00XK218) liegen als `1.jpg` … `3.jpg` daneben – beide Schemata
/// prüfen, sonst bleibt jede fünfte Karte ohne Foto. Reihenfolge = Vorrang.
fn image_urls(id: &str) -> Vec<String> {
    let base = format!("https://download.lenovo.com/Images/Parts/{id}");
    let mut urls = vec![format!("{base}/{id}_A.jpg")];
    for n in 1..=3 {
        urls.push(format!("{base}/{n}.jpg"));
    }
    urls
}

/// Kandidaten der Reihe nach holen; das erste echte Bild gewinnt. `NoImage`
/// nur, wenn jeder Kandidat 404 liefert – ein Aussetzer bleibt wiederholbar.
fn fetch_first(cfg: &Config, urls: &[String]) -> Fetch {
    let mut failed = false;
    for url in urls {
        match fetch(cfg, url) {
            Fetch::Ok(raw) => return Fetch::Ok(raw),
            Fetch::NoImage => continue,
            Fetch::Failed => failed = true,
        }
    }
    if failed {
        Fetch::Failed
    } else {
        Fetch::NoImage
    }
}

/// Marker „Teil ohne Bild“ nur einen Tag gelten lassen: ein einzelner
/// Aussetzer bei Lenovo soll die Karte nicht tagelang leer lassen.
fn recent_miss(path: &Path) -> bool {
    const DAY: std::time::Duration = std::time::Duration::from_secs(86_400);
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < DAY)
}

/// Vorschau als JPEG-Bytes: aus dem Cache, sonst frisch gebaut.
/// `None`, wenn das Teil kein Bild hat (Lenovo liefert dann 404).
pub fn thumb(cfg: &Config, id: &str, width: u32) -> Option<Vec<u8>> {
    if !crate::api::safe_part_id(id) {
        return None;
    }
    let path = cache_path(&cfg.paths.state_dir, id, width);
    if let Ok(bytes) = std::fs::read(&path) {
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }
    // Teile ohne Bild: Lenovo antwortet dann 404. Der Marker neben dem Cache
    // gilt einen Tag und verhindert, dass jeder Seitenaufbau neu fragt.
    let miss = path.with_extension("missing");
    if recent_miss(&miss) {
        return None;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let raw = match fetch_first(cfg, &image_urls(id)) {
        Fetch::Ok(raw) => raw,
        // 404: das Teil hat wirklich kein Bild – einen Tag lang merken.
        Fetch::NoImage => {
            let _ = std::fs::write(&miss, b"");
            return None;
        }
        // Netz- oder curl-Fehler: kein Marker, der nächste Aufruf versucht es
        // erneut. Sonst blendet ein kurzer Aussetzer die Bilder eine Woche aus.
        Fetch::Failed => return None,
    };
    let small = shrink(&raw, width)?;
    let _ = std::fs::write(&path, &small);
    Some(small)
}

/// Fehlende Vorschauen nacheinander bauen, mit Pause zwischen zwei Teilen –
/// die Seite zeigt bis zu 100 Karten, ein kaltes Bild kostet ~0,7 s (Original
/// holen + 5-MP-JPEG dekodieren), warm sind es Millisekunden. Liefert die Zahl
/// der neu gebauten Bilder.
pub fn warm(cfg: &Config, ids: &[String], gap: std::time::Duration) -> usize {
    let mut built = 0;
    for id in ids {
        if !crate::api::safe_part_id(id) {
            continue;
        }
        let path = cache_path(&cfg.paths.state_dir, id, THUMB_WIDTH);
        let miss = path.with_extension("missing");
        if path.exists() || recent_miss(&miss) {
            continue;
        }
        if thumb(cfg, id, THUMB_WIDTH).is_some() {
            built += 1;
        }
        std::thread::sleep(gap);
    }
    built
}

/// Ergebnis eines Bild-Abrufs: Bild, „gibt es nicht“ (HTTP 4xx/5xx) oder ein
/// Fehler, der beim nächsten Versuch wiederholt werden darf.
enum Fetch {
    Ok(Vec<u8>),
    NoImage,
    Failed,
}

/// Nur ein echtes 404 heißt „dieses Teil hat kein Bild“. 403, 429 oder 503
/// (Bot-Schutz, Drosselung bei schnellem Abruf) sind Aussetzer und werden
/// beim nächsten Aufruf wiederholt – sonst bleibt die Karte tagelang leer.
fn is_missing_code(code: &str) -> bool {
    code == "404"
}

/// Original holen: eigener curl-Pfad, weil die Bild-URLs ohne Bot-Schutz
/// direkt ausgeliefert werden (kein Impersonation-Binary nötig). Der Rumpf
/// landet in einer temporären Datei, damit `-w` den Statuscode sauber auf
/// stdout ausgeben kann (er entscheidet über „kein Bild“ gegen „nochmal“).
fn fetch(cfg: &Config, url: &str) -> Fetch {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let part = std::env::temp_dir().join(format!("lt-img-{}-{}.part", std::process::id(), n));
    let out = match Command::new(&cfg.api.image_curl_path)
        .args(["-sS", "--max-time", "25", "-o"])
        .arg(&part)
        .arg("-w")
        .arg("%{http_code}")
        .arg(url)
        .output()
    {
        Ok(out) => out,
        Err(_) => return Fetch::Failed,
    };
    let code = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let result = if is_missing_code(&code) {
        Fetch::NoImage
    } else if !out.status.success() || code != "200" {
        Fetch::Failed
    } else {
        match std::fs::read(&part) {
            Ok(bytes) if !bytes.is_empty() => Fetch::Ok(bytes),
            _ => Fetch::Failed,
        }
    };
    let _ = std::fs::remove_file(&part);
    result
}

/// JPEG auf `width` px einpassen und kompakt neu kodieren.
fn shrink(raw: &[u8], width: u32) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    let img = image::load_from_memory_with_format(raw, image::ImageFormat::Jpeg).ok()?;
    let small = if img.width() > width || img.height() > width {
        img.resize(width, width, FilterType::Triangle)
    } else {
        img
    };
    let mut out = Vec::with_capacity(24 * 1024);
    JpegEncoder::new_with_quality(&mut out, 78)
        .encode_image(&small)
        .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_pfad_enthaelt_id_und_breite() {
        let p = cache_path("/tmp/lt-thumb", "5B21K93288", 320);
        assert!(p.ends_with("thumbs/5B21K93288_320w.jpg"), "{:?}", p);
    }

    #[test]
    fn marker_liegt_neben_dem_cache_und_gilt_einen_tag() {
        let marker = cache_path("/tmp/lt-thumb", "5B21K93288", 320).with_extension("missing");
        assert!(
            marker.ends_with("thumbs/5B21K93288_320w.missing"),
            "{:?}",
            marker
        );
        let dir = std::env::temp_dir().join(format!("lt-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("Temp-Verzeichnis");
        let marker = dir.join("A_320w.missing");
        let _ = std::fs::remove_file(&marker);
        assert!(!recent_miss(&marker), "fehlender Marker gilt nicht");
        std::fs::write(&marker, b"").expect("Marker");
        assert!(recent_miss(&marker), "frischer Marker gilt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kandidaten_decken_beide_namensschemata() {
        let urls = image_urls("00XK218");
        assert_eq!(urls.len(), 4);
        assert!(urls[0].ends_with("00XK218/00XK218_A.jpg"), "{}", urls[0]);
        assert!(urls[1].ends_with("00XK218/1.jpg"), "{}", urls[1]);
        assert!(urls[3].ends_with("00XK218/3.jpg"), "{}", urls[3]);
    }

    #[test]
    fn nur_404_gilt_als_kein_bild() {
        assert!(is_missing_code("404"));
        for code in ["200", "403", "429", "500", "503", "000", ""] {
            assert!(!is_missing_code(code), "{} ist ein Aussetzer", code);
        }
    }

    #[test]
    fn shrink_verkleinert_und_bleibt_jpeg() {
        // 800×600-Testbild erzeugen, dann auf 320 px einpassen.
        let src = image::RgbImage::from_fn(800, 600, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut raw = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut raw, 90)
            .encode_image(&image::DynamicImage::ImageRgb8(src))
            .expect("Test-JPEG");
        let small = shrink(&raw, 320).expect("verkleinert");
        assert!(small.starts_with(&[0xFF, 0xD8]), "kein JPEG-Header");
        let img = image::load_from_memory(&small).expect("lesbar");
        assert_eq!((img.width(), img.height()), (320, 240));
        assert!(small.len() < raw.len());
    }

    #[test]
    fn kaputte_daten_liefern_none() {
        assert!(shrink(b"kein jpeg", 320).is_none());
    }
}
