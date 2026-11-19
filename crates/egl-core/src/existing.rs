//! Copies of a mod installed without the launcher: by the mod's own setup, by
//! hand from the zip (and added to Steam), or by a Steam Deck installer. The
//! launcher can take one over: it then updates and checks that folder in
//! place instead of installing a second copy.

use crate::catalog::{Channel, Game};
use crate::install::InstallError;
use crate::package::{self, PackageFile};
use crate::releases::{self, GhRelease, Release};
use crate::steam::{self, GameInstall};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// The mod's own setup (Windows uninstall entry).
    Setup,
    /// A Steam entry that starts the mod's launcher.
    SteamEntry,
    /// The handler of the mod's links (ember:).
    LinkHandler,
    /// One of the folders the mod's installers use.
    UsualFolder,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub dir: PathBuf,
    pub source: Source,
    /// The version the package's own files name (BUILD_INFO.txt), if any.
    pub version: Option<String>,
    /// The version the mod's setup wrote in its uninstall entry.
    pub setup_version: Option<String>,
}

/// Whether `dir` holds this mod: its launcher and, when the catalog names
/// one, its marker file (USF4's and SFV's launchers are both Launcher.exe).
pub fn is_mod_folder(game: &Game, dir: &Path) -> bool {
    dir.join(&game.module.launcher).is_file()
        && game.module.existing.marker.as_deref().is_none_or(|m| dir.join(m).is_file())
}

/// The release the package's BUILD_INFO.txt names ("Release: 1.6.1").
pub fn version_from_files(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("BUILD_INFO.txt")).ok()?;
    text.trim_start_matches('\u{feff}').lines().find_map(|l| {
        let v = l.trim().strip_prefix("Release:")?.trim();
        (!v.is_empty() && v.len() < 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b)))
            .then(|| v.to_string())
    })
}

/// A catalog folder with its {localappdata} or {prefix} filled in, when that
/// place exists on this system.
fn expand(template: &str, install: Option<&GameInstall>) -> Option<PathBuf> {
    if let Some(rest) = template.strip_prefix("{localappdata}/") {
        return cfg!(windows).then(|| dirs::data_local_dir().map(|b| b.join(rest))).flatten();
    }
    if let Some(rest) = template.strip_prefix("{prefix}/") {
        return (!cfg!(windows)).then(|| install.map(|i| steam::proton_prefix(i).join(rest))).flatten();
    }
    None
}

/// The exe of a quoted or bare command line ("C:\x\a.exe" --flag).
pub fn exe_of_command(cmd: &str) -> Option<PathBuf> {
    let cmd = cmd.trim();
    let exe = match cmd.strip_prefix('"') {
        Some(rest) => &rest[..rest.find('"')?],
        None => cmd.split_whitespace().next()?,
    };
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

#[cfg(windows)]
fn from_registry(game: &Game) -> Vec<(PathBuf, Source, Option<String>)> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY};
    use winreg::RegKey;
    let mut out = Vec::new();
    let e = &game.module.existing;
    let roots = [
        (RegKey::predef(HKEY_CURRENT_USER), KEY_READ),
        (RegKey::predef(HKEY_LOCAL_MACHINE), KEY_READ | KEY_WOW64_64KEY),
        (RegKey::predef(HKEY_LOCAL_MACHINE), KEY_READ | KEY_WOW64_32KEY),
    ];
    for key in &e.uninstall_keys {
        for (root, flags) in &roots {
            let path = format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{key}");
            let Ok(k) = root.open_subkey_with_flags(&path, *flags) else { continue };
            if let Ok(dir) = k.get_value::<String, _>("InstallLocation") {
                let dir = dir.trim().trim_matches('"').trim_end_matches(['\\', '/']).to_string();
                if !dir.is_empty() {
                    out.push((PathBuf::from(dir), Source::Setup, k.get_value::<String, _>("DisplayVersion").ok()));
                }
            }
        }
    }
    if let Some(scheme) = &e.link_scheme {
        let path = format!(r"Software\Classes\{scheme}\shell\open\command");
        for (root, flags) in &roots[..2] {
            if let Ok(k) = root.open_subkey_with_flags(&path, *flags) {
                if let Some(exe) = k.get_value::<String, _>("").ok().as_deref().and_then(exe_of_command) {
                    if let Some(dir) = exe.parent() {
                        out.push((dir.to_path_buf(), Source::LinkHandler, None));
                    }
                }
            }
        }
    }
    out
}

#[cfg(not(windows))]
fn from_registry(_: &Game) -> Vec<(PathBuf, Source, Option<String>)> {
    Vec::new()
}

/// Every copy of the mod on this PC, best first, without `skip` (the folder
/// the launcher already manages). Folders that do not hold the mod are left out.
pub fn find(game: &Game, install: Option<&GameInstall>, steam_root: Option<&Path>, skip: Option<&Path>) -> Vec<Found> {
    let mut raw = from_registry(game);
    if let Some(root) = steam_root {
        for exe in crate::shortcuts::exes(root) {
            if let Some(dir) = exe_of_command(&exe).and_then(|e| e.parent().map(Path::to_path_buf)) {
                raw.push((dir, Source::SteamEntry, None));
            }
        }
    }
    for f in &game.module.existing.folders {
        if let Some(dir) = expand(f, install) {
            raw.push((dir, Source::UsualFolder, None));
        }
    }
    // The launcher in a Proton prefix shows up as C:\... in Steam's entries.
    let raw = raw.into_iter().map(|(d, s, v)| (to_local(&d, install), s, v));
    let mut out: Vec<Found> = Vec::new();
    for (dir, source, setup_version) in raw {
        if skip.is_some_and(|s| steam::same_folder(s, &dir)) || !is_mod_folder(game, &dir) {
            continue;
        }
        if let Some(have) = out.iter_mut().find(|f| steam::same_folder(&f.dir, &dir)) {
            have.setup_version = have.setup_version.take().or(setup_version);
            continue;
        }
        out.push(Found { version: version_from_files(&dir), dir, source, setup_version });
    }
    out
}

/// A Windows path seen from inside the game's Proton prefix (C:\Ember) as a
/// path on this Linux system; other paths unchanged.
fn to_local(p: &Path, install: Option<&GameInstall>) -> PathBuf {
    let s = p.to_string_lossy();
    match (cfg!(windows), install) {
        (false, Some(i)) if s.len() > 3 && s[..3].eq_ignore_ascii_case("c:\\") => {
            steam::proton_prefix(i).join("drive_c").join(s[3..].replace('\\', "/"))
        }
        _ => p.to_path_buf(),
    }
}

/// How many releases per channel are compared when the files name no version.
const LOOK_BACK: usize = 12;

/// Which release a copy is: the one its BUILD_INFO.txt names, else, among
/// the releases whose launcher is byte for byte the copy's, the one with the
/// most files the same (newest on a tie), so a damaged copy is still known.
/// `lists` holds GitHub's release list per channel, in catalog order.
pub async fn identify(
    http: &reqwest::Client,
    game: &Game,
    dir: &Path,
    lists: &[(Channel, Vec<GhRelease>)],
    cache: &Path,
) -> Result<Option<(String, Release)>, InstallError> {
    if let Some(v) = version_from_files(dir) {
        for (ch, list) in lists {
            if let Some(r) = releases::candidates(ch, list).into_iter().find(|r| r.tag == v || r.tag == format!("v{v}"))
            {
                return Ok(Some((ch.id.clone(), r)));
            }
        }
    }
    // Each local file is read once, however many releases list it.
    let mut local: HashMap<String, Option<(u64, u32)>> = HashMap::new();
    let mut same = |f: &PackageFile| {
        let have = *local.entry(f.path.clone()).or_insert_with(|| {
            let p = dir.join(&f.path);
            let size = std::fs::metadata(&p).ok().filter(|m| m.is_file())?.len();
            Some((size, package::crc_of(&p).ok()?))
        });
        have == Some((f.size, f.crc32))
    };
    let mut best: Option<(usize, String, Release)> = None;
    let mut tried = HashSet::new();
    for (ch, list) in lists {
        for r in releases::candidates(ch, list).into_iter().take(LOOK_BACK) {
            if !tried.insert(r.url.clone()) {
                continue;
            }
            let files = package::release_files(http, &r, cache).await?;
            if !files.iter().find(|f| f.path == game.module.launcher).is_some_and(&mut same) {
                continue;
            }
            let score = files.iter().filter(|f| same(f)).count();
            if best.as_ref().is_none_or(|(b, _, _)| score > *b) {
                best = Some((score, ch.id.clone(), r));
            }
        }
    }
    Ok(best.map(|(_, ch, r)| (ch, r)))
}

/// The copy's files compared with its release's package.
pub async fn check_against(
    http: &reqwest::Client,
    dir: &Path,
    release: &Release,
    cache: &Path,
) -> Result<package::Check, InstallError> {
    let files: Vec<PackageFile> = package::release_files(http, release, cache).await?;
    Ok(package::check(dir, &files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::Catalog;

    #[test]
    fn reads_the_release_line_with_or_without_bom() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(
            t.path().join("BUILD_INFO.txt"),
            "\u{feff}SF4 Ember Netplay\r\nRelease: 1.6.1-nightly20261116\r\n",
        )
        .unwrap();
        assert_eq!(version_from_files(t.path()).as_deref(), Some("1.6.1-nightly20261116"));
        std::fs::write(t.path().join("BUILD_INFO.txt"), "Release: 1.0 ; rm -rf\n").unwrap();
        assert_eq!(version_from_files(t.path()), None);
        assert_eq!(version_from_files(&t.path().join("nothing")), None);
    }

    #[test]
    fn commands_give_their_exe() {
        assert_eq!(
            exe_of_command(r#""C:\ember-ci\b119\stage\Launcher.exe" --join-link "%1""#),
            Some(PathBuf::from(r"C:\ember-ci\b119\stage\Launcher.exe"))
        );
        assert_eq!(exe_of_command("/usr/bin/steam -applaunch 1"), Some(PathBuf::from("/usr/bin/steam")));
        assert_eq!(exe_of_command("  "), None);
        assert_eq!(exe_of_command("\"unterminated"), None);
    }

    #[test]
    fn a_folder_counts_only_with_its_launcher_and_marker() {
        let c = Catalog::bundled();
        let (usf4, sfv) = (c.game("usf4").unwrap(), c.game("sfv").unwrap());
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("Launcher.exe"), "").unwrap();
        assert!(!is_mod_folder(usf4, t.path()));
        std::fs::write(t.path().join("Sidecar.dll"), "").unwrap();
        assert!(is_mod_folder(usf4, t.path()));
        assert!(!is_mod_folder(sfv, t.path()), "SF4's folder is not SFV's, though both have Launcher.exe");
    }

    fn gh(tag: &str, asset: &str, at: &str) -> GhRelease {
        serde_json::from_value(serde_json::json!({
            "tag_name": tag, "prerelease": true, "published_at": at, "html_url": "",
            "assets": [{"name": asset, "size": 100, "browser_download_url": format!("https://github.com/o/r/releases/download/{tag}/{asset}")}]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn identifies_by_build_info_or_by_matching_the_launcher() {
        let c = Catalog::bundled();
        let umvc3 = c.game("umvc3").unwrap();
        let ch = umvc3.module.channels[0].clone();
        let (a, b) = ("umvc3-ember-1.0.0-nightly.1.aaa.zip", "umvc3-ember-1.0.0-nightly.2.bbb.zip");
        let list = vec![gh("v1.0.0-nightly.2.bbb", b, "2026-11-18"), gh("v1.0.0-nightly.1.aaa", a, "2026-11-10")];
        let t = tempfile::tempdir().unwrap();
        let cache = t.path().join("cache");
        let dir = t.path().join("Programs/UMvC3 Ember Netplay");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("umvc3-ember-launcher.exe"), "old launcher").unwrap();
        std::fs::write(dir.join("umvc3-ember.dll"), "old dll").unwrap();
        // The release lists as fetched before (no network in this test).
        let entry = |p: &str, body: &str| PackageFile {
            path: p.into(),
            size: body.len() as u64,
            crc32: crc32fast::hash(body.as_bytes()),
        };
        for (asset, l, d) in [(a, "old launcher", "old dll"), (b, "new launcher", "new dll")] {
            let files = vec![entry("umvc3-ember-launcher.exe", l), entry("umvc3-ember.dll", d)];
            crate::install::write_atomic(
                &cache.join(format!("{asset}-100.json")),
                &serde_json::to_vec(&files).unwrap(),
            )
            .unwrap();
        }
        let http = crate::install::client();
        let lists = vec![(ch, list)];
        let (channel, r) = identify(&http, umvc3, &dir, &lists, &cache).await.unwrap().unwrap();
        assert_eq!((channel.as_str(), r.tag.as_str()), ("nightly", "v1.0.0-nightly.1.aaa"));
        assert!(check_against(&http, &dir, &r, &cache).await.unwrap().ok());

        // A damaged DLL: still this release, and the check names the file.
        std::fs::write(dir.join("umvc3-ember.dll"), "old dl!").unwrap();
        let (_, r2) = identify(&http, umvc3, &dir, &lists, &cache).await.unwrap().unwrap();
        assert_eq!(r2.tag, r.tag);
        assert_eq!(check_against(&http, &dir, &r, &cache).await.unwrap().changed, ["umvc3-ember.dll"]);
        std::fs::write(dir.join("umvc3-ember.dll"), "old dll").unwrap();

        std::fs::write(dir.join("umvc3-ember-launcher.exe"), "something else").unwrap();
        assert_eq!(identify(&http, umvc3, &dir, &lists, &cache).await.unwrap(), None);
        let c2 = check_against(&http, &dir, &r, &cache).await.unwrap();
        assert_eq!(c2.changed, ["umvc3-ember-launcher.exe"]);

        // A version in BUILD_INFO.txt wins without comparing files.
        std::fs::write(dir.join("BUILD_INFO.txt"), "Release: 1.0.0-nightly.2.bbb\n").unwrap();
        let (_, r) = identify(&http, umvc3, &dir, &lists, &cache).await.unwrap().unwrap();
        assert_eq!(r.tag, "v1.0.0-nightly.2.bbb");
    }

    #[cfg(not(windows))]
    #[test]
    fn finds_a_deck_install_in_the_prefix_once() {
        let c = Catalog::bundled();
        let usf4 = c.game("usf4").unwrap();
        let t = tempfile::tempdir().unwrap();
        let lib = t.path().join("lib");
        let install = GameInstall {
            appid: 45760,
            library: lib.clone(),
            dir: lib.join("steamapps/common/x"),
            exe: lib.join("steamapps/common/x/SSFIV.exe"),
            manual: false,
        };
        let ember = lib.join("steamapps/compatdata/45760/pfx/drive_c/Ember");
        std::fs::create_dir_all(&ember).unwrap();
        for f in ["Launcher.exe", "Sidecar.dll"] {
            std::fs::write(ember.join(f), "").unwrap();
        }
        std::fs::write(ember.join("BUILD_INFO.txt"), "Release: 1.6.1\n").unwrap();
        let found = find(usf4, Some(&install), None, None);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].source, found[0].version.as_deref()), (Source::UsualFolder, Some("1.6.1")));
        assert_eq!(to_local(Path::new(r"C:\Ember\ember-play.sh"), Some(&install)), ember.join("ember-play.sh"));
        assert!(find(usf4, Some(&install), None, Some(&ember)).is_empty(), "the managed folder is not listed again");
    }
}
