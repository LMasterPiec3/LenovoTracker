//! Helfer: Logging-Makros, Zeit-/Zahl-/Escape-Utilities, Konstanten.

use std::path::Path;

pub const PAGE_URL: &str = "https://support.lenovo.com/de/de/parts-lookup/clearance-sale";
pub const DEFAULT_CONFIG_PATH: &str = "/etc/lenovotracker/config.toml";
pub const BACKOFFS: [u64; 3] = [5, 15, 45];

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// RFC3339 (UTC, Sekunden-Genauigkeit) ohne `time`-Crate: Hinnants
/// civil_from_days, damit keine Abhängigkeit für eine Log-Zeile nötig ist.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        yoe + era * 400 + if m <= 2 { 1 } else { 0 },
        m as u32,
        d as u32,
    )
}

pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

macro_rules! log_info {
    ($($arg:tt)*) => { println!("[{} INFO] {}", now_iso(), format_args!($($arg)*)) };
}
macro_rules! log_warn {
    ($($arg:tt)*) => { println!("[{} WARN] {}", now_iso(), format_args!($($arg)*)) };
}
macro_rules! log_error {
    ($($arg:tt)*) => { println!("[{} ERROR] {}", now_iso(), format_args!($($arg)*)) };
}

/// (aktuelles RSS, bisheriger Peak) in KiB – Linux über /proc/self/status,
/// Windows über GetProcessMemoryInfo; andere Plattformen: (None, None).
pub fn rss_kib() -> (Option<u64>, Option<u64>) {
    current_mem_kib()
}

#[cfg(target_os = "linux")]
fn current_mem_kib() -> (Option<u64>, Option<u64>) {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return (None, None),
    };
    let field = |name: &str| -> Option<u64> {
        status.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?;
            rest.split_whitespace().next()?.parse().ok()
        })
    };
    (field("VmRSS:"), field("VmHWM:"))
}

#[cfg(windows)]
fn current_mem_kib() -> (Option<u64>, Option<u64>) {
    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    let cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    // SAFETY: cb entspricht der Struct-Größe; die API schreibt ausschließlich
    // in den übergebenen Puffer des aktuellen Prozesses.
    unsafe {
        let mut counters: ProcessMemoryCounters = std::mem::zeroed();
        counters.cb = cb;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, cb) == 0 {
            return (None, None);
        }
        (
            Some(counters.working_set_size as u64 / 1024),
            Some(counters.peak_working_set_size as u64 / 1024),
        )
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn current_mem_kib() -> (Option<u64>, Option<u64>) {
    (None, None)
}

pub fn fmt_de(number: i64) -> String {
    let negative = number < 0;
    let digits = number.abs().to_string();
    let len = digits.len();
    // Kapazität: Ziffern + Tausenderpunkte + optionales Minus
    let mut out = String::with_capacity(len + len / 3 + usize::from(negative));
    if negative {
        out.push('-');
    }
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(ch);
    }
    out
}

/// 5989 → "59,89 €" (Tausenderpunkt, Dezimalkomma); unbekannte Währung → "59,89".
pub fn fmt_money(cents: i64, symbol: &str) -> String {
    let cents = cents.max(0);
    let mut out = fmt_de(cents / 100);
    out.push(',');
    out.push_str(&format!("{:02}", cents % 100));
    if !symbol.is_empty() {
        out.push(' ');
        out.push_str(symbol);
    }
    out
}

/// Währungssymbol aus dem Locale-Pfad der API-URL – die API trägt kein
/// Währungsfeld, das Locale bestimmt Preis und Währung (Lenovo-API.md §9).
/// Die Regionen, die Lenovo dort anbietet, sind DE/EU (€), GB (£), US ($),
/// JP/CN (¥); alles ohne `/api/` in der URL bekommt kein Symbol.
pub fn currency_symbol(api_url: &str) -> &'static str {
    let Some(idx) = api_url.rfind("/api/") else {
        return "";
    };
    let mut segs = api_url[..idx].rsplit('/').filter(|s| !s.is_empty());
    let _language = segs.next();
    match segs.next().unwrap_or("").to_ascii_uppercase().as_str() {
        "GB" => "£",
        "US" => "$",
        "JP" | "CN" => "¥",
        _ => "€",
    }
}

/// Datei atomar ersetzen: temporäre Datei im selben Verzeichnis, dann rename.
/// Ein abgebrochener Schreibvorgang lässt so nie eine halbe Datei zurück.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = std::path::PathBuf::from(format!("{}.tmp", path.display()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn esc(s: &str) -> String {
    // Häufigster Fall: nichts zu escapen – ein Scan, eine Allocation.
    if !s
        .as_bytes()
        .iter()
        .any(|&b| matches!(b, b'&' | b'<' | b'>' | b'"'))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}
