//! Takes the Ember Games Launcher's log bundles, keeps them for a while and tells the developers about each one
//! (docs/LOG_UPLOADS.md). A bundle is checked but never unpacked here.

use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Cursor;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct Config {
    pub dir: PathBuf,
    /// Bytes all kept bundles may use together.
    pub budget: u64,
    pub webhook: Option<String>,
    /// Signs download links.
    pub key: Vec<u8>,
    /// The service's own https address, for the links.
    pub public: String,
    /// Behind a reverse proxy: the client is the last X-Forwarded-For address.
    pub trust_proxy: bool,
    pub max_bundle: usize,
    pub per_hour: usize,
    pub per_day: usize,
    pub keep_days: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dir: PathBuf::from("/var/lib/ember-logs"),
            budget: 20 << 30,
            webhook: None,
            key: Vec::new(),
            public: String::new(),
            trust_proxy: false,
            max_bundle: 25 << 20,
            per_hour: 5,
            per_day: 20,
            keep_days: 90,
        }
    }
}

pub struct AppState {
    cfg: Config,
    /// Upload times per hashed address; in memory only.
    seen: Mutex<HashMap<[u8; 32], Vec<u64>>>,
    salt: [u8; 32],
    full_told: AtomicBool,
    http: reqwest::Client,
}

impl AppState {
    pub fn new(cfg: Config) -> AppState {
        let mut salt = [0u8; 32];
        getrandom::getrandom(&mut salt).expect("random");
        AppState { cfg, seen: Mutex::default(), salt, full_told: AtomicBool::new(false), http: reqwest::Client::new() }
    }

    /// Removes bundles older than the keep time. Returns how many went.
    pub fn sweep(&self, now: u64) -> usize {
        let limit = now.saturating_sub(self.cfg.keep_days * 86_400);
        let mut removed = 0;
        for (path, _, at) in stored(&self.cfg.dir) {
            if at < limit {
                let _ = std::fs::remove_file(path.with_extension("json"));
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Counts an attempt from `ip`; false when it is over the hourly or daily limit.
    fn admit(&self, ip: IpAddr, now: u64) -> bool {
        let mut h = Sha256::new();
        h.update(self.salt);
        h.update(ip.to_string());
        let id: [u8; 32] = h.finalize().into();
        let mut seen = self.seen.lock().unwrap();
        seen.retain(|_, times| {
            times.retain(|t| now - t < 86_400);
            !times.is_empty()
        });
        let times = seen.entry(id).or_default();
        let hour = times.iter().filter(|t| now - **t < 3_600).count();
        if hour >= self.cfg.per_hour || times.len() >= self.cfg.per_day {
            return false;
        }
        times.push(now);
        true
    }

    fn sign(&self, code: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.cfg.key).expect("hmac key");
        mac.update(code.as_bytes());
        hex::encode(&mac.finalize().into_bytes()[..16])
    }

    fn signed(&self, code: &str, k: &str) -> bool {
        let Ok(given) = hex::decode(k) else { return false };
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.cfg.key).expect("hmac key");
        mac.update(code.as_bytes());
        given.len() == 16 && mac.verify_truncated_left(&given).is_ok()
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Kept bundles: path, size, upload time (from the file's modification time).
fn stored(dir: &std::path::Path) -> Vec<(PathBuf, u64, u64)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "zip"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let at = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            Some((e.path(), m.len(), at))
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub schema: u32,
    pub launcher: String,
    pub os: String,
    pub games: Vec<GameMeta>,
    #[serde(default)]
    pub note: String,
    pub bundle_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GameMeta {
    pub id: String,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
}

fn is_id(s: &str) -> bool {
    (1..=16).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn short_text(s: &str) -> bool {
    s.len() <= 64 && !s.chars().any(char::is_control)
}

impl Meta {
    fn check(&self) -> Result<(), &'static str> {
        let fine = self.schema == 1
            && short_text(&self.launcher)
            && ["windows", "linux", "steamdeck"].contains(&self.os.as_str())
            && (1..=8).contains(&self.games.len())
            && self.games.iter().all(|g| {
                is_id(&g.id) && g.channel.as_deref().is_none_or(short_text) && g.tag.as_deref().is_none_or(short_text)
            })
            && self.note.chars().count() <= 1_000
            && self.bundle_sha256.len() == 64
            && self.bundle_sha256.bytes().all(|b| b.is_ascii_hexdigit());
        fine.then_some(()).ok_or("meta")
    }
}

const MAX_ENTRIES: usize = 200;
const MAX_UNPACKED: u64 = 128 << 20;
const EXTENSIONS: [&str; 4] = [".log", ".txt", ".json", ".dmp"];

/// Checks a bundle without unpacking it: the hash, the entry count, the names (manifest.json and
/// <game>/<file> for the games in the meta) and the unpacked size the headers give.
pub fn check_bundle(bytes: &[u8], meta: &Meta) -> Result<usize, &'static str> {
    if hex::encode(Sha256::digest(bytes)) != meta.bundle_sha256.to_ascii_lowercase() {
        return Err("sha256");
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "zip")?;
    if zip.len() > MAX_ENTRIES {
        return Err("entries");
    }
    let mut total = 0u64;
    let mut manifest = false;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(|_| "zip")?;
        let name = f.name();
        total = total.saturating_add(f.size());
        if name == "manifest.json" {
            manifest = true;
            continue;
        }
        let Some((game, file)) = name.split_once('/') else { return Err("name") };
        let ok_file = !file.is_empty()
            && !file.starts_with('.')
            && !file.contains(['/', '\\', ':'])
            && !file.chars().any(char::is_control)
            && file.len() <= 128
            && EXTENSIONS.iter().any(|x| file.to_ascii_lowercase().ends_with(x));
        if !ok_file || !meta.games.iter().any(|g| g.id == game) {
            return Err("name");
        }
    }
    if !manifest {
        return Err("manifest");
    }
    if total > MAX_UNPACKED {
        return Err("unpacked");
    }
    Ok(zip.len())
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A report code like L-7F3K-Q2MX: 40 random bits.
pub fn new_code() -> String {
    let mut r = [0u8; 8];
    getrandom::getrandom(&mut r).expect("random");
    let c: String = r.iter().map(|b| CROCKFORD[(*b & 31) as usize] as char).collect();
    format!("L-{}-{}", &c[..4], &c[4..])
}

fn is_code(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 11
        && s.starts_with("L-")
        && b[6] == b'-'
        && b.iter().enumerate().all(|(i, c)| i < 2 || i == 6 || CROCKFORD.contains(c))
}

pub fn router(state: Arc<AppState>) -> Router {
    let limit = state.cfg.max_bundle + (64 << 10);
    Router::new()
        .route("/v1/logs", post(upload))
        .route("/v1/logs/{code}", get(download))
        .route("/health", get(|| async { "ok" }))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state)
}

fn refuse(status: StatusCode, reason: &str) -> Response {
    (status, Json(serde_json::json!({ "error": reason }))).into_response()
}

fn client_ip(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> Option<IpAddr> {
    if state.cfg.trust_proxy {
        let last = headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next()?.trim().to_string();
        return last.parse().ok();
    }
    peer.map(|p| p.ip())
}

async fn upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    mut form: Multipart,
) -> Response {
    let Some(ip) = client_ip(&state, &headers, peer.map(|Extension(ConnectInfo(a))| a)) else {
        return refuse(StatusCode::BAD_REQUEST, "address");
    };
    let now = now();
    if !state.admit(ip, now) {
        return refuse(StatusCode::TOO_MANY_REQUESTS, "too_many");
    }
    let (mut meta, mut bundle) = (None, None);
    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                return refuse(StatusCode::PAYLOAD_TOO_LARGE, "too_large")
            }
            Err(_) => return refuse(StatusCode::UNPROCESSABLE_ENTITY, "form"),
        };
        let name = field.name().unwrap_or_default().to_string();
        let Ok(bytes) = field.bytes().await else { return refuse(StatusCode::PAYLOAD_TOO_LARGE, "too_large") };
        match name.as_str() {
            "meta" if meta.is_none() && bytes.len() <= 8 << 10 => meta = Some(bytes),
            "bundle" if bundle.is_none() => bundle = Some(bytes),
            _ => return refuse(StatusCode::UNPROCESSABLE_ENTITY, "form"),
        }
    }
    let (Some(meta), Some(bundle)) = (meta, bundle) else { return refuse(StatusCode::UNPROCESSABLE_ENTITY, "form") };
    if bundle.len() > state.cfg.max_bundle {
        return refuse(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    }
    let Ok(meta) = serde_json::from_slice::<Meta>(&meta) else {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, "meta");
    };
    if let Err(r) = meta.check() {
        return refuse(StatusCode::UNPROCESSABLE_ENTITY, r);
    }
    let entries = match check_bundle(&bundle, &meta) {
        Ok(n) => n,
        Err(r) => return refuse(StatusCode::UNPROCESSABLE_ENTITY, r),
    };
    let used: u64 = stored(&state.cfg.dir).iter().map(|s| s.1).sum();
    if used + bundle.len() as u64 > state.cfg.budget {
        if !state.full_told.swap(true, Ordering::SeqCst) {
            notify(&state, "Log storage is full; new bundles are refused until old ones expire.".into()).await;
        }
        return refuse(StatusCode::INSUFFICIENT_STORAGE, "full");
    }
    state.full_told.store(false, Ordering::SeqCst);
    let code = new_code();
    let zip = state.cfg.dir.join(format!("{code}.zip"));
    let record =
        serde_json::json!({ "code": code, "received": now, "size": bundle.len(), "entries": entries, "meta": meta });
    let saved =
        std::fs::write(zip.with_extension("json"), record.to_string()).and_then(|_| std::fs::write(&zip, &bundle));
    if saved.is_err() {
        let _ = std::fs::remove_file(zip.with_extension("json"));
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, "storage");
    }
    let link = format!("{}/v1/logs/{code}?k={}", state.cfg.public, state.sign(&code));
    notify(&state, message(&code, &meta, bundle.len(), entries, &link)).await;
    (StatusCode::CREATED, Json(serde_json::json!({ "code": code }))).into_response()
}

/// The Discord message for a bundle. The player's note goes in a code block, so it cannot format or mention.
pub fn message(code: &str, meta: &Meta, size: usize, entries: usize, link: &str) -> String {
    let games: Vec<String> = meta
        .games
        .iter()
        .map(|g| {
            let mut s = g.id.clone();
            if let Some(t) = &g.tag {
                s += &format!(" {t}");
            }
            if let Some(c) = &g.channel {
                s += &format!(" ({c})");
            }
            s
        })
        .collect();
    let mut text = format!(
        "**{code}** · {} · launcher {} · {} files, {} KB\n{}\n<{link}>",
        meta.os,
        meta.launcher,
        entries.saturating_sub(1),
        size.div_ceil(1024),
        games.join(", ")
    );
    let note: String = meta.note.replace("```", "'''").chars().take(1_500).collect();
    if !note.trim().is_empty() {
        text += &format!("\n```\n{note}\n```");
    }
    text
}

async fn notify(state: &AppState, content: String) {
    let Some(url) = &state.cfg.webhook else { return };
    let body = serde_json::json!({ "content": content, "allowed_mentions": { "parse": [] } });
    if let Err(e) = state.http.post(url).json(&body).send().await.and_then(|r| r.error_for_status()) {
        eprintln!("ember-logs: webhook failed: {e}");
    }
}

#[derive(Deserialize)]
struct Key {
    k: String,
}

async fn download(
    State(state): State<Arc<AppState>>,
    UrlPath(code): UrlPath<String>,
    Query(q): Query<Key>,
) -> Response {
    if !is_code(&code) || !state.signed(&code, &q.k) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match std::fs::read(state.cfg.dir.join(format!("{code}.zip"))) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, "application/zip".to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{code}.zip\"")),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests;
