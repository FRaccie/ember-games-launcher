//! Log bundles: the mods' logs zipped per game or for all games at once.
//! A ledger of what was already sent means the next bundle holds only what
//! is new: files that changed, and for a log that only grew, just the part
//! written since. Bundles wait in logs/outbox until sent, then move to
//! logs/sent, so nothing is lost and nothing is sent twice.

use crate::catalog::{Game, LogSource};
use crate::install::{write_atomic, InstallError};
use crate::steam::{self, GameInstall};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Per log file: how far it was sent, and a hash of its first bytes so a log
/// that was replaced (not grown) is sent whole again.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sent {
    pub offset: u64,
    pub head: String,
    /// How many first bytes `head` covers.
    #[serde(default)]
    pub head_len: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Ledger {
    /// Keyed by "<game id>/<file name>".
    #[serde(default)]
    pub files: BTreeMap<String, Sent>,
}

/// One file's part in a bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub game: String,
    pub name: String,
    /// The byte the part starts at in the original file (0 = whole file).
    pub from: u64,
    pub to: u64,
    pub head: String,
    pub head_len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub launcher_version: String,
    pub platform: String,
    pub created: u64,
    pub games: Vec<BundleGame>,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BundleGame {
    pub id: String,
    pub mod_tag: Option<String>,
    pub channel: Option<String>,
    pub appid: Option<u32>,
}

/// A bundle waiting in the outbox or already sent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bundle {
    pub path: PathBuf,
    pub file: String,
    pub size: u64,
    pub games: Vec<String>,
    pub parts: usize,
    pub sent: bool,
    /// The report code the server gave it, for a sent bundle.
    pub code: Option<String>,
}

const HEAD: usize = 4096;
/// Bigger new parts keep only their last bytes (the newest lines matter most).
pub const MAX_PART: u64 = 8 << 20;

pub fn logs_dir(data: &Path) -> PathBuf {
    data.join("logs")
}

/// The Windows folders a log source names, for the player on this PC.
/// On Linux they are inside the game's Proton prefix.
pub fn source_dir(src: &LogSource, mod_dir: Option<&Path>, game: Option<&GameInstall>) -> Option<PathBuf> {
    let (token, rest) = ["{appdata}", "{localappdata}", "{mod}"]
        .iter()
        .find_map(|t| src.dir.strip_prefix(t).map(|r| (*t, r.trim_start_matches('/'))))?;
    let base = match token {
        "{mod}" => mod_dir?.to_path_buf(),
        _ if cfg!(windows) => {
            if token == "{appdata}" {
                dirs::config_dir()?
            } else {
                dirs::data_local_dir()?
            }
        }
        _ => {
            let users = steam::proton_prefix(game?).join("drive_c/users/steamuser/AppData");
            users.join(if token == "{appdata}" { "Roaming" } else { "Local" })
        }
    };
    Some(if rest.is_empty() { base } else { base.join(rest) })
}

/// The log files of a game that exist now.
pub fn find_logs(game: &Game, mod_dir: Option<&Path>, install: Option<&GameInstall>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for src in &game.module.logs {
        let Some(dir) = source_dir(src, mod_dir, install) else { continue };
        let pats: Vec<glob::Pattern> = src.files.iter().filter_map(|f| glob::Pattern::new(f).ok()).collect();
        let opts = glob::MatchOptions { case_sensitive: false, ..Default::default() };
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_file() && pats.iter().any(|p| p.matches_with(&name, opts)) {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The hash of a file's first `len` bytes (fewer when it is shorter).
fn head_hash(path: &Path, len: u64) -> std::io::Result<String> {
    let mut buf = Vec::new();
    std::fs::File::open(path)?.take(len).read_to_end(&mut buf)?;
    Ok(hex::encode(Sha256::digest(&buf)))
}

/// The part of a file that is new since the ledger, or None when nothing is.
pub fn new_part(game: &str, path: &Path, ledger: &Ledger) -> std::io::Result<Option<Part>> {
    let size = std::fs::metadata(path)?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let head_len = size.min(HEAD as u64);
    let head = head_hash(path, head_len)?;
    let from = match ledger.files.get(&format!("{game}/{name}")) {
        // Grew from the same start: only the new tail.
        Some(s) if s.offset <= size && head_hash(path, s.head_len)? == s.head => s.offset,
        // Replaced or rotated, or never sent: whole file.
        _ => 0,
    };
    if from == size {
        return Ok(None);
    }
    let from = from.max(size.saturating_sub(MAX_PART));
    Ok(Some(Part { game: game.into(), name, from, to: size, head, head_len }))
}

/// Replaces the player's account name in text, so paths in logs do not
/// carry it.
pub fn redact(text: &str) -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
    if user.len() < 2 {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let needle = user.to_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        // Only whole path parts: \name\ or /name/ (or at the end).
        let before = lower[..i].chars().last();
        let after = lower[i + needle.len()..].chars().next();
        let sep = |c: Option<char>| matches!(c, Some('\\' | '/') | None);
        if matches!(before, Some('\\' | '/')) && sep(after) && i >= last {
            out.push_str(&text[last..i]);
            out.push_str("<user>");
            last = i + needle.len();
        }
    }
    out.push_str(&text[last..]);
    out
}

fn read_range(path: &Path, from: u64, to: u64) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity((to - from) as usize);
    f.take(to - from).read_to_end(&mut buf)?;
    Ok(buf)
}

/// A log never sent before goes in only when it changed this recently.
pub const NEW_FILE_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
/// At most this many files and bytes per game in one bundle, newest first.
pub const MAX_FILES: usize = 30;
pub const MAX_GAME_BYTES: u64 = 32 << 20;

/// The parts of one game's logs that go in the next bundle.
pub fn pick_parts(
    game: &str,
    files: &[PathBuf],
    ledger: &Ledger,
    now: std::time::SystemTime,
) -> std::io::Result<Vec<(Part, PathBuf)>> {
    let mut by_age: Vec<(std::time::SystemTime, &PathBuf)> =
        files.iter().filter_map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok().map(|t| (t, f))).collect();
    by_age.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = Vec::new();
    let mut bytes = 0;
    for (modified, f) in by_age {
        if out.len() >= MAX_FILES || bytes >= MAX_GAME_BYTES {
            break;
        }
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let known = ledger.files.contains_key(&format!("{game}/{name}"));
        let recent = now.duration_since(modified).map(|d| d <= NEW_FILE_AGE).unwrap_or(true);
        if !known && !recent {
            continue;
        }
        if let Some(p) = new_part(game, f, ledger)? {
            bytes += p.to - p.from;
            out.push((p, f.clone()));
        }
    }
    Ok(out)
}

pub struct GameLogs<'a> {
    pub game: &'a Game,
    pub info: BundleGame,
    pub files: Vec<PathBuf>,
}

/// Zips what is new for these games into logs/outbox. Returns None when no
/// game has anything new. Older outbox bundles for the same games are
/// replaced, since the new one holds everything they did.
pub fn make_bundle(data: &Path, games: &[GameLogs]) -> Result<Option<Bundle>, InstallError> {
    let dir = logs_dir(data);
    let ledger = load_ledger(data);
    let mut parts = Vec::new();
    for g in games {
        parts.extend(pick_parts(&g.game.id, &g.files, &ledger, std::time::SystemTime::now())?);
    }
    if parts.is_empty() {
        return Ok(None);
    }
    let ids: Vec<String> =
        games.iter().filter(|g| parts.iter().any(|(p, _)| p.game == g.game.id)).map(|g| g.game.id.clone()).collect();
    let created = crate::install::now();
    let manifest = Manifest {
        launcher_version: env!("CARGO_PKG_VERSION").into(),
        platform: std::env::consts::OS.into(),
        created,
        games: games.iter().filter(|g| ids.contains(&g.game.id)).map(|g| g.info.clone()).collect(),
        parts: parts.iter().map(|(p, _)| p.clone()).collect(),
    };
    for old in outbox(data) {
        if old.games.iter().all(|g| ids.contains(g)) {
            let _ = std::fs::remove_file(&old.path);
        }
    }
    let name = format!(
        "ember-logs-{}-{created}.zip",
        if ids.len() == games.len() && ids.len() > 1 { "all".to_string() } else { ids.join("-") }
    );
    let path = dir.join("outbox").join(&name);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("zip.tmp");
    {
        let mut z = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        z.start_file("manifest.json", opt)?;
        z.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes())?;
        for (p, file) in &parts {
            let bytes = read_range(file, p.from, p.to)?;
            let text = redact(&String::from_utf8_lossy(&bytes));
            z.start_file(format!("{}/{}", p.game, p.name), opt)?;
            if p.from > 0 {
                writeln!(z, "[Ember Games Launcher: continues from byte {} of {}]", p.from, p.name)?;
            }
            z.write_all(text.as_bytes())?;
        }
        z.finish()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(Some(read_bundle(&path, false)?))
}

fn read_manifest(path: &Path) -> Result<Manifest, InstallError> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut text = String::new();
    z.by_name("manifest.json")?.read_to_string(&mut text)?;
    serde_json::from_str(&text).map_err(|e| InstallError::Io(std::io::Error::other(e)))
}

fn read_bundle(path: &Path, sent: bool) -> Result<Bundle, InstallError> {
    let m = read_manifest(path)?;
    Ok(Bundle {
        path: path.to_path_buf(),
        file: path.file_name().unwrap().to_string_lossy().into_owned(),
        size: std::fs::metadata(path)?.len(),
        games: m.games.iter().map(|g| g.id.clone()).collect(),
        parts: m.parts.len(),
        sent,
        code: std::fs::read_to_string(path.with_extension("code")).ok().map(|c| c.trim().to_string()),
    })
}

fn list(dir: &Path, sent: bool) -> Vec<Bundle> {
    let mut v: Vec<Bundle> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "zip"))
        .filter_map(|p| read_bundle(&p, sent).ok())
        .collect();
    v.sort_by(|a, b| b.file.cmp(&a.file));
    v
}

pub fn outbox(data: &Path) -> Vec<Bundle> {
    list(&logs_dir(data).join("outbox"), false)
}

pub fn sent(data: &Path) -> Vec<Bundle> {
    list(&logs_dir(data).join("sent"), true)
}

fn ledger_path(data: &Path) -> PathBuf {
    logs_dir(data).join("sent.json")
}

pub fn load_ledger(data: &Path) -> Ledger {
    std::fs::read_to_string(ledger_path(data)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// After a successful upload: the ledger moves forward to what the bundle
/// held, and the bundle moves to logs/sent.
pub fn mark_sent(data: &Path, bundle: &Path) -> Result<PathBuf, InstallError> {
    let m = read_manifest(bundle)?;
    let mut ledger = load_ledger(data);
    for p in &m.parts {
        ledger.files.insert(
            format!("{}/{}", p.game, p.name),
            Sent { offset: p.to, head: p.head.clone(), head_len: p.head_len },
        );
    }
    write_atomic(&ledger_path(data), serde_json::to_string_pretty(&ledger).unwrap().as_bytes())?;
    let to = logs_dir(data).join("sent").join(bundle.file_name().unwrap());
    std::fs::create_dir_all(to.parent().unwrap())?;
    std::fs::rename(bundle, &to)?;
    Ok(to)
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("no upload server is set up yet; the bundle is saved in the outbox")]
    NotConfigured,
    #[error("{0}")]
    Bundle(#[from] InstallError),
    #[error("upload failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{}", refused_text(*.0))]
    Refused(u16),
}

fn refused_text(status: u16) -> String {
    match status {
        413 => "the bundle is too big to send; send one game at a time".into(),
        429 => "too many bundles sent from this connection; try again in an hour".into(),
        507 => "the log server is full right now; try again later".into(),
        _ => format!("the log server refused the bundle ({status})"),
    }
}

/// What the log service is told about a bundle (docs/LOG_UPLOADS.md).
pub fn upload_meta(m: &Manifest, bytes: &[u8], note: &str) -> serde_json::Value {
    let deck = std::env::var("SteamDeck").is_ok_and(|v| v == "1");
    let os = match m.platform.as_str() {
        "windows" => "windows",
        _ if deck => "steamdeck",
        _ => "linux",
    };
    let games: Vec<_> =
        m.games.iter().map(|g| serde_json::json!({ "id": g.id, "channel": g.channel, "tag": g.mod_tag })).collect();
    let note: String = note.trim().chars().filter(|c| !c.is_control() || *c == '\n').take(1_000).collect();
    serde_json::json!({
        "schema": 1, "launcher": m.launcher_version, "os": os, "games": games, "note": note,
        "bundle_sha256": hex::encode(sha2::Sha256::digest(bytes)),
    })
}

/// Sends a bundle as multipart (a `meta` JSON part and the `bundle` zip), marks it sent and keeps the report
/// code the server answers beside it. Returns that code.
pub async fn upload(
    http: &reqwest::Client,
    url: Option<&str>,
    data: &Path,
    bundle: &Path,
    note: &str,
) -> Result<String, UploadError> {
    // https only; plain http only to this machine (the contract test against the real service).
    let url = url
        .filter(|u| u.starts_with("https://") || u.starts_with("http://127.0.0.1:"))
        .ok_or(UploadError::NotConfigured)?;
    let m = read_manifest(bundle)?;
    let bytes = std::fs::read(bundle).map_err(InstallError::from)?;
    let meta = upload_meta(&m, &bytes, note);
    let form = reqwest::multipart::Form::new()
        .part("meta", reqwest::multipart::Part::text(meta.to_string()).mime_str("application/json")?)
        .part("bundle", reqwest::multipart::Part::bytes(bytes).file_name("logs.zip").mime_str("application/zip")?);
    let resp = http.post(url).multipart(form).send().await?;
    if !resp.status().is_success() {
        return Err(UploadError::Refused(resp.status().as_u16()));
    }
    let answer: serde_json::Value = resp.json().await?;
    let code = answer["code"].as_str().unwrap_or_default().to_string();
    let sent = mark_sent(data, bundle)?;
    if !code.is_empty() {
        write_atomic(&sent.with_extension("code"), code.as_bytes()).map_err(InstallError::from)?;
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    fn info(id: &str) -> BundleGame {
        BundleGame { id: id.into(), mod_tag: Some("v1".into()), channel: None, appid: None }
    }

    fn names(bundle: &Path) -> BTreeMap<String, String> {
        let mut z = zip::ZipArchive::new(std::fs::File::open(bundle).unwrap()).unwrap();
        (0..z.len())
            .map(|i| {
                let mut f = z.by_index(i).unwrap();
                let mut s = String::new();
                f.read_to_string(&mut s).unwrap();
                (f.name().to_string(), s)
            })
            .collect()
    }

    #[test]
    fn second_bundle_holds_only_what_is_new() {
        let t = tempfile::tempdir().unwrap();
        let data = t.path().join("data");
        let logs = t.path().join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let a = logs.join("sf4e.log");
        let b = logs.join("launcher.log");
        std::fs::write(&a, "line 1\n").unwrap();
        std::fs::write(&b, "boot\n").unwrap();
        let cat = Catalog::bundled();
        let g = cat.game("usf4").unwrap();
        let set = |files: Vec<PathBuf>| vec![GameLogs { game: g, info: info("usf4"), files }];

        let first = make_bundle(&data, &set(vec![a.clone(), b.clone()])).unwrap().unwrap();
        assert_eq!(first.parts, 2);
        assert_eq!(outbox(&data).len(), 1);
        // Not sent yet: a new bundle still holds everything and replaces it.
        let again = make_bundle(&data, &set(vec![a.clone(), b.clone()])).unwrap().unwrap();
        assert_eq!(again.parts, 2);
        let sent_to = mark_sent(&data, &again.path).unwrap();
        assert!(sent_to.exists());
        assert!(outbox(&data).is_empty());
        assert_eq!(sent(&data).len(), 1);

        // Nothing new: no bundle.
        assert!(make_bundle(&data, &set(vec![a.clone(), b.clone()])).unwrap().is_none());

        // a grows, b is replaced by a new run's log.
        std::fs::write(&a, "line 1\nline 2\n").unwrap();
        std::fs::write(&b, "second boot\n").unwrap();
        let next = make_bundle(&data, &set(vec![a, b])).unwrap().unwrap();
        let files = names(&next.path);
        assert!(files["usf4/sf4e.log"].contains("continues from byte 7"));
        assert!(files["usf4/sf4e.log"].ends_with("line 2\n"));
        assert!(!files["usf4/sf4e.log"].contains("line 1"));
        assert_eq!(files["usf4/launcher.log"], "second boot\n");
        assert!(files.contains_key("manifest.json"));
    }

    #[test]
    fn redacts_the_account_name_in_paths_only() {
        let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default();
        if user.len() < 2 {
            return;
        }
        let text = format!("C:\\Users\\{user}\\AppData x /home/{user}/a {user}s");
        let out = redact(&text);
        assert!(out.contains("C:\\Users\\<user>\\AppData"));
        assert!(out.contains("/home/<user>/a"));
        assert!(out.ends_with(&format!("{user}s")));
    }

    #[cfg(unix)]
    #[test]
    fn linux_logs_live_in_the_proton_prefix() {
        let g = GameInstall {
            appid: 357190,
            library: PathBuf::from("/lib"),
            dir: PathBuf::new(),
            exe: PathBuf::new(),
            manual: false,
        };
        let src = LogSource { dir: "{appdata}/umvc3e".into(), files: vec!["*.log".into()] };
        assert_eq!(
            source_dir(&src, None, Some(&g)).unwrap(),
            PathBuf::from("/lib/steamapps/compatdata/357190/pfx/drive_c/users/steamuser/AppData/Roaming/umvc3e")
        );
        let m = LogSource { dir: "{mod}/data".into(), files: vec!["*.log".into()] };
        assert_eq!(source_dir(&m, Some(Path::new("/m")), None).unwrap(), PathBuf::from("/m/data"));
    }

    #[test]
    fn old_unsent_files_stay_out_and_counts_are_capped() {
        let t = tempfile::tempdir().unwrap();
        let mut files = Vec::new();
        for i in 0..(MAX_FILES + 5) {
            let f = t.path().join(format!("session-{i}.log"));
            std::fs::write(&f, "x").unwrap();
            files.push(f);
        }
        let now = std::time::SystemTime::now();
        assert_eq!(pick_parts("usf4", &files, &Ledger::default(), now).unwrap().len(), MAX_FILES);
        // Seen from 30 days later, none of them is recent and none was sent: nothing.
        let later = now + std::time::Duration::from_secs(30 * 24 * 3600);
        assert!(pick_parts("usf4", &files, &Ledger::default(), later).unwrap().is_empty());
        // A file sent before still goes in when it grew, however old it is.
        let mut ledger = Ledger::default();
        ledger.files.insert("usf4/session-0.log".into(), Sent { offset: 0, head: String::new(), head_len: 0 });
        std::fs::write(&files[0], "xy").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30 * 24 * 3600);
        assert_eq!(pick_parts("usf4", &files, &ledger, later).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_log_service_takes_a_real_bundle_and_its_code_is_kept() {
        let t = tempfile::tempdir().unwrap();
        let store = t.path().join("server");
        std::fs::create_dir_all(&store).unwrap();
        let state = std::sync::Arc::new(ember_logs::AppState::new(ember_logs::Config {
            dir: store.clone(),
            key: vec![3; 32],
            public: "https://logs.example".into(),
            ..ember_logs::Config::default()
        }));
        let app = ember_logs::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/logs", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let f = t.path().join("sf4e.log");
        std::fs::write(&f, "hello\n").unwrap();
        let cat = Catalog::bundled();
        let b =
            make_bundle(t.path(), &[GameLogs { game: cat.game("usf4").unwrap(), info: info("usf4"), files: vec![f] }])
                .unwrap()
                .unwrap();
        let code =
            upload(&reqwest::Client::new(), Some(&url), t.path(), &b.path, "froze on stage select").await.unwrap();
        assert!(code.starts_with("L-"), "{code}");
        assert!(store.join(format!("{code}.zip")).is_file());
        let record = std::fs::read_to_string(store.join(format!("{code}.json"))).unwrap();
        assert!(record.contains("froze on stage select"));
        let sent = list(&logs_dir(t.path()).join("sent"), true);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].code.as_deref(), Some(code.as_str()));
    }

    #[tokio::test]
    async fn upload_without_a_server_keeps_the_bundle() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("x.log");
        std::fs::write(&f, "x").unwrap();
        let cat = Catalog::bundled();
        let b =
            make_bundle(t.path(), &[GameLogs { game: cat.game("sfv").unwrap(), info: info("sfv"), files: vec![f] }])
                .unwrap()
                .unwrap();
        let r = upload(&crate::install::client(), None, t.path(), &b.path, "").await;
        assert!(matches!(r, Err(UploadError::NotConfigured)));
        assert!(b.path.exists());
    }
}
