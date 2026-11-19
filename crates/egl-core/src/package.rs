//! A release package's file list (path, size, CRC-32), read from the zip's
//! central directory, and checking an installed folder against it. A release
//! on GitHub is read with two small range requests, not downloaded, so an
//! installed copy can be matched to its release and checked cheaply.

use crate::install::{self, InstallError};
use crate::releases::{self, Release};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageFile {
    /// '/' separated, relative to the mod folder (the zip's one top folder dropped).
    pub path: String,
    pub size: u64,
    pub crc32: u32,
}

const EOCD: u32 = 0x0605_4b50;
const CDH: u32 = 0x0201_4b50;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// (offset, size) of the central directory, from the end of the zip. `tail`
/// is the zip's last bytes.
pub fn central_directory_at(tail: &[u8]) -> Result<(u64, u64), InstallError> {
    let bad = || InstallError::Zip(zip::result::ZipError::InvalidArchive("no end of central directory".into()));
    let min = 22;
    if tail.len() < min {
        return Err(bad());
    }
    let at = (0..=tail.len() - min).rev().find(|&i| u32_at(tail, i) == Some(EOCD)).ok_or_else(bad)?;
    let size = u32_at(tail, at + 12).ok_or_else(bad)?;
    let offset = u32_at(tail, at + 16).ok_or_else(bad)?;
    if size == u32::MAX || offset == u32::MAX {
        // Zip64: no Ember package is near 4 GB.
        return Err(InstallError::Zip(zip::result::ZipError::UnsupportedArchive("zip64")));
    }
    Ok((offset as u64, size as u64))
}

/// The file entries of a central directory (folders left out).
pub fn parse_central_directory(cd: &[u8]) -> Result<Vec<PackageFile>, InstallError> {
    let bad = || InstallError::Zip(zip::result::ZipError::InvalidArchive("bad central directory".into()));
    let mut out = Vec::new();
    let mut i = 0;
    while i + 46 <= cd.len() && u32_at(cd, i) == Some(CDH) {
        let crc32 = u32_at(cd, i + 16).ok_or_else(bad)?;
        let size = u32_at(cd, i + 24).ok_or_else(bad)? as u64;
        let name_len = u16_at(cd, i + 28).ok_or_else(bad)? as usize;
        let extra = u16_at(cd, i + 30).ok_or_else(bad)? as usize;
        let comment = u16_at(cd, i + 32).ok_or_else(bad)? as usize;
        let name = cd.get(i + 46..i + 46 + name_len).ok_or_else(bad)?;
        let name = String::from_utf8_lossy(name).replace('\\', "/");
        if !name.ends_with('/') {
            out.push(PackageFile { path: name, size, crc32 });
        }
        i += 46 + name_len + extra + comment;
    }
    Ok(strip_root(out))
}

/// Drops the one top folder every entry sits in, as unpacking does.
fn strip_root(files: Vec<PackageFile>) -> Vec<PackageFile> {
    let names: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    match install::common_root(&names) {
        Some(root) => files
            .into_iter()
            .map(|f| PackageFile { path: f.path[root.len()..].trim_start_matches('/').to_string(), ..f })
            .collect(),
        None => files,
    }
}

/// The file list of a zip on disk.
pub fn files_of_zip(path: &Path) -> Result<Vec<PackageFile>, InstallError> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?)?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i)?;
        if !f.is_dir() {
            out.push(PackageFile { path: f.name().replace('\\', "/"), size: f.size(), crc32: f.crc32() });
        }
    }
    Ok(strip_root(out))
}

async fn range(http: &reqwest::Client, url: &str, from: u64, to: u64) -> Result<Vec<u8>, InstallError> {
    let resp = http.get(url).header("Range", format!("bytes={from}-{to}")).send().await?.error_for_status()?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(InstallError::Host(format!("{url} (no range support)")));
    }
    Ok(resp.bytes().await?.to_vec())
}

/// A release's file list, read from GitHub with range requests and kept in
/// `cache` (a release asset never changes; a replaced one gets a new size).
pub async fn release_files(
    http: &reqwest::Client,
    r: &Release,
    cache: &Path,
) -> Result<Vec<PackageFile>, InstallError> {
    let file = cache.join(format!("{}-{}.json", r.asset, r.size));
    if let Some(files) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        return Ok(files);
    }
    if !releases::allowed_url(&r.url) {
        return Err(InstallError::Host(r.url.clone()));
    }
    let files = fetch_files(http, &r.url, r.size).await?;
    if !files.is_empty() {
        let _ = install::write_atomic(&file, &serde_json::to_vec(&files).unwrap());
    }
    Ok(files)
}

/// Reads the zip's last 64 KB, then its central directory when that is not
/// already inside them.
async fn fetch_files(http: &reqwest::Client, url: &str, size: u64) -> Result<Vec<PackageFile>, InstallError> {
    if size < 22 {
        return Err(InstallError::Zip(zip::result::ZipError::InvalidArchive("too small".into())));
    }
    let tail_start = size - size.min(64 * 1024);
    let tail = range(http, url, tail_start, size - 1).await?;
    let (offset, len) = central_directory_at(&tail)?;
    if offset + len > size {
        return Err(InstallError::Zip(zip::result::ZipError::InvalidArchive("directory past the end".into())));
    }
    let cd = if offset >= tail_start {
        tail[(offset - tail_start) as usize..(offset - tail_start + len) as usize].to_vec()
    } else {
        range(http, url, offset, offset + len - 1).await?
    };
    parse_central_directory(&cd)
}

/// CRC-32 of a file, as a zip records it.
pub fn crc_of(path: &Path) -> std::io::Result<u32> {
    let mut f = std::fs::File::open(path)?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(h.finalize());
        }
        h.update(&buf[..n]);
    }
}

/// Whether the file in `dir` is the package's copy.
pub fn matches(dir: &Path, f: &PackageFile) -> bool {
    let p = dir.join(&f.path);
    std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() == f.size) && crc_of(&p).is_ok_and(|c| c == f.crc32)
}

/// How an installed folder compares with its package.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub files: usize,
    pub missing: Vec<String>,
    pub changed: Vec<String>,
}

impl Check {
    pub fn ok(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty()
    }
}

/// Compares every package file with the folder. Files the package does not
/// have (settings, logs, an uninstaller) are not looked at.
pub fn check(dir: &Path, files: &[PackageFile]) -> Check {
    let mut out = Check { files: files.len(), ..Check::default() };
    for f in files {
        let p = dir.join(&f.path);
        if !p.is_file() {
            out.missing.push(f.path.clone());
        } else if !matches(dir, f) {
            out.changed.push(f.path.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
    fn central_directory_from_the_tail_matches_the_zip_reader() {
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("m.zip");
        make_zip(&z, &[("m-1.0/", ""), ("m-1.0/Launcher.exe", "launcher"), ("m-1.0/a/b.dll", "bee")]);
        let bytes = std::fs::read(&z).unwrap();
        let tail = &bytes[bytes.len().saturating_sub(40)..];
        // 40 bytes hold the end record but not the directory: the offset says where it is.
        let (off, size) = central_directory_at(tail).unwrap();
        let from_tail = parse_central_directory(&bytes[off as usize..(off + size) as usize]).unwrap();
        assert_eq!(from_tail, files_of_zip(&z).unwrap());
        let paths: Vec<_> = from_tail.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["Launcher.exe", "a/b.dll"]);
        assert_eq!(from_tail[0].crc32, crc32fast::hash(b"launcher"));
        assert!(central_directory_at(b"not a zip at all, just text").is_err());
    }

    #[test]
    fn check_finds_missing_and_changed_files_and_ignores_player_files() {
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("m.zip");
        make_zip(&z, &[("Launcher.exe", "L"), ("x.dll", "X"), ("data/y.pak", "Y")]);
        let dir = t.path().join("mod");
        install::install_zip(&z, &dir, "Launcher.exe").unwrap();
        std::fs::write(dir.join("settings.json"), "mine").unwrap();
        let files = files_of_zip(&z).unwrap();
        assert!(check(&dir, &files).ok());
        std::fs::write(dir.join("x.dll"), "Z").unwrap(); // same size, other bytes
        std::fs::remove_file(dir.join("data/y.pak")).unwrap();
        let c = check(&dir, &files);
        assert_eq!((c.files, c.missing, c.changed), (3, vec!["data/y.pak".to_string()], vec!["x.dll".to_string()]));
    }

    #[tokio::test]
    async fn release_files_reads_github_with_two_ranges() {
        use axum::{http::HeaderMap, http::StatusCode, routing::get, Router};
        let t = tempfile::tempdir().unwrap();
        let z = t.path().join("m.zip");
        // 300 entries make a directory bigger than the 64 KB tail, so both ranges are used.
        let names: Vec<String> = (0..300).map(|i| format!("m/{}/{i}.dat", "d".repeat(200))).collect();
        let mut entries: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "x")).collect();
        entries.push(("m/Launcher.exe", "L"));
        make_zip(&z, &entries);
        let bytes = std::fs::read(&z).unwrap();
        let served = bytes.clone();
        let app = Router::new().route(
            "/m.zip",
            get(move |h: HeaderMap| {
                let b = served.clone();
                async move {
                    let r = h.get("range").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let (a, z) = r.trim_start_matches("bytes=").split_once('-').unwrap();
                    let (a, z): (usize, usize) = (a.parse().unwrap(), z.parse().unwrap());
                    (StatusCode::PARTIAL_CONTENT, b[a..=z].to_vec())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut r = Release {
            tag: "v1".into(),
            title: String::new(),
            published: String::new(),
            notes: String::new(),
            page: String::new(),
            asset: "m.zip".into(),
            url: format!("http://{addr}/m.zip"),
            size: bytes.len() as u64,
            sha256: None,
            checksum_url: None,
        };
        let http = install::client();
        // Only GitHub's hosts are read.
        assert!(matches!(release_files(&http, &r, t.path()).await, Err(InstallError::Host(_))));
        let files = fetch_files(&http, &r.url, r.size).await.unwrap();
        assert_eq!(files, files_of_zip(&z).unwrap());
        assert_eq!(files.len(), 301);
        // A cached list is used without asking again.
        r.url = "https://github.com/o/r/releases/download/v1/m.zip".into();
        install::write_atomic(&t.path().join(format!("m.zip-{}.json", r.size)), &serde_json::to_vec(&files).unwrap())
            .unwrap();
        assert_eq!(release_files(&http, &r, t.path()).await.unwrap(), files);
    }
}
