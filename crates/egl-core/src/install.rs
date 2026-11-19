//! Downloading, checking and unpacking a mod into its own folder, and the
//! record of what is installed where.

use crate::releases::{self, Release};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("download failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("not a zip the launcher can read: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("refused to download from {0}")]
    Host(String),
    #[error("the release has no checksum to check the download against")]
    NoChecksum,
    #[error("the download is damaged (checksum {got}, expected {want})")]
    Checksum { got: String, want: String },
    #[error("the checksum file and GitHub disagree about this download")]
    ChecksumConflict,
    #[error("the package has an unsafe path: {0}")]
    UnsafePath(String),
    #[error("the package has no {0}")]
    MissingLauncher(String),
    #[error("cancelled")]
    Cancelled,
}

/// One installed mod.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub channel: String,
    pub tag: String,
    pub dir: PathBuf,
    pub installed_at: u64,
    /// The game folder it was installed for.
    pub game_dir: PathBuf,
    pub appid: u32,
    #[serde(default)]
    pub steam_shortcut: bool,
    /// A copy the launcher took over (the mod's own setup, the zip, a Deck
    /// installer made it). It is updated in place and never deleted.
    #[serde(default)]
    pub adopted: bool,
    /// CRC-32 of the mod's launcher when the launcher last installed or
    /// checked it. Another value means something else changed the copy.
    #[serde(default)]
    pub launcher_crc: Option<u32>,
}

/// Everything the launcher remembers, in <data>/state.json.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(default)]
    pub mods: BTreeMap<String, Installed>,
    /// Game folders the player picked by hand, by game id.
    #[serde(default)]
    pub manual_folders: BTreeMap<String, PathBuf>,
    /// The channel picked per game before anything is installed.
    #[serde(default)]
    pub channels: BTreeMap<String, String>,
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        write_atomic(path, serde_json::to_string_pretty(self).unwrap().as_bytes())
    }
}

/// Writes beside the file, then renames over it, so a crash leaves the old
/// file or the new one, never half of one.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// The launcher's own folder: %LOCALAPPDATA%\EmberGames or ~/.local/share/ember-games.
pub fn data_dir() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(if cfg!(windows) { "EmberGames" } else { "ember-games" })
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("ember-games-launcher/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("http client")
}

pub async fn fetch_text(http: &reqwest::Client, url: &str) -> Result<String, InstallError> {
    if !releases::allowed_url(url) {
        return Err(InstallError::Host(url.into()));
    }
    Ok(http.get(url).send().await?.error_for_status()?.text().await?)
}

/// The hash the download must have. GitHub's digest and a published checksum
/// file must agree when both exist; a release with neither is refused.
pub async fn expected_sha256(http: &reqwest::Client, r: &Release) -> Result<String, InstallError> {
    let from_file = match &r.checksum_url {
        Some(url) => releases::checksum_for(&fetch_text(http, url).await?, &r.asset),
        None => None,
    };
    match (&r.sha256, from_file) {
        (Some(a), Some(b)) if *a != b => Err(InstallError::ChecksumConflict),
        (Some(a), _) => Ok(a.clone()),
        (None, Some(b)) => Ok(b),
        (None, None) => Err(InstallError::NoChecksum),
    }
}

/// Streams the asset to `dest`, hashing as it goes. `progress(done, total)`
/// returns false to cancel.
pub async fn download(
    http: &reqwest::Client,
    r: &Release,
    want: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Result<(), InstallError> {
    if !releases::allowed_url(&r.url) {
        return Err(InstallError::Host(r.url.clone()));
    }
    let mut resp = http.get(&r.url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(r.size);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::File::create(dest)?;
    let mut hash = Sha256::new();
    let mut done = 0u64;
    while let Some(chunk) = resp.chunk().await? {
        hash.update(&chunk);
        file.write_all(&chunk)?;
        done += chunk.len() as u64;
        if !progress(done, total) {
            drop(file);
            let _ = std::fs::remove_file(dest);
            return Err(InstallError::Cancelled);
        }
    }
    file.sync_all()?;
    let got = hex::encode(hash.finalize());
    if got != want {
        let _ = std::fs::remove_file(dest);
        return Err(InstallError::Checksum { got, want: want.into() });
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

/// The entry's path inside the target, or an error for anything that would
/// land outside it (absolute paths, "..", drive letters).
fn safe_entry(name: &str) -> Result<PathBuf, InstallError> {
    let p = Path::new(name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) if !s.to_string_lossy().contains(':') => out.push(s),
            Component::CurDir => {}
            _ => return Err(InstallError::UnsafePath(name.into())),
        }
    }
    Ok(out)
}

/// The one top folder every entry sits in (SFV's and SF4's zips have one),
/// so it can be dropped.
pub(crate) fn common_root(names: &[String]) -> Option<String> {
    let first = names.iter().find(|n| !n.is_empty())?;
    let top = first.split('/').next()?.to_string();
    let all_inside =
        names.iter().all(|n| n.strip_prefix(&top).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')));
    let has_files_inside = names.iter().any(|n| n.len() > top.len() + 1);
    (all_inside && has_files_inside && names.iter().any(|n| n.starts_with(&format!("{top}/")))).then_some(top)
}

/// Unpacks the zip into `dest`, which must not exist yet.
pub fn extract(zip_path: &Path, dest: &Path) -> Result<(), InstallError> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let names: Vec<String> =
        (0..zip.len()).map(|i| zip.by_index_raw(i).map(|f| f.name().replace('\\', "/"))).collect::<Result<_, _>>()?;
    let root = common_root(&names);
    std::fs::create_dir_all(dest)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        let mut name = entry.name().replace('\\', "/");
        if let Some(r) = &root {
            name = name[r.len()..].trim_start_matches('/').to_string();
        }
        if name.is_empty() {
            continue;
        }
        let rel = safe_entry(&name)?;
        let out = dest.join(&rel);
        if entry.is_dir() || name.ends_with('/') {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut f)?;
    }
    Ok(())
}

/// Puts a freshly unpacked folder in place of `dir`. Files the old folder has
/// and the package does not (settings, logs, replays the mod wrote) move
/// over, so an update never loses them.
pub fn swap_in(staged: &Path, dir: &Path) -> Result<(), InstallError> {
    if dir.exists() {
        carry_over(dir, staged)?;
        let old = dir.with_extension("old");
        if old.exists() {
            std::fs::remove_dir_all(&old)?;
        }
        std::fs::rename(dir, &old)?;
        if let Err(e) = std::fs::rename(staged, dir) {
            let _ = std::fs::rename(&old, dir);
            return Err(e.into());
        }
        let _ = std::fs::remove_dir_all(&old);
    } else {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(staged, dir)?;
    }
    Ok(())
}

fn carry_over(from: &Path, to: &Path) -> std::io::Result<()> {
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let target = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            if target.is_dir() {
                carry_over(&e.path(), &target)?;
            } else if !target.exists() {
                copy_dir(&e.path(), &target)?;
            }
        } else if !target.exists() {
            std::fs::copy(e.path(), &target)?;
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()))?;
        } else {
            std::fs::copy(e.path(), to.join(e.file_name()))?;
        }
    }
    Ok(())
}

/// Unpacks a checked download and swaps it in, refusing a package without
/// the mod's launcher.
pub fn install_zip(zip: &Path, dir: &Path, launcher: &str) -> Result<(), InstallError> {
    let staged = dir.with_extension("new");
    if staged.exists() {
        std::fs::remove_dir_all(&staged)?;
    }
    let result = extract(zip, &staged).and_then(|_| {
        if staged.join(launcher).is_file() {
            Ok(())
        } else {
            Err(InstallError::MissingLauncher(launcher.into()))
        }
    });
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(e);
    }
    swap_in(&staged, dir)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_zip(path: &Path, files: &[(&str, &str)]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, body) in files {
            if name.ends_with('/') {
                z.add_directory(name.trim_end_matches('/'), zip::write::SimpleFileOptions::default()).unwrap();
            } else {
                z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
        }
        z.finish().unwrap();
    }

    #[test]
    fn strips_one_top_folder_only() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            common_root(&names(&["sfv-ember-0.1.0/", "sfv-ember-0.1.0/Launcher.exe", "sfv-ember-0.1.0/a/b.png"]))
                .as_deref(),
            Some("sfv-ember-0.1.0")
        );
        assert_eq!(common_root(&names(&["assets/x.png", "launcher.exe"])), None);
        assert_eq!(common_root(&names(&["Launcher.exe"])), None);
        assert_eq!(common_root(&names(&["ab/x", "abc/y"])), None);
    }

    #[test]
    fn refuses_paths_outside_the_folder() {
        assert!(safe_entry("../evil.dll").is_err());
        assert!(safe_entry("/etc/x").is_err());
        assert!(safe_entry("a/../../x").is_err());
        assert!(safe_entry("C:/x").is_err());
        assert_eq!(safe_entry("./a/b.txt").unwrap(), PathBuf::from("a/b.txt"));

        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("bad.zip");
        make_zip(&z, &[("Launcher.exe", "x"), ("../escape.txt", "x")]);
        assert!(matches!(install_zip(&z, &t.path().join("mod"), "Launcher.exe"), Err(InstallError::UnsafePath(_))));
        assert!(!t.path().join("escape.txt").exists());
        assert!(!t.path().join("mod").exists());
        assert!(!t.path().join("mod.new").exists());
    }

    #[test]
    fn install_then_update_keeps_player_files() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("mods/sfv-ember");
        let v1 = t.path().join("v1.zip");
        make_zip(
            &v1,
            &[
                ("sfv-ember-0.1.0/", ""),
                ("sfv-ember-0.1.0/Launcher.exe", "v1"),
                ("sfv-ember-0.1.0/old-only.dll", "gone"),
            ],
        );
        install_zip(&v1, &dir, "Launcher.exe").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("Launcher.exe")).unwrap(), "v1");

        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/settings.ini"), "mine").unwrap();
        std::fs::write(dir.join("settings.json"), "mine too").unwrap();

        let v2 = t.path().join("v2.zip");
        make_zip(&v2, &[("Launcher.exe", "v2"), ("settings.json", "package default")]);
        install_zip(&v2, &dir, "Launcher.exe").unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("Launcher.exe")).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(dir.join("data/settings.ini")).unwrap(), "mine");
        // A file the package ships is the package's.
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), "package default");
        assert!(!dir.with_extension("old").exists());
    }

    #[test]
    fn refuses_a_package_without_its_launcher_and_keeps_the_old_one() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("m");
        let good = t.path().join("g.zip");
        make_zip(&good, &[("run.exe", "1")]);
        install_zip(&good, &dir, "run.exe").unwrap();
        let bad = t.path().join("b.zip");
        make_zip(&bad, &[("other.exe", "2")]);
        assert!(matches!(install_zip(&bad, &dir, "run.exe"), Err(InstallError::MissingLauncher(_))));
        assert_eq!(std::fs::read_to_string(dir.join("run.exe")).unwrap(), "1");
    }

    #[test]
    fn state_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("state.json");
        assert_eq!(State::load(&p), State::default());
        let mut s = State::default();
        s.channels.insert("usf4".into(), "nightly".into());
        s.save(&p).unwrap();
        assert_eq!(State::load(&p), s);
    }

    #[tokio::test]
    async fn checksum_rules_without_network() {
        let http = client();
        let mut r = Release {
            tag: "v1".into(),
            title: String::new(),
            published: String::new(),
            notes: String::new(),
            page: String::new(),
            asset: "a.zip".into(),
            url: "https://github.com/x/a.zip".into(),
            size: 1,
            sha256: None,
            checksum_url: None,
        };
        assert!(matches!(expected_sha256(&http, &r).await, Err(InstallError::NoChecksum)));
        r.sha256 = Some("ab".repeat(32));
        assert_eq!(expected_sha256(&http, &r).await.unwrap(), "ab".repeat(32));
        r.checksum_url = Some("https://evil.example/sums".into());
        assert!(matches!(expected_sha256(&http, &r).await, Err(InstallError::Host(_))));
    }
}
