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

fn image_url(id: &str) -> String {
    format!("https://download.lenovo.com/Images/Parts/{id}/{id}_A.jpg")
}

/// True, wenn die Datei existiert und jünger als eine Woche ist.
fn recent(path: &Path) -> bool {
    const WEEK: std::time::Duration = std::time::Duration::from_secs(7 * 86_400);
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < WEEK)
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
    // gilt eine Woche und verhindert, dass jeder Seitenaufbau neu fragt.
    let miss = path.with_extension("missing");
    if recent(&miss) {
        return None;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Some(raw) = download(cfg, &image_url(id)) else {
        let _ = std::fs::write(&miss, b"");
        return None;
    };
    let small = shrink(&raw, width)?;
    let _ = std::fs::write(&path, &small);
    Some(small)
}

/// Original holen: eigener curl-Pfad, weil die Bild-URLs ohne Bot-Schutz
/// direkt ausgeliefert werden (kein Impersonation-Binary nötig).
fn download(cfg: &Config, url: &str) -> Option<Vec<u8>> {
    let out = Command::new(&cfg.api.image_curl_path)
        .args(["-sS", "-f", "--max-time", "25", url])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(out.stdout)
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
    fn marker_liegt_neben_dem_cache_und_gilt_eine_woche() {
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
        assert!(!recent(&marker), "fehlender Marker gilt nicht");
        std::fs::write(&marker, b"").expect("Marker");
        assert!(recent(&marker), "frischer Marker gilt");
        let _ = std::fs::remove_dir_all(&dir);
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
