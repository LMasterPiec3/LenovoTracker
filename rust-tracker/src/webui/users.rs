//! Benutzer und Sitzungen: Login mit Benutzername/Passwort, Rollen `admin` und
//! `user`. Die Benutzer stehen in `users.toml` neben der Config, die Passwörter
//! als PBKDF2-HMAC-SHA256 (`pbkdf2$runden$salt$hash`).
//!
//! Sitzungen liegen nur im Speicher: ein Neustart meldet alle ab. Der Cookie
//! ist `HttpOnly` und `SameSite=Lax`, gilt 12 Stunden und wird bei jedem
//! Login neu vergeben.

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const SESSION_COOKIE: &str = "lt_sid";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
const ROUNDS: u32 = 120_000;
const MIN_PASS: usize = 8;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Role {
    Admin,
    User,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }

    fn parse(text: &str) -> Role {
        if text == "admin" {
            Role::Admin
        } else {
            Role::User
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    hash: String,
    #[serde(default = "role_default")]
    role: String,
}

fn role_default() -> String {
    "user".to_string()
}

#[derive(Default, Serialize, Deserialize)]
struct UserFile {
    #[serde(default)]
    users: BTreeMap<String, Entry>,
}

struct Session {
    user: String,
    role: Role,
    until: Instant,
}

static SESSIONS: Mutex<Vec<(String, Session)>> = Mutex::new(Vec::new());

// ---------------------------------------------------------------------------
// Benutzerdatei
// ---------------------------------------------------------------------------

fn users_path(config_path: &str) -> std::path::PathBuf {
    std::path::Path::new(config_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("users.toml")
}

fn load_users(config_path: &str) -> UserFile {
    std::fs::read_to_string(users_path(config_path))
        .ok()
        .and_then(|text| basic_toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_users(config_path: &str, file: &UserFile) -> Result<(), String> {
    let text = basic_toml::to_string(file).map_err(|e| e.to_string())?;
    let path = users_path(config_path);
    write_atomic(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600));
    Ok(())
}

/// Gibt es überhaupt Benutzer? Ohne Benutzer und ohne `admin_pass` bleibt die
/// Seite offen wie bisher.
fn users_exist(config_path: &str) -> bool {
    !load_users(config_path).users.is_empty()
}

// ---------------------------------------------------------------------------
// Passwörter
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    let mut outer = Sha256::new();
    for byte in block.iter() {
        inner.update([byte ^ 0x36]);
        outer.update([byte ^ 0x5c]);
    }
    inner.update(data);
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn pbkdf2(pass: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut seed = salt.to_vec();
    seed.extend_from_slice(&[0, 0, 0, 1]);
    let mut u = hmac(pass, &seed);
    let mut out = u;
    for _ in 1..rounds.max(1) {
        u = hmac(pass, &u);
        for (a, b) in out.iter_mut().zip(u.iter()) {
            *a ^= b;
        }
    }
    out
}

fn hash_password(pass: &str) -> String {
    let salt = random_bytes(16);
    let hash = pbkdf2(pass.as_bytes(), &salt, ROUNDS);
    format!("pbkdf2${}${}${}", ROUNDS, hex(&salt), hex(&hash))
}

fn verify_password(stored: &str, pass: &str) -> bool {
    let mut parts = stored.split('$');
    let (Some("pbkdf2"), Some(rounds), Some(salt), Some(want)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(rounds), Some(salt), Some(want)) = (rounds.parse::<u32>(), unhex(salt), unhex(want))
    else {
        return false;
    };
    ct_eq(&pbkdf2(pass.as_bytes(), &salt, rounds), &want)
}

/// Zufall aus `/dev/urandom`; ohne die Datei ein Notbehelf aus der Uhrzeit.
fn random_bytes(n: usize) -> Vec<u8> {
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let mut buf = vec![0u8; n];
        use std::io::Read;
        if file.read_exact(&mut buf).is_ok() {
            return buf;
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (0..n)
        .map(|i| ((nanos >> (i % 16 * 8)) as u8) ^ (i as u8))
        .collect()
}

// ---------------------------------------------------------------------------
// Sitzungen
// ---------------------------------------------------------------------------

fn sessions() -> std::sync::MutexGuard<'static, Vec<(String, Session)>> {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner())
}

fn open_session(user: &str, role: Role) -> String {
    let token = hex(&random_bytes(24));
    let now = Instant::now();
    let mut list = sessions();
    list.retain(|(_, s)| s.until > now);
    list.push((
        token.clone(),
        Session {
            user: user.to_string(),
            role,
            until: now + SESSION_TTL,
        },
    ));
    token
}

fn close_session(token: &str) {
    sessions().retain(|(t, _)| t != token);
}

/// Angemeldeter Benutzer aus dem Cookie, sofern die Sitzung noch gilt.
pub(crate) fn session_user(headers: &BTreeMap<String, String>) -> Option<(String, Role)> {
    let token = cookie(headers, SESSION_COOKIE)?;
    let now = Instant::now();
    let list = sessions();
    let (_, session) = list.iter().find(|(t, s)| *t == token && s.until > now)?;
    Some((session.user.clone(), session.role))
}

/// Ist die Anfrage ein Admin – per Sitzung oder per HTTP-Basic aus der Config?
pub(crate) fn is_admin(cfg: &Config, req: &Request) -> bool {
    admin_ok(cfg, &req.headers)
        || session_user(&req.headers).is_some_and(|(_, role)| role == Role::Admin)
}

fn cookie(headers: &BTreeMap<String, String>, name: &str) -> Option<String> {
    let raw = headers.get("cookie")?;
    raw.split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn set_cookie(token: &str) -> String {
    format!(
        "Set-Cookie: {}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}\r\n",
        SESSION_COOKIE,
        token,
        SESSION_TTL.as_secs()
    )
}

// ---------------------------------------------------------------------------
// Routen
// ---------------------------------------------------------------------------

/// Braucht diese Anfrage eine Anmeldung? Seite, Login und Abmelden bleiben offen.
pub(crate) fn login_required(cfg: &Config, config_path: &str, req: &Request) -> bool {
    let open = matches!(
        (req.method.as_str(), req.path.as_str()),
        ("GET", "/")
            | ("GET", "/index.html")
            | ("POST", "/api/login")
            | ("GET", "/api/me")
            | ("POST", "/api/logout")
    );
    !open && (!cfg.webui.admin_pass.is_empty() || users_exist(config_path))
}

pub(crate) fn login(cfg: &Config, config_path: &str, body: &[u8]) -> Response {
    #[derive(Deserialize)]
    struct Input {
        user: String,
        pass: String,
    }
    let input: Input = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::error(400, &format!("Ungültiges JSON: {}", e)),
    };
    if let Some(wait) = login_wait() {
        return too_many(wait);
    }
    let name = input.user.trim().to_ascii_lowercase();
    let file = load_users(config_path);
    let (role, ok) = match file.users.get(&name) {
        Some(entry) => (
            Role::parse(&entry.role),
            verify_password(&entry.hash, &input.pass),
        ),
        // Startzugang aus der Config, solange noch kein Benutzer angelegt ist.
        None => {
            let admin = cfg.webui.admin_user.to_ascii_lowercase();
            let same = ct_eq(name.as_bytes(), admin.as_bytes());
            (
                Role::Admin,
                !cfg.webui.admin_pass.is_empty()
                    & same
                    & ct_eq(input.pass.as_bytes(), cfg.webui.admin_pass.as_bytes()),
            )
        }
    };
    if !ok {
        return login_brake(cfg)
            .unwrap_or_else(|| Response::error(401, "Benutzer oder Passwort falsch."));
    }
    note_ok();
    let token = open_session(&name, role);
    let mut resp = Response::json(&json!({ "user": name, "role": role.as_str() }));
    resp.extra = set_cookie(&token);
    resp
}

pub(crate) fn logout(req: &Request) -> Response {
    if let Some(token) = cookie(&req.headers, SESSION_COOKIE) {
        close_session(&token);
    }
    let mut resp = Response::json(&json!({ "ok": true }));
    resp.extra = format!(
        "Set-Cookie: {}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0\r\n",
        SESSION_COOKIE
    );
    resp
}

pub(crate) fn me_json(cfg: &Config, req: &Request) -> Response {
    if let Some((user, role)) = session_user(&req.headers) {
        return Response::json(&json!({ "user": user, "role": role.as_str() }));
    }
    // Basic-Zugang zählt als Admin (Notzugang, wenn kein Benutzer angelegt ist).
    if admin_ok(cfg, &req.headers) {
        return Response::json(&json!({ "user": cfg.webui.admin_user, "role": "admin" }));
    }
    Response::error(401, "nicht angemeldet")
}

/// Benutzerverwaltung: nur Admins.
pub(crate) fn users_route(cfg: &Config, config_path: &str, req: &Request) -> Response {
    if !is_admin(cfg, req) {
        return Response::error(403, "nur für Admins");
    }
    match req.method.as_str() {
        "GET" => {
            let file = load_users(config_path);
            let list: Vec<serde_json::Value> = file
                .users
                .iter()
                .map(|(name, entry)| json!({ "user": name, "role": Role::parse(&entry.role).as_str() }))
                .collect();
            Response::json(&json!({ "users": list }))
        }
        "POST" => save_user(config_path, &req.body),
        "DELETE" => delete_user(config_path, &req.body),
        _ => Response::error(405, "Methode nicht erlaubt"),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

fn save_user(config_path: &str, body: &[u8]) -> Response {
    #[derive(Deserialize)]
    struct Input {
        user: String,
        #[serde(default)]
        pass: String,
        #[serde(default)]
        role: String,
    }
    let input: Input = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::error(400, &format!("Ungültiges JSON: {}", e)),
    };
    let name = input.user.trim().to_ascii_lowercase();
    if !valid_name(&name) {
        return Response::error(
            400,
            "Benutzername: 1–32 Zeichen, nur a–z, 0–9, Punkt, Unterstrich, Minus",
        );
    }
    let role = Role::parse(input.role.trim());
    let mut file = load_users(config_path);
    let entry = file.users.get(&name).cloned();
    if entry.is_none() && input.pass.chars().count() < MIN_PASS {
        return Response::error(400, &format!("Passwort: mindestens {} Zeichen", MIN_PASS));
    }
    if !input.pass.is_empty() && input.pass.chars().count() < MIN_PASS {
        return Response::error(400, &format!("Passwort: mindestens {} Zeichen", MIN_PASS));
    }
    // Den letzten Admin nicht zum einfachen Benutzer machen.
    let last_admin = entry
        .as_ref()
        .is_some_and(|e| Role::parse(&e.role) == Role::Admin)
        && file.users.len() == 1;
    if last_admin && role == Role::User {
        return Response::error(400, "mindestens ein Admin muss bleiben");
    }
    let hash = if input.pass.is_empty() {
        entry.map(|e| e.hash).unwrap_or_default()
    } else {
        hash_password(&input.pass)
    };
    file.users.insert(
        name.clone(),
        Entry {
            hash,
            role: role.as_str().to_string(),
        },
    );
    match save_users(config_path, &file) {
        Ok(()) => Response::json(&json!({ "ok": true, "user": name, "role": role.as_str() })),
        Err(e) => Response::error(500, &format!("Schreiben fehlgeschlagen: {}", e)),
    }
}

fn delete_user(config_path: &str, body: &[u8]) -> Response {
    #[derive(Deserialize)]
    struct Input {
        user: String,
    }
    let input: Input = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return Response::error(400, &format!("Ungültiges JSON: {}", e)),
    };
    let name = input.user.trim().to_ascii_lowercase();
    let mut file = load_users(config_path);
    let Some(entry) = file.users.get(&name) else {
        return Response::error(404, "Benutzer nicht gefunden");
    };
    if Role::parse(&entry.role) == Role::Admin
        && file
            .users
            .values()
            .filter(|e| Role::parse(&e.role) == Role::Admin)
            .count()
            == 1
    {
        return Response::error(400, "der letzte Admin lässt sich nicht löschen");
    }
    file.users.remove(&name);
    // Offene Sitzungen des Benutzers sofort beenden.
    sessions().retain(|(_, s)| s.user != name);
    match save_users(config_path, &file) {
        Ok(()) => Response::json(&json!({ "ok": true })),
        Err(e) => Response::error(500, &format!("Schreiben fehlgeschlagen: {}", e)),
    }
}
